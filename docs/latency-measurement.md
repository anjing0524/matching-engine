# 交易系统延迟测量

本文说明本仓库的延迟指标边界，以及要做到**纳秒级 wire-to-wire** 还需要哪些外部条件。
原则：**不报平均值**，报分位、最大值和抖动；先说清测量边界，再比较数字。

## 1. 术语与边界

| 术语 | 边界 |
|---|---|
| tick-to-trade | 市场数据到达（EOF）到订单发送（EOF）；也有 SOF→SOF、EOT→SOF 口径 |
| wire-to-wire | 入站报文完成到出站报文完成；需要硬件时间戳才能精确到 ns |
| socket-to-socket | 软件从 `recv` 返回到 `send` 完成；本仓库目前实现的是这个口径 |
| request latency | 命令入队 → 撮合线程回复（含 actor 排队、撮合、WAL commit） |
| matching latency | 仅订单簿撮合函数 |
| commit latency | 一批 WAL 的 flush/fsync |
| queue wait | 命令在撮合 actor 队列中的等待 |

本仓库的软件时间戳使用 `std::time::Instant`，只能近似 socket-to-socket；它包含内核网络栈、
用户态调度和发送缓冲，**不是** wire-to-wire。

## 2. 指标

- 必看：p50、p90、p99、p99.9、p99.99、max、标准差/jitter、尾比（p99.9/p50）
- 负载矩阵：1x/4x/8x 市场速率；burst 与持续过载
- 吞吐：orders/s、trades/s、reject/s、drop/s、队列深度
- 资源：CPU cycles/op、cache miss、context switch、page fault
- 恢复：RTO、WAL 重放条数、恢复内存

Prometheus 查询示例：

```promql
histogram_quantile(0.99, sum by (le) (rate(matching_engine_request_latency_nanoseconds_bucket[1m])))
histogram_quantile(0.999, sum by (le) (rate(matching_engine_socket_to_socket_nanoseconds_bucket[1m])))
rate(matching_engine_commit_latency_nanoseconds_sum[1m])
  / rate(matching_engine_commit_latency_nanoseconds_count[1m])
```

注意 `histogram_quantile` 是桶内插值；追求 ns 级结论时应保留原始样本或硬件时间戳，
而不是只看 Prometheus 的桶分位。

## 3. 硬件时间戳方案（Linux 生产环境）

1. 网卡/交换机支持硬件时间戳（如 ExaNIC、Napatech、Arista 7130/7135），并启用 PTP：
   `ptp4l` + `phc2sys`，确保主机与交易所时钟同步。
2. 采集边界：入站市场数据 packet 的 NIC RX timestamp；出站订单 packet 的 NIC TX timestamp。
   wire-to-wire = TX timestamp − RX timestamp，必要时拆成 SOF/EOF 口径。
3. 若不能改内核/网卡，用 capture appliance（Corvil/Endace/FMADIO）或交换机时间戳做旁路测量。
4. 软件侧可用 Linux `SO_TIMESTAMPING` 获取内核/硬件收发时间戳；标准
   `std::net::TcpStream` 不直接暴露 ancillary data，需要 raw socket/io_uring 或专门采集程序。
   本仓库的 `socket_to_socket_ns` 只能作为回归监控，不能对外宣称是 wire-to-wire。
5. 报告时必须写清：时间源（PTP/PHC/系统时钟）、测量点（NIC/交换机/用户态）、
   是否包含内核协议栈、负载倍数、样本量、分位口径。

## 4. 参考

- Databento, *What is tick-to-trade latency?*:
  <https://app0.databento.com/microstructure/tick-to-trade>
- Databento, *What is wire-to-wire latency?*:
  <https://app0.databento.com/microstructure/wire-to-wire-latency>
- STAC-T1 EMINI 报告示例（ns 级 mean/p99/max/stddev）:
  <https://docs.stacresearch.com/news/ADHC240918>

外部资料只作为测量口径参考；具体数字必须在目标硬件、目标负载下复测。
