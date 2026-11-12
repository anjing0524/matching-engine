# Core snapshot design

## Scope

`TradingCore::snapshot()` captures continuous-book and business state:

- contract spec and symbol
- resting orders in level/FIFO order
- next order id
- order metadata
- positions and frozen close reservations
- phase and trading day
- trade/private/public sequence counters
- last time tick and trading timestamp

It does **not** capture:

- call-auction staging (`auction_orders`, `auction_meta`)
- combo queues and shared leg books (captured by `RegistrySnapshot` for the
  registry seam; the legacy `ComboEngine` runtime uses `ComboEngineSnapshot`)
- ephemeral private/public event buffers

Auction staging (`auction_orders`, `auction_meta`, auction sequence) is included
in the core snapshot and round-tripped. Registry-backed combo state lives in
`RegistrySnapshot`, which covers the ordinary Contract cores (books and
positions), static pairs, base Combo Order books, leg margin reservations, and
the Combo Private sequence. The legacy `ComboEngineSnapshot` covers only the
`ComboEngine` queue, synthetic leg books, and its margin state.
`save_snapshot` writes the complete state without a separate drain step.

## Restore

`restore_snapshot` rebuilds the book via `TickBasedOrderBook::restore`, replaces
order metadata/positions/frozen reservations and auction staging, and clears
ephemeral event buffers. The rebuilt snapshot compares equal to the captured
snapshot in tests.

## Runtime snapshot shapes

`TradingRuntime::snapshot()` writes a versioned `TradingSnapshot` with two
mutually exclusive shapes:

- legacy/core runtime: `core = Some(CoreSnapshot)` and optional `combo`
  (`ComboEngineSnapshot`)
- production single-process registry runtime: `registry =
  Some(RegistrySnapshot)`, with `core = None` and `combo = None`

`TradingRuntime::restore_snapshot()` rejects a snapshot whose shape does not
match the runtime command path, so a restore cannot silently mix the legacy
single-core path with registry-backed combo state. The runtime snapshot schema
version is 8; older snapshots must be regenerated or replayed from the command
log. A registry restore also enables fail-closed combo margin: a restored venue
with Combo Pairs requires explicit account balances because balances are venue
configuration and are not part of `RegistrySnapshot`.

## Snapshot compatibility

| Snapshot | Version | Source |
|---|---:|---|
| `CoreSnapshot` | 7 | `trading/snapshot/mod.rs` |
| `RegistrySnapshot` | 4 | `trading/snapshot/mod.rs` |
| `TradingSnapshot` | 8 | `trading/runtime/engine.rs` |

These versions move independently; a change to any schema must update this
table and the corresponding reject-old-version tests.

## Registry-backed combo snapshots

`InstrumentRegistry::snapshot()` captures the M1 registry seam as one
`RegistrySnapshot`:

- every registered ordinary Contract as a `CoreSnapshot` (leg books, orders,
  positions, frozen close, core private sequence)
- static Combo Pair definitions and prior settlements
- each pair's base Combo Book: resting Combo Orders, per-book arrival sequence,
  and last Combo trade price
- the leg margin ledger: per-account/leg frozen and actual margin plus the
  freeze reservation carried by each open Combo Order
- the registry-owned Combo Private Fill buffer and next private sequence
- the venue public sequence shared by every ordinary Contract event and Combo
  trade

The registry owns one venue public sequence; each ordinary Contract core's own
`public_seq` is a core-local counter that the registry replaces when it drains
public events. Ordinary Contract events and Combo trades therefore reach the
publisher and repair log as one strictly increasing sequence with no duplicate
or regressing values. The registry snapshot carries that next venue sequence, so
a restore continues the stream instead of restarting it.

`InstrumentRegistry::restore_snapshot` is fail-closed and all-or-none. It
validates the whole snapshot before assigning any field and rejects explicit
inconsistency instead of repairing it: duplicate instruments, pairs, books, or
orders; a pair without its book; a book for an unknown pair; a pair leg or prior
settlement without its instrument; a next Combo Order id that would collide; a
resting Combo Order without a matching margin reservation; a reservation with no
resting order; frozen leg totals that disagree with the reservations; and a
Combo Private sequence that does not advance. Error variants are enumerated in
`RegistrySnapshotError` and `ComboMarginSnapshotError`.

`snapshot::registry` adds versioned persistence and replay:
`save`, `load`, and `replay_from_snapshot`. Files use
`REGISTRY_SNAPSHOT_VERSION` plus the shared CRC32 envelope. Replay restores the
snapshot and routes the remaining commands to the combo book when `combo_id` is
non-zero and to the ordinary core otherwise.

Reconnect replay is registry-scoped: `replay_combo_private_after(last_seq)`
returns buffered Combo Private Fills with `seq > last_seq`. The buffer is
drained by `drain_combo_private_events`, so already-delivered fills cannot be
replayed from it; full gateway/log replay remains outside this scope.

## Persistence

The current implementation also provides versioned JSON persistence:

- `snapshot_json()` / `restore_snapshot_json()`
- `save_snapshot(path)` writes a temporary file and renames it atomically
- `snapshot_file::load::<CoreSnapshot>(path, CORE_SNAPSHOT_VERSION)` rejects
  unknown versions

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
- Core snapshots include auction staging; registry-backed combo state is
  captured by `RegistrySnapshot`. A rejected `RegistrySnapshot` leaves the
  running registry unchanged.
