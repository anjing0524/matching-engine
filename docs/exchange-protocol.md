# 交易所核心协议与共享内存布局（草案）

状态：**协议草案，先对齐字段和偏移，未接运行时**。  
上游决策见 [exchange-core-architecture.md](exchange-core-architecture.md)。

## 1. 原则

- 所有整数小端；协议不传字符串合约名。
- 价格 = 整数 tick；数量 = 整数手/张；时间 = u64 纳秒。
- ID 用位拼接，不使用 hash / UUID。
- `global_seq` 由 middleware 单 writer 分配。
- private/public 是两个协议面，字段不合并。
- command/public 结果区按交易日预分配，不覆盖。
- 拒绝消息保留在 ingress log，`core_eligible=false`。
- core 不读 wall-clock、不碰 socket、不碰文件。

## 2. 通用 ID

```text
order_id  = (epoch << 48) | global_seq
trade_id  = (epoch << 48) | trade_seq
```

- epoch：交易日/启动批次；48 位 seq 不足时升级到 u128 或调整位宽。
- `combo_id` 由 reference data 分配，u32。
- `instrument_id` 由 reference data 分配，u32。
- `gateway_id/session_id/member_id/user_id/seat_id` 均为内部 ID。

## 3. Command（middleware → core）

固定 slot 256B。字段建议：

| offset | size | field | 说明 |
|---:|---:|---|---|
| 0 | 8 | `global_seq` | middleware 全局序号 |
| 8 | 8 | `gateway_seq` | gateway 本地序号 |
| 16 | 8 | `ingress_ts_ns` | middleware 入口时间 |
| 24 | 8 | `exchange_ts_ns` | 交易所时间/PTP |
| 32 | 8 | `client_order_id` | 会员订单号 |
| 40 | 8 | `session_id` | 会员会话 |
| 48 | 4 | `gateway_id` | 前置 ID |
| 52 | 4 | `member_id` | 会员 |
| 56 | 4 | `user_id` | 用户 |
| 60 | 4 | `seat_id` | 席位 |
| 64 | 4 | `instrument_id` | 合约 |
| 68 | 4 | `combo_id` | 组合定义；0 表示非组合 |
| 72 | 4 | `session_epoch` | 会话重连代数 |
| 76 | 1 | `command_type` | New/Cancel/Control/TimeTick |
| 77 | 1 | `price_type` | Limit/Market/Advanced |
| 78 | 1 | `time_condition` | GFD/IOC/GIS |
| 79 | 1 | `volume_condition` | AV/MV/CV |
| 80 | 1 | `side` | Buy/Sell |
| 81 | 1 | `offset` | Open/Close/CloseToday |
| 82 | 1 | `hedge` | Spec/Hedge/Arb |
| 83 | 1 | `flags` | 保留 |
| 84 | 4 | `leg2_ratio` | 组合第二腿比例 |
| 88 | 8 | `price_ticks` | 整数 tick |
| 96 | 8 | `quantity` | 手数 |
| 104 | 8 | `trigger_price_ticks` | 止损触发价；0 表示无 |
| 112 | 4 | `leg2_instrument_id` | 组合第二腿 |
| 116 | 1 | `leg2_offset` | 第二腿开平 |
| 117 | 1 | `leg2_hedge` | 第二腿投保 |
| 118 | 1 | `trigger_type` | None/Stop/... |
| 119 | 1 | `reserved0` | 对齐 |
| 120 | 8 | `cancel_order_id` | Cancel 用 |
| 128 | 128 | `reserved` | 扩展到 256B |

说明：

- `price_type × time_condition × volume_condition` 正交；FAK/FOK 是组合，不单列枚举。
- `combo_id != 0` 时，`leg2_*` 有效；两腿共用同一 `order_id`。
- `Cancel` 只用 `cancel_order_id`，其他字段为零。
- 256B 定长，所有消息可 memcpy；不足补零。

## 4. PrivateEvent（core → middleware → gateway）

固定 slot 128B，按 session 路由：

| offset | size | field | 说明 |
|---:|---:|---|---|
| 0 | 8 | `private_seq` | 每 session 严格递增 |
| 8 | 8 | `global_seq` | 对应命令/成交序号 |
| 16 | 8 | `order_id` | 交易所订单号 |
| 24 | 8 | `client_order_id` | 会员订单号 |
| 32 | 8 | `exchange_ts_ns` | 交易所时间 |
| 40 | 8 | `filled_qty` | 累计成交 |
| 48 | 8 | `remaining_qty` | 剩余 |
| 56 | 8 | `last_price_ticks` | 最近成交价 |
| 64 | 8 | `canceled_qty` | 撤销量 |
| 72 | 4 | `instrument_id` | 合约 |
| 76 | 4 | `combo_id` | 组合 |
| 80 | 4 | `session_id` | 路由 |
| 84 | 1 | `event_type` | Accepted/Rejected/Partial/Filled/Canceled |
| 85 | 1 | `side` | 方向 |
| 86 | 1 | `offset` | 开平 |
| 87 | 1 | `hedge` | 投保 |
| 88 | 2 | `reason_code` | 拒绝/撤销原因 |
| 90 | 1 | `leg` | 组合腿标记 |
| 91 | 37 | `reserved` | 对齐到 128B |

