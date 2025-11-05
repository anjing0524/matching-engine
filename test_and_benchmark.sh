#!/bin/bash
# 完整的编译、测试和基准测试脚本
# 用于在有网络连接的环境中验证所有优化

set -e

echo "=================================="
echo "匹配引擎 - 完整测试和基准测试套件"
echo "=================================="
echo ""

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 函数：打印带颜色的状态
print_status() {
    local status=$1
    local message=$2
    case $status in
        "info")
            echo -e "${BLUE}[INFO]${NC} $message"
            ;;
        "success")
            echo -e "${GREEN}[SUCCESS]${NC} $message"
            ;;
        "warning")
            echo -e "${YELLOW}[WARNING]${NC} $message"
            ;;
        "error")
            echo -e "${RED}[ERROR]${NC} $message"
            ;;
    esac
}

# 1. 环境检查
echo "===================="
echo "1. 环境检查"
echo "===================="

print_status "info" "检查 Rust 环境..."
rustc --version || { print_status "error" "Rust 未安装"; exit 1; }
cargo --version || { print_status "error" "Cargo 未安装"; exit 1; }

print_status "info" "检查 Git 状态..."
git status | head -3

print_status "success" "环境检查完成"
echo ""

# 2. 清理和准备
echo "===================="
echo "2. 清理旧构建"
echo "===================="

print_status "info" "清理 target 目录..."
cargo clean
rm -rf target/criterion 2>/dev/null || true

print_status "success" "清理完成"
echo ""

# 3. 依赖下载
echo "===================="
echo "3. 下载依赖"
echo "===================="

print_status "info" "更新依赖索引..."
cargo update

print_status "info" "下载所有依赖..."
cargo fetch

print_status "success" "依赖下载完成"
echo ""

# 4. 编译检查
echo "===================="
echo "4. 编译检查"
echo "===================="

print_status "info" "运行 cargo check..."
cargo check --release

print_status "info" "检查所有基准测试..."
cargo check --release --benches

print_status "success" "编译检查通过"
echo ""

# 5. 完整编译
echo "===================="
echo "5. Release 编译"
echo "===================="

print_status "info" "编译 release 版本 (启用所有优化)..."
time cargo build --release

if [ -f "target/release/matching-engine" ]; then
    BINARY_SIZE=$(du -h target/release/matching-engine | cut -f1)
    print_status "success" "编译成功！二进制大小: $BINARY_SIZE"
else
    print_status "error" "编译失败"
    exit 1
fi
echo ""

# 6. 单元测试
echo "===================="
echo "6. 运行单元测试"
echo "===================="

print_status "info" "运行所有单元测试..."
cargo test --release

print_status "success" "单元测试通过"
echo ""

# 7. 基准测试
echo "===================="
echo "7. 基准测试套件"
echo "===================="

print_status "info" "准备运行基准测试..."
echo ""

# 创建结果目录
mkdir -p benchmark_results
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULT_DIR="benchmark_results/$TIMESTAMP"
mkdir -p "$RESULT_DIR"

# 7.1 OrderBook 基准测试
print_status "info" "运行 OrderBook 基准测试..."
cargo bench --bench orderbook_benchmark 2>&1 | tee "$RESULT_DIR/orderbook_benchmark.log"
print_status "success" "OrderBook 基准测试完成"
echo ""

# 7.2 Comprehensive 基准测试
print_status "info" "运行 Comprehensive 基准测试 (约 10-15 分钟)..."
cargo bench --bench comprehensive_benchmark 2>&1 | tee "$RESULT_DIR/comprehensive_benchmark.log"
print_status "success" "Comprehensive 基准测试完成"
echo ""

# 7.3 Network 基准测试
print_status "info" "运行 Network 基准测试..."
cargo bench --bench network_benchmark 2>&1 | tee "$RESULT_DIR/network_benchmark.log"
print_status "success" "Network 基准测试完成"
echo ""

# 7.4 E2E Network 基准测试
print_status "info" "运行 E2E Network 基准测试..."
cargo bench --bench e2e_network_benchmark 2>&1 | tee "$RESULT_DIR/e2e_network_benchmark.log"
print_status "success" "E2E Network 基准测试完成"
echo ""

# 8. 结果分析
echo "===================="
echo "8. 结果分析"
echo "===================="

print_status "info" "提取关键性能指标..."

# 提取 OrderBook Match 时间
ORDERBOOK_TIME=$(grep -A5 "1-to-1 Match" "$RESULT_DIR/orderbook_benchmark.log" | grep "time:" | awk '{print $3, $4}' || echo "未找到")

