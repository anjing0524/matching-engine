# Trading functional acceptance report

Date: 2026-11-06

## Verified end-to-end

| Requirement | Evidence |
|---|---|
| gateway login + command | `runtime::order_flows...` |
| command ordering + core matching | `middleware_assigns_global_sequence...` |
| private accepted/fill/cancel | core private event tests |
| private ack + reconnect replay | `gateway_runtime` reconnect tests |
| auth token / member credentials / seats | `auth` + gateway tests |
| session takeover / redirect / drain | gateway runtime tests |
| public UDP multicast | `market_data` loopback multicast test |
| public NACK repair | `repair` + tracker tests |
| result/command logs | `command_log` / `result_log` tests |
| snapshot + CRC + replay | `core` snapshot/replay tests |
| full runtime snapshot | `trading_snapshot_file_roundtrip`, `binary_restores_runtime_snapshot` |
| combo snapshot/restore | `snapshot_roundtrips_combo_queue`, `full_snapshot_restores_combo_runtime` |
| reconnect churn pruning | `reconnect_churn_returns_to_zero_active_sessions` |
| 1-hour soak | 7.6366B commands, 2.121M cmd/s, 140,764 KB max RSS |
| combo queue + atomic fills | combo engine/execution tests |
| combo command ring + private/public | `runtime` combo e2e test |
| binary deployment smoke | `tests/trading_binary_smoke.rs` |
| CI two-job run | GitHub Actions run 42766069814: test + container, success |

The binary smoke test:

1. starts `trading-engine run` with ephemeral ports
2. logs in over gateway TCP
3. submits a limit order
4. receives a private event
5. scrapes `/metrics` and verifies counters/histogram
6. also writes a command-log segment

## Deferred / external

- Product-level FOK/FAK attribute confirmation with CZCE
- Runtime multicast interface hot-switch
- Multi-hour and many-gateway performance tests on the Linux server the user
  will provide
- Real multi-host rolling deployment drill

## Verdict

Functional core, gateway, middleware, combo, recovery, logging, metrics, and
single-process deployment paths are implemented and covered by tests. The
remaining blockers are external validation and target-hardware load testing,
not missing trading functionality in this repository.
