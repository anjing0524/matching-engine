# Data-structure review

Date: 2026-11-10

## What changed

### 1. Shared mixed hashing for dense IDs

New module: `src/shared/collections/identity.rs`

```text
U64IdentityMap / U64IdentitySet   identity hashing
U64MixMap / U64MixSet             splitmix64 mixing
```

The order book already used identity hashing for `order_locations`. The kernel
and the hot gateway/session maps now use the mixed variant:

```text
TradingCore::orders
TradingCore::auction_meta
GatewayRuntime::connections
GatewayRuntime::next_private_seq
SessionRegistry::sessions
ReplayBuffer::per_user
```

Rejected: plain identity hashing for the kernel. Dense sequential IDs produced
poor hashbrown control-bit distribution and regressed the resting workload.
The splitmix64 finalizer avoids SipHash while keeping distribution.

### 2. Removed the parallel `public_orders` set

`OrderMeta` now carries `public: bool`; the kernel no longer maintains a second
hash set of resting public order IDs. This removes one hash lookup per fill and
cancel and one full-ID set from core memory.

`CORE_SNAPSHOT_VERSION` is now `5` because `OrderMeta` gained a persisted field.

### 3. Unified private replay buffers

`core` and `combo` replay buffers are one generic `ReplayBuffer<T>` keyed by a
`ReplaySequenced` trait, with producer aliases.

## Benchmark evidence

Fedora i7-8559U, `trading_core_benchmark`, sample-size 10.

| workload | before | after |
|---|---:|---:|
| crossing 200k | 1,902,807 cmd/s | 2,037,448 cmd/s |
| resting 200k | 1,107,047 cmd/s | 1,162,600 cmd/s |

A later run reached 2,230,429 cmd/s crossing and 1,230,076 cmd/s resting; the
gap is benchmark variance, not a second change.

The benchmark is a wall-clock harness, not a stabilised criterion benchmark;
treat the numbers as directional.

### 4. Removed persistent auction metadata

`auction_orders: Vec<AuctionOrder>` already holds user/side/offset/hedge, so the
persistent `auction_meta` map was removed. `uncross_auction` builds one transient
lookup for the duration of the uncross. `CORE_SNAPSHOT_VERSION` is now `6`.

### 5. Packed position keys

`PositionKey` and `CloseKey` now implement `Hash` by packing account/side/hedge/
offset into one `u128`; `Positions` and `FrozenClose` use `U128MixMap`. This
removes struct-field SipHash from every fill and reservation lookup.

Latest benchmark:

| workload | before all changes | after |
|---|---:|---:|
| crossing 200k | 1,902,807 cmd/s | 2,317,146 cmd/s |
| resting 200k | 1,107,047 cmd/s | 1,186,502 cmd/s |

### 6. Single replay queue and pending index

`ConnectionState` now holds one seq-ordered `Vec<ReplayEvent>` instead of
separate `Option<Vec<PrivateEvent>>` and `Option<Vec<ComboPrivateFill>>` fields.
`GatewayRuntime` also keeps `replay_pending: U64MixSet`, so a tick drains only
connections that actually have queued replay instead of scanning every
connection.

`take_replay_batches` now returns `Vec<(u64, Vec<ReplayEvent>)>`; the temporary
`HashMap<u64, Vec<ReplayEvent>>` is gone. `take_all_replays` was removed and the
middleware binary drains `take_replay_batches` directly.

### 7. Redirect pending index

Redirects are rare but `take_redirects` previously scanned every connection and
built a temporary `HashMap` on each tick. `redirect_pending: U64MixSet` now
indexes displaced connections; `take_redirects` returns `Vec<(u64, String)>`.

### 8. Session secondary indexes

`SessionRegistry` no longer scans all sessions for every count, user lookup, or
gateway drain. It now maintains:

```text
active_count / closed_count
by_user: U64MixMap<u64>          user_id -> active session
by_gateway: U64MixMap<Vec<u64>>  gateway_id -> active sessions
by_identity: U128MixMap<u128, u64> (member,user,seat) -> active session
closed_order: VecDeque<u64>      bounded pruning order
```

This makes `session_counts` and `find_active_by_user` O(1) and
`drain_gateway` O(sessions on that gateway) instead of O(all sessions).
`find_active_by_user` is on the private-dispatch path, so this also removes a
per-event scan. Login takeover resolves its `(member,user,seat)` key directly
instead of scanning every session.

### 9. Combo maps use the shared mixer

`ComboQueue::locations` and the combo engine's close/margin maps are keyed by
dense order/user IDs and now use `U64MixMap` instead of SipHash.

### 10. Idle expiry profile

An ignored test profiles 64k connections:

```text
cargo test --lib trading::gateway::runtime::tests::idle_expiry_64k_profile -- --ignored --nocapture
idle_expiry_64k: 64000 connections in 2.771373ms
```

Called once per `timeout/2` (7.5s for the default 15s timeout), that scan is
negligible in debug; a timing wheel is not justified. Keep the simpler scan.

### 11. Removed dead auction sequence

