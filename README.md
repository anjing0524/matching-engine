# Rust 高性能期货撮合引擎

一个以**正确性与持久性优先**的期货撮合引擎：撮合核心是单线程、无锁、预分配的
价格数组；所有被接受的订单与撤单先写入 WAL，再应用到内存订单簿；恢复时严格重放
WAL，校验 CRC 与序列号连续性，任何损坏都会显式报错，而不是静默忽略。

## 设计原则

- **先持久化，后确认**：订单/撤单先追加 WAL 并应用订单簿；批量 commit 的 flush/fsync 成功后才回复客户端。
- **热路径无序列化框架**：网络与 WAL 使用固定的紧凑二进制编码（`shared::wire`），
  不依赖 bincode/serde；serde 仅用于 JSON 工具与测试。
- **恢复即验证**：先加载最近快照，再重放尾部 WAL；快照/WAL 损坏、截断、序列号缺口都会使启动失败，不做"尽力恢复"。
- **没有伪默认值**：领域方法不提供返回 `None`/`Err("not implemented")` 的默认实现。
- **静态分发、少分配**：价格层使用侵入式双向链表（O(1) 撤单）和位图找最优价；
  网络连接是具体类型（无 `Box<dyn Connection>`），收发复用缓冲区，热路径不做动态分发。
- **明确的容量边界**：价格区间与每档队列容量在启动时确定；超出边界的行为有文档、有日志。

## 核心能力

| 能力 | 说明 |
|---|---|
| 撮合核心 | Tick 价格 → 数组下标 O(1)，`FastBitmap` + `POPCNT/TZCNT` 查找最优价 |
| 价格时间优先 | 同价位 FIFO，跨价位按价格优先 |
| 撤单与查询 | 校验订单归属；支持剩余数量/部分成交状态查询；撮合返回 `Filled`/`Rested`/`Rejected` 显式结果 |
| 持久化 | CRC32 校验、长度分帧、按大小轮转的 WAL；快照 + 尾部 WAL 恢复；确认前 flush + 可选 fsync |
| 崩溃恢复 | 从头重放 WAL，严格校验 CRC、截断与序列号连续性 |
| 网络 | 阻塞 `TcpConnection`/`TcpTransport`，每连接一个 OS 线程；`u32` 大端长度前缀 + 自研紧凑二进制编码；无 async runtime |
| 可观测性 | Prometheus `/metrics`、`/health`、`/health/ready`、`/health/live`（默认 9090 端口） |


## 运行时边界

本仓库包含两套运行时：

- **exchange**：期货交易所核心，gateway/middleware/core 拆分，生产路径。
  入口：`exchange`、`exchange-middleware`、`exchange-core`、`exchange-gateway`。
- **legacy persistent service**：原有单订单簿 + WAL + standby 服务，
  入口：`matching-engine`、`gateway`、`standby` 等。

两套运行时共享基础类型，但命令路径独立。新增交易所功能只改 `src/exchange`；
不要把 exchange 命令接入 legacy 路径。完整边界见 [docs/architecture-map.md](docs/architecture-map.md)。

## 快速开始

要求 Rust 1.99+（edition 2024）。

```bash
# 构建
cargo build --release

# 启动单品种持久化撮合服务（默认监听 127.0.0.1:8080）
cargo run --release -- --data-dir ./data --symbol BTCUSDT

# 两进程模式：独立撮合进程 + 独立事件驱动 gateway
cargo run --release --bin matching-engine -- --engine-uds ./data/engine.sock --data-dir ./data --snapshot-interval 0
cargo run --release --bin gateway -- --listen 127.0.0.1:8080 --engine-uds ./data/engine.sock

# 查看全部参数
cargo run --release -- --help

# 只打印配置，不启动服务
cargo run --release -- --dry-run
```

CLI 参数：`--host`、`--port`、`--queue-capacity`、`--log-level`、`--dry-run`、
`--metrics-port`（默认 9090，0 表示关闭）、`--wal-sync`（默认 true）、`--data-dir`、
`--symbol`、`--min-price`、`--max-price`、`--tick-size`、`--max-connections`、
`--idle-timeout-secs`、`--commit-batch-size`（group commit）、`--dedup-cache-size`（幂等缓存）、
`--max-in-flight`（每连接 pipelining 深度，默认 1）；gateway 另有 `--io-threads`（固定 event-loop 线程数，默认 1）、`--snapshot-interval`（默认 0 关闭，避免撮合线程等待快照 barrier）、`--busy-poll`（撮合线程空转等待命令）、`--async-fsync`（WAL flush 在撮合线程、fsync 在独立线程；快照前走 durability barrier）、`--matching-core`
（需要以 `--features cpu-affinity` 构建）。

