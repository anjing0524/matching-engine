# 代码审查：按八条工程原则（2026-11-16）

- 审查基线：`main @ 9b70d63e`（已完成历史分支合并，见下文）
- 范围：`crates/` + `src/` 共 72 个 `.rs` 文件、约 22,757 行；本轮聚焦撮合内核、组合保证金、网关边界、快照与配置
- 方法：逐原则人工阅读 + 定向 grep（`unwrap_or*`、`let _ =`、`saturating_*`、`HashMap` 迭代、哨兵值），并用 `cargo check --workspace --all-targets` 与 `cargo test -p trading-domain` 验证合并后基线可编译、领域测试通过

审查前置工作：`diag/gateway-write-stats`、`perf/sparse-level-table` 已合并进 `main`（`dca4aff7`、`9b70d63e`），其余 10 个分支经补丁等价（`git cherry`）/ 已被后续提交取代（`253da735`、`804211d4`）确认后删除；全部原始分支已打 `backup/20261116/*` 标签，可随时恢复。

## 结论摘要

| 原则 | 判断 | 主要问题 |
| --- | --- | --- |
| 1 Fail Fast | ✅ 已收敛 | 内核/组合/集合竞价路径的持仓错误已改为显式失败 |
| 2 单一权威实现 | ⚠️ 局部 | AdvancedMarket 双路径语义仍不一致（待统一） |
| 3 显式状态 | ⚠️ 部分收敛 | 鉴权已 fail-closed；扁平 `Command` 的 0 哨兵与 `flags` 多义仍待改 |
| 4 强不变量 | ✅ 已收敛 | 成交路径、快照去重、`FrozenClose` 严格扣减均已落地 |
| 5 边界校验 | ✅ 基本达标 | 线格式 tag/长度校验到位；缺解码失败指标与重定向地址截断告警 |
| 6 禁止静默兜底 | ⚠️ 基本收敛 | 结算价、释放、腿价兜底已显式化；`margin_amount` 仍饱和（方向为多收保证金） |
| 7 确定性 | ✅ 基本达标 | 快照各层已排序；哈希映射为定长指针型，无随机种子 |
| 8 最小抽象 | ✅ 达标 | 仅 1 个业务 trait（4 个实现），无空壳抽象 |

## 高风险发现

### H1 内核吞掉持仓更新失败（原则 1、4）

`src/trading/core/kernel.rs:1049`、`src/trading/core/kernel.rs:1090`：

```rust
let _ = self.positions.apply_close(key, offset, quantity);
```

`Positions::apply_close` 在数量超过可平仓量时返回 `InsufficientPosition`（`positions.rs:141-146`），但结果被丢弃——成交回报照常发出，持仓却不减，账实不符且无任何错误信号。同类问题见 `kernel.rs:310`、`927`、`1046`、`1087` 对 `apply_open` 的调用（溢出被吞掉）。

建议：`apply_fill` 改为返回 `Result` 并把失败升级为拒单/停机断言；持仓扣减失败必须让该笔成交整体失败，而不是“报告成交但不动仓位”。

### H2 保证金快照恢复把缺失结算价静默当 0（原则 6）

`src/trading/core/combo_margin.rs:576-583`：

```rust
let leg1_prior_settlement = prior_settlements
    .get(&u64::from(pair.leg1_instrument_id))
    .copied()
    .unwrap_or(0);
```

实时路径对缺失结算价是显式失败（`ComboMarginError::MissingPriorSettlement`，`combo_margin.rs:75-83`），恢复路径却静默取 0：算出 `leg1_frozen = 0`。若快照中该值也恰为 0（例如保证金率配置为 0），校验会通过，之后按 0 结算价释放预留，账户可用保证金被静默高估。这是同一业务规则两套语义（同时违反原则 2）。

建议：恢复路径复用同一结算价查找函数并返回 `MissingPriorSettlement`，禁止 `unwrap_or(0)`。

### H3 保证金释放找不到预留时静默返回（原则 1、4）

`src/trading/core/combo_margin.rs:393-395`、`431-434`、`650-658`：`release_on_fill`/`release_all`/`release_frozen` 在预留或账户腿记录不存在时直接 `return`。成交了却没有预留、或账户记录被删，都会让冻结额度永久滞留或凭空消失，且无日志、无计数。

