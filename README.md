# Rust Futures Trading System Engine

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
CoreRunner → TradingCore
  │  private/public events
  ▼
TradingRuntime
  ├─ GatewayRuntime private downlink
  ├─ EventFanout: result logs, repair, market data
  └─ ComboRuntime (optional)
```

- `domain`: tick 价格订单簿，无 I/O。
- `trading::core`: 确定性业务状态机；订单、持仓、冻结、竞价、组合队列。
- `trading::gateway_*`: 会员连接、认证、session、限速、private replay。
- `trading::transport::middleware` / `transport`: 全局序号与共享内存 ring。
- `trading::runtime::event_fanout`: result log、repair、market data、计数。
- `trading::runtime`: 组合 runtime，`tick()` 驱动。

详细 ownership 与边界见 [docs/architecture-map.md](docs/architecture-map.md)。

## 命名

```text
crate:     trading-system-engine
lib:       trading_system_engine
binaries:  trading-engine / trading-core / trading-middleware / trading-gateway
```

## 构建与测试

```bash
cargo build --release --bins
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --check
```

Fedora 目标机验证结果见 [docs/trading-fedora-results.md](docs/trading-fedora-results.md)。

## 运行

单进程：

```bash
target/release/trading-engine run \
  --listen 0.0.0.0:8000 \
  --metrics-listen 127.0.0.1:9100 \
  --command-log /var/lib/trading/commands.log \
  --public-log /var/lib/trading/public.log \
  --private-log /var/lib/trading/private.log
```

拆分进程：

```bash
# 创建 rings 并运行 gateway/middleware
target/release/trading-middleware \
  --listen 127.0.0.1:8000 \
  --command-ring /run/trading/command.ring \
  --public-ring /run/trading/public.ring \
  --private-ring /run/trading/private.ring

# core 打开同一组 rings
target/release/trading-core \
  --command-ring /run/trading/command.ring \
  --public-ring /run/trading/public.ring \
  --private-ring /run/trading/private.ring

# 可选：独立 gateway relay
target/release/trading-gateway \
  --listen 127.0.0.1:8001 \
  --middleware 127.0.0.1:8000
```

## 快照

单进程完整快照：

```bash
trading-engine run --snapshot /var/lib/trading/runtime.json
```

split core 仅恢复 core：

```bash
trading-core --snapshot /var/lib/trading/core.json ...
```

`TradingSnapshot` 包含 core + 可选 combo；文件带 version、CRC32、原子替换。
见 [docs/core-snapshot.md](docs/core-snapshot.md) 与
[docs/trading-replay-runbook.md](docs/trading-replay-runbook.md)。

## Docker

```bash
docker build -f deploy/Dockerfile -t trading-engine .
docker run --rm trading-engine --iterations 100
```

## 文档

- [架构地图](docs/architecture-map.md)
- [协议](docs/trading-protocol.md)
- [核心架构](docs/trading-core-architecture.md)
- [生产配置](docs/trading-production-config.md)
- [运维](docs/trading-operations.md)
- [就绪清单](docs/trading-readiness.md)
- [验收报告](docs/trading-acceptance-report.md)
- [性能](docs/trading-performance.md)
- [Linux handoff](docs/trading-linux-handoff.md)
- [Crate split evaluation](docs/workspace-evaluation.md)
- [Data-structure review](docs/data-structure-review.md)