示例客户端/服务端：

```bash
cargo run --example network_server
cargo run --example network_client
```

## 架构

依赖单向流动：

```text
interfaces → application → domain ← infrastructure
```

```text
src/
├── domain/
│   └── orderbook/tick_based.rs   # 撮合核心（侵入式链表 O(1) 撤单）
├── application/services/
│   ├── matching_runtime.rs       # 撮合 actor：group commit、回执顺序、fatal/lifecycle
│   ├── persistent_service.rs     # 单订单簿 + 同步 WAL + 严格恢复 + 撤单
├── infrastructure/
│   ├── network/                  # 阻塞 TCP 传输 + wire framing（无 dyn/async runtime）
│   ├── persistence/              # snapshot + order journal + WAL/恢复内部实现
│   └── observability/            # 健康检查与 Prometheus HTTP 服务
├── interfaces/
│   ├── cli/                      # 配置/装配/信号
│   ├── engine_ipc.rs             # engine 侧 Unix socket IPC（gateway 转发协议）
│   ├── gateway.rs                # 单进程阻塞 gateway（兼容路径）
│   └── session.rs                # 单进程阻塞 session（兼容路径）
├── bin/
│   ├── gateway.rs                # 独立事件驱动 gateway（mio，固定线程数）
│   └── load_generator.rs
└── shared/                       # 协议、SymbolId intern、FastBitmap、纳秒直方图、时间戳、metrics
```

### 撮合与持久化路径

1. 客户端消息经 `u32` 长度前缀分帧、解码为 `ClientRequest`（`PROTOCOL_VERSION` + `request_id` + `ClientMessage`）；响应通过 `ServerResponse` 回显 `request_id`。
   Session 默认单 in-flight（最低延迟）；`--max-in-flight > 1` 时 reader 只负责收包/提交，
   独立 writer 按 request_id 顺序回包，形成有界 pipelining。
2. `NewOrderRequest.client_order_id` 非 0 时先查幂等表；重复请求不会再次执行。
3. `MatchingRuntime` 的撮合线程按 `--commit-batch-size` 聚合命令；
   `PersistentMatchingService` 通过 `Journal::append` 写 `WalEntry`，再调用订单簿，
   每批一次 `Journal::commit`（flush/fsync）后才回执。
4. 生成的成交与挂单确认写入 WAL，并返回给客户端。
5. 重启时先加载最近的 `snapshot.bin`（如果存在），恢复订单簿/next ids/幂等 key，
   再由 `Journal::recover` 校验 CRC/序列号并重放 snapshot 之后的 WAL 尾段；
   仅允许截断最后一个未写完的记录，其余损坏均拒绝启动。
6. 撤单通过 `order_id -> slab node` 索引直接摘链，O(1)，不再重建价位队列。

### 进程边界：gateway 与 engine

部署上推荐两个进程：

```text
client TCP ──► gateway 进程（mio event loop，固定线程数）
                    │ Unix domain socket
                    ▼
              engine 进程（单写者撮合 + WAL）
```

- **gateway**：一个 `mio` event loop 管理 listener、上游 IPC 和所有客户端 fd；
  每连接只有状态机/读写缓冲，不为连接创建线程；慢客户端受每连接写缓冲和
  `--max-in-flight` 限制，不会反压撮合线程。
- **engine**：`--engine-uds` 启动后不监听客户端 TCP，只接受 gateway 的 IPC；
  engine 模式把 WAL writer 移交给独立 journaler 线程：撮合线程只把带序号的
  `WalRecord` 放入有界 staging/channel，由 journaler 负责写盘/flush/fsync；
  回执仍等待 journaler 提交。快照状态由撮合线程只做内存构建，独立 snapshotter
  等待 journaler 的 durable seq 后再写文件，撮合线程不做快照 I/O 或 barrier。
- 单进程 `matching-engine --port` 路径保留用于测试/兼容，但生产部署应使用两进程。

当前边界：撮合线程仍做订单簿状态转移和内存 staging，不碰文件/网络系统调用；
journal channel 满时会有界背压（不会无界增长）。snapshotter 写完快照后会通知
journaler rotate + prune 已覆盖 WAL 文件，因此 decoupled 模式的 WAL 也会收敛。

### 主备复制（warm standby）

