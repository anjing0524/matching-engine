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

Two members, 500k commands, 32 in flight, gateway pinned to one CPU:

```text
n=500000 elapsed=12.585s rate=39730/s
p50=1.606ms p99=2.094ms
```

Reallocated: middleware CPU0-1, core CPU2, gateway CPU3, client CPU4-7:

```text
1 member in-flight=32:
n=100000 rate=90480/s p50=322.8µs p99=573.3µs

2 members in-flight=32:
n=500000 rate=54752/s p50=1.215ms p99=1.666ms
```

Allocating two CPUs to middleware improved the 2-member case, but the gateway
proxy still has one CPU and a thread pair per member. This is a single gateway
process with a proxy relay, not yet the intended multiple-gateway deployment.

The serial split path is in the expected 100k/s range, but the pipelined path
still collapses to ~40k/s and millisecond p99. That points at the generator or a
proxy/middleware queueing path, not at the core matching rate. It is not yet
evidence for or against 1M capacity.

### multi-gateway processes (shared middleware/core)

Script:

```bash
scripts/run-multi-gateway-aggregate.sh 2 50000 1
```

Fedora 4C/8T, middleware CPU0, core CPU1, gateways CPU2/3, clients CPU4/5.

Serial, two gateways, one member each:

```text
gateway-0: 78465/s
gateway-1: 31590/s
aggregate: 110055/s
```

Pipelined, two gateways, in-flight=32, original single-thread client:

```text
gateway-0: 18964/s p99=2.48ms
gateway-1: 18573/s p99=2.48ms
aggregate: 37537/s
```

After splitting writer/reader into separate client threads:

```text
gateway-0: 17514/s p99=1.95ms
gateway-1: 16766/s p99=2.91ms
aggregate: 34280/s
```

The threaded client did not improve the split path. The bottleneck is therefore
not the client's single-thread loop; it is CPU/queueing in the gateway/middleware
chain on this 4C/8T host.

The multi-gateway identity collision is fixed (unique member offsets). Serial
scales to ~110k aggregate, but the pipelined client path still collapses. The
pipeline generator remains the suspect, not the gateway process count.

### direct middleware listener (no gateway proxy)

To isolate the proxy, clients can connect directly to the middleware gateway
listener while middleware/core are separate processes.

```text
1 member, in-flight=1:  rate=90762/s p50=3671ns p99=11639ns
2 members, in-flight=1: rate=47010/s p50=6.4µs p99=41.5µs
2 members, in-flight=32:rate=42276/s p50=946µs p99=2.85ms
```

Two members do not scale linearly even without the gateway proxy. The combined
middleware gateway listener and middleware loop on one CPU is the leading
suspect; this is not a hardware-count conclusion yet.

### process observation

Under a two-member serial run with middleware pinned to CPU0 and core to CPU1:

```text
trading-middleware: 3-4 threads, 78-98% CPU
trading-core:       1 thread, ~99% CPU
```

The core process is also a busy-poll loop, so 99% CPU does not by itself mean it
is saturated by matching work. Perf counters show the core executing billions of
instructions in the sample, confirming that the poll loop is hot.

The next step is command/event instrumentation in the middleware and core loops;
CPU percentage alone cannot distinguish "busy polling" from "work saturated".

## Next measurement

Use an independent load-generator host/process and per-process CPU affinity:

1. one gateway with 1–2 members
2. independent middleware/core CPUs
3. measure one gateway's rate and p99
4. replicate gateway processes and verify aggregate
