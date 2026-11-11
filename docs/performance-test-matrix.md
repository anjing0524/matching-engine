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

Repeatability, three runs each:

```text
1 shard: 88827, 93209, 92566 /s   (median 92566)
2 shards:149236, 148107, 156361 /s (median 149236)
```

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

## Client concurrency vs server capacity

A single load client caps the observed rate near 100k/s; this is a client limit,
not a server limit. Multiple independent client processes against one
middleware/core shard:

```text
4 clients: 100837 + 76842 + 75549 + 83513 = 336741/s
6 clients: 55832 + 60284 + 65609 + 75520 + 63374 + 76818 = 397437/s

2 gateway proxies + 6 clients:
47958 + 45670 + 50889 + 49995 + 66443 + 50453 = 311408/s
```

So one middleware/core pair on CPU0-1 is not capped at ~95k/s. The measured
server-side aggregate is ~337-397k/s with clients on the SMT threads of the same
4C/8T host. This still is not a client-independent capacity number.

Gateway batch read was added (one `read` for up to 64 frames):

```text
before: ~92566/s, p99 562.86µs
after:   95675/s, p99 486.66µs
```

Batch private writes were retested with the corrected client:

```text
batch write:  47658/s
serial write: 95675/s
```

Still a regression; reverted.

## 4C/8T multi-client target (current)

Single-shard, one middleware + one core, buy-only, 6 independent clients:

```text
4 clients: 330195/s total
6 clients: 400128/s total
```

Batch gateway read helps single-client p99; batch encoded private writes are a
tradeoff:

```text
single client: batch write slower than inline
4 clients:     batch write 330195/s
6 clients:     batch write 400128/s
```

The batch is only useful when many responses are queued; single-frame batches
should stay on the inline path or be avoided.

Two gateways + 4 clients (middleware/core CPU0-1, gateways CPU2-3, clients on
SMT CPUs4-7), after switching the proxy to a 64KiB reusable relay buffer:

```text
205230/s -> 232335/s aggregate
p50 ~277-296µs, p99 ~1.2-1.3ms
```

Gateway proxy overhead is now the main gap versus the direct 330-400k path.

### splice rejected

Linux `splice` relay (zero-copy socket-to-pipe-to-socket) was tested on the same
2-gateway + 4-client setup:

```text
userspace 64KiB relay: 232335/s
splice relay:          235206/s
```

Only +1.2%, with unsafe Linux-only code and no p50 improvement; reverted.

### rejected: gateway event batching

Combining frames read in one syscall into a single `GatewayEvent::Frames` for
middleware:

```text
2 gateways + 4 clients: 226277/s
64KiB relay baseline:   232335/s
```

No improvement; extra Vec allocation and larger messages offset fewer channel
sends. Reverted.

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