engine 可以额外启动 WAL shipping：

```bash
# primary
cargo run --release --bin matching-engine -- --engine-uds ./data/engine.sock   --replica-listen ./data/repl.sock --data-dir ./data --snapshot-interval 0

# standby（必须在 primary 开始接收订单前连接）
cargo run --release --bin standby -- --primary-uds ./data/repl.sock   --data-dir ./standby-data --wal-sync true
```

primary journaler 在 WAL commit 后把已提交记录发给 standby；standby 只持久化
WAL。故障时停止 standby，用 `matching-engine --data-dir ./standby-data` 恢复并
人工提升。

自动化演练：

```bash
./scripts/replication-drill.sh
./scripts/gateway-reconnect-drill.sh
./scripts/failover-drill.sh
./scripts/auto-failover-drill.sh
./scripts/lease-fencing-drill.sh
./scripts/standby-catchup-drill.sh
```

当前限制：

- 没有生产级自动 failover；`scripts/auto-failover-drill.sh` 只是“最后已知健康状态 +
  watchdog 提升”的演练；
- 新增本地 `--lease-file` 独占锁：primary 持有 lease 时第二个 engine 拒绝启动，
  primary 崩溃后 OS 释放 lease，promotion 可获取。它只提供**本机/共享文件系统**
  的 fencing，不是分布式 lease/共识；
- 已有 bootstrap catch-up：迟到 standby 会收到 snapshot + WAL 前缀；如果 WAL 前缀
  已被裁剪且没有 snapshot，primary 会拒绝 bootstrap；
- standby data directory 目前要求为空/全新，不能把一个已有 WAL 的目录作为 bootstrap
  目标；
- bootstrap 队列在极端高流量下仍可能丢弃过慢的新 standby；需要更强的快照/追平控制面；
- primary 的 replication channel 是异步有界队列，如果 standby 长期落后，可能丢弃
  副本而不是阻塞撮合；监控 `matching_engine_replication_lag_records`、
  `matching_engine_replication_connected`、`matching_engine_replication_dropped_batches`；
  `/health` details 也会返回这些字段，提升前应要求 lag=0、dropped=0、connected>=1；
- standby 每个 batch commit 后回 ack；primary 用 ack 计算 lag，但还没有自动 fencing；
- 没有复制确认、票据 fencing 或 leader election。

### 为什么去掉 Tokio / 不用 `Box<dyn Connection>`

结论：**Tokio 已从整个 crate 移除**，网关和观测端点都是阻塞 OS 线程，撮合核心本来就不依赖
runtime。理由：

- 撮合核心是同步单写者，Tokio 只服务网络/观测，却引入 future 轮询、waker、任务调度和
  mpsc/oneshot 唤醒；对这些连接级请求/响应路径是额外复杂度，不是必要能力。
- 网关的客户端会话数是有界的（默认 `--max-connections 1024`），每连接一个线程在这个规模下
  简单、可预测；`Gateway` 负责 accept/连接注册/停机唤醒，`Session` 负责分帧与回执，
  CLI 只剩配置和装配。没有每消息 Future 装箱，也没有 async worker 上的阻塞 fsync。
- 连接线程通过有界 `crossbeam` channel 提交命令、通过有界 reply channel 等结果；撮合线程
  仍是唯一写者，语义不变。
- 代价：线程数随连接数增长（栈大小 256 KiB），>数百/上千会话时应换成 epoll/io_uring
  事件循环；这是当前明确的扩展边界，不是隐藏行为。
- 具体类型 `TcpConnection`/`TcpTransport` 替代 `Box<dyn Connection>`：`recv`/`send` 可内联，
  `recv` 复用连接内缓冲区，稳态无每消息分配。若将来需要第二后端，优先泛型静态分发。

### 其他被移除的动态/共享开销

- **CLI 不再用 `Arc<Mutex<Service>>`**：`MatchingRuntime` 拥有独立 OS 线程、命令队列、
  group commit 和回执顺序；`Gateway` 拥有 accept/registry/limit/shutdown，
  `Session` 只看到 `MatchingHandle::submit_order/submit_cancel`。
  无共享撮合锁；默认模式 fsync 仍在撮合线程，`--async-fsync` 可把 fsync 移到独立线程。
- **WAL 不再使用 `Arc<Mutex<BufWriter>>`**：单写者模型，`BufWriter` 和 `AtomicU64`
  直接放在 `WalWriter` 中，每记录少一次互斥锁和原子引用计数。
