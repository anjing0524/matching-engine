# Rust Futures Exchange Core

一个面向商品期货的交易所核心：gateway 接入、middleware 全局排序、单写者 core
撮合、组合单、快照/重放、private replay、public multicast 与 NACK 修复。

## 架构

```text
member TCP
  │  GatewayFrame (264B)
  ▼
GatewayRuntime ── session/auth/rate limit/reconnect replay
  │  Command (256B), globally sequenced by MiddlewareSequencer
  ▼
shared-memory command ring
  │
  ▼
CoreRunner → ExchangeCore
  │  private/public events
  ▼
ExchangeRuntime
  ├─ GatewayRuntime private downlink
  ├─ EventFanout: result logs, repair, market data
  └─ ComboRuntime (optional)
```

- `domain`: tick 价格订单簿，无 I/O。
- `exchange::core`: 确定性业务状态机；订单、持仓、冻结、竞价、组合队列。
- `exchange::gateway_*`: 会员连接、认证、session、限速、private replay。
- `exchange::middleware` / `transport`: 全局序号与共享内存 ring。
- `exchange::event_fanout`: result log、repair、market data、计数。
- `exchange::exchange_runtime`: 组合 runtime，`tick()` 驱动。

详细 ownership 与边界见 [docs/architecture-map.md](docs/architecture-map.md)。

## 构建与测试

```bash
cargo build --release --bins
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

Fedora 目标机验证结果见 [docs/exchange-fedora-results.md](docs/exchange-fedora-results.md)。

## 运行

单进程：

```bash
target/release/exchange run \
  --listen 0.0.0.0:8000 \
  --metrics-listen 127.0.0.1:9100 \
  --command-log /var/lib/exchange/commands.log \
  --public-log /var/lib/exchange/public.log \
  --private-log /var/lib/exchange/private.log
```

拆分进程：

```bash
# 创建 rings 并运行 gateway/middleware
target/release/exchange-middleware \
  --listen 127.0.0.1:8000 \
  --command-ring /run/exchange/command.ring \
  --public-ring /run/exchange/public.ring \
  --private-ring /run/exchange/private.ring

# core 打开同一组 rings
target/release/exchange-core \
  --command-ring /run/exchange/command.ring \
  --public-ring /run/exchange/public.ring \
  --private-ring /run/exchange/private.ring

# 可选：独立 gateway relay
target/release/exchange-gateway \
  --listen 127.0.0.1:8001 \
  --middleware 127.0.0.1:8000
```

## 快照

单进程完整快照：

```bash
exchange run --snapshot /var/lib/exchange/runtime.json
```

split core 仅恢复 core：

```bash
exchange-core --snapshot /var/lib/exchange/core.json ...
```

`ExchangeSnapshot` 包含 core + 可选 combo；文件带 version、CRC32、原子替换。
见 [docs/core-snapshot.md](docs/core-snapshot.md) 与
[docs/exchange-replay-runbook.md](docs/exchange-replay-runbook.md)。

## Docker

```bash
docker build -f deploy/Dockerfile -t exchange-runtime .
docker run --rm exchange-runtime --iterations 100
```

## 文档

- [架构地图](docs/architecture-map.md)
- [协议](docs/exchange-protocol.md)
- [核心架构](docs/exchange-core-architecture.md)
- [生产配置](docs/exchange-production-config.md)
- [运维](docs/exchange-operations.md)
- [就绪清单](docs/exchange-readiness.md)
- [验收报告](docs/exchange-acceptance-report.md)
- [性能](docs/exchange-performance.md)
- [Linux handoff](docs/exchange-linux-handoff.md)
