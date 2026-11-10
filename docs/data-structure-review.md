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

Make the gateway replay path own one queue instead of rebuilding
`HashMap<u64, Vec<ReplayEvent>>` batches on every reconnect tick.
