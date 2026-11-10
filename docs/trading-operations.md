# Trading operations runbook (current slices)

This runbook describes the processes implemented in this repository. It is not
a complete production deployment: persistent logs, metrics, and service units
are still open items.

## Process layout

| Process | Owns | Network / files |
|---|---|---|
| `gateway` | member TCP sessions, login, seat mapping, private downlink | TCP listeners |
| `middleware` | global sequencing, command ring, logs/index (slice), private routing, public multicast, NACK repair | TCP + UDP + shared memory |
| `core` | orders, books, trades, positions, phases | shared memory only |

Deployment order:

1. create/bind shared-memory command/public/private rings
2. start middleware (repair + multicast publisher)
3. start core consumer
4. start gateway TCP listener
5. open members

## Configuration

- See `docs/trading-production-config.md` for `TradingConfig`.
- See `examples/trading_production_config.rs` for a runnable example.
- Public multicast must select a real NIC via `multicast_interface` in production.
- `idle_timeout = None` disables gateway expiry; default is 30s.

## Planned gateway drain

Before stopping one gateway process:

1. call `GatewayRuntime::drain_gateway(gateway_id)`
2. close the returned TCP connections
3. clients reconnect through another gateway with `last_private_seq`
4. middleware replays retained private events after that sequence

Active traffic is not redirected automatically; the client/member is responsible
for reconnecting.

## Gateway failure

- Connection loss: middleware marks the session inactive on `Closed`.
- Client reconnect: login again with `last_private_seq`; middleware replays
  retained private events after that sequence.
- New commands from a closed session are rejected with `Unauthorized`.
- Idle sessions are expired and their TCP connections are closed.

## Core failure

- Core is a single failure domain; there is no distributed consensus.
- The current core has **no snapshot**; recovery requires replaying the
  command log from the last known state.
- A production runbook must add: snapshot cadence, log retention, replay
  validation, and a documented RTO/RPO.
- On restart, middleware must not admit new commands until replay is complete.

## Public market data

- Public events are fixed 128-byte packets over UDP.
- Consumers detect `market_data_seq` gaps and send a fixed NACK datagram.
- Repair is served from a bounded in-memory replay buffer.
- If repair cannot serve a gap, the consumer must disconnect and rebuild from a
  fresh sequence; there is no full snapshot stream.

## Failure policy

- Fail fast on malformed frames, ring corruption, or unrecoverable I/O.
- No per-message application ack; use private sequence ack/replay.
- Avoid automatic core failover; operator decides when to restart/replay.
- Keep gateway and middleware stateless enough to restart independently.

## Known operational gaps

- no service unit files or container images
- no metrics/tracing export for queue depth, NACKs, replay, latency
- no persistent core snapshot
- no disk-full / mmap-failure handling policy
- no multi-gateway session takeover policy
