# 基准测试运行指南 (Benchmark Instructions)

## ⚠️ 重要提示

由于网络限制，本次优化提交时无法运行基准测试验证。**请在有网络连接的环境下运行以下步骤以验证优化效果。**

---

## 快速开始 (Quick Start)

### 1. 构建项目

```bash
# 清理之前的构建（如果有）
cargo clean

# 构建 release 版本（启用所有优化）
cargo build --release
```

### 2. 运行所有基准测试

```bash
# 运行完整基准测试套件（约需 20-30 分钟）
cargo bench

# 结果将保存在: target/criterion/
```

### 3. 查看结果

```bash
# 打开 HTML 报告（最直观）
open target/criterion/report/index.html   # macOS
xdg-open target/criterion/report/index.html   # Linux
start target/criterion/report/index.html  # Windows

# 或者查看终端输出
```

---

## 详细测试步骤 (Detailed Instructions)

### 运行特定基准测试

```bash
# 1. OrderBook 基准测试
cargo bench --bench orderbook_benchmark
# 测试: 1000 价格层级的订单匹配性能

# 2. 综合基准测试（推荐先运行这个）
cargo bench --bench comprehensive_benchmark
# 测试内容:
#   - 订单添加（无匹配）
#   - 完全匹配
#   - 部分匹配
#   - 内存池复用
#   - 价格层级查询 (10, 100, 1000, 10000 levels)
#   - FIFO 队列深度 (1, 10, 100, 1000)
#   - 交易分配 (1, 10, 100, 1000 trades)
#   - JSON 序列化
#   - 最坏情况（1000 价格层级完全交叉）

# 3. 网络层基准测试
cargo bench --bench network_benchmark
# 测试内容:
#   - JSON 编码/解码
#   - 字节操作 (BytesMut)
#   - 长度分帧
#   - 请求/响应管道
#   - 广播克隆

# 4. 端到端网络基准测试
cargo bench --bench e2e_network_benchmark
# 测试内容:
#   - TCP Echo RTT (100B, 400B)
#   - 订单匹配 E2E
#   - 新建连接 vs 复用连接
```

---

## 预期结果对比 (Expected Results)

### 优化前基准 (Pre-Optimization)

根据 `BENCHMARK_CONSOLIDATED_REPORT.md`:

```
OrderBook - Match (1000 levels):     108.09 µs
OrderBook - Add Order (No Match):    229.14 µs
OrderBook - Full Match:              254.86 µs
OrderBook - Partial Match:           227.58 µs
Network - Request Pipeline:          886.45 ns
Network - JSON Encode (Order):       316.84 ns
Network - JSON Encode (Trade):       807.42 ns
Comprehensive - Worst Case:         1568.0 µs
```

### 优化后预期 (Post-Optimization Expected)

```
OrderBook - Match (1000 levels):     ~70-80 µs    (↓26-35%)
OrderBook - Add Order (No Match):    ~150-170 µs  (↓26-35%)
OrderBook - Full Match:              ~170-190 µs  (↓26-35%)
OrderBook - Partial Match:           ~150-170 µs  (↓26-35%)
Network - Request Pipeline:          ~500-600 ns  (↓32-44%)
Network - JSON Encode (Order):       ~250-300 ns  (↓5-20%)
Network - JSON Encode (Trade):       ~650-750 ns  (↓7-19%)
Comprehensive - Worst Case:         ~1000-1200 µs (↓24-36%)
```

---

## 关键性能指标 (Key Performance Indicators)

运行基准测试后，重点关注以下指标的变化：

### 1. 订单匹配延迟 (最关键)

```
指标名称: "OrderBook - Match in 1000 levels"
优化前: ~108 µs
目标: < 80 µs (提升 26%+)
```

**如何验证**:
- 查看 `target/criterion/OrderBook*/report/index.html`
- 对比 "Mean" 值
- 检查 "Change" 列（应显示负数百分比）

### 2. Vec 预分配效果

```
指标名称: "OrderBook - Full Match"
优化前: ~255 µs
目标: < 190 µs (提升 25%+)
```

**验证方法**:
- 多次匹配场景应该显著改善
- 查看 "Throughput" 值（应该提升）

### 3. Arc<str> 克隆效果

所有涉及 symbol 的操作都应该更快：
- `Add Order (No Match)`: 应该提升 15-25%
- `Partial Match`: 应该提升 15-25%
- `Worst Case`: 应该提升 20-30%

### 4. 时间戳批量生成效果

查看 CPU 使用率和系统调用次数（需要 profiling 工具）:
```bash
# 使用 perf 分析（Linux）
cargo build --release
perf record --call-graph dwarf target/release/matching-engine
perf report

# 查找 SystemTime::now 的调用次数（应该减少）
```

---

## 对比分析 (Comparative Analysis)

