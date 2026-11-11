# Throughput test plan and targets

## Target topology

```text
1 middleware process
1 core process
2 gateway processes
N external client processes
```

CPU allocation on the 4C/8T host:

```text
CPU0: middleware
CPU1: core
CPU2: gateway-0
CPU3: gateway-1
CPU4-7: clients only when no better host is available (SMT siblings)
```

A valid aggregate number requires all of:

1. no stale trading processes
2. release binaries rebuilt from the current source
3. clients as independent processes, not one client process
4. client CPU not shared with middleware/core if the number is claimed as
   headroom
5. buy-only workload or a protocol-aware client for multi-event responses
6. at least three runs; report median, not the best run

## Workloads

- `--buy-only`: one resting order, one Accepted/Rejected private event per
  command. Use for ingress/core throughput.
- mixed buy/sell: multiple private events per command; needs a protocol-aware
  completion model, not one ack per command.

## Current results

```text
core only:            ~2.1M cmd/s, p99 ~1.5µs
direct middleware:    432k/s with 8 independent clients
direct middleware:    400k/s with 6 independent clients
2 gateways:           232k/s with 4 clients
2 gateways + splice:  235k/s (rejected: +1.2%, unsafe)
```

The 8-client run is the current observed server ceiling on this host:
432065/s. More clients only oversubscribe the remaining SMT threads.

The direct path and gateway path use the same middleware/core; the difference is
the two gateway relay processes.

## Validity failures already found

- single client caps near 100k/s: client limit, not server limit
- 4 clients through two gateways with a WiFi client host: ~8k/s; network RTT
  11-75ms dominates
- 4C/8T cannot host 2 middleware/core shards plus clients without SMT sharing
- batch private writes regress single-client latency but help multi-client
  aggregate; keep the adaptive path
- gateway event batching and single-thread busy-poll proxy were both slower

## 1M requirement

At the current single-middleware design, 1M/s requires approximately:

```text
2.5x the current direct 400k/s
4.3x the current 2-gateway 232k/s
```

This cannot be validated on the current single 4C/8T host. It needs either:

1. more physical cores for gateway/middleware, or
2. a client host with wired sub-100µs RTT and independent CPUs, or
3. a redesigned batched gateway/middleware I/O path.

Recording a 1M result from the current WiFi + single-host setup would be invalid.