- `Rejected` 只带 reason，不产生订单状态。
- private 全交易日按 session 保留，重连重发。

## 5. PublicEvent（core → middleware → 组播）

固定 slot 128B：

| offset | size | field | 说明 |
|---:|---:|---|---|
| 0 | 8 | `market_data_seq` | 全局行情序号 |
| 8 | 8 | `exchange_ts_ns` | 交易所时间 |
| 16 | 8 | `order_id` | 订单/成交关联 |
| 24 | 8 | `trade_id` | Trade 用 |
| 32 | 8 | `price_ticks` | 价格 |
| 40 | 8 | `quantity` | 数量/剩余 |
| 48 | 8 | `buy_order_id` | Trade |
| 56 | 8 | `sell_order_id` | Trade |
| 64 | 4 | `instrument_id` | 合约 |
| 68 | 4 | `combo_id` | 组合 |
| 72 | 4 | `session_id` | SessionSwitch/TimeTick 用 |
| 76 | 1 | `event_type` | Rested/Canceled/Trade/TimeTick/SessionSwitch/AuctionResult |
| 77 | 1 | `side` | 方向/主动方 |
| 78 | 1 | `phase` | 交易节 |
| 79 | 1 | `flags` | combo/synthetic 等 |
| 80 | 2 | `reason_code` | cancel 原因 |
| 82 | 2 | `reserved0` | 对齐 |
| 84 | 44 | `reserved` | 对齐到 128B |

- combo 成交发两条普通 leg event：leg1 order/trade，leg2 order/trade；带 `combo_id` 和 leg。
- TimeTick / SessionSwitch 用同一 slot，`instrument_id=0`。
- 不发订单簿快照；下游自行构建。

## 6. 共享内存区域

按交易日映射文件：

```text
control block        4 KiB
command log          10,000,000 x 256B = 2.56 GiB
public result log    10,000,000 x 128B = 1.28 GiB
private ring         1,000,000 x 128B = 128 MiB
reserved             alignment / future
```

### control block

```text
magic/version
trading_day
epoch
command_write_seq / command_read_seq
result_write_seq / result_read_seq
private_write_seq / private_read_seq
phase
health/flags
```

所有 head/tail 按 64B cache line 分开，单 writer 原则。

### command log

- middleware 单 writer 追加，core 单 reader 顺序消费。
- 定长 256B，不覆盖；日切轮换新文件。
- core 记录 `processed_seq`；崩溃后从当天起点全量重放。

### public result log

- core 单 writer 追加，middleware 单 reader。
- 定长 128B，不覆盖；全天保留。
- middleware 从同一块内存组播，不复制。
- NACK 补发直接按 `market_data_seq` 读取。

### private ring

- core 单 writer，middleware 单 reader。
- 事件带 `session_id`；middleware 按 session 分发到 TCP。
- 有界即可，不需要全天；middleware 保存 private event 到交易日结束用于重发。

## 7. 并发与可见性

- 所有跨进程 head/tail 使用 `AtomicU64`，release/acquire。
- slot 数据写完后再 publish tail；consumer 读到 tail 后 acquire。
- 每个 head/tail 独立 cache line（64B 对齐）。
- command/public 为 append-only，不做 ring wrap；private 可用 ring wrap，但需防止覆盖未消费记录。
- core 不从文件读命令，只读 mmap 的 command log。

## 8. 序号规则

```text
gateway_seq      每 gateway 会话本地递增
global_seq       middleware 全局递增，进入 command log
private_seq      每 user/session 在 gateway downlink 分配，用于重连重发
market_data_seq  core public 事件全局递增
trade_seq        core 成交递增，派生 trade_id
```

- gateway 重连带 `session_epoch + last_private_seq`。
- gateway downlink 把 core 与 combo 的 private effects 合并后统一编号；重连时
  `GatewayRuntime` 按同一个 `private_seq` 重发 core + combo 事件。
- public gap 由 gateway 发 NACK，middleware 按 seq 补发。

## 9. 未决项

1. FOK 的官方支持依据。
2. 市价单剩余处理。
3. 持仓限额是否计未成交开仓。
4. 集合竞价 tie-break 正式条款。
5. combo implied 首版范围。
6. 峰值事件数、真实内存预算、生产 slot 大小。
