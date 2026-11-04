# HFT 撮合引擎架构提升计划

- 版本：v1（proposed）
- 日期：2026-11-04
- 范围：当前仓库的单品种持久化撮合服务、网络接入、WAL、观测与测试
- 目标读者：HFT 工程师、交易系统架构评审、运维/SRE
- 关联文档：[README](../README.md)、[延迟测量边界](latency-measurement.md)

## 0. 修订记录

- **2026-11-04**：Tokio/axum 已从整个 crate 移除；网关、观测、示例、负载生成器全部改为
  阻塞 OS 线程 + `std::net`/`crossbeam`。本文的 Phase 2 中"Tokio 数据面替换"部分已提前完成，
  SPSC/WAL 异步提交仍待做。Phase 0 的 `--matching-core` 已通过可选 `cpu-affinity` feature 实现；
  `--network-cores` 与 perf 计数器仍待做。
- **2026-11-04（架构 review）**：MatchingRuntime、Gateway、Session、Journal 深模块已抽取；
  PartitionedService（无调用方的第二执行路径）已删除；
  服务 stage/commit 降为 `pub(crate)`；订单簿返回显式 `Filled / Rested / Rejected` outcome；
  WAL sequence/record framing/recovery 不再泄漏给服务。CLI 从 1051 行降到约 440 行；
  新增 runtime/journal/gateway 测试。下文保留原始问题描述与原始决策，新增一节记录评估。

## 1. 执行摘要

当前架构的**领域骨架是正确的**：单写者撮合线程、订单簿同步核心、WAL 先持久化后确认、
有界命令队列、O(1) 撤单、静态分发、失败 fail-fast。这些是交易系统约束，而不是语言选择。

下一步不应重写成 C++，也不应先上 DPDK。应按照**测量到的瓶颈**分阶段增强数据面：

1. **P0：协议关联 + SymbolId + 连接 pipelining**
   - 解决每连接单 in-flight、每笔 symbol 分配/哈希、回包无法关联请求。
2. **P1：SPSC 数据面 + WAL 异步提交 + 绑核**
   - 解决 Tokio mpsc/oneshot 唤醒抖动、fsync 阻塞撮合线程、调度迁移。
3. **P2：快照 + 幂等持久化 + 流式恢复**
   - 解决恢复时间/内存随 WAL 线性增长、重放语义漂移、重启后幂等缺失。
4. **P3：硬件时间戳/PTP + 性能计数器 + 运维/高可用**
   - 解决 ns 级 wire-to-wire 不可测、CPU/缓存抖动不可见、单点问题。

判断依据不是"更酷的技术"，而是当前证据：撮合本身已在 **p50 291 ns、p99 750 ns** 量级，
而网络压测的请求 p50 在 **1 ms** 量级；同时 150 万条 WAL 重放约需 **3 秒**。因此
**先修协议/分配/唤醒/恢复，再谈内核旁路**，收益更大、风险更低。

## 2. 基线与证据

### 2.1 环境

- 机器：Apple M4（Darwin arm64，开发机）
- 工具链：rustc 1.99.1，edition 2024
- 发布构建、进程内基准；网络压测为 debug 构建 + 本机 macOS（仅用于观察趋势，不是生产数字）

### 2.2 当前实测

| 项目 | 结果 | 说明 |
|---|---|---|
| 订单簿撮合（release） | p50 291 ns / p90 375 ns / p99 750 ns / p99.9 1.79 µs / p99.99 8.19 µs / max 91.3 µs | `cargo bench --bench latency_percentiles`，100 万次，含 `Instant` 读取开销 |
| 撮合吞吐 | ~3.2 M ops/s | 同一基准 |
| O(1) 撤单 | p50 0 ns / p90 42 ns / p99 42 ns / p99.99 166 ns / max 8.5 µs | 100k 挂单后逐笔撤单；p50=0 是 `Instant` 分辨率现象 |
| 网络请求（debug, 64 客户端） | p50 1 ms / p99 5 ms / p99.9 25 ms / p99.99 50 ms | `load_generator` 抓 `/metrics`；含排队、撮合、WAL、编码/发送 |
| WAL commit（debug） | p50 5 µs / p99 100 µs / p99.9 500 µs / p99.99 5 ms | group commit 每批一次 flush/fsync |
| decode / encode（debug） | p50 500 ns / 200 ns | 网络分段指标 |
| group commit 对比 | batch=1 约 7 TPS；batch=1024 约 768 TPS | 64 客户端、`wal_sync=true`、debug；约 110× |
| 恢复 | 1,500,393 条 WAL，约 3 秒重放，WAL 约 84 MB | 全量载入 `Vec<WalRecord>` 后重放 |
| 测试 | 91 lib + 4 integration + 1 doc；clippy -D warnings 通过 | 2026-11-04 |