建议：预留缺失属于账本不变量破坏，应返回错误并带 `order_id` 上报；仅“无此账户”可视为合法空操作并注明。

### H4 网关鉴权默认放行（原则 3、6）

`src/trading/gateway/session_policy.rs:54-60`：

```rust
self.auth_token.is_none_or(|expected| expected == auth_token)
```

未配置 `--auth-token` 且未注入 `GatewayCredentials` 时，`login_allowed` 对任意 token 返回 `true`。`SessionPolicy::default()` 正是该状态（`gateway/runtime.rs:104`），启动阶段没有任何“生产环境必须配置鉴权”的校验或告警。开发便利的默认值在生产会变成“匿名可下单”。

建议：把“无鉴权”改成显式开关（如 `SessionPolicy::insecure_allow_anonymous()`），默认构造不可登录；或启动时检测到无凭据直接拒绝启动。

### H5 市价单的“有效价”与“存储价”不一致（原则 2、3）

`kernel.rs:807-812` 用 `spec.max_price/min_price` 作为市价单撮合价，但 `kernel.rs:897` 存的是 `command.price_ticks as u64`（市价单通常传 0），撤单事件 `kernel.rs:694`、`720` 又把这个 0 当作成交价上报。同一订单在撮合、元数据、回报三处有三个价格语义。

建议：`OrderMeta.price_ticks` 存实际生效价（市价单即撮合边界价），并让市价单在边界校验阶段要求 `price_ticks == 0`，否则拒单。

## 中低风险发现

### 原则 1：Fail Fast

- `combo_margin.rs:396`：`remaining = remaining.saturating_sub(filled)`，成交数量大于预留数量时静默夹到 0，掩盖重复成交/超量释放。应报错。
- `gateway/server.rs:333-346`：`GatewayFrame::decode` 失败直接 `return` 关闭连接，无日志、无 `decode_errors` 计数，排障时无法区分对端发错帧与网络断开。

### 原则 2：单一权威实现

- AdvancedMarket：`combo_admission.rs:48-55` 允许，`kernel.rs:813-820` 明确“未实现”拒单。协议枚举支持不等于路径支持，两条路径需统一为“组合不支持市价高级单”或都实现。
- `registry.rs:523`：`u64::try_from(self.margin.available(...)).unwrap_or(0)`——可用保证金是 `u128`，转 `u64` 失败时变成 0。当前 `available <= balance <= u64::MAX` 不会触发，但写法把“转换失败”伪装成“额度为零”；改用 `as u64` 或 `expect` 表达真实不变量。

### 原则 3：显式状态

- `protocol.rs:263-272`：`Command` 是扁平结构，`cancel_order_id`、`trigger_price_ticks`、`leg2_*` 在所有命令类型上都有值，`Command::zeroed()`（`protocol.rs:278`）用 0 表示“不适用”。解码时（`protocol.rs:352-385`）只校验 tag，不校验不适用字段为零。
- `protocol.rs:263`：`flags: u8` 同时承载 Control 命令的 `TradingPhase`（`kernel.rs:1147`、`registry.rs:1686`），普通新单携带非零 `flags` 会被静默忽略。建议内部用 `enum CommandKind { New(NewOrder), Cancel { id }, Control { phase }, Tick }` 表达，线格式层保持扁平。

### 原则 4：强不变量

- `positions.rs:102-104`：`Positions::restore` 用 `collect()` 直接重建映射，重复键会静默覆盖；`CoreSnapshot`（`kernel.rs:163-180`）恢复时同样 `into_iter().collect()` 到 `orders`。快照是外部输入，应像保证金快照那样逐条校验重复/溢出。
- `positions.rs:219-226`：`FrozenClose::release` 饱和扣减且注释写明“容忍重复释放”——这让“同一笔成交被处理两次”这类严重 bug 变得不可见。
- `positions.rs:44-50`、`66-73`：`PositionKey`/`CloseKey` 手写 `Hash`，把 `account << 5 | side | hedge << 2` 直接塞进 `u128`。当前 `Side` 2 值、`Hedge` 3 值恰好不冲突，但枚举一旦加值（如 `Side::Unknown`）就会静默合并不同持仓。
- `tick_based.rs:108-117`：`LevelTable` 的 `Index` 对未分配页返回 `EMPTY_LEVEL`，`IndexMut` 却对越界索引直接 panic（`self.pages[page_index]`），读越界静默、写越界崩溃，两种失败模式不一致。

