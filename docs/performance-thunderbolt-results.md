# Thunderbolt Mac-client / Fedora-server benchmark

Date: 2026-11-16

## Topology

```text
Mac client                         Fedora server
bridge0 169.254.78.137  <--TB-->  thunderbolt0 169.254.140.240
```

- Mac Thunderbolt bridge reported the peer on a 20 Gb/s Thunderbolt link.
- ICMP RTT: min 0.8 ms, avg 3.0 ms, max 7.4 ms.
- Fedora: 4 physical cores / 8 threads.
- Server: `trading-middleware` pinned CPU0, `trading-core` pinned CPU1.
- Client: one `trading-load` process, 8 member threads, `--buy-only`.

## Commands

Fedora server:

```bash
taskset -c 0 trading-middleware \
  --allow-anonymous \
  --listen 0.0.0.0:18000 \
  --command-ring /tmp/tb-run/command.ring \
  --public-ring /tmp/tb-run/public.ring \
  --private-ring /tmp/tb-run/private.ring \
  --iterations 0 --idle-timeout-secs 0

taskset -c 1 trading-core \
  --command-ring /tmp/tb-run/command.ring \
  --public-ring /tmp/tb-run/public.ring \
  --private-ring /tmp/tb-run/private.ring \
  --iterations 0
```

Mac client:

```bash
trading-load --addr 169.254.140.240:18000 \
  --members 8 --orders-per-member 100000 \
  --in-flight 256 --buy-only
```

## Results

Single runs, 8 members:

| orders/member | in-flight | rate | p50 | p99 | p999 |
|---:|---:|---:|---:|---:|---:|
| 50k | 32 | 240k/s | 0.99ms | 1.81ms | 9.46ms |
| 50k | 64 | 410k/s | 0.99ms | 1.47ms | 68ms |
| 50k | 128 | 450k/s | 1.74ms | 6.11ms | 87ms |
| 50k | 256 | 647k/s | 2.45ms | 3.36ms | 80ms |
| 100k ×3 | 256 | 858 / 780 / 807k/s | ~2.0ms | 3.6–7.7ms | ~90ms |
| 100k | 512 | 727k/s | 3.73ms | 124ms | 168ms |
| 100k | 1024 | 868k/s | 6.30ms | 57ms | 95ms |

## Reading

- Thunderbolt removed the WiFi network as the primary bottleneck (WiFi was
  ~8k/s with 75ms average RTT).
- At in-flight 256 the server sustains roughly 0.8M orders/s with single-digit
  ms p99, but p999 is ~90ms: there is visible queueing/tail latency.
- Pushing in-flight to 512/1024 raises raw throughput but destroys p99; do not
  use those as a latency result.
- The Mac client is not CPU-pinned and the Thunderbolt bridge adds latency; this
  is an end-to-end remote-client result, not a server-only capacity claim.

## Tail-latency diagnostics (2026-11-16, later runs)

Same server, `trading-load --buy-only`, 100k orders/member unless noted.

### Member/process scaling (remote Mac client)

| clients | total orders | in-flight | aggregate rate | p50 | p99 | p999 |
|---|---:|---:|---:|---:|---:|---:|
| 1 member | 800k | 256 | 248k/s | 0.96ms | 1.43ms | 1.52ms |
| 2 members | 800k | 256 | 478k/s | 0.96ms | 1.68ms | 1.99ms |
| 4 members | 800k | 256 | 648k/s | 1.20ms | 2.23ms | 70ms |
| 4 separate processes, 1 member each | 800k | 256 | ~1.0M/s total | 0.8ms | 1.2ms | 47–75ms |
| 4 separate processes, 1 member each | 800k | 64 | ~556k/s total | 0.39ms | 0.50ms | 0.73–0.81ms |

The 47–75ms tail appears across independent client processes at
`in-flight=256`, while `in-flight=64` removes it. This points at a
session/batch/send-path effect rather than a single Mac thread or the
Thunderbolt link.

### Server pinning

| server layout | rate | p999 |
|---|---:|---:|
| middleware CPU0, core CPU1 | 457–518k/s | 18–68ms |
| unpinned | 690–827k/s | 17–65ms |

Pinning middleware/core to CPU0/1 reduced throughput and did not remove the
tail; gateway threads and Thunderbolt IRQs (IRQ 144–146, affinity 0–7) were
competing with the pinned processes.

### Slow-client probe

One client was `SIGSTOP`ped for 1s while a second client ran:

```text
victim: 178k/s, p50 0.47ms, p99 7.7ms, p999 9.0ms, max 51.7ms
stopped client max: 1008ms
```

A stopped client did not produce a 1s global stall, so the simplest
"blocking `write_all` HOL-blocks middleware" model is not confirmed. It did
slow the victim substantially.

### Open hypothesis

The tail is consistent with per-session private-event batching at
`in-flight=256` interacting with TCP receive/delayed-ACK or gateway write
blocking. Next probe should measure, on the server, per-batch
`write_all` duration (max/histogram) and per-session outbound queue depth at
the gateway seam; do not claim a fix before that data exists.
