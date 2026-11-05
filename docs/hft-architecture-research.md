# HFT 架构调研与目标设计

本文记录调研结论和本仓库的下一步架构。目标不是宣称当前代码已经达到生产级 HFT，
而是把**前置 gateway 进程**和**撮合进程**的职责、线程模型、IPC 与背压设计清楚，
并据此落地可验证的最小实现。

## 1. 调研结论

### 1.1 撮合核心是单写者状态机

LMAX Disruptor 的核心不是"更快的队列"，而是把一个可串行化的业务逻辑建模为
**single writer**，前面用预分配 ring buffer 接收命令，消费者通过序列号/内存屏障
推进，避免锁、避免每消息分配。撮合、风控、账务这类需要确定性顺序的逻辑适合这种模型。

来源：[LMAX-Exchange/disruptor](https://github.com/LMAX-Exchange/disruptor)

对本项目的直接含义：

- 撮合线程必须是唯一修改订单簿的线程；
- ingress 不能把锁、socket、磁盘 I/O 带进撮合线程；
- 队列应该是有界、预分配、可观测背压的，而不是无界 `mpsc`；
- 订单/撤单/成交/持久化事件都应有明确序号。

### 1.2 网关和撮合通常是分离的进程/组件

Aeron 的公开案例（Rapid Addition RA Platform）把交易平台拆成模块化组件：
Aeron Transport 负责低延迟可靠消息，Aeron Archive 负责持久化/回放，撮合/命令日志
采用 single-threaded processing；生产环境强调确定性延迟、故障恢复和主备复制，
而不是把连接处理和撮合塞进同一个线程池。

来源：[Rapid Addition case study](https://aeron.io/case-studies/rapid-addition-high-performance-trading-infrastructure/)、
[Aeron](https://github.com/aeron-io/aeron)

对本项目的直接含义：

- 前置 gateway 负责：连接、协议解码、认证/限流、会话状态、背压；
- 撮合进程负责：顺序执行、订单簿、成交、确定性事件输出；
- 两者之间用明确的 IPC/消息协议，而不是共享订单簿或共享锁；
- 撮合进程不应因为某个客户端慢而阻塞，网关负责吸收/拒绝慢客户端。

### 1.3 前置网关不能 thread-per-connection

Linux `epoll` 的设计目标就是在大量 fd 上扩展，配合非阻塞 fd 和
edge-triggered 模式，一个事件循环可以承载大量连接。每个连接创建一个线程会在
1 万连接时产生 1 万线程，调度、栈内存、上下文切换都不可接受。

来源：[epoll(7)](https://man7.org/linux/man-pages/man7/epoll.7.html)

目标 gateway 线程模型：

- 固定数量 event-loop 线程（初始 1 个，后续按 CPU 核扩展）；
- 每个 event loop 用 `mio`（epoll/kqueue 封装）管理 listener、上游 IPC、所有客户端 fd；
- 每连接只保存状态机：读缓冲、写缓冲、in-flight 计数、关闭标记；
- 读写全部非阻塞，EAGAIN 后回到 poll；
- 慢客户端达到写缓冲/in-flight 上限就停止读它或断开，不能反压撮合线程。

`mio` 只是 I/O 事件通知库，不是 async runtime；不引入 future/waker/每消息装箱。

### 1.4 持久化不能阻塞撮合

真实交易系统通常把命令日志、复制、快照当作独立组件：

- 撮合线程产生带序号的事件；
- journaler/archive 负责落盘、fsync、复制；
- 快照由 recovery/snapshot 组件从日志/副本构建，而不是让撮合线程在热路径做文件 I/O；
- 确认给客户端发生在持久化/复制满足策略之后，但撮合线程可以继续处理下一批命令。

Aeron Archive/Chronicle Queue 都是"消息/日志持久化 + 回放"这个方向的成熟实现：
[Chronicle Queue](https://github.com/OpenHFT/Chronicle-Queue)。

对本项目：

- 当前 `--async-fsync` 已把 fsync 移出撮合线程，但撮合线程仍可能在
  commit channel 满、快照 barrier 时停顿；
- 目标是把 WAL 记录通过**有界 SPSC/ring**交给独立 journaler，
  撮合线程只在 ingress 有界队列满时对网关施加背压，不做磁盘/快照 I/O；
- 快照需要独立 snapshotter（从 WAL/副本构建），不能要求撮合线程等待 fsync barrier。

## 2. 目标架构

```text
 clients
   │  TCP (nonblocking)
   ▼
┌─────────────────────────────┐
│ gateway process             │
│  - N fixed event loops      │
│  - per-conn state machine   │
│  - bounded read/write bufs  │
│  - in-flight cap / backpressure
│  - auth/rate-limit later    │
└─────────────┬───────────────┘
              │ Unix domain socket / later shared-memory SPSC
              ▼
┌─────────────────────────────┐
│ matching process            │
│  - IPC reader/writer threads│
│  - single matching thread   │
│  - orderbook + dedup        │
│  - sequenced events         │
└─────────────┬───────────────┘
              │ bounded event ring
              ▼
┌─────────────────────────────┐
│ journaler / snapshotter     │
│  - WAL append + fsync       │
│  - snapshots/replay         │
│  - replication later        │
└─────────────────────────────┘
```

关键约束：

1. gateway 线程数固定，不随连接数增长；
2. 撮合线程只执行订单簿状态转移和内存队列 push；
3. IPC/网络/磁盘失败通过错误事件上报，不把阻塞调用放进撮合线程；
4. 所有队列有界，满时显式 backpressure/reject，而不是无界内存增长；
5. 内部消息带 `request_id`/`client_order_id`，端到端可关联、可幂等。

## 3. 本仓库落地顺序

### Phase A（已落地）

- 已新增 engine IPC 模式：引擎进程通过 Unix domain socket 接收 gateway 转发的
  client request，返回 `ServerResponse`；
- 已新增独立 `gateway` 二进制：`mio` 单事件循环，非阻塞 accept/read/write，
  每连接状态机，固定线程数；
- 已保留原单进程模式作为兼容/测试路径；
- 已新增 `tests/gateway_split_test.rs` 验证两进程下单/撤单。

### Phase B

- gateway 多 event loop：已实现 `--io-threads`（每个 loop 独立 SO_REUSEPORT listener + 上游 IPC）；
- gateway→engine 从 UDS 换 shared-memory SPSC ring（或至少 `SOCK_SEQPACKET`）；
  engine 侧 ring consumer/producer 与 `ring_probe` 已落地，gateway event-loop 接入待做；
- 每连接 in-flight 上限与显式 reject；
- 引擎 IPC 背压指标。

### Phase C

- 独立 journaler：撮合线程只 push event，journaler 负责 WAL/fync；
- 独立 snapshotter：从 WAL/副本构建，撮合线程不参与 barrier；
- 主备复制、恢复演练。

### Phase D

- 内核旁路/RDMA、PTP/硬件时间戳、CPU 隔离、NUMA/hugepage；
- 这些依赖目标机器和内核，不能只用 macOS 开发机验证。

## 4. 当前差距

| 项 | 当前 | 目标 |
|---|---|---|
| 前置连接 | 每连接一个阻塞线程 | 固定 event-loop 线程 |
| 进程边界 | gateway + matching 同进程 | 独立 gateway / engine 进程 |
| 撮合阻塞 | fsync 可异步，但 commit 队列满/快照 barrier 仍可能停顿 | 撮合只 push 有界队列，不碰磁盘 |
| IPC | 无 | UDS，后续共享内存 ring |
| 持久化 | 引擎内 WAL/snapshot | 独立 journaler/snapshotter |
| 背压 | 连接上限 + 队列容量 | 端到端 in-flight/字节水位 |
| 时钟 | 软件时间戳 | PTP/硬件时间戳（目标机） |

## 5. 参考

- [LMAX Disruptor](https://github.com/LMAX-Exchange/disruptor)
- [Aeron](https://github.com/aeron-io/aeron)
- [Rapid Addition + Aeron case study](https://aeron.io/case-studies/rapid-addition-high-performance-trading-infrastructure/)
- [Chronicle Queue](https://github.com/OpenHFT/Chronicle-Queue)
- [epoll(7)](https://man7.org/linux/man-pages/man7/epoll.7.html)
