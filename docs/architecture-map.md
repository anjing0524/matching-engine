# Architecture map

This crate ships one production runtime: the trading core. Low-level order-book
and ring types are shared; there is no second command path.

## Module layout

```text
src/
├── domain/                 # order book
├── shared/                 # symbols, SPSC ring, order protocol types
├── bin/                    # trading-engine/core/middleware/gateway
└── trading/
    ├── core/               # kernel, auction, positions, plugins, events, protocol
    ├── gateway/            # frame, runtime, server, proxy, directory, auth, session, replay
    ├── transport/          # ring, middleware, runner, command/result logs
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
GatewayRuntime ── sessions/auth/rate limit/reconnect replay
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
- `CoreRunner` drains commands and calls the kernel. It does not retain
  diagnostic `CoreEvent`s.
- `GatewayRuntime` owns per-connection state (`ConnectionState`) and assigns
  the single per-user private sequence used by both core and combo effects.
  `poll_effects()` returns redirects, replays, and session counts together.
- `EventFanout` owns public/private result logs, repair, market-data publishing,
  and their counters.
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

## Package layout decision

The project intentionally remains one Cargo package for now. See
[workspace-evaluation.md](workspace-evaluation.md) for the cycle analysis and
the conditions under which a three-crate workspace would be justified.

## When adding a feature

1. Keep trading as the only command path; do not reintroduce a second runtime.
2. Put business state in `TradingCore`; keep it deterministic and I/O-free.
3. Put output delivery in `EventFanout` or GatewayRuntime.
4. Add a test at the narrowest interface that can observe the change.