- **健康状态不再用 `RwLock`**：三种状态用 `AtomicU8`；连接计数也不再套 `Arc`，
  使用 `LazyLock<MetricsCollector>` 静态实例。
- **SymbolId 替代 `Arc<str>`**：wire 上仍传 symbol 字符串，decode 时借用帧字节并 intern 成
  `SymbolId(u32)`；订单簿/成交只传 id，不再有每笔 `Arc` 分配/引用计数。
- **Prometheus 标签不再每笔订单解析**：服务启动时解析一次 symbol 相关 label，
  缓存 `Counter`/`Histogram` handle。
- **订单位置表使用恒等哈希**：key 是本订单簿生成的连续 `u64` order id，用
  `BuildHasherDefault<OrderIdHasher>` 取代 SipHash，撮合/撤单少一次哈希计算。
- 仍保留的动态结构是确实需要的：分区服务的 `HashMap<SymbolId, OrderBook>`、
  crossbeam 通道与 Prometheus 注册表；symbol intern 表本身无界但品种数很少。

### 延迟指标（交易系统视角）

交易系统不关心平均值，关心**分布尾部**：

- `matching_engine_request_latency_nanoseconds`：从命令入队到撮合线程回复的端到端延迟
  （包含 actor 排队 + 撮合 + WAL group commit），按 symbol 标注；
- `matching_engine_actor_queue_wait_nanoseconds`：命令在撮合线程队列中的等待时间；
- `matching_engine_matching_latency_nanoseconds`：仅订单簿撮合；
- `matching_engine_wal_latency_nanoseconds`：WAL append（写入缓冲）耗时；
- `matching_engine_commit_latency_nanoseconds`：每批 group commit 的 flush/fsync 耗时；
- `matching_engine_network_decode_nanoseconds` / `..._network_encode_nanoseconds`：
  请求解码 / 响应编码；
- `matching_engine_socket_send_nanoseconds`：socket 发送耗时；
- `matching_engine_socket_to_socket_nanoseconds`：软件侧 socket-to-socket
  （读取完成→响应发送完成），是 wire-to-wire 的近似，不含硬件时间戳。

详细测量边界与硬件时间戳/PTP 方案见 [docs/latency-measurement.md](docs/latency-measurement.md)。

架构提升路线（问题→决策→阶段验收）见
[docs/architecture-improvement-plan.md](docs/architecture-improvement-plan.md)。

交易所核心系统（gateway / middleware / core）的需求澄清与架构决策见
[docs/exchange-core-architecture.md](docs/exchange-core-architecture.md)。
协议字段、共享内存布局与序号规则见
[docs/exchange-protocol.md](docs/exchange-protocol.md)。

使用 Prometheus `histogram_quantile(0.99, ...)` / `0.999` / `0.9999` 查看尾部；
`max` 应结合进程/SO 监控单独观察。`load_generator` 会直接抓取 `/metrics` 并打印
p50/p90/p99/p99.9/p99.99，不再输出平均值。基准 `cargo bench --bench latency_percentiles`
在进程内打印订单簿撮合的纳秒分位（包含 `Instant` 读取开销）。

### 容量与失败语义

- 价格区间 `[min_price, max_price]` 按 `tick_size` 预分配价格层；订单价格越界会被拒绝。
- 单个价格层队列达到 `queue_capacity` 时订单不会被加入订单簿，并记录 warning。
- 每个订单/撤单所属的批次在返回成功前都会 flush WAL；`wal_sync = true` 时同时执行 fsync。
- 崩溃只可能留下未写完的尾部记录，恢复时自动丢弃这段尾部；
  已 flush 的确认数据不会丢失。`wal_sync = false` 时已确认数据可能仍在 OS 页缓存中，
  断电可能丢失，适合可容忍该窗口的部署。

## 测试