### 2.3 已经完成的能力（本计划的基础）

- 单写者撮合线程 + 有界 crossbeam channel actor；无共享撮合锁。
- group commit：`stage_order`/`stage_cancel` + 单次 `commit`，回执在 flush/fsync 后。
- `client_order_id` 幂等表：有界内存缓存 + WAL 恢复 seed；重复请求返回缓存回执或拒绝。
- 侵入式双向链表 + slab：O(1) 撤单、O(1) 状态查询，已删除 `RingBuffer`。
- WAL 进程锁、目录 fsync、SIGINT/SIGTERM 优雅退出、连接上限/空闲超时、持久化失败 fail-fast。
- 纳秒级阶段直方图：request / queue wait / matching / WAL append / commit / decode / encode / socket send / socket-to-socket。
- `MatchingRuntime` 深模块：命令队列、group commit、回执顺序、fatal/lifecycle 集中在一处；
  CLI 只保留 socket/session。
- 订单簿返回显式 `MatchOutcome`（`Filled` / `Rested` / `Rejected`）；`can_accept_order` 的
  预检不再由调用方从 `None` 推断结果。
- `Journal` 深模块隐藏 WAL sequence/framing/rotation/recovery；`WalWriter`/`WalReader`/
  `RecoveryManager` 已降为 crate 内部实现，服务只调用 `append`/`commit`/`recover`。
- `Gateway` 深模块隐藏 accept/registry/connection limit/shutdown/fatal；`Session` 深模块隐藏
  分帧、提交、回执编码和阶段延迟；CLI 只剩配置、装配与信号。
- 协议加入 `PROTOCOL_VERSION`（当前 2）+ `request_id` 的 `ClientRequest`/`ServerResponse` envelope；
  版本不匹配明确拒绝。Session 已实现有界 pipelining：默认 `--max-in-flight 1` 保持单线程
  低延迟路径，>1 时 reader/writer 分离、按 request_id 回包；实测 64 客户端下吞吐提升但
  p50/p99 排队延迟上升，因此默认仍是 1，按目标调参。
- `SymbolId` intern 已实现：wire decode 借用 symbol 字符串并查表，订单簿/成交只携带
  `SymbolId(u32)`；稳态下不再为每笔消息分配 `Arc<str>`。
- 撮合线程支持可选 `--busy-poll`：空队列时自旋而不是休眠，需与绑核/隔离核配合；
  默认关闭，避免无意义 CPU 消耗。
- 可选 `--async-fsync`：撮合线程只做 WAL flush，fsync 在独立线程完成，回执仍等待 fsync；
  快照前通过 commit barrier 等待所有先前批次 fsync，再写快照，因此可与快照同时启用。
- 重启恢复后 writer 打开编号最大的 WAL 文件继续追加，避免 snapshot rotate 后从 0 号文件
  覆盖旧文件或打乱序列；旧 header-only 文件的回收策略仍待做。
- 新增独立 `gateway` 二进制（mio 单 event-loop、固定线程数）与 engine `--engine-uds`
  IPC 模式；两进程测试覆盖下单/撤单。单进程阻塞 gateway 仅保留为兼容路径。
- 协议拒绝响应已改为 `Rejected { code: ErrorCode, message }`，提供稳定错误类别；
  更细的校验错误码（queue-full/duplicate/unknown-order 等）仍可继续细分。
- 快照已实现：订单簿/next ids/幂等 key 写入 `snapshot.bin`（CRC/原子替换），恢复时加载
  snapshot 并只重放其后 WAL 尾段；CLI `--snapshot-interval` 默认 0（撮合优先不被 barrier 阻塞），按需开启。
  WAL `scan_from` 已按 snapshot seq 过滤：旧记录仍校验 CRC/序列号但不 decode、不保留，
  恢复内存随尾段增长。snapshot 写入后会先 rotate，再删除已完全覆盖的 WAL 文件并
  原子更新 `wal.start`（首个存活文件号）；恢复扫描随尾段 + 当前活跃文件增长，
  文件数量不再随 snapshot 次数无限增长。
- 延迟测量边界与硬件时间戳方案文档化。

### 2.4 Tokio 移除评估（2026-11-04）

**决策：移除 Tokio 和 axum，整个 crate 不再依赖 async runtime。**

评估依据：

1. 撮合核心本来就是同步单写者；Tokio 只服务网络/观测，不参与撮合正确性。
2. 网关客户端会话数有界（默认 `--max-connections 1024`），每连接一个阻塞线程在这个规模下
   比 future/waker/调度器更简单、更可预测；没有每消息 Future 装箱，也没有 async worker
   被 fsync 阻塞的问题。