`auction_order_seq` was incremented and snapshotted but never read. It is
removed; `CORE_SNAPSHOT_VERSION` is now `7`.

### 12. Reused runtime event buffers

`TradingRuntime` now owns `private_events` and `public_events` buffers.
`TradingCore::drain_private_events_into` / `drain_public_events_into` append
into them instead of `mem::take`-ing a new Vec every tick; `drain(..)` leaves
the buffers' capacity in place for the next tick.

### 13. 64k gateway profile

Ignored test:

```text
cargo test --release --lib trading::gateway::runtime::tests::gateway_64k_profile -- --ignored --nocapture
```

Fedora result:

```text
gateway_64k: lookup=2.522362ms poll_1000=36.991µs dispatch=24.490622ms replay=1.949319ms
```

Interpretation:

- 64k `find_active_by_user` lookups: ~2.5ms total (~39ns each)
- 1000 `poll_effects` ticks with 64k connections: ~37µs total
- 64k private dispatch calls: ~24.5ms including server-send error paths
- 64k replay batches: ~1.95ms

The pending indexes removed the full scans as intended. The remaining dispatch
cost is mostly per-event delivery and allocation, not map lookup.

### 14. Per-connection TCP write locks

`GatewayServerHandle` previously held one global mutex over all connections and
wrote to the socket while holding it. Writes are now structured as:

```text
lock connection map -> clone Arc<Mutex<TcpStream>> -> unlock map
lock that connection -> write_all
```

The map lock is short; a slow client no longer blocks writes to every other
connection. The map itself uses `U64MixMap`.

Fedora TCP load after the change:

```text
TRADING_LOAD_CLIENTS=64 TRADING_LOAD_ORDERS=2000
128000 gateway orders in 749.588293ms (170760 order/s)
```

This is not a before/after benchmark; it confirms the path still runs at the
previous order of magnitude. The remaining structural limit is one reader thread
per connection, which needs an event-loop design rather than another map change.

### 15. Acceptance: per-connection write locks

Accepted. The connection map lock now covers only lookup/clone; the blocking
`write_all` runs under a per-connection mutex. A test with four writer threads
sending to one connection verifies that 400 frames remain intact.

Residual limits, intentionally not changed now:

- the connection map is still one mutex; do not shard it without a measured
  contention profile
- one reader thread per TCP connection remains the dominant structural limit
- `write_all` can still block a single connection's writer; a non-blocking
  writer queue is the next design step if that shows up in a real profile

### 16. Rejected: Linux mio event-loop gateway (superseded)

This was implemented to test a 64k-connection hypothesis, then reverted when the
actual topology was confirmed: several gateways, each facing only a few futures
firm members, all feeding one middleware. In that topology thread-per-member is
simpler and lower overhead; the event loop optimized a connection count that is
not part of the design. The measurements below are retained as historical data,
not as a recommendation.


The thread-per-connection gateway was replaced with one `mio` event-loop thread:

```text
Poll
├── listener token     accept
├── waker token        wake for writer commands / stop
└── connection tokens  non-blocking read + write queues
```

The handle sends frames through an mpsc command channel and wakes the loop;
each connection owns a bounded outbound `VecDeque<u8>`. The core is untouched.

Measurements on Fedora:

```text
4096 connections:  582µs, 4096 unique ids
8192 connections:  1.151ms, 8192 unique ids
16384 connections: 2.281ms, 16384 unique ids

128000 gateway orders: 187630 order/s (64 clients)
```

The earlier thread-per-connection load was 170760 order/s; treat the difference
as directional, not a controlled before/after benchmark.

Remaining limits:

- one event-loop thread per gateway process; shard by `SO_REUSEPORT` or run
  multiple gateway processes when one CPU saturates
- outbound queues are capped at 1 MiB per connection; overflow closes the
  connection
- io_uring is intentionally not used yet; it needs a syscall profile and a
  container seccomp decision first

### 17. Single-core latency profile

Synthetic core-only command latency (`Command → TradingCore`, no gateway,
middleware, sockets, or wall clock), release build, Fedora:

```text
CORE_LATENCY_ITERS=5000000
core_latency: n=5000000 p50=281ns p99=1707ns p999=3989ns max=178357173ns
core_latency: n=5000000 p50=281ns p99=1744ns p999=3768ns max=165342945ns
```

Interpretation:

- p99 is ~1.7µs for the synthetic limit-order path
- p99.9 is ~3.8µs
- 5M commands complete in roughly 2.5s (~2M cmd/s), so a 1M order/s target is
  within this synthetic single-core capacity
- the 165–178ms max values are scheduler/preemption or allocator outliers, not
  matching arithmetic; production needs an isolated CPU and realistic workload
- this is **core-internal latency**, not TCP wire-to-wire latency

Remaining caveats: one contract per `TradingCore`, no multi-contract routing,
no network stack, and no real order mix/plugin chain in this profile.

### 18. Multi-contract profile

3000 independent `TradingCore` books in one thread, narrow synthetic price ranges,
1M commands routed by `instrument_id`:

