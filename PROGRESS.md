# Safe Rust Futures Matching Engine - 实施进度

本文档用于跟踪基于技术架构文档的撮合引擎实现进度。

## 阶段 1: 项目设置与核心数据结构

- [x] 初始化 Cargo 项目 `matching-engine`
- [ ] **任务**: 配置 `Cargo.toml`
    - [ ] 添加 `tokio` (full features)
    - [ ] 添加 `bytes`, `tokio-util` (codec feature)
    - [ ] 添加 `crossbeam-channel`
    - [ ] 添加 `parking_lot`
    - [ ] 添加 `bumpalo` (collections feature)
    - [ ] 添加 `serde`, `serde_json` (derive feature)
    - [ ] 添加 `tracing`, `tracing-subscriber` (env-filter feature)
    - [ ] 添加 `criterion` (dev-dependency)
    - [ ] 添加 `tikv-jemallocator` (optional, for release builds)
- [ ] **任务**: 定义核心协议 (`src/protocol.rs`)
    - [ ] `OrderType` (Buy, Sell)
    - [ ] `NewOrderRequest`
    - [ ] `CancelOrderRequest`
    - [ ] `TradeNotification`
    - [ ] `OrderConfirmation`
- [ ] **任务**: 实现订单簿数据结构 (`src/orderbook.rs`)
    - [ ] `OrderNode` 结构 (用于索引链表)
    - [ ] `OrderBook` 结构
        - [ ] `BTreeMap<Price, PriceLevel>`
        - [ ] `Vec<OrderNode>` 作为节点池
        - [ ] `Option<usize>` free list head
    - [ ] `add_order` 方法
    - [ ] `remove_order` 方法

## 阶段 2: 撮合引擎核心

- [ ] **任务**: 实现撮合逻辑 (`src/orderbook.rs`)
    - [ ] `match_order` 内部方法
    - [ ] 处理市价单与限价单逻辑
    - [ ] 生成成交记录 (`Trade`)
- [ ] **任务**: 实现引擎主循环 (`src/engine.rs`)
    - [ ] `MatchingEngine` 结构，包含 `OrderBook`
    - [ ] `run` 方法，监听来自 `crossbeam-channel` 的订单请求
    - [ ] 处理 `NewOrderRequest` 和 `CancelOrderRequest`
    - [ ] 将成交回报和订单确认发送到输出通道

## 阶段 3: 网络与集成

- [ ] **任务**: 实现网络服务 (`src/network.rs`)
    - [ ] `run_server` 函数，启动 TCP 监听
    - [ ] 使用 `LengthDelimitedCodec` 处理消息帧
    - [ ] 为每个连接创建一个任务，处理读写
    - [ ] 反序列化客户端请求，发送到撮合引擎通道
    - [ ] 接收引擎结果，序列化后发送回客户端
- [ ] **任务**: 组装应用 (`src/main.rs`)
    - [ ] 初始化 `tracing` 日志系统
    - [ ] 创建 `crossbeam-channel`
    - [ ] 在单独线程中启动 `MatchingEngine`
    - [ ] 在主线程中启动 `tokio` 运行时和网络服务

## 阶段 4: 测试与性能优化

- [ ] **任务**: 编写单元与集成测试
    - [ ] `orderbook.rs` 的单元测试
    - [ ] `engine.rs` 的集成测试
- [ ] **任务**: 编写基准测试 (`benches/`)
    - [ ] 使用 `criterion` 测试 `OrderBook::add_order` 的性能
    - [ ] 测试核心撮合循环的吞吐量
- [ ] **任务**: 实现负载生成器
    - [ ] 创建一个新的 binary (`src/bin/load_generator.rs`)
    - [ ] 模拟大量并发客户端发送订单
    - [ ] 测量端到端延迟 (p50, p90, p99) 和吞吐量 (orders/s)
- [ ] **任务**: 性能调优
    - [ ] 配置 `jemalloc`
    - [ ] 在 Release 模式下编译和测试
    - [ ] (可选) 线程绑定
