# Core snapshot design

## Scope

`ExchangeCore::snapshot()` captures continuous-book and business state:

- contract spec and symbol
- resting orders in level/FIFO order
- next order id
- order metadata
- positions and frozen close reservations
- phase and trading day
- trade/private/public sequence counters
- last time tick and exchange timestamp

It does **not** capture:

- call-auction staging (`auction_orders`, `auction_meta`)
- combination queues
- ephemeral private/public event buffers

Callers must drain combo queues and auction staging before snapshotting.

## Restore

`restore_snapshot` rebuilds the book via `TickBasedOrderBook::restore`, replaces
order metadata/positions/frozen reservations, and clears ephemeral/auction/combo
state. The rebuilt snapshot compares equal to the captured snapshot in tests.

## Persistence

The current snapshot is in-memory only. A production implementation needs:

1. a stable wire format (versioned, little-endian, explicit field ids)
2. checksum + atomic file replace
3. snapshot cadence and command-log segment pinning
4. replay from snapshot to the last sequenced command
5. startup validation before middleware admits new commands

## Failure policy

- Never serve a partially written snapshot.
- On restore failure, fail closed and require a full command-log replay.
- Do not snapshot while combo/auction state is non-empty.