3. 观测端点是低频 `/metrics`/`/health`，一个阻塞 HTTP 线程足够；引入 axum 反而增加依赖和
   runtime。
4. 移除后依赖树不再包含 `tokio`/`axum`，信号处理改用 `signal-hook`；SIGINT/SIGTERM、
   group commit、幂等、O(1) 撤单、WAL 恢复保持原有语义，E2E 冒烟通过。
5. 撮合线程绑核通过 `--matching-core` + `--features cpu-affinity` 可选启用；默认不绑，
   避免在容器/共享机器上硬编码核号。

实测（debug、macOS、`wal_sync=false`、fire-and-forget 负载，仅看趋势）：

| 客户端数 | 吞吐（trades/s） | 请求 p50 | 请求 p99 |
|---:|---:|---:|---:|
| 8 | ~6,089 | 100 µs | 250 µs |
| 32 | ~5,089 | 250 µs | 5 ms |
| 64 | ~4,820 | 1 ms | 5 ms |

最新一次 64 客户端 debug 运行记录到 **~59.8k orders/s**（fire-and-forget，绝大多数订单挂单/排队，
只有约 4.5k trades/s）。release 构建同负载约 **87.5k orders/s、4.8k trades/s**，
请求 p50 50 µs / p99 1 ms / p99.9 25 ms。这批数字有运行方差，且旧 Tokio 版本的负载生成器/服务端实现不同，不能直接当作
"Tokio 更快/更慢"的结论。真正结论是：**在本项目的会话规模下，去掉 runtime 没有破坏功能和
基本吞吐；线程数随连接数线性增长，是明确的扩展上限。**

代价与边界：

- 每连接一个 OS 线程（256 KiB 栈）；>数百/上千会话时应换成 epoll/io_uring 事件循环。
- 没有 async 生态的成熟 HTTP/TLS；生产 HTTPS/认证需要额外实现或前置代理。
- 需要自己处理信号、连接注册表、停机唤醒；当前已实现并用 SIGTERM 验证。

后续：Phase 2 的 SPSC/ring、WAL 异步提交、绑核仍然有效；只是不再以"替换 Tokio"为前提。

## 3. 问题清单与根因

### P0：限制吞吐或破坏 ns 级测量的结构性问题

| 编号 | 问题 | 证据 | 根因 |
|---|---|---|---|
| P0-1 | 每连接同一时间只有一个 in-flight 请求 | handler 读完一帧后 `await` actor 回执，才读下一帧；64 客户端下请求 p50 1 ms | 请求/响应无关联 id；同步等待模型；Little's law 下吞吐被 RTT/排队限制 |
| P0-2 | 每笔订单 decode 产生 `Arc<str>` symbol，成交再 clone `Arc` | `NewOrderRequest.symbol`、`TradeNotification.symbol`；decode 路径 `read_arc_str` | 没有进程级 symbol intern；`Arc<str>` 是为安全共享做的折中 |
| P0-3 | ~~热路径经过 Tokio mpsc + oneshot + task 调度~~ **已关闭** | 已改为 crossbeam channel + 阻塞线程；Tokio/axum 已移除 | 下一步是 SPSC/ring，进一步消除唤醒成本 |
| P0-4 | WAL 恢复是"全量载入 + 重新执行撮合" | `recover()` 先 `recovery.recover()` 得到全部 records，再 replay；150 万条约 3 秒 | 没有快照；恢复内存/时间 O(WAL)；撮合语义变更会改变历史状态 |
| P0-5 | 协议没有版本协商、请求关联 id、稳定错误码 | `ClientMessage`/`ServerMessage` 直接枚举；`Rejected(String)` | 协议演进会破坏旧客户端；无法可靠支持 pipelining/回包匹配 |

### P1：影响延迟确定性、可运维性与恢复语义

| 编号 | 问题 | 证据 | 根因 |
|---|---|---|---|
| P1-1 | 撮合线程没有绑核/隔离，网络与观测线程可能竞争同一核 | CLI 仅创建线程；`core_affinity` 只在分区服务中使用 | 调度迁移、频率变化、共享缓存抖动直接影响 p99.9 |
| P1-2 | fsync 在撮合线程同步执行 | `commit()` 在 actor 线程内 `flush`/fsync；debug 下 commit p50 5 µs、p99 100 µs | 撮合核心被磁盘延迟阻塞；group commit 只降低频率，不隐藏延迟 |
| P1-3 | 幂等只保证进程内、有限窗口；重启后只能拒绝不能重放原回执 | 有界 `dedup` 表 + WAL seed；缓存淘汰后旧 id 可再次执行 | 去重结果没有持久化；没有在回包中携带 client_order_id |
| P1-4 | 没有 CPU cycles / cache miss / context switch 指标 | 仅有 wall-clock 直方图 | 无法判断抖动来自调度、缓存、分支还是 I/O |
| P1-5 | 没有硬件时间戳/PTP，socket-to-socket 不是 wire-to-wire | 文档已说明，指标只有软件口径 | 软件 `Instant` 边界在用户态，无法测量 NIC/交换机时间 |

