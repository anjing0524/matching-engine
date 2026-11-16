# Trading performance baseline

Hardware: Apple M4, 10 logical cores, macOS, optimized `bench` profile.
Benchmarks are un-pinned single-process runs; treat numbers as baselines, not
service SLOs. Re-measured 2026-11-06 after the best-price, slot-pop, and snapshot
optimizations; thermal/background load causes visible run-to-run variance.

## Reproduce

```text
cargo bench --bench trading_core_benchmark
cargo bench --bench trading_middleware_benchmark
cargo test --test combo_matching
cargo test --release --test trading_soak
cargo test --release --test trading_combo_soak
```

## Core command path

| Workload | Result |
|---|---|
| crossing 200k commands | ~2.2–2.7M cmd/s |
| resting 200k commands | ~1.5–2.0M cmd/s |
| middleware submit 200k | ~11–28M cmd/s |
| core consume 200k via shared ring | ~2.4–2.7M cmd/s |
| end-to-end TCP fan-out, 4k orders / 8 clients | ~95k order/s |
| end-to-end TCP fan-out, 64k orders / 32 clients | ~178k order/s |

Key fixes that moved these numbers:

1. best bid/ask updated incrementally instead of scanning the price bitmap per command
2. shared-memory slots popped into stack buffers instead of `Vec` allocation
3. `execute_with_plugins_into` appends events without a per-command `Vec`
4. snapshots iterate only populated price levels

## Combo path

| Workload | Result |
|---|---|
| combo queue | ~6.0–6.8M order/s |
| combo cancel | ~5.4–8.1M cancel/s |
| atomic two-leg fills | ~1.15–1.25M fill/s |

`ComboExecutor` prechecks aggregate side quantity in O(1), reuses rollback
snapshot buffers, and sweeps both leg books atomically.

## Soak

| Test | Workload |
|---|---|
| `trading_soak` | 1,000,000 core commands, 100k batches |
| `trading_combo_soak` | 1M combo queue/cancel + 50k atomic fills |
| `trading_combo_e2e_soak` | 10k combo commands through middleware ring |

Debug-build soak times are not throughput targets; they are stability checks.

## Known limits

- No CPU pinning / NUMA tuning in these runs.
- No long-running multi-hour soak.
- No TCP + multicast fan-out load test.
- Successful combo execution still snapshots both leg books for rollback.

## Post-refactor Fedora baseline (2026-11-09)

Machine: Intel i7-8559U, 8 logical CPUs, Rust 1.99.1.

```text
trading core crossing: 1,771,223 cmd/s
trading core resting:  1,037,213 cmd/s
middleware submit:      7,034,739 cmd/s
core consume:           1,920,480 cmd/s
combo queue:            7,624,631 order/s
combo cancel:           7,338,229 cancel/s
combo atomic fills:       926,210 fill/s
60s soak:               2,121,725 cmd/s
5-minute soak max RSS:    140,656 KB
30-minute soak:        4,097,200,000 commands, 2,276,170 cmd/s
30-minute max RSS:       140,632 KB
1-hour soak:            7,636,600,000 commands, 2,121,271 cmd/s
1-hour max RSS:           140,764 KB
```

These numbers include the kernel outcome projection, EventFanout, unified
connection state, book trial execution, and canonical snapshot payload changes.
Combo atomic fill throughput is lower than the pre-refactor copy/rollback
baseline because each fill now performs a non-mutating trial pass; the trade is
removal of whole-book copies and silent rollback.
