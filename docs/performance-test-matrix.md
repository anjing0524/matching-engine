# Performance test matrix

Environment: Fedora, 4 physical cores / 8 threads (CPU0-3 physical, CPU4-7 SMT
siblings). All commands run from the repo root. Remote CI is not used for these
profiles.

Every split-process script rebuilds all release binaries first. Earlier runs
that reused stale binaries or left busy-poll processes behind were discarded.

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

Split aggregate scripts:

```bash
scripts/run-split-aggregate.sh [members] [orders] [in-flight]
scripts/run-multi-gateway-aggregate.sh [gateways] [orders] [in-flight]
scripts/run-sharded-aggregate.sh [shards] [orders] [in-flight]
```

## Protocol-correct load workload

Mixed buy/sell commands can emit multiple private events per command, which makes
per-command ack accounting ambiguous. The throughput profiles therefore use:

```text
--buy-only
```

Each NewOrder rests and emits exactly one Accepted/Rejected private event.
This is an ingress/core workload, not a matching/trade workload.

## Valid results

### single shard, direct middleware

```text
scripts/run-sharded-aggregate.sh 1 50000 32   (USE_PROXY=0)
n=50000 rate=92835/s p50=291.48µs p99=562.86µs
```

### two shards, direct middleware

```text
scripts/run-sharded-aggregate.sh 2 50000 32   (USE_PROXY=0)
shard-0: rate=82178/s p50=319.76µs p99=567.65µs
shard-1: rate=66811/s p50=507.22µs p99=575.28µs
aggregate: 148989/s
```

Two shards scale to ~1.6x on this host. Each shard owns its middleware/core;
clients are pinned to SMT siblings, so this is a conservative 4C/8T number.

### core latency

```text
core only, 5M commands: p50=258ns p99=1549ns p999=3524ns
3000 contract books:    p50=303ns p99=7773ns p999=30894ns
```

## Rejected / invalid measurements

- **Stale binaries**: the `trading-middleware` binary still contained a reverted
  writer-thread implementation. Scripts now rebuild first.
- **Stale busy-poll processes**: timed-out tests left trading processes running
  and contaminated CPU/latency numbers.
- **Gateway proxy**: a clean per-shard number was not established; the proxy's
  client CPU placement was not fair on this host. Do not use the ~12.3k/s proxy
  number as a capacity result.
- **Batch private writes**: 24681/s vs 48740/s for the older path; reverted.
- **Per-connection writer threads**: 24681/s; reverted.
- Mixed buy/sell per-command private accounting: not usable for completion
  tracking without a protocol-aware client.

## Interpretation

```text
single shard direct:  ~93k/s
two shards direct:    ~149k/s aggregate
core-only matching:   ~1.5µs p99 (single contract)
```

1M aggregate would require roughly 7 such shards/hosts in this configuration.
That is a horizontal-scaling estimate, not a claim that 4C/8T is insufficient.

## Next measurement

1. Put clients on a separate host or non-SMT cores.
2. Measure the gateway proxy path with fair CPU placement.
3. Try 4 shards only on a machine with enough physical cores.
4. Use a protocol-aware client for matched/trade workloads; buy-only remains the
   ingress/core baseline.
