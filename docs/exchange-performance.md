# Exchange performance baseline

Hardware: Apple M4, 10 logical cores, macOS, optimized `bench` profile.
Benchmarks are un-pinned single-process runs; treat numbers as baselines, not
service SLOs.

## Reproduce

```text
cargo bench --bench exchange_core_benchmark
cargo bench --bench exchange_middleware_benchmark
cargo bench --bench exchange_combo_benchmark
cargo test --release --test exchange_soak
cargo test --release --test exchange_combo_soak
```

## Core command path

| Workload | Result |
|---|---|
| crossing 200k commands | ~2.5–2.7M cmd/s |
| resting 200k commands | ~1.8–2.0M cmd/s |
| middleware submit 200k | ~11–28M cmd/s |
| core consume 200k via shared ring | ~2.5–2.7M cmd/s |

Key fixes that moved these numbers:

1. best bid/ask updated incrementally instead of scanning the price bitmap per command
2. shared-memory slots popped into stack buffers instead of `Vec` allocation
3. `execute_with_plugins_into` appends events without a per-command `Vec`
4. snapshots iterate only populated price levels

## Combo path

| Workload | Result |
|---|---|
| combo queue | ~6.0M order/s |
| combo cancel | ~5.4–8.1M cancel/s |
| atomic two-leg fills | ~1.2–1.25M fill/s |

`ComboExecutor` prechecks aggregate side quantity in O(1), reuses rollback
snapshot buffers, and sweeps both leg books atomically.

## Soak

| Test | Workload |
|---|---|
| `exchange_soak` | 1,000,000 core commands, 100k batches |
| `exchange_combo_soak` | 1M combo queue/cancel + 50k atomic fills |
| `exchange_combo_e2e_soak` | 10k combo commands through middleware ring |

Debug-build soak times are not throughput targets; they are stability checks.

## Known limits

- No CPU pinning / NUMA tuning in these runs.
- No long-running multi-hour soak.
- No TCP + multicast fan-out load test.
- Successful combo execution still snapshots both leg books for rollback.