# 提取 Comprehensive 关键指标
FULL_MATCH_TIME=$(grep -A5 "Full Match" "$RESULT_DIR/comprehensive_benchmark.log" | grep "time:" | awk '{print $3, $4}' || echo "未找到")
WORST_CASE_TIME=$(grep -A5 "worst_case" "$RESULT_DIR/comprehensive_benchmark.log" | grep "time:" | awk '{print $3, $4}' || echo "未找到")

echo ""
echo "===== 关键性能指标 ====="
echo "OrderBook Match (1000 levels): $ORDERBOOK_TIME"
echo "Full Match: $FULL_MATCH_TIME"
echo "Worst Case (1000 levels crossed): $WORST_CASE_TIME"
echo "========================"
echo ""

# 9. 性能对比
print_status "info" "生成性能对比报告..."

cat > "$RESULT_DIR/PERFORMANCE_COMPARISON.md" << 'EOF'
# 性能对比报告

## 测试环境
- 日期: $(date)
- 主机: $(hostname)
- CPU: $(lscpu | grep "Model name" | cut -d: -f2 | xargs)
- 内存: $(free -h | grep Mem | awk '{print $2}')
- Rust: $(rustc --version)

## 优化前 vs 优化后对比

### OrderBook 性能

| 指标 | 优化前 | 优化后 | 改进 |
|------|--------|--------|------|
| Match (1000 levels) | 108.09 µs | 待测量 | 待计算 |
| Add Order | 229.14 µs | 待测量 | 待计算 |
| Full Match | 254.86 µs | 待测量 | 待计算 |

### Network 性能

| 指标 | 优化前 | 优化后 | 改进 |
|------|--------|--------|------|
| JSON Encode (Order) | 316.84 ns | 待测量 | 待计算 |
| Request Pipeline | 886.45 ns | 待测量 | 待计算 |

### 关键优化项

1. ✅ Arc<str> 替代 String: 预期 +15-25%
2. ✅ Vec 预分配: 预期 +10-15%
3. ✅ 批量时间戳: 预期 +3-5%
4. ✅ BTreeMap 优化: 预期 +5-8%
5. ✅ jemalloc: 预期 +8-15%
6. ✅ bumpalo arena: 预期 +10-20%
7. ⏳ crossbeam (待集成): 预期 +15-25%

**预期总体提升**: 61-105%

EOF

print_status "success" "性能对比报告已生成: $RESULT_DIR/PERFORMANCE_COMPARISON.md"
echo ""

# 10. 生成 HTML 报告
echo "===================="
echo "10. 生成报告"
echo "===================="

print_status "info" "Criterion HTML 报告位置: target/criterion/report/index.html"
print_status "info" "所有日志文件位置: $RESULT_DIR/"

# 创建汇总报告
cat > "$RESULT_DIR/SUMMARY.txt" << EOF
====================================
匹配引擎基准测试汇总
====================================

测试日期: $(date)
Git Branch: $(git branch --show-current)
Git Commit: $(git rev-parse --short HEAD)

测试结果:
- OrderBook 基准: 查看 orderbook_benchmark.log
- Comprehensive 基准: 查看 comprehensive_benchmark.log
- Network 基准: 查看 network_benchmark.log
- E2E 基准: 查看 e2e_network_benchmark.log

关键指标:
- OrderBook Match: $ORDERBOOK_TIME
- Full Match: $FULL_MATCH_TIME
- Worst Case: $WORST_CASE_TIME

HTML 报告:
- 主报告: ../../../target/criterion/report/index.html

性能对比:
- 查看 PERFORMANCE_COMPARISON.md

====================================
EOF

print_status "success" "汇总报告已生成: $RESULT_DIR/SUMMARY.txt"
echo ""

# 11. 完成
echo "====================================="
echo "✅ 所有测试和基准测试完成！"
echo "====================================="
echo ""
echo "结果位置: $RESULT_DIR/"
echo ""
echo "查看详细结果:"
echo "  1. HTML 报告: target/criterion/report/index.html"
echo "  2. 汇总报告: $RESULT_DIR/SUMMARY.txt"
echo "  3. 性能对比: $RESULT_DIR/PERFORMANCE_COMPARISON.md"
echo ""
echo "下一步:"
echo "  - 分析性能数据"
echo "  - 验证是否达到预期提升 (61-105%)"
echo "  - 如需要，继续优化"
echo ""

# 自动打开 HTML 报告（可选）
if command -v xdg-open &> /dev/null; then
    read -p "是否打开 HTML 报告? (y/n) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        xdg-open target/criterion/report/index.html
    fi
fi

exit 0
