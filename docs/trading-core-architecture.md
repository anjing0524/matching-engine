# 交易所核心系统架构决策

状态：**设计已对齐，尚未落代码**。本文记录本轮需求澄清后的目标架构，避免后续实现继续建立在错误假设上。

## 1. 目标与边界

目标是期货交易所核心系统，不是 HFT 策略机：

```text
会员 → gateway（前置）→ middleware（全局排序/日志/行情分发）→ core（业务状态机/撮合）
```

- gateway：会员连接、鉴权、会话、席位映射、消息转发；不判断交易节。
- middleware：全局排序、ingress/command log、完整消息索引、private 路由、public 组播、NACK 补发。
- core：唯一业务状态机，持有订单簿、订单、成交、持仓、限额计数、交易节状态、reference data；不持有客户资金/保证金；不直接连 gateway；无网络。
- public 行情与 private 回报是两个协议面：public 走组播，private 走 TCP。

## 2. 数据所有权

| 数据 | 权威持有者 | 说明 |
|---|---|---|
| 会员会话、席位映射 | gateway | middleware 校验 member/session/seat 映射 |
| 全局序号、ingress/command log | middleware | 唯一排序权威 |
| 原始消息 trace | middleware | 审计/追查；磁盘保存 |
| canonical command | middleware | 定长 256B，作为 core 输入 |
| 订单簿、订单、成交 | core | matching 唯一写者 |
| 持仓（多空/今昨/投保） | core | 毛持仓，不净额 |
| 可平冻结 `frozen_close_qty` | core | 风控额度计数器，不是对象关联 |
| 交易限额计数 | core | 按交易所规则 |
| reference data | core | 启动时版本化加载；gateway 只读路由缓存 |
| 交易节/phase | core | middleware 产生有序 TimeTick/SessionControl，core 应用并输出 SessionSwitch |
| private event 保留 | middleware | 按 session 保存整个交易日，用于重连重发 |
| public result | middleware | 全天预分配 append log，用于组播/NACK |
| 客户/会员资金、保证金、结算 | 不在 core | 由会员/结算系统负责 |

## 3. 流与协议

```text
1. gateway ↔ middleware：TCP 全双工，命令上行 + private 下行
2. core ↔ middleware：共享内存
3. core → middleware → gateway/member：public 组播
```

- gateway 每条消息有 `gateway_seq`，middleware 存本地号并分配 `global_seq`。
- private 每 session 一个 `private_seq`，在 gateway downlink 统一分配；core 与 combo 的 private effects 合并编号。重连带 `last_private_seq`，GatewayRuntime 重发 core + combo。
- public 每条消息有 `market_data_seq`；gateway 发现 gap 发 NACK，middleware 从全天结果区补发。
- TimeTick 每 250ms 一条，进入 command log；`SessionControl` 在交易节边界精确发送。
- 不做 query 协议、不做 per-message ack、不做 Halt 广播、不做自动故障恢复。

## 4. 订单属性

订单属性是正交字段，不是互斥枚举：

```text
price_type       = Limit / Market / 高级市价
time_condition   = GFD / IOC / GIS
volume_condition = AV / MV / CV
side / offset / hedge
```

- FAK = IOC + MV；
- FOK = IOC + CV；
- 限价/市价与 FAK/FOK 正交；
- FAK/FOK 不 resting；
- 目标 profile 的合法组合由 reference data/profile 决定，非法组合直接拒绝。

## 5. 校验边界

### gateway

- 会员/用户/席位登录、鉴权；
- 连接/消息速率；
- 转发命令，不判断 phase。

### middleware

- session/phase 准入；
- 合约存在性、字段格式、字段组合；
- 重复报单/会话序号；
- 被拒消息写 inbound trace，不进 command log。

### core

- 合约状态、交易时段、价格 tick/涨跌停、数量；
- 交易权限、持仓限额、交易限额；
- 平仓/平今可平量；
- 组合腿合法性、组合限仓；
- 自成交预防作为可选插件，默认关闭；
- **不校验客户/会员保证金**。

插件原则：

```text
decode → kernel mandatory checks → plugin chain → matching → state update → events
```

- 插件静态泛型单态化，编译期固定集合；
- 插件只读状态，只能 Accept/Reject；
- 持仓/限额/权限/组合规则做成插件；
- 止损作为独立 TriggerPlugin，首版只预留接口。

## 6. 持仓与订单

- Position 和 Order 是分离对象，不把 position 挂在 order 上。
- 持仓维度：member、client/trading_code、instrument、long/short、spec/hedge/arb、today/yesterday。
- 平仓/平今严格区分，不自动转换。
- 接受平仓单时扣减 `frozen_close_qty`；成交/撤单释放，防止多笔平仓单累计超平。
- 日盘收盘当日委托失效；夜盘未成交委托转入同一交易日日盘。

## 7. 撮合与组合

- 连续竞价：价格优先、时间优先；成交价取买价/卖价/前成交价中位数。
- 集合竞价：最大成交量；tie-break 最接近上一结算价，再取较低价（以交易所细则为准）；市价/套利不参与。
- 市价单优先于限价单；市价/FAK/FOK 的剩余处理以 profile 规则为准。
- 组合单：2 腿标准组合，拆到单腿订单簿原子成交；无独立组合订单簿；组合队列按价差优先/时间优先。
- 组合单可部分成交，两腿同手数、同时成交，不允许腿风险。
- 自成交预防不是首版核心。

## 8. 日志与容量

```text
IngressRecord:
  global_seq, gateway_id, session_epoch, gateway_seq,
  raw_msg_hash, canonical_cmd, validation_result, core_eligible
```

- 合并 inbound trace 与 command log，拒绝记录也在 log 中，`core_eligible=false`；
- 内存保留 canonical command；原始报文 trace 落磁盘；
- 开发/自测容量：1000 万条；
- public result 定长 128B slot；command 定长 256B slot；
- 文件映射按 trading_day 轮换；日终异步落库；
- core 无 snapshot，崩溃后从当天 command log 全量重放；日志不裁剪；
- 生产容量需按实际峰值重新计算。

## 9. 故障语义

- 不做复杂控制面。
- gateway/middleware 断线：gateway 重连，带 `last_private_seq`，GatewayRuntime 重发合并后的 private 流。
- 重连失败：认为故障，停止会员接入。
- 时间源故障：全局停止，不广播 Halt，不自动恢复。
- core 崩溃：从 command log 全量重放；恢复期间不接新命令。
- 不做自动 leader election/分布式 lease。

## 10. 未决项

1. 郑商所 FOK 的官方支持依据（公开资料冲突；实现前以官方接口规范/生产验证为准）。
2. 市价单在“有报价但流动性不足”时剩余如何处理。
3. 持仓限额是否计入未成交开仓委托（按交易所细则）。
4. 集合竞价 tie-break 的正式细则条款。
5. 组合 implied-in 的首版范围。
6. active combo definition / order 规模，用于 combo queue 性能参数。

## 11. 下一步

1. 固化协议 schema（command/private/public）。
2. 画 middleware/core 共享内存布局与 ring/append log。
3. 按 kernel + plugin 拆分 core 模块。
4. 先实现连续竞价 + 限价单，再接 FAK/FOK、集合竞价、组合单。
5. 用回放和确定性测试验证同一 command log 产生同一状态。