### 原则 6：禁止静默兜底

- `combo_margin.rs:662-668`：`margin_amount` 全程 `saturating_mul` 后再除，溢出时结果是一个“很大的数”而非错误；`try_freeze` 里再 `saturating_add` 累加。金融计算建议 `checked_mul`/`checked_add` 并返回显式错误。
- `registry.rs:1450-1465`：`direct_leg_reference` 的兜底链“最优价 → 昨结算 → 0”有文档说明，但 0 价会被写进成交腿价；至少应把“使用了兜底价”体现在事件/日志里。
- `kernel.rs:605`：`result.price_ticks.unwrap_or(0)`——集合竞价无成交价时公开事件报 `0`，与“无成交”在语义上不可区分。

### 原则 7：确定性

- 已确认：`Registry::snapshot`（`registry.rs:666-690`）、`TradingCore::snapshot`（`kernel.rs:229-251`）、`ComboMarginLedger::snapshot`（`combo_margin.rs:494-520`）都对集合做了排序；`U64MixHasher`（`identity.rs:37-61`）无随机种子，相同输入得到相同哈希分布。
- 待确认：`rate_limit`（`session_policy.rs:8-14`）用 `f64`，不影响撮合结果，但跨平台浮点边界可能不同；若限流需要可复现，应改整数令牌桶。

### 原则 8：最小抽象

- 全仓仅 `PreTradePlugin` 一个业务 trait（`plugins.rs:28`，4 个实现），无单实现的空壳 trait；`OrderIdentity` 等 newtype 均有实际序列化/校验用途。本原则未见需要削减的抽象。

## 修复记录（2026-11-16 收敛）

| 编号 | 处理方式 | 关键改动 |
| --- | --- | --- |
| H1 | 持仓更新失败不再被吞 | `apply_fill`/`apply_auction_fill` 返回 `Result<(), PositionError>`，成交路径用 `expect` 把不一致升级为显式失败；`apply_combo_fill_position` 同样返回错误 |
| H1b | 集合竞价拒绝平仓单 | `queue_auction_order` 返回 `Result<u64, PositionError>`，平仓指令显式拒绝，避免竞价成交时无法扣减持仓 |
| H2 | 快照缺结算价显式失败 | `restore_snapshot` 用 `MissingPriorSettlement` 取代 `unwrap_or(0)` |
| H3 | 未知预留/超量释放显式失败 | `release_on_fill`/`release_all` 返回 `Result`，新增 `UnknownReservation`、`OverRelease`、`UnknownLegMargin`、`FrozenUnderflow` |
| H4 | 鉴权默认 fail-closed | `SessionPolicy` 默认 `Deny`；匿名访问改成显式 `with_anonymous_access()`；两个二进制无凭据时拒绝启动，需 `--auth-token` 或显式 `--allow-anonymous` |
| H5 | 市价单价格统一 | `OrderMeta.price_ticks` 与挂单/撤单公开事件统一使用实际生效价 `matching_price` |
| H6 | 快照重复键拒绝 | `restore_snapshot` 在改动状态前校验 resting/active order、position、pending-open、frozen-close 重复键 |
| H7 | 可用保证金转换显式化 | `available_margin` 用 `expect` 表达“可用额度不超过 u64 余额”的不变量 |
| H8 | 直接组合腿价兜底移除 | 腿簿无报价时要求昨结算价，不再静默回落到 0 |
| H9 | 回归测试锁定 | `combo_margin` 新增 3 项（未知预留、超量释放、缺结算价恢复）、`kernel` 新增 3 项（竞价拒平仓、市价生效价、快照重复键）、`positions` 新增 1 项（重复释放被拒） |
| H10 | `FrozenClose` 严格扣减 | `release` 返回 `Result<(), PositionError>`，缺失预留或超量释放显式失败；三个调用点分别用 `?` 或 `expect` 处理 |

