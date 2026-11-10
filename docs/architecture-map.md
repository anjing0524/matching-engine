# Architecture map

This crate ships two runtimes. They share low-level types but not a command path.
The legacy runtime is behind the `legacy` feature (enabled by default); the
exchange bins build with `--no-default-features`.

## 1. Exchange core (production)

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
CoreRunner → ExchangeCore
  │  private events / public events
  ▼
ExchangeRuntime
  ├─ GatewayRuntime private downlink
  ├─ EventFanout: result logs, repair buffer/server, market data
  └─ ComboRuntime (optional)
```

Key ownership rules:

- `ExchangeCore` owns the book, active order metadata, positions, frozen
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
- `ExchangeRuntime` composes the above from one `RuntimeOptions` value;
  `tick()` is the only driver.

Entry points:

- `exchange` — single process
- `exchange-middleware` — middleware + gateway
- `exchange-core` — core only
- `exchange-gateway` — standalone relay

## 2. Legacy persistent service

```
interfaces → application → domain ← infrastructure
```

This is the original single-book service with WAL, snapshots, standby
replication, and its own gateway. It is kept for compatibility and the
operations scripts under `scripts/`. Do not route exchange commands through it.

## Tests as interfaces

- `exchange::core` tests exercise the kernel directly.
- `exchange::exchange_runtime` tests exercise the composed tick.
- `exchange_binary_smoke` / `exchange_split_smoke` exercise process entry points.
- `exchange_soak` / `exchange_timed_soak` exercise long-running state.

## When adding a feature

1. Decide which runtime owns it; do not add a second path through the legacy
   stack.
2. Put business state in `ExchangeCore`; keep it deterministic and I/O-free.
3. Put output delivery in `EventFanout` or GatewayRuntime.
4. Add a test at the narrowest interface that can observe the change.
