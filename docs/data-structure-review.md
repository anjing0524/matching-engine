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

## Remaining data-structure candidates

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