### P2：规模、运维和生态风险

| 编号 | 问题 | 影响 |
|---|---|---|
| P2-1 | 单实例、单 WAL、无主备/复制 | 单点故障；恢复期间不可用 |
| P2-2 | 没有 TLS/认证、按用户/品种限流 | `user_id` 可伪造；连接/消息速率无准入控制 |
| P2-3 | 协议无版本、错误码不稳定 | 客户端升级/回滚困难；字符串匹配错误 |
| P2-4 | 分区服务无持久化，`submit_order_sync` 阻塞 | 多品种路径不可直接用于生产 |
| P2-5 | 混沌/模糊测试仍以 ignored 为主 | 磁盘满、fsync 失败、断网、半包、重复请求缺自动化覆盖 |
| P2-6 | 没有 CI 性能门槛 | 尾延迟/吞吐回归不会被自动阻止 |

## 4. 目标架构

```text
                    control plane / observability (blocking threads, metrics HTTP)
                                      │
  NIC RX ──► network ingress thread(s) ──► SPSC ring (per partition)
              │  decode: header → request_id, symbol → SymbolId
              │  backpressure: bounded in-flight per connection
              ▼
         matching thread (pinned, busy-poll option)
              │  match: price array + bitmap + intrusive FIFO
              │  stage WAL record batch
              ▼
         WAL thread / io_uring  ──► flush/fsync (group commit)
              │  commit barrier → responses become durable
              ▼
         egress SPSC ring ──► network egress thread(s) ──► NIC TX
```

关键属性：

- **单一写者**：每个 symbol/partition 只有一条撮合线程；状态无共享可变访问。
- **有界**：ingress/egress、in-flight、WAL 队列都有上限；过载时拒绝/背压，不产生不可控排队。
- **稳态零分配**：SymbolId、对象池/arena、复用帧 buffer；热路径不碰 allocator。
- **无 async runtime**：撮合、网络、观测都是阻塞 OS 线程；控制面/指标也不需要 Tokio。
- **可测**：每阶段 p50/p99/p99.9/p99.99/max；硬件时间戳用于 wire-to-wire；CPU 计数器辅助归因。
- **失败语义**：持久化失败 fail-fast；恢复只接受可验证状态；不假装恢复。

## 5. 关键决策与论证

### ADR-1：不重写 C++，只借 C++ 数据面纪律

**决策**：继续 Rust；不因为 ns 目标重写语言。

**理由**：

- 当前架构的 70–80% 与 C++ 低延迟撮合设计同构（单写者、actor/队列、group commit、侵入列表、静态分发）。
- C++ 的优势主要在数据面控制（SPSC、io_uring、绑核、arena、内核旁路），这些在 Rust 同样能做。
- Rust 的借用检查降低了 UAF/数据竞争风险；交易系统"稳定可靠"的收益真实存在。
- 重写成本极高，且不会自动解决网络栈/fsync/时钟同步问题。

**会借用的 C++ 做法**：SymbolId/arena、SPSC + cacheline padding、busy-poll、pin/isolcpus、
io_uring/独立 WAL 线程、硬件时间戳/PTP、perf 计数器。

### C++ 对照速览

| 层 | 当前 Rust 设计 | C++ 更可能的设计 | 计划动作 |
|---|---|---|---|
| 撮合核心 | 单写者、价格数组+bitmap、侵入式 FIFO、slab | 相同；可能用 `boost::intrusive` 裸指针或索引 | 保留，不重写 |
| 进程内通信 | crossbeam bounded channel + reply channel（无 runtime） | SPSC ring + busy-poll + futex fallback | Phase 2 增加 SPSC 数据面 |
| symbol | `SymbolId`，decode 借用字符串查表 | SymbolId intern + arena | 已实现 SymbolId；更彻底零拷贝待 symbol id 协商 |
| WAL | 撮合线程内 BufWriter + group commit | 独立 WAL 线程/io_uring，按字节/时间 commit | Phase 2/3 |
| 网络 | blocking TCP + 每连接线程 | epoll/io_uring + 绑核网关线程 | 已移除 Tokio；高连接数再换事件循环 |
| 内存 | jemalloc + 有界缓存 | arena/对象池 + hugepages | Phase 0/2 |
| 错误 | `Result` + 具体错误枚举 | `std::expected`/Status，异常关闭 | 保留 Rust 风格；协议错误码 |
| 测量 | 软件 `Instant` + ns 直方图 | 硬件时间戳/PTP + perf | Phase 4 |

