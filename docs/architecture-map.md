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
    ├── transport/          # SPSC ring, middleware, runner, session directory, logs
    ├── market_data/        # publisher, tracker, repair, metrics
    ├── combo/              # queue, engine, exec, runtime, market_data, margin
    ├── snapshot/           # snapshot_file, replay
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
  │
  ▼
CoreRunner → TradingCore
  │  private events / public events
  ▼
TradingRuntime
  ├─ GatewayRuntime private downlink
  ├─ EventFanout: result logs, repair buffer/server, market data
  └─ ComboRuntime (optional)
```

Key ownership rules:

- `TradingCore` owns the book, active order metadata, positions, frozen
  close, and auction staging. Combo queue/leg books live in `ComboEngine`. The
  kernel has no sockets or file I/O.
- `CoreRunner` drains commands and calls the kernel. `poll_events()` is the one
  event projection; transport flushing is internal.
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

1. Keep trading as the only command path; do not reintroduce a second runtime.
2. Put business state in `TradingCore`; keep it deterministic and I/O-free.
3. Put output delivery in `EventFanout` or GatewayRuntime.
4. Add a test at the narrowest interface that can observe the change.
