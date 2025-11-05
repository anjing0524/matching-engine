# matching-engine 性能优化笔记

## 阶段 1: 文档清理与初步分析

- **目标**: 移除过时文档并分析项目现状。
- **操作**:
    - 删除了 22 个过时的 Markdown 文件（涉及 `io_uring`、旧基准测试等）。
    - 更新了 `ARCHITECTURE.md` 和 `README.md`，使其与当前源代码和 `BENCHMARK_CONSOLIDATED_REPORT.md` 保持一致。

## 阶段 2: 性能瓶颈分析

### 已识别的瓶颈:

1.  **高昂的连接成本**: `e2e_network_benchmark` 显示新连接的成本约为 279µs，这是主要瓶颈。
2.  **JSON 序列化**: 所有网络通信均使用 `serde_json`，相比二进制格式，CPU 开销更大。
3.  **低效的 `load_generator`**: 负载测试工具中包含 100µs 的休眠，人为地限制了吞吐量。
4.  **热点路径上的内存分配**: 在 `orderbook.rs` 的撮合循环中发现了不必要的 `String` 克隆 (`symbol.clone()`)。
5.  **基准测试 Bug**: `e2e_network_benchmark` 在持久连接测试中因 `BrokenPipe` 错误而失败。

## 阶段 3: 优化方案执行

### 步骤 1: 修复与测量改进 (已完成)

- **`BrokenPipe` Bug**: 已修复 `e2e_network_benchmark.rs` 中的测试服务器，使其能正确处理持久连接。
- **`load_generator`**: 已移除 100µs 的休眠，以进行真实的压力测试。
- **`orderbook_benchmark`**: 已确认该基准测试的实现是正确的，解决了之前对其准确性的担忧。

### 步骤 2: 切换到二进制序列化 (进行中)

- **目标**: 使用性能更高的 `bincode` 替换 `serde_json`。
- **进度**:
    - 已将 `bincode` 依赖项添加到 `Cargo.toml`。
    - 已为 `protocol.rs` 中的所有数据结构添加了 `bincode::Encode` 和 `bincode::Decode` 派生。
- **下一步**: 修改 `network.rs` 以使用 `bincode` 替换 JSON。

### 步骤 3: 未来优化 (计划中)

- **网络**: 在负载生成器和客户端中实现连接池。
- **核心逻辑**: 使用 `Arc<String>` 或其他高效方法优化 `orderbook.rs` 中的字符串克隆。