结论：**架构同构，实现层按测量结果逐步借用 C++ 数据面纪律；不重写语言。**

### ADR-2：先协议关联与 pipelining，再换数据面

**决策**：协议 v2 引入 `request_id`（以及响应回显）、版本字段、错误码；连接允许多 in-flight；
然后再把 crossbeam 命令通道换成 SPSC。

**理由**：

- 没有 request_id，无法安全 pipelining，也无法可靠关联乱序响应；先做协议是后做 SPSC 的前提。
- pipelining 直接提高 in-flight 数量，是当前吞吐/排队延迟的最低成本改善。
- 协议改动会破坏兼容性，必须一次设计对（版本协商 + 迁移窗口），避免后面反复改。

**验收**：每连接 N 个 in-flight；回包 request_id 正确；超时/断连不串包；pipelined 基准。

### ADR-3：SymbolId 先于 SPSC/zero-copy

**决策**：进程级 SymbolTable；decode 阶段字符串 → `SymbolId(u32)`；内部订单簿、成交、分区路由用 id；
对外回包时再映射回字符串。

**理由**：

- 当前每笔 decode 产生 `Arc<str>`，成交 clone；分配与原子引用计数增加抖动。
- `HashMap<Arc<str>, ...>` 的字符串哈希在分区服务每笔订单都存在。
- SymbolId 是零分配 decode、零拷贝和稳定哈希的前置条件；先做它可以独立验证收益。

**验收**：稳态 decode 0 分配；symbol 哈希只在入站 intern 一次；成交结构不再含 `Arc<str>`（或只在出口惰性映射）。

### ADR-4：无 async runtime；SPSC 只用于数据面

**决策**：Tokio/axum 已移除；当前数据面是"阻塞 socket + 每连接线程 + crossbeam 有界队列"。
下一阶段把撮合入口/出口换成有界、cacheline padding 的 SPSC ring；可选 busy-poll。
控制面（metrics/signals）继续用阻塞线程。

**理由**：

- 撮合核心不需要 async；网络/观测也不需要 future 轮询和 waker。
- 有界会话数下，阻塞线程模型更简单、更可预测，且没有每消息 Future 装箱。
- crossbeam 通道仍有唤醒成本；ns 目标下进一步换成 SPSC + busy-poll，但必须与绑核/隔离
  一起作为可配置模式。
- 全部去 runtime 的代价是线程数随连接数增长；这是已记录的扩展边界。

**验收**：数据面无 async 任务；ring 满时明确背压；CPU 使用率/功耗在可接受范围；
>数百连接时需要 epoll/io_uring 网关（后续阶段）。

### ADR-5：WAL 从撮合线程移出，group commit 加时间/字节阈值

**决策**：撮合线程只负责产生有序 WAL 记录批次和 commit barrier；独立 WAL 线程（或 io_uring）
执行 append/flush/fsync；完成后再回执。group commit 增加"最大等待时间/最大字节数"参数。

**理由**：

- 当前 commit 在撮合线程同步执行：debug 下 commit p50 5 µs、p99 100 µs，撮合核心 291 ns；
  fsync 直接阻塞撮合，p99 被磁盘支配。
- 远程/生产磁盘 fsync 可能是 ms 级；仅靠批大小无法同时优化延迟和吞吐。
- 独立 WAL 线程可让撮合继续处理下一批，同时保持 WAL 顺序和"未 commit 不回执"语义。

**风险**：WAL 落后需要背压；多 batch in-flight 需要严格顺序和完成通知；不能破坏崩溃一致性。

**验收**：撮合线程不再调用阻塞 I/O；commit 延迟直方图与撮合延迟独立；kill -9 恢复仍正确。

### ADR-6：快照 + 状态版本，恢复不再全量重放

**决策**：周期性生成订单簿/WAL 序列/ID/幂等状态的快照；恢复 = 最近有效快照 + 尾段 WAL 流式重放；
快照和 WAL 都带格式版本与校验。

**理由**：

- 实测 150 万条记录约 3 秒、84 MB WAL；生产会话可达数十亿条，全量载入不可接受。
- 重放"重新执行撮合"会随撮合代码变更改变历史状态；快照 + 版本锁定语义。
- 快照还解决恢复内存上界，便于 RTO 目标。

**验收**：RTO 与快照间隔相关而非 WAL 长度；恢复内存有界；快照损坏回退到上一个有效快照并告警；
跨版本恢复有明确拒绝/迁移策略。

### ADR-7：先测量后优化，禁止用均值验收

**决策**：所有阶段以 p50/p99/p99.9/p99.99/max + 抖动验收；延迟基准固定硬件、绑核、warmup；
报告必须写清测量边界（软件 socket-to-socket 还是硬件 wire-to-wire）。

