# Architecture map

This crate ships one production single-process runtime (`trading-engine`) and a
split core path (`trading-core`). Both enter command polling through
`VenueRunner`; a registry target owns combo execution, a core target stays
combo-free. See `docs/system-architecture.md` for the target model.

## Module layout

```text
src/
├── domain/                 # order book
├── shared/                 # symbols, order protocol types, collections
├── bin/                    # trading-engine/core/middleware/gateway
└── trading/
    ├── core/               # kernel, auction, positions, plugins, protocol
    │                       # combo_admission/book/journal/margin/pair
    │                       # domain_ids, order_identity, leg_liquidity
    │                       # private_events, public_events
    ├── gateway/            # frame, runtime, server, proxy, directory, auth, replay
    │                       # private_downlink: seq/replay/routing/batch delivery
    ├── transport/          # SPSC ring, middleware, VenueRunner (+ core/registry
    │                       # target bodies), sessions, logs
    ├── market_data/        # publisher, tracker, repair, metrics
    ├── snapshot/           # schema registry, snapshot_file, replay
    └── runtime/            # engine composition, snapshot envelope, event fanout
```

## 1. Trading core (production)

```
member TCP
  │  GatewayFrame (264B)
  ▼
GatewayRuntime ── thread-per-member accept/read + per-connection write lock
  │  sessions/auth/rate limit/reconnect replay
  │  Command (256B), globally sequenced by MiddlewareSequencer
  ▼
shared-memory command ring
  │  combo_id == 0                    combo_id != 0
  ▼                                      ▼
VenueRunner::registry ─────────► InstrumentRegistry
  │  ordinary Contract cores      │  Combo Pair books, shared leg cores,
  │                               │  leg margin, combo private fills
  │  private/public events        │
  ▼                               ▼
TradingRuntime
  ├─ GatewayRuntime private downlink (one per-user sequence for core + combo)
  ├─ EventFanout: result logs, repair buffer/server, market data
  └─ registry combo path (single-process production)
```

Key ownership rules:

- `TradingCore` owns one Contract book, active order metadata, positions,
  frozen close, and auction staging. The kernel has no sockets or file I/O.
- `InstrumentRegistry` owns one ordinary Contract core per instrument and one
  base Combo Book per static Combo Pair. Legs are real Contracts, so combo
  matching consumes the same books as ordinary orders. The registry also owns
  static pair validation, leg margin reservations, and Combo Private Fills.
- `VenueRunner` is the one runtime seam. It owns the selected target and the
  shared command-drain / event-transport output tail:
  - registry target: production single-process, routes `combo_id != 0` to
    `InstrumentRegistry::execute_combo`;
  - core target: split `trading-core`, one `TradingCore`, combo-free.
- `RegistryRunner` / `CoreRunner` remain the two target bodies behind
  `VenueRunner`; Phase 3 of ADR-0003 collapses their remaining duplication.
- Order identity is one `OrderIdentity` (account/session/client); order
  lifecycle status is one `OrderStatus`, with `PrivateStatus` as the wire alias.
- `poll_events()` is the one event projection for both targets. A registry
  poll returns core private/public events plus Combo Private Fills; transport
  flushing is internal.
- `GatewayRuntime` owns per-connection state (`ConnectionState`). `PrivateDownlink`
  owns the single per-user private sequence, replay retention, session routing,
  and batched delivery for core and combo private effects. `apply_effects()`
  sends redirects and replay, closes ended connections, expires idle sessions,
  prunes closed records, and returns counts; both drivers use it.
- `SessionPolicy` owns auth, rate-limit configuration, redirect choice, and the
  idle-expiry decision, so frame routing does not carry policy configuration.
- `MiddlewareSequencer` owns the `SessionRegistry`; session queries go through
  `sessions()`/`sessions_mut()`, not duplicated pass-through methods.
- `EventFanout` accepts one `EventFanoutOptions` value and exposes `emit`;
  result logs, repair, and market-data publishing stay inside the module.
- `GatewayServerHandle` exposes the frame-level `send` seam; typed frame
  constructors live on `GatewayFrame`, and the encoded batch path is internal.
- `EventFanout` owns public/private result logs, repair forwarding, market-data
  publishing, and their counters. `RepairServer` is the sole repair retention
  owner.
- Snapshots use `snapshot_file`: schema registry, version, CRC32, payload bytes, atomic replace. `TradingSnapshot` has a closed Core/Registry data enum.
- `TradingRuntime` composes the above from one `RuntimeOptions` value;
  `tick()` is the only driver.

Entry points:

- `trading` — single process
- `trading-middleware` — middleware + gateway
- `trading-core` — core only
- `trading-gateway` — standalone relay

## Tests as interfaces

- `trading::core` tests exercise the kernel directly.
- `trading::runtime` tests exercise the composed tick.
- `trading_binary_smoke` / `trading_split_smoke` exercise process entry points.
- `trading_soak` / `trading_timed_soak` exercise long-running state.

Module dependencies are acyclic: core has no runtime dependencies, snapshot and
transport depend on core, gateway/combo depend on transport/core, and runtime is
the composition root.

## Package layout decision

The project intentionally remains one Cargo package for now. See
[workspace-evaluation.md](workspace-evaluation.md) for the cycle analysis and
the conditions under which a three-crate workspace would be justified.

## When adding a feature

1. Keep one command path per deployment: `RegistryRunner` single-process, `CoreRunner` split; never run both in one process.
2. Put business state in `TradingCore`; keep it deterministic and I/O-free.
3. Put output delivery in `EventFanout` or GatewayRuntime.
4. Add a test at the narrowest interface that can observe the change.
