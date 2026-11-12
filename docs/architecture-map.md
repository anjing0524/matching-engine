# Architecture map

This crate ships one production runtime: the trading core. Low-level order-book
and ring types are shared; there is no second command path.

## Module layout

```text
src/
├── domain/                 # order book
├── shared/                 # symbols, order protocol types, collections
├── bin/                    # trading-engine/core/middleware/gateway
└── trading/
    ├── core/               # kernel, auction, positions, plugins, events, protocol
    ├── gateway/            # frame, runtime, server, proxy, directory, auth, replay
    │                       # private_downlink: seq/replay/routing/batch delivery
    ├── transport/          # SPSC ring, middleware, CoreRunner/RegistryRunner, sessions, logs
    ├── market_data/        # publisher, tracker, repair, metrics
    ├── combo/              # legacy ComboEngine/queue/exec/runtime, market_data
    ├── snapshot/           # core and registry snapshot_file, replay
    └── runtime/            # engine composition, event fanout
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
RegistryRunner ────────────────► InstrumentRegistry
  │  ordinary Contract cores      │  Combo Pair books, shared leg cores,
  │                               │  leg margin, combo private fills
  │  private/public events        │
  ▼                               ▼
TradingRuntime
  ├─ GatewayRuntime private downlink (one per-user sequence for core + combo)
  ├─ EventFanout: result logs, repair buffer/server, market data
  └─ (optional legacy ComboRuntime, not used by the single-process binary)
```

Key ownership rules:

- `TradingCore` owns one Contract book, active order metadata, positions,
  frozen close, and auction staging. The kernel has no sockets or file I/O.
- `InstrumentRegistry` owns one ordinary Contract core per instrument and one
  base Combo Book per static Combo Pair. Legs are real Contracts, so combo
  matching consumes the same books as ordinary orders. The registry also owns
  static pair validation, leg margin reservations, and Combo Private Fills.
- `RegistryRunner` is the production single-process command seam. It routes
  `combo_id != 0` commands to `InstrumentRegistry::execute_combo` and all other
  commands to the ordinary Contract core named by `instrument_id`. The runtime
  composition root is `TradingRuntime::from_registry`.
- `CoreRunner` remains the split-deployment (`trading-core`) and test seam over
  one `TradingCore`. ADR-0001 keeps combo execution out of split deployment.
  `TradingRuntime::new` and the legacy `ComboRuntime` are compatibility paths;
  the single-process binary always builds `RegistryRunner`.
- `poll_events()` is the one event projection for both runners. A registry
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
- Snapshots use `snapshot_file`: version, CRC32, payload bytes, atomic replace.
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