**理由**：

- 交易系统尾部决定盈亏；均值和单次跑分都会掩盖问题。
- 当前软件指标已能归因到协议/队列/fsync；ns 级结论必须硬件时间戳，否则数字不可比。

## 6. 阶段计划

### Phase 0：测量与绑核（低风险，先做）

**范围**

- CLI 已增加 `--matching-core`（可选 `cpu-affinity` feature）；`--network-cores` 仍待做。
  Linux 生产建议配合 `isolcpus`/`nohz_full`，macOS 只用于开发近似。
- 撮合线程命名/绑核；网络/观测线程避开撮合核；可选 `mlockall`/hugepage 预留在后续。
- 增加 perf 采样（Linux `perf_event_open` 或 eBPF）：cycles、cache-misses、context-switches、page-faults；
  无权限时降级为无操作并记录日志。
- 基准脚本固定 CPU/构建参数；报告 p50–p99.99 + max + 标准差。

**为什么**：这是最便宜的确定性改善，也是后续所有优化的测量基础；没有稳定基线，无法判断 SPSC/io_uring 是否真的有效。

**验收**：撮合线程固定在指定核；基准输出硬件计数器；同一提交多次跑分 p99 波动在约定范围。

### Phase 1：协议 v2 + SymbolId + pipelining（P0）

**范围**

- 消息头：`version`、`kind`、`request_id`；响应回显 `request_id`；错误码枚举替代字符串。
- `NewOrderRequest` 保留 `client_order_id`；回包携带 `client_order_id` 或可关联字段。
- 连接读循环与匹配解耦：有界 in-flight（按连接/全局），writer task 负责按序/关联发送；超时与断连清理。
- SymbolTable：decode 时 intern；`OrderBook`/`Trade`/partition 路由使用 `SymbolId`；对外编码惰性映射。
- 扩展 `load_generator`：pipelined 模式、每连接 in-flight 配置、request_id 校验、乱序/丢失统计。

**为什么**：当前 64 客户端下请求 p50 1 ms，而撮合 p99 750 ns；瓶颈在连接模型/排队，不在撮合。
pipelining 提高 in-flight；当前实现每连接额外一个 writer 线程，高会话数时需要改成 gateway 级 egress，SymbolId 去掉每笔分配/哈希，是后续零拷贝的前提。

**验收**：pipelined 压测无串包/丢包；稳态 decode 0 分配；相同硬件下吞吐提升、请求 p99.9 改善；
旧客户端在版本协商下被明确拒绝而非错乱解析。

### Phase 2：SPSC 数据面 + WAL 异步提交 + 绑核强化（P1）

**范围**

- 有界 SPSC ingress/egress ring（cacheline padding，index-based，无每消息分配）；可选 busy-poll。
- 撮合线程从 ring 取 batch；响应通过 egress ring 回网络线程。
- WAL 线程/io_uring：按字节/时间阈值 group commit；commit barrier 后回执；WAL 落后时背压。
- 当前阻塞线程网关作为基线；SPSC 作为可配置数据面，默认保持阻塞线程直到目标硬件验证。
- 队列深度、ring 满次数、WAL 队列深度进入 metrics。

**为什么**：ADR-4/5；解决唤醒抖动和 fsync 阻塞。SPSC 只有在绑核/隔离后才值得开启，否则忙等会抢占其他线程。

**验收**：数据面无 async 任务；撮合线程无阻塞 I/O；p99.9/max 对比 Phase 1 改善；kill -9 恢复一致。

### Phase 3：快照 + 幂等持久化 + 流式恢复（P2）

**范围**

- 快照格式：magic/version、WAL seq、订单簿状态、next order/trade id、幂等表（client id→结果/租约）、CRC。
- 原子写：临时文件 + fsync + rename + 目录 fsync；保留最近 N 个快照。
- 恢复：加载最近有效快照，校验后流式重放尾段 WAL；损坏时回退/拒绝策略。
- 幂等：把 client_order_id→原始回执/结果持久化（WAL 或快照）；重启后重试返回一致结果或确定性拒绝。
- 恢复测试：截断、CRC 错误、快照损坏、快照/WAL 交界处崩溃。

**为什么**：ADR-6；当前恢复 O(WAL) 且语义随代码漂移；幂等窗口有限是交易重试的实际风险。

**验收**：RTO 由快照间隔决定；恢复内存有界；重复请求跨重启行为确定；故障注入全部覆盖。

### Phase 4：硬件时间戳、性能门槛与运维/高可用（P2/P3）

**范围**

