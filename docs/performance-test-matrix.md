# Performance test matrix

Environment: Fedora, 4 cores / 8 threads. All commands run from the repo root.
Remote CI is not used for these profiles.

## Categories

```text
correctness  fmt + clippy + workspace tests
core         single- and multi-contract core latency
e2e          in-process end-to-end single-member latency
remote       out-of-process client latency
split        gateway -> middleware -> core latency
load         two-member load/latency
```

Runner:

```bash
scripts/run-trading-profiles.sh <category>
```

## Latest results

### correctness

```text
trading-domain:         30 passed
trading-system-engine:  180 passed, 4 ignored
fmt + clippy clean
```

### core — single contract

```text
CORE_LATENCY_ITERS=5000000
p50=258ns p99=1549ns p999=3524ns max=160846191ns
```

### core — 3000 contract books, one thread

```text
contracts=3000 n=1000000
p50=303ns p99=7773ns p999=30894ns
VmRSS=483MB (event buffers not drained in this profile)
```

### in-process end-to-end

```text
n=100000
p50=7.6µs p99=26.5µs p999=62.6µs
```

### remote client, out-of-process runtime

```text
n=100000
p50=8.0µs p99=26.9µs p999=66.7µs max=1.98ms
```

CPU-pinned variant measured earlier:

```text
taskset -c 2,3
p50=3.2µs p99=8.9µs p999=24.8µs
```

### split process (gateway -> middleware -> core)

```text
n=100000
p50=29.6µs p99=48.6µs p999=73.9µs max=664µs
```

### load

Two members, unpaced burst:

```text
n=100000 elapsed=706.8ms rate=141483/s
p50=337ms p99=451ms
```

Two members, targeted 100k/s:

```text
n=100000 elapsed=817.1ms rate=122379/s
p50=40.3ms p99=80.1ms
```

The paced two-member p50 is distorted by busy-spin sender threads competing
with the runtime; do not use it as a production latency figure.

## Interpretation

- core-internal latency is single-digit microseconds
- single-member end-to-end p99 is ~25µs unpinned and ~9µs on two pinned CPUs
- split topology adds about another 20µs to p99
- single-process burst throughput is ~140k–150k order/s in this harness
- 1M aggregate is not measurable on this 4C/8T host with in-process clients
- busy-poll processes must be assigned separate CPU budgets; pinning all of
  gateway/middleware/core/client onto two CPUs starves them

### split aggregate (gateway -> middleware -> core)

Script:

```bash
scripts/run-split-aggregate.sh 4 250000 32
```

Fedora 4C/8T, processes pinned (middleware CPU0, core CPU1, gateway CPU2,
client CPU4-7).

Serial calibration, one member, one in-flight:

```text
n=50000 rate=110819/s p50=143ns p99=20925ns
```

Two members, 500k commands, 32 in flight:

```text
n=500000 elapsed=12.585s rate=39730/s
p50=1.606ms p99=2.094ms
```

The serial split path is in the expected 100k/s range, but the pipelined path
still collapses to ~40k/s and millisecond p99. That points at the generator or a
proxy/middleware queueing path, not at the core matching rate. It is not yet
evidence for or against 1M capacity.

## Next measurement

Use an independent load-generator host/process and per-process CPU affinity:

1. one gateway with 1–2 members
2. independent middleware/core CPUs
3. measure one gateway's rate and p99
4. replicate gateway processes and verify aggregate