验证：`cargo test --lib` 171 项通过；`combo_*`、`implied_in`、`leg_liquidity`、`runtime_*`、`trading_binary_smoke`、`trading_split_smoke`、`trading_multi_gateway_smoke`、`trading_snapshot_binary`、`trading_session_churn` 全部通过；`cargo clippy --workspace --all-targets` 无告警。

## 验证记录

- `git fetch origin --prune`：网络恢复后成功拉取，`origin/main` = `20400fd6`；经 `git merge-base` 核对其为本地 `main` 的祖先，即远端没有比本地更新的提交，本地 `main` 领先 21 个提交。
- 全量非性能测试：`cargo test --lib` 171 项、20 个集成测试套件（`combo_*`、`implied_in`、`leg_liquidity`、`multi_contract_core`、`runtime_*`、`trading_*`）全部通过。
- `cargo clippy --workspace --all-targets`、`cargo fmt --all --check`：通过。
- `cargo check --workspace --all-targets`：通过。
- `cargo test -p trading-domain`：30 passed, 0 failed。
- 合并冲突（`tick_based.rs` 构造函数）按语义解决：价位表用 sparse 分支的 `LevelTable::new`，节点用 diag 分支的 `OrderNodeSlab::new`。

## 剩余判断（本轮明确不改）

以下三项需要产品决策或线格式兼容，不属于“实现错误”，已记录并在本轮保留：

1. **AdvancedMarket 双路径**：组合路径实现了保护价语义（`combo_admission.rs`），普通内核明确拒绝“未实现”（`kernel.rs`）。这是功能覆盖差异，不是同一条规则的两份实现；统一需要先定产品语义，故不改。
2. **`Command` 扁平哨兵与 `flags` 复用**：`Command` 直接镜像 256 字节线格式，改成内部枚举会引入线格式与内存布局的双重维护；`flags` 承载 Control 的 phase 属于既有协议约定。若后续要改，建议“线格式层保持扁平 + 内核入口转 `enum CommandKind`”，作为独立重构。
3. **`margin_amount` 饱和乘法**：溢出方向是多算保证金（fail-closed，会拒单而非少收保证金），且真实参数远达不到 u128 溢出；改成 `checked_*` 需要给释放/恢复/实际成交三条路径新增错误变体，收益低于复杂度，故保留并在函数文档注明饱和语义。

## 第二轮审查（combo_book / runtime / transport）

聚焦审查报告建议的三个方向后，发现并修复 2 处，其余确认无问题：

| 编号 | 原则 | 问题 | 处理 |
| --- | --- | --- | --- |
| R1 | 强不变量 / Fail Fast | `ComboBook::consume_resting` 只有 `debug_assert!` 保护，随后用未检查减法；release 构建下超量成交会让 `remaining` 回绕成巨大值 | 改为 `checked_sub`，超量时返回新增的 `QuantityExceedsRemaining { order_id, remaining, requested }`，并补回归测试 |
| R2 | Fail Fast / 不静默 | `MiddlewareSequencer::submit` 中命令日志 `append` 失败会返回错误，但紧随其后的 `fsync` 用 `let _ =` 忽略：日志可能未落盘却已向上游确认 | 改为 `log.sync().map_err(CommandTransportError::Io)?`，与轮转日志的错误处理保持一致 |

确认无问题的部分：

- `SpscRing`：容量/槽位在 `open` 时校验，head/tail 使用 release/acquire 且计数回绕安全，pop 时校验槽位长度，未发现越界或撕裂读。
- `ComboBook::match_taker` / `snapshot` / `restore_snapshot`：价格优先、时间优先一致；恢复时校验重复 arrival、过期 next seq、交叉盘口。
- `TradingRuntime::tick` 与 `EventFanout`：私有事件在派发前先写日志；sink 错误计入 `fanout_errors`，不吞掉计数。
- `SessionRegistry`：重复/零 session 显式拒绝，takeover 先停旧会话再激活新会话。

## 下一轮审查建议

- `combo_book.rs`（1156 行）：撮合/隐含撮合的边界与优先级不变量。
- `runtime/engine.rs`、`transport/*`：并发、重放、快照一致性。
- 若确定要统一 AdvancedMarket，先补 ADR 再实现普通内核保护价语义。