- Linux `SO_TIMESTAMPING`/PTP 或采集设备：wire-to-wire、tick-to-trade 指标；报告时间源/边界。
- 每阶段 CPU cycles/cache-miss/context-switch 指标；回归门槛。
- 混沌测试：磁盘满、fsync 失败、半包、断网、超时、重复请求、慢消费者。
- 运维：健康状态 degraded、WAL 空间告警、快照/恢复告警；TLS/认证/限流；后续主备/复制。

**为什么**：ns 级测量必须硬件时间戳；单点/安全/故障注入是生产准入条件，但不应阻塞数据面优化。

**验收**：硬件时间戳报告可复现；CI 性能门槛生效；故障注入通过；运维手册完成。

### 6.1 问题闭环矩阵

| 问题 | 对应阶段 | 关闭条件 |
|---|---|---|
| P0-1 单 in-flight / 无 request_id | Phase 1 | pipelined 压测无串包；回包 request_id 正确；in-flight 可配置 |
| P0-2 `Arc<str>` / 每笔分配 | Phase 1 | 稳态 decode 0 分配；symbol 只 intern 一次 |
| P0-3 Tokio 唤醒抖动 | 已完成（Tokio 移除） | 数据面无 async 任务；后续 SPSC 再降低唤醒成本 |
| P0-4 全量 WAL 重放/语义漂移 | Phase 3 | 快照+尾段流式恢复；RTO/内存有界；状态版本校验 |
| P0-5 协议无版本/错误码 | Phase 1 | 版本握手；稳定错误码；旧客户端明确拒绝 |
| P1-1 未绑核 | Phase 0 | 撮合线程固定核；p99.9 波动下降 |
| P1-2 fsync 阻塞撮合 | Phase 2 | 撮合线程无阻塞 I/O；commit 与撮合延迟独立 |
| P1-3 幂等不持久 | Phase 3 | 跨重启重复请求返回一致结果或确定性拒绝 |
| P1-4 缺 CPU/缓存指标 | Phase 0/4 | perf 采样可见；回归门槛 |
| P1-5 无硬件时间戳 | Phase 4 | wire-to-wire 指标可复现；报告边界 |
| P2-1..P2-6 运维/HA/测试 | Phase 4 + 持续 | 混沌/模糊/性能门槛/运维手册 |

## 7. 优先级矩阵

| 计划 | 延迟收益 | 稳定性/正确性收益 | 工作量 | 风险 | 优先级 |
|---|---|---|---|---|---|
| Phase 0 绑核/测量 | 中 | 中（可观测） | S | 低 | P0 |
| Phase 1 协议/SymbolId/pipelining | 高 | 高（关联/兼容） | M | 中 | P0 |
| Phase 2 SPSC/WAL 异步提交 | 高（ns 目标） | 中 | L | 高 | P1 |
| Phase 3 快照/幂等持久化 | 低（稳态） | 高（恢复/重试） | L | 中高 | P1 |
| Phase 4 硬件时间戳/HA | 测量/运维 | 高（生产） | M/L | 中 | P2 |

排序原则：**先消除协议/分配/唤醒的结构性瓶颈，再替换数据面；恢复/幂等与数据面并行推进，
但先保证语义正确；硬件时间戳用于验收，不先于被测对象。**

## 8. 验收指标与基准协议

### 8.1 指标

- 内部撮合：p50/p90/p99/p99.9/p99.99/max、stddev、p99.9/p50 尾比。
- 请求：入队→回复、decode、encode、socket send、socket-to-socket、wire-to-wire（硬件）。
- 排队：每连接 in-flight、actor/ring 队列深度、ring 满次数、WAL 队列深度。
- 提交：WAL append、commit、fsync 延迟分布；未提交批次 age。
- 吞吐：orders/s、trades/s、reject/s；1x/4x/8x 负载；过载行为。
- 分配：稳态 allocations/op（目标 0）、syscalls/op。
- 恢复：RTO、重放记录数、峰值内存、快照大小。
- 资源：CPU cycles/op、cache-miss、context-switch、page-fault。
- 正确性：重复请求、乱序响应、崩溃点恢复、WAL 损坏、快照损坏。

### 8.2 基准协议

- release + `target-cpu=native` + 固定工具链；记录机器/OS/编译参数。
- 绑核、warmup ≥ 100k 次、采样 ≥ 1M；多次运行报告分位稳定性。
- 延迟只报分位/max/抖动，不报平均值；网络报告写清软件/硬件时间戳边界。
- 每次优化必须给出"优化前/后 + 置信度/波动"；不能只看单次跑分。
- CI：至少跑小规模 smoke + 关键路径 p99 门槛；完整基准在固定 runner 上跑。

## 9. 不做/延后

