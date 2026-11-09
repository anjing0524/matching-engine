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

Callers must drain combo queues before snapshotting. `save_snapshot` uses
`snapshot_checked` and refuses to write when combo queues are non-empty.
Auction staging (`auction_orders`, `auction_meta`, auction sequence) is included
in the snapshot and round-tripped.

## Restore

`restore_snapshot` rebuilds the book via `TickBasedOrderBook::restore`, replaces
order metadata/positions/frozen reservations, and clears ephemeral/auction/combo
state. The rebuilt snapshot compares equal to the captured snapshot in tests.

## Persistence

The current implementation also provides versioned JSON persistence:

- `snapshot_json()` / `restore_snapshot_json()`
- `save_snapshot(path)` writes a temporary file and renames it atomically
- `load_snapshot(path)` rejects unknown versions

Implemented corruption detection:

- CRC32 over the canonical snapshot payload
- checksum stored in the version envelope
- mismatches are rejected on both JSON restore and file load

Still required for production:

1. snapshot cadence and command-log segment pinning
3. replay from snapshot to the last sequenced command
4. startup validation before middleware admits new commands

## Failure policy

- Never serve a partially written snapshot.
- On restore failure, fail closed and require a full command-log replay.
- Do not snapshot while combo/auction state is non-empty.