```bash
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

基准测试（按需运行，耗时较长）：

```bash
cargo bench --bench tick_orderbook_benchmark
cargo bench --bench hft_benchmark
cargo bench --bench latency_percentiles
cargo bench --bench multicore_benchmark
cargo bench --bench network_benchmark
cargo bench --bench network_middleware_benchmark
cargo bench --bench e2e_network_benchmark
cargo bench --bench e2e_matching_engine_benchmark
```

负载生成器（默认连接 `127.0.0.1:8080`，运行 10 秒）：

```bash
# 先以 --wal-sync false 启动服务以获得吞吐数据
cargo run --bin load_generator
```

## 当前限制与已知问题

### 协议与语义

- **幂等键**：`NewOrderRequest.client_order_id` 非 0 时启用去重（有界内存缓存，
  `--dedup-cache-size` 默认 262144）。缓存淘汰后旧 client id 再提交可能重新执行；
  需要严格幂等应持久化去重表或扩大容量。进程重启后从 WAL 恢复已见 id，重试会被
  拒绝而不是重复下单；原始回执不跨重启重放。
- **撤单成功已有 `CancelAcknowledged` 回执**；拒绝响应带稳定类别
  `Rejected { code: ErrorCode, message }`（OrderRejected / CancelRejected / ProtocolVersion /
  Internal），细分校验原因仍在 message 中（后续可扩成更细的错误码）。
- 协议有版本字段（`PROTOCOL_VERSION`）和 `request_id`，但当前只支持单一版本，没有多版本兼容/握手；不匹配的版本会收到明确拒绝。
- **没有自成交防护**：同一用户可以作为买卖双方成交；生产需要 STP 策略。
- **没有风控**：价格带、名义金额、持仓/额度限制均不在引擎内。
- wire 仍每笔订单传 symbol 字符串；decode 会做一次 intern 哈希（无分配），
  更彻底的零拷贝需要按连接握手/symbol id 协商。
- 时间戳来自墙钟，可能因校时而回退；排序/审计应以 WAL sequence 为准。

### 持久化与恢复

- WAL 使用进程级文件锁（`<data_dir>/.lock`），同一目录只能被一个进程打开。
- 已有快照（`--snapshot-interval`，默认每 100 万条 WAL 记录），恢复只重放 snapshot 之后的尾段；
  snapshot 写入后会先 rotate，再删除快照覆盖的旧 WAL 文件，并原子更新 `wal.start`
  manifest 记录首个存活文件；旧 record 不解码/不保留，恢复内存和扫描时间随尾段 +
  当前活跃文件增长，而不是全量 WAL。
- **恢复通过重放订单重新执行撮合**：撮合语义变更仍可能改变 snapshot 之后尾段的恢复状态；
  snapshot 已锁定大部分状态，但完整语义版本化仍需 WAL/快照格式版本策略。
- WAL 创建/轮转后会 fsync 父目录；新建或修复 header 也会先 fsync。
- 重启后 writer 会打开**编号最大的 WAL 文件**继续追加，而不是回到 `wal.000000.log`，
  避免 snapshot 轮转后覆盖已有文件/打乱序列。
- 已实现 group commit：`--commit-batch-size` 控制单次 flush/fsync 最多聚合多少条命令；
  仍没有跨 socket/多批次的时间窗口聚合，`wal_sync=true` 时极限吞吐仍由 fsync 决定。
- 单个订单簿一份 WAL；多品种通过每品种独立进程（或未来重新引入有持久化的分区执行器）。

### 订单簿

- 撤单与状态查询已是 O(1)（侵入式链表 + 订单→节点索引）；撮合仍按 FIFO 遍历头部。
- 订单节点使用 slab 复用；节点数量峰值决定内存，价位头不再逐价位分配对象。
- 价格区间按 tick 预分配，`--min-price/--max-price/--tick-size` 决定内存；CLI 已限制最多
  100 万档，但库调用方仍可构造超大 `ContractSpec`。
- 单个价位队列满时，服务端在写 WAL 前拒绝（`can_accept_order`）；库直接调用
  `TickBasedOrderBook::match_order` 仍可能静默丢弃剩余量（不返回确认）。
- 仅支持限价单；撤销仅对仍在簿中的订单有效。

### 运行与运维

- 单进程兼容路径有连接空闲超时（`--idle-timeout-secs`，0 关闭）、最大连接数
  （`--max-connections`，默认 1024），但仍是每连接一个 OS 线程；独立 `gateway`
  路径使用固定 `mio` event-loop 线程，连接数不再转化为线程数。仍缺少按用户/品种限流和 fd/CPU 配额。
- WAL 持久化失败会触发进程 fail-fast（而不是继续返回错误并保持 healthy）；健康端点
  本身没有 degraded 状态。
- 没有 TLS/认证；订单里的 `user_id` 由客户端自报，仅用于撤单归属校验。
- Metrics 已加入 `matching_engine_{matching,wal,actor_queue_wait,request}_latency_nanoseconds`
  直方图；仍缺少 CPU cycles/cache misses/context switches、socket 解码/编码分段延迟、
  WAL 队列深度和“每次 flush 的 fsync 时长”分布。
- 单实例、无主备/复制/故障切换。
