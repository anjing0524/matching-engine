# 贡献指南

感谢您对 Rust 高性能期货撮合引擎项目的关注！我们欢迎各种形式的贡献。

---

## 📋 目录

1. [行为准则](#行为准则)
2. [如何贡献](#如何贡献)
3. [开发环境设置](#开发环境设置)
4. [代码规范](#代码规范)
5. [提交流程](#提交流程)
6. [测试要求](#测试要求)
7. [文档规范](#文档规范)
8. [性能基准](#性能基准)

---

## 🤝 行为准则

### 我们的承诺

为了营造一个开放和友好的环境，我们承诺：

- ✅ 尊重不同的观点和经验
- ✅ 优雅地接受建设性批评
- ✅ 专注于对社区最有利的事情
- ✅ 对其他社区成员表示同理心

### 不可接受的行为

- ❌ 使用性暗示的语言或图像
- ❌ 人身攻击或侮辱性评论
- ❌ 公开或私下骚扰
- ❌ 未经许可发布他人私人信息

---

## 💡 如何贡献

### 报告Bug

在提交Bug报告之前，请：

1. **搜索现有Issue** - 避免重复报告
2. **使用最新版本** - 确认问题在最新代码中仍存在
3. **提供详细信息** - 使用Bug报告模板

**Bug报告应包含**:

```markdown
**环境信息**:
- OS: Ubuntu 22.04
- Rust版本: 1.75.0
- 项目版本: v0.1.0

**复现步骤**:
1. 运行命令 `cargo run --release`
2. 提交订单 ...
3. 观察到错误

**预期行为**: 订单应该被正确处理

**实际行为**: 引擎崩溃，错误信息：xxx

**日志**:
```
[paste logs here]
```

**可能的原因**: (如果有的话)
```

### 提出新功能

在提交功能请求前：

1. **检查路线图** - 查看 [ARCHITECTURE.md](ARCHITECTURE.md)
2. **讨论设计** - 在Issue中先讨论设计思路
3. **考虑性能影响** - HFT系统对性能极其敏感

**功能请求模板**:

```markdown
**功能描述**: 简要描述新功能

**使用场景**: 为什么需要这个功能？

**设计方案**:
- 方案1: ...
- 方案2: ...

**性能影响**:
- 预期性能影响: +/-X%
- 内存影响: +/-XMB

**替代方案**: 考虑过哪些其他方案？

**额外信息**: 相关参考资料
```

### 改进文档

文档同样重要！欢迎：

- 修正拼写错误和语法问题
- 添加代码示例
- 改进API文档注释
- 翻译文档（英文/中文）

---

## 🛠️ 开发环境设置

### 1. 克隆仓库

```bash
git clone https://github.com/your-org/matching-engine.git
cd matching-engine
```

### 2. 安装依赖

**Rust工具链**:

```bash
# 安装Rust (如未安装)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装开发工具
rustup component add rustfmt clippy

# 安装cargo工具
cargo install cargo-watch cargo-edit cargo-tarpaulin
```

**可选工具**:

```bash
# 性能分析
cargo install flamegraph

# 依赖检查
cargo install cargo-audit cargo-outdated

# 文档工具
cargo install mdbook
```

### 3. 验证环境

```bash
# 编译项目
cargo build

# 运行测试
cargo test

# 运行Clippy
cargo clippy -- -D warnings

# 检查格式
cargo fmt -- --check
```

---

## 📐 代码规范

### Rust风格指南

遵循 [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)：

**命名规范**:

```rust
// ✅ 好的命名
pub struct OrderBook { ... }
pub fn match_order(&mut self, order: Order) -> Result<Trade> { ... }
const MAX_PRICE: u64 = 100_000;

// ❌ 避免
pub struct orderbook { ... }  // 类型应使用PascalCase
pub fn MatchOrder(...) { ... }  // 函数应使用snake_case
const maxPrice: u64 = 100000;   // 常量应使用SCREAMING_SNAKE_CASE
```

**文档注释**:

```rust
/// 撮合订单
///
/// 将新订单与订单簿中的现有订单进行撮合。
///
/// # Arguments
///
/// * `order` - 待撮合的新订单
///
/// # Returns
///
/// * `Ok(Vec<Trade>)` - 成交记录列表
/// * `Err(MatchingError)` - 撮合失败的错误
///
/// # Examples
///
/// ```
/// use matching_engine::OrderBook;
///
/// let mut book = OrderBook::new();
/// let trades = book.match_order(order)?;
/// ```
///
/// # Performance
///
/// - Time complexity: O(log n)
/// - Space complexity: O(1)
pub fn match_order(&mut self, order: Order) -> Result<Vec<Trade>, MatchingError> {
    // 实现...
}
```

**错误处理**:

```rust
// ✅ 使用Result和自定义错误类型
pub fn process_order(order: Order) -> Result<Confirmation, OrderError> {
    validate_order(&order)?;

    match self.orderbook.match_order(order) {
        Ok(trades) => Ok(Confirmation::new(trades)),
        Err(e) => Err(OrderError::MatchingFailed(e)),
    }
}

// ❌ 避免unwrap (生产代码)
pub fn process_order(order: Order) -> Confirmation {
    validate_order(&order).unwrap();  // ❌
    let trades = self.orderbook.match_order(order).unwrap();  // ❌
    Confirmation::new(trades)
}
```

**性能关键代码**:

```rust
// 在热路径上添加性能注释
#[inline]  // 小函数内联
pub fn add_order(&mut self, order: Order) {
    // 快速路径优化
    if order.price < self.min_price {
        return;
    }

    // 主逻辑...
}

// 使用#[cold]标记冷路径
#[cold]
fn handle_error(&self, error: Error) {
    // 错误处理...
}
```

### Clippy规则

项目使用严格的Clippy检查：

```toml
# .clippy.toml (如需自定义)
cognitive-complexity-threshold = 30
```

**必须通过的lint**:

```bash
cargo clippy -- \
  -D warnings \
  -W clippy::all \
  -W clippy::pedantic \
  -W clippy::nursery
```

**常见问题修复**:

```rust
// 避免不必要的clone
// ❌
fn process(data: &Vec<u8>) {
    let copy = data.clone();  // 不必要
}

// ✅
fn process(data: &[u8]) {
    // 直接使用引用
}

// 避免过度嵌套
// ❌
fn complex_logic(x: Option<i32>) -> Option<i32> {
    if let Some(val) = x {
        if val > 0 {
            Some(val * 2)
        } else {
            None
        }
    } else {
        None
    }
}

// ✅
fn complex_logic(x: Option<i32>) -> Option<i32> {
    x.filter(|&val| val > 0).map(|val| val * 2)
}
```

---

## 🔄 提交流程

### 1. Fork和分支

```bash
# Fork项目到你的账户
# 然后克隆

git clone https://github.com/YOUR_USERNAME/matching-engine.git
cd matching-engine

# 添加上游仓库
git remote add upstream https://github.com/original-org/matching-engine.git

# 创建功能分支
git checkout -b feature/my-awesome-feature
```

### 2. 开发和提交

**提交消息规范** (遵循 [Conventional Commits](https://www.conventionalcommits.org/)):

```
<type>(<scope>): <subject>

<body>

<footer>
```

**类型 (type)**:
- `feat`: 新功能
- `fix`: Bug修复
- `docs`: 文档更改
- `style`: 代码格式（不影响功能）
- `refactor`: 重构
- `perf`: 性能优化
- `test`: 测试相关
- `chore`: 构建/工具链更改

**示例**:

```
feat(orderbook): 添加订单簿快照功能

实现了订单簿的快照和恢复机制，支持：
- 快照压缩 (gzip)
- 增量快照
- 异步持久化

性能影响: 快照操作 <10ms

Closes #123
```

**提交检查清单**:

- [ ] 代码通过 `cargo build --release`
- [ ] 所有测试通过 `cargo test`
- [ ] Clippy检查通过 `cargo clippy -- -D warnings`
- [ ] 代码格式化 `cargo fmt`
- [ ] 文档已更新
- [ ] 提交消息符合规范

### 3. 推送和Pull Request

```bash
# 推送到你的fork
git push origin feature/my-awesome-feature

# 创建Pull Request
# 在GitHub上点击 "New Pull Request"
```

**PR描述模板**:

```markdown
## 变更描述
简要描述本次PR的主要变更

## 变更类型
- [ ] Bug修复
- [ ] 新功能
- [ ] 重构
- [ ] 性能优化
- [ ] 文档更新

## 测试
- [ ] 添加了单元测试
- [ ] 添加了集成测试
- [ ] 手动测试通过

## 性能影响
- 吞吐量变化: +5%
- 延迟变化: -10μs
- 内存使用: 无变化

## CheckList
- [ ] 代码遵循项目规范
- [ ] 通过所有CI检查
- [ ] 更新了相关文档
- [ ] 添加了changelog条目

## 相关Issue
Closes #XXX
```

### 4. Code Review

**Review要点**:

1. **功能正确性** - 代码是否实现了预期功能
2. **性能影响** - 是否影响关键路径性能
3. **测试覆盖** - 是否有足够的测试
4. **代码质量** - 是否遵循最佳实践
5. **文档完整** - 是否更新了文档

**响应Review**:

```bash
# 根据反馈修改代码
git add .
git commit -m "refactor: 根据review意见优化代码"
git push origin feature/my-awesome-feature

# PR会自动更新
```

---

## 🧪 测试要求

### 单元测试

**覆盖率要求**: 核心模块 >80%

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_matching() {
        let mut book = OrderBook::new();

        let buy_order = Order::new(OrderType::Buy, 100, 10);
        let sell_order = Order::new(OrderType::Sell, 100, 10);

        book.add_order(buy_order);
        let trades = book.match_order(sell_order).unwrap();

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price, 100);
        assert_eq!(trades[0].quantity, 10);
    }

    #[test]
    #[should_panic(expected = "invalid price")]
    fn test_invalid_order() {
        let order = Order::new(OrderType::Buy, 0, 10);  // 价格为0
        validate_order(&order);  // 应该panic
    }
}
```

### 集成测试

```rust
// tests/integration_test.rs
use matching_engine::*;

#[test]
fn test_end_to_end_matching() {
    let engine = MatchingEngine::new(Config::default());

    // 提交一系列订单
    let orders = vec![
        Order::new(OrderType::Buy, 100, 10),
        Order::new(OrderType::Sell, 100, 5),
    ];

    for order in orders {
        engine.submit_order(order);
    }

    // 验证结果
    let trades = engine.get_trades();
    assert_eq!(trades.len(), 1);
}
```

### 性能测试

```bash
# 运行基准测试
cargo bench

# 比较性能
cargo bench -- --save-baseline master
git checkout feature-branch
cargo bench -- --baseline master
```

### 测试命令

```bash
# 快速测试 (debug)
cargo test

# 完整测试 (release)
cargo test --release

# 单个测试
cargo test test_order_matching

# 显示输出
cargo test -- --nocapture

# 代码覆盖率
cargo tarpaulin --out Html
```

---

## 📚 文档规范

### 代码文档

**模块级文档**:

```rust
//! 订单簿模块
//!
//! 提供高性能的订单簿实现，支持：
//! - Tick-based数组索引
//! - O(1)价格查找
//! - FastBitmap硬件加速
//!
//! # Examples
//!
//! ```
//! use matching_engine::orderbook::TickBasedOrderBook;
//!
//! let spec = ContractSpec::new("BTC/USD", 1, 1000, 100000);
//! let mut book = TickBasedOrderBook::new(spec);
//! ```

pub mod tick_based;
pub mod traits;
```

**公共API文档**:

- 必须有文档注释
- 必须有示例代码
- 必须标注性能特性（如果相关）

**私有函数**:

- 复杂逻辑需要注释
- 算法来源需要引用

### Markdown文档

**结构要求**:

```markdown
# 标题

简要描述

## 目录

1. [章节1](#章节1)
2. [章节2](#章节2)

## 章节1

内容...

### 小节1.1

内容...

## 相关链接

- [相关文档](link)
```

### 生成文档

```bash
# 生成API文档
cargo doc --no-deps --open

# 包含私有项
cargo doc --document-private-items

# 生成项目文档 (mdbook)
mdbook build docs/
mdbook serve docs/
```

---

## ⚡ 性能基准

### 基准测试要求

**新功能必须包含基准测试**:

```rust
// benches/my_feature_benchmark.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use matching_engine::*;

fn bench_order_matching(c: &mut Criterion) {
    let mut book = OrderBook::new();

    c.bench_function("order_matching", |b| {
        b.iter(|| {
            let order = Order::new(OrderType::Buy, 100, 10);
            black_box(book.match_order(order))
        })
    });
}

criterion_group!(benches, bench_order_matching);
criterion_main!(benches);
```

### 性能回归检测

**CI会自动运行基准测试**:

- PR必须不引入>5%的性能回归
- 性能优化PR应包含前后对比数据

**本地测试**:

```bash
# 保存基线
cargo bench --bench tick_orderbook_benchmark -- --save-baseline main

# 测试PR
git checkout my-pr
cargo bench --bench tick_orderbook_benchmark -- --baseline main

# 查看报告
open target/criterion/report/index.html
```

---

## 🎯 Review检查清单

### 代码质量

- [ ] 遵循Rust风格指南
- [ ] 通过Clippy检查
- [ ] 代码格式化 (rustfmt)
- [ ] 无unwrap/expect在热路径
- [ ] 错误处理完整

### 测试覆盖

- [ ] 单元测试覆盖核心逻辑
- [ ] 边界条件测试
- [ ] 错误场景测试
- [ ] 性能基准测试 (如适用)

### 文档

- [ ] 公共API有文档注释
- [ ] 复杂算法有解释
- [ ] README更新 (如适用)
- [ ] Changelog更新

### 性能

- [ ] 无明显性能回归
- [ ] 热路径优化
- [ ] 内存使用合理

---

## 🏆 贡献者

感谢所有贡献者！

查看完整贡献者列表: [Contributors](https://github.com/your-org/matching-engine/graphs/contributors)

---

## 📞 获取帮助

- 💬 [GitHub Discussions](https://github.com/your-org/matching-engine/discussions)
- 🐛 [Issue Tracker](https://github.com/your-org/matching-engine/issues)
- 📧 Email: dev@matching-engine.org

---

**感谢您的贡献！** 🎉
