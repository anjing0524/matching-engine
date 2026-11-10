# Linux handoff: build, deploy, and load-test

This is the target-machine checklist for the Linux server the user will provide.

## 1. Build

```bash
cargo build --release --bins
cargo test --all-features
docker build -f deploy/Dockerfile -t trading-runtime:linux .
```

### Docker Hub workaround (Fedora tested)

When Docker Hub is unreachable, pull the same images from ECR Public and tag
them locally, then build with host networking:

```bash
docker pull public.ecr.aws/docker/library/rust:1.99-alpine
docker pull public.ecr.aws/docker/library/alpine:3.21
docker tag public.ecr.aws/docker/library/rust:1.99-alpine rust:1.99-alpine
docker tag public.ecr.aws/docker/library/alpine:3.21 alpine:3.21
docker build --network host -f deploy/Dockerfile -t trading-runtime:fedora .
docker run --rm trading-engine:fedora --iterations 100
```

Verified on Fedora: image built (27 MB), container printed the gateway/metrics
addresses and exited cleanly.

Produces:

- `target/release/trading-engine` — single-process gateway+middleware+core
- `target/release/trading-middleware` — split middleware/gateway
- `target/release/trading-core` — split core
- `target/release/trading-gateway` — standalone gateway relay

## 2. Single-process smoke

```bash
target/release/trading-engine run \
  --listen 0.0.0.0:8000 \
  --metrics-listen 127.0.0.1:9100 \
  --iterations 0 \
  --command-log-dir /var/lib/trading/commands \
  --public-result-log /var/lib/trading/public.log \
  --private-result-log /var/lib/trading/private.log
```

Check:

```bash
curl -s http://127.0.0.1:9100/metrics | head
ss -ltnp | grep -E '8000|9100'
```

## 3. Split-process smoke

```bash
# terminal 1: creates rings, runs gateway+middleware
target/release/trading-middleware \
  --listen 127.0.0.1:8000 \
  --command-ring /run/trading/command.ring \
  --public-ring /run/trading/public.ring \
  --private-ring /run/trading/private.ring

# terminal 2: core opens the same rings
target/release/trading-core \
  --command-ring /run/trading/command.ring \
  --public-ring /run/trading/public.ring \
  --private-ring /run/trading/private.ring
```

## 4. Load tests (target hardware only)

```bash
TRADING_SOAK_SECS=3600 \
cargo test --release --test trading_timed_soak -- --ignored --nocapture

TRADING_LOAD_CLIENTS=32 \
TRADING_LOAD_ORDERS=2000 \
cargo test --release --test trading_tcp_load -- --ignored --nocapture
```

Capture:

- commands/s from timed soak
- TCP fan-out orders/s
- public delivery ratio
- CPU utilization and RSS
- `trading_tick_latency_seconds` histogram

## 5. Failure drill

1. `kill -9` core, restart with `--snapshot` and replay log
2. stop one gateway, confirm clients reconnect with `last_private_seq`
3. force multicast gap, confirm NACK repair
4. fill disk, confirm fail-fast behavior

## 6. Deferred external items

- CZCE product-level FOK/FAK attribute confirmation
- multi-host rolling deployment drill
- target-hardware multi-hour soak numbers
