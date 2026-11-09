# Fedora target-machine test results

Date: 2026-11-09

## Machine

```text
Host: fedora
OS: Fedora 44, kernel 6.19.10-300.fc44.x86_64
CPU: Intel(R) Core(TM) i7-8559U @ 2.70GHz
Logical CPUs: 8
Rust: 1.99.1 (updated with `rustup update stable`)
Cargo: 1.99.1
```

Repository: `~/code/matching-engine`, cloned from `main` at `0827b96`.

## Test suite

- `cargo test --all-features`: pass
- `cargo test --release --all-features`: pass
- 1M command integration soak: pass

## Benchmarks

`cargo bench --bench exchange_core_benchmark`

```text
crossing: 200000 commands in 134.996ms -> 1,481,521 cmd/s
resting:  200000 commands in 210.644ms ->   949,468 cmd/s
```

`cargo bench --bench exchange_middleware_benchmark`

```text
middleware submit: 200000 in 27.949ms -> 7,155,832 cmd/s
core consume:      200000 in 114.543ms -> 1,746,063 cmd/s
```

`cargo bench --bench exchange_combo_benchmark`

```text
combo queue:    7,686,052 order/s
combo cancel:   7,746,653 cancel/s
atomic fills:   1,104,162 fill/s
snapshot only:  8 ns/snapshot
```

## Duration soak

A 5-minute soak initially grew to ~30 GB RSS and was OOM-killed. Root causes
were unfreed filled-order metadata and unbounded `CoreRunner` diagnostic
`CoreEvent` retention. After fixes:

```bash
EXCHANGE_SOAK_SECS=300 /usr/bin/time -v <timed_soak_binary> --ignored --nocapture
```

```text
5:00.05 wall time
99% CPU
Maximum resident set size: 155,700 KB
exit status: 0
```

A separate 60-second run after the fixes:

```text
128,400,000 commands in 60.03s -> 2,138,859 cmd/s
```

A 15-minute follow-up confirmed stable memory:

```text
15:00.02 wall time
99% CPU
Maximum resident set size: 155,744 KB
exit status: 0
```

A 30-minute detached run completed:

```text
3,995,200,000 commands in 1800.03s
≈ 2,219,519 cmd/s
Elapsed: 30:00.04
Maximum resident set size: 155,636 KB
RSS sampler range: 113,736–143,540 KB
exit status: 0
```

Post-refactor regression (kernel outcome projection, combo trials, unified
snapshot codec, EventFanout, connection state):

```text
636,600,000 commands in 300.04s
≈ 2,121,725 cmd/s
Maximum resident set size: 140,656 KB
exit status: 0
```

A 1-hour run completed after the server was reachable again:

```text
7,254,700,000 commands in 3600.04s
≈ 2,015,172 cmd/s
Elapsed: 1:00:00
Maximum resident set size: 155,524 KB
RSS sampler range: 108,096–143,380 KB (60 samples)
exit status: 0
```

Earlier 60-second run before the fixes was ~1.10M cmd/s and memory grew without
bound.

## TCP fan-out

```bash
EXCHANGE_LOAD_CLIENTS=32 EXCHANGE_LOAD_ORDERS=2000 \
cargo test --release --test exchange_tcp_load -- --ignored --nocapture
```

```text
64,000 gateway orders in 358.71ms -> 178,417 order/s
public packets received: 64,000 / 64,000
```

## Gateway split and real-NIC multicast

- `cargo test --release --test exchange_split_smoke` ✅
- `cargo test --release --test exchange_multi_gateway_smoke` ✅
  (two standalone gateway relays → one middleware/core)
- `EXCHANGE_MCAST_IFACE=10.92.252.146 cargo test --release --test multicast_nic -- --ignored` ✅
  (publisher/subscriber on Wi-Fi NIC `wlp0s20f3`)

## Docker build

Docker Hub metadata resolution timed out from Fedora. Workaround:

1. pull `rust:1.99-alpine` and `alpine:3.21` from `public.ecr.aws`
2. tag them as the Dockerfile names
3. `docker build --network host`

Result:

```text
exchange-runtime:fedora  27 MB
container smoke: gateway=0.0.0.0:8000 metrics=0.0.0.0:9100
exit status: 0
```

## Split-process smoke

```text
cargo test --release --test exchange_split_smoke
1 passed
```

Starts separate `exchange-middleware` and `exchange-core` processes over shared
ring files, logs in through the gateway listener, submits one order, and reads
the private event back through middleware.

## Notes

- This is a 4-core/8-thread laptop-class CPU, not a production server; numbers
  are lower than the Apple M4 baseline and should not be treated as capacity
  planning inputs.
- No container build or multi-host run was performed on this host.
- UDP fan-out was verified on loopback with a concurrently draining subscriber.