- **不重写 C++**：ADR-1。
- **不在没有测量前上 DPDK/AF_XDP/FPGA**：当前软件栈瓶颈明确；硬件旁路需要目标网卡/OS/团队运维能力。
- **不把撮合核心 async 化**：只会增加调度和 future 开销。
- **不为了"自愈"掩盖持久化错误**：fail-fast 是交易系统正确性要求。
- **不做无界缓存/无界队列**：幂等、in-flight、WAL 队列都必须有上限。
- **不先做 HA 而忽略单机数据面**：单机正确性和 RTO 是 HA 的前提。

## 10. 下一步 backlog（建议顺序）

### Sprint A（协议与分配）

1. `src/shared/protocol.rs`：消息头/版本/`request_id`/错误码；响应回显；wire 编解码与测试。
2. `src/shared/symbol.rs`（新）：SymbolTable/SymbolId；decode 使用 id；订单簿/成交/分区路由改造。
3. `src/interfaces/cli/mod.rs`：连接读循环与匹配解耦；有界 in-flight；writer 任务；断连/超时清理。
4. `src/bin/load_generator.rs`：pipelined 模式、in-flight 配置、request_id 校验、乱序/丢包统计。
5. `benches/latency_percentiles.rs`：pipelined 请求基准；零分配断言工具。

### Sprint B（数据面与 WAL）

6. `src/interfaces/cli/mod.rs` + 新 `shared/ring.rs`：有界 SPSC ring（cacheline padding、index-based）、busy-poll 选项。
7. `src/application/services/persistent_service.rs`：WAL 提交从撮合线程移出；commit barrier/回执顺序。
8. `src/infrastructure/persistence/wal.rs`：group commit 时间/字节阈值；WAL 队列/背压指标；io_uring 可选实现。
9. `src/interfaces/cli/mod.rs`：绑核/隔离配置；SPSC 数据面开关。
10. `src/shared/metrics.rs`：ring/队列/CPU 指标。

### Sprint C（恢复与幂等）

11. `src/infrastructure/persistence/snapshot.rs`（新）：快照格式/校验/原子写。
12. `src/application/services/persistent_service.rs`：加载快照 + 流式尾段重放；状态版本。
13. `src/application/services/persistent_service.rs`：幂等结果持久化与恢复语义。
14. `tests/`：崩溃点、损坏、快照交界、幂等跨重启测试。

## 11. 风险与缓解

| 风险 | 缓解 |
|---|---|
| 协议 v2 破坏旧客户端 | 版本握手；双版本解码窗口；明确拒绝并给出升级错误码 |
| WAL v4 迁移 | 格式版本 + 明确拒绝旧格式；提供离线导出/导入工具而非静默兼容 |
| SPSC/busy-poll 抢占 CPU/功耗 | 配置开关；仅与绑核/隔离联合启用；默认阻塞线程 |
| 快照一致性/IO 抖动 | 暂停/写时复制；原子 rename；CRC；保留多版本；后台限速 |
| io_uring 可移植性 | trait/feature 抽象 + 阻塞 WAL 线程 fallback；目标 OS 验证 |
| 幂等表内存/持久化开销 | 有界 + 租约/窗口；快照增量；指标 hit/evict/expire |
| 测量过拟合开发机 | Linux 目标机 + 硬件时间戳复测；报告边界和置信度 |
| 过度设计 | 每阶段独立验收；不达标不进入下一阶段；保留回滚路径 |

## 12. 附录：复现命令与当前状态

```bash
# 进程内撮合/撤单分位（release）
cargo bench --bench latency_percentiles

# 网络 + 服务端分位（debug；生产改用 release）
CLIENTS=64 cargo run --bin load_generator

# 单元/集成测试
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

当前状态（2026-11-04）：

- 91 lib tests + 4 integration + 1 doc test 通过；clippy `-D warnings` 通过。
- WAL 格式 v3；wire 协议尚未版本化。
- 已实现 group commit、幂等（有界/进程内+重启 seed）、O(1) 撤单、阶段 ns 指标。
- 未实现：协议 v2、SymbolId、SPSC、快照、硬件时间戳、HA。

## 13. 结论

**会像 C++ 一样设计核心，但不会照搬实现。** 单写者、group commit、侵入式 FIFO、slab、
静态分发这些都会保留；C++ 更激进的数据面纪律（SymbolId/arena、SPSC、busy-poll、io_uring、
绑核、硬件时间戳）应当逐步借到 Rust 实现里。

当前最值得做的不是换语言，而是：

1. 用协议 v2 + pipelining 解开连接级吞吐限制；
2. 用 SymbolId 消除每笔分配/字符串哈希；
3. 用绑核/SPSC/WAL 异步提交把撮合线程从调度和 fsync 中解放（Tokio 已移除）；
4. 用快照 + 幂等持久化解决恢复 RTO/语义与重试正确性；
5. 最后才用硬件时间戳和 CI 性能门槛验收 ns 级目标。