```text
CORE_MULTI_CONTRACTS=3000 CORE_LATENCY_ITERS=1000000
multi_contract: contracts=3000 n=1000000 p50=302ns p99=7517ns p999=28136ns max=163893ns VmRSS: 482952 kB
```

This is not a production multi-contract design; it only measures the cost of
routing across many books on one thread. Observations:

- p99 is ~7.5µs; p99.9 is ~28µs
- throughput is roughly 1.6M commands/s for this synthetic workload
- RSS is high because each core accumulates private/public event buffers that
  the profile never drains; a real runtime drains every tick
- each `TradingCore` is still single-contract; a multi-contract wrapper and
  sequence/event merge do not exist yet

### 19. End-to-end single-member latency

Profile: `tests/trading_latency_e2e.rs`

```text
LATENCY_ITERS=100000
e2e_latency: p50=7613ns p99=26545ns p999=62618ns max=338934ns

taskset -c 2,3 (two CPUs)
e2e_latency: p50=7619ns p99=22297ns p999=53269ns max=3001435ns
```

Boundary: TCP command write -> gateway -> middleware -> core -> private
response read, one member, one outstanding order, standard Linux TCP stack.

The p50 is single-digit microseconds, but p99 is ~22–27µs. This does **not**
meet a strict single-digit-microsecond p99 goal. CPU affinity helps the p99 but
does not remove the multi-thread handoff and kernel TCP cost. Kernel bypass is
explicitly out of scope for now.

### 20. Load + latency at target rates

`tests/trading_load_latency.rs` sends commands from member client threads while
the main thread drives the runtime; private responses are timestamped per
client order. The test refuses configurations that oversubscribe the host:
the in-process profile needs roughly `3 * members + 1` threads.

Single member, paced at ~100k/s:

```text
members=1 target=100000/s n=100000 elapsed=690.601234ms rate=144801/s
p50=6990591ns p99=24708451ns p999=24798999ns
```

Single member, unpaced burst:

```text
members=1 n=100000 elapsed=659.870668ms rate=151545/s
p50=386540086ns p99=526246687ns
```

Two members, unpaced burst (6-ish threads on an 8-thread host):

```text
members=2 n=100000 elapsed=729.621507ms rate=137057/s
p50=332948596ns p99=487310548ns
```

The earlier 16-member / 1M run is **invalid**: it used 16 sender threads and 16
reader threads plus runtime/gateway on an 8-thread host, so it measured CPU
oversubscription, not capacity. It must not be used as evidence that one process
cannot reach a target rate.

What the valid runs establish:

- single-process burst throughput is roughly 140k–150k order/s in this
  same-host harness
- at 100k/s paced, single-member end-to-end p99 is ~25µs
- an unpaced burst makes p99 backlog-dominated (hundreds of ms)
- 1M aggregate throughput is not measurable from this same-process harness;
  it needs an external load generator and / or multiple gateway/core hosts

### 21. Out-of-process latency and busy-poll find

`tests/trading_remote_latency.rs` starts `trading-engine` in a child process and
runs only the member client in the test process, avoiding same-process client /
runtime contention.

First run exposed a real production bug: the runtime binaries slept 1ms (or
100µs in split mode) in their tick loops. Remote p50 was ~1.1ms because of that
sleep alone. The runtime loops now busy-poll with `spin_loop()`.

After removing the sleeps:

```text
LATENCY_ITERS=100000, no CPU pinning
p50=6996ns p99=24844ns p999=35500ns

taskset -c 2,3
p50=3157ns p99=8937ns p999=24752ns max=2943106ns
```

So:

- busy-poll fixes the millisecond-scale poll floor
- unpinned p99 is ~25µs on the 4C/8T host
- with client and server pinned to two CPUs, p99 is ~8.9µs
- p99.9 remains ~25µs and max outliers remain milliseconds
- kernel bypass is still not used

### 22. Split-process single-member latency

`tests/trading_split_latency.rs` starts the real split topology:

```text
trading-gateway -> trading-middleware -> trading-core
```

and drives one member through the proxy.

```text
LATENCY_ITERS=100000, no CPU pinning
p50=32456ns p99=51061ns p999=206399ns max=2168395ns
```

The split path adds proxy relay, middleware/core process handoff, and shared
memory rings; p99 is ~51µs versus ~25µs for the single-process runtime.

Pinning all four busy-poll processes to two CPUs caused starvation and had to be
killed; it is not a valid measurement. Proper pinning needs one CPU/thread budget
per process, or separate hosts.

## Remaining data-structure candidates## Remaining data-structure candidates

1. **Event vectors**
   Production already uses `execute_with_plugins_discard`; `CoreEvent` vectors
   remain only on the test/API surface. Do not add more variants without a
   discard path.

2. **Gateway replay batching**
   `take_all_replays` / `take_replay_batches` build temporary maps and re-merge
   `ReplayEvent`s. A single downlink-owned queue would remove the temporary
   allocations, but reconnect is not hot.

## Next recommended step

Stop adding indexes unless a profile shows a hot scan. The remaining structural
debt is event materialisation (`CoreEvent` vectors on the test/API surface) and
snapshot `Vec<(K,V)>` encoding; both are cold-path concerns.