### 使用 Criterion 的对比功能

Criterion 会自动对比上一次运行的结果：

```bash
# 第一次运行（建立基线）
cargo bench

# 修改代码...

# 第二次运行（对比）
cargo bench
# Criterion 会显示: "Change: -25.3% [±2.1%]"
```

### 生成对比报告

```bash
# 保存基线
cargo bench -- --save-baseline before-opt

# 应用优化...

# 对比新结果
cargo bench -- --baseline before-opt
```

---

## 故障排查 (Troubleshooting)

### 编译错误

**问题**: `error: could not compile matching-engine`

**解决**:
1. 确保 Rust 版本 >= 1.70
   ```bash
   rustc --version
   rustup update stable
   ```

2. 清理并重建
   ```bash
   cargo clean
   cargo build --release
   ```

### 基准测试不稳定

**问题**: 结果波动很大（标准差 > 10%）

**解决**:
1. 关闭后台程序
2. 禁用 CPU 频率调节
   ```bash
   # Linux
   sudo cpupower frequency-set --governor performance

   # macOS
   sudo systemsetup -setcomputersleep Never
   ```

3. 增加样本数量
   ```bash
   cargo bench -- --sample-size 200
   ```

### 性能提升不明显

**可能原因**:
1. **编译器版本**: 旧版本 rustc 可能优化不足
2. **CPU 型号**: 某些优化在特定 CPU 上效果更好
3. **测试数据**: 小数据集可能看不出差异

**验证**:
```bash
# 运行最坏情况测试（数据量大）
cargo bench --bench comprehensive_benchmark -- "worst_case"

# 检查编译器优化级别
cat Cargo.toml | grep -A5 "\[profile.release\]"
```

---

## 高级分析 (Advanced Analysis)

### 使用 flamegraph 分析

```bash
# 安装 flamegraph
cargo install flamegraph

# 生成火焰图
cargo flamegraph --bench comprehensive_benchmark

# 查看结果
open flamegraph.svg
```

**关键点**:
- 查找 `String::clone` 的占比（应该显著减少）
- 查找 `Vec::grow` 的占比（应该减少或消失）
- 查找 `SystemTime::now` 的占比（应该减少）

### 使用 cachegrind 分析缓存性能

```bash
# Linux only
cargo build --release
valgrind --tool=cachegrind ./target/release/matching-engine

# 查看报告
cg_annotate cachegrind.out.<pid>
```

---

## 性能回归检测 (Performance Regression Detection)

### 自动化基准测试

```bash
#!/bin/bash
# run_bench.sh

# 运行基准测试
cargo bench --bench comprehensive_benchmark -- --save-baseline main

# 检查是否有显著回退
if cargo bench -- --baseline main | grep "Performance has regressed"; then
    echo "❌ 性能回退检测到！"
    exit 1
else
    echo "✅ 性能正常"
fi
```

### CI/CD 集成

```yaml
# .github/workflows/bench.yml
name: Benchmark
on: [push, pull_request]

jobs:
  benchmark:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      - name: Run benchmarks
        run: cargo bench --no-fail-fast
```

---

## 报告生成 (Report Generation)

### 创建性能对比报告

```bash
# 运行优化前后的基准测试
cargo bench --bench comprehensive_benchmark -- --save-baseline before
# ... 应用优化 ...
cargo bench --bench comprehensive_benchmark -- --baseline before

# 结果保存在 target/criterion/*/report/
```

### 生成 Markdown 报告

```bash
# 安装 criterion-table
cargo install criterion-table

# 生成表格
criterion-table -c target/criterion > BENCHMARK_RESULTS.md
```

---

## 下一步行动 (Next Steps)

完成基准测试后:

1. ✅ **验证优化效果**
   - 检查是否达到预期性能提升（38-63%）
   - 如果低于预期，分析瓶颈

2. 📊 **更新基准报告**
   - 复制 Criterion 结果到 `BENCHMARK_CONSOLIDATED_REPORT.md`
   - 添加对比数据

3. 🔍 **识别新瓶颈**
   - 使用 flamegraph 分析
   - 查找下一个优化目标

4. 🚀 **部署到生产**
   - 在测试环境验证稳定性
   - 逐步灰度发布
   - 监控生产性能指标

5. 📝 **文档更新**
   - 更新 API 文档（String -> Arc<str>）
   - 添加性能最佳实践指南
   - 更新示例代码

---

## 联系和支持 (Support)

如果遇到问题：

1. 检查 `OPTIMIZATION_REPORT.md` 了解详细优化内容
2. 查看 Git 提交历史了解具体代码变更
3. 运行 `git diff HEAD~1` 查看本次优化的所有更改

---

**最后更新**: 2025-11-05
**优化版本**: v0.2.0
**状态**: ⏳ 待基准测试验证
