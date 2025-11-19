# 文档索引

本项目文档组织结构如下，方便快速查找所需信息。

## 📚 核心文档

### 快速开始
- **[README.md](README.md)** - 项目概览、快速开始、性能指标
  - 适合：首次了解项目的开发者
  - 包含：性能数据、架构概览、安装指南

### 架构设计
- **[ARCHITECTURE.md](ARCHITECTURE.md)** - Hexagonal/Onion架构详细设计
  - 适合：理解系统架构和设计决策
  - 包含：五层架构、依赖倒置、模块职责

### 部署运维
- **[DEPLOYMENT.md](DEPLOYMENT.md)** - 生产环境部署指南
  - 适合：运维工程师、DevOps
  - 包含：系统要求、配置选项、监控方案

### 开发贡献
- **[CONTRIBUTING.md](CONTRIBUTING.md)** - 贡献指南和开发规范
  - 适合：项目贡献者、新加入的开发者
  - 包含：代码规范、提交流程、测试要求

## 📊 性能与优化

- **[PERFORMANCE_FINAL_REPORT.md](PERFORMANCE_FINAL_REPORT.md)** - 性能基准测试报告
  - 9.34M ops/s 单核吞吐量
  - 架构演进对比 (V1→V2→V3)
  - 详细性能数据和分析

- **[docs/OPTIMIZATION_GUIDE.md](docs/OPTIMIZATION_GUIDE.md)** - 性能优化指南
  - FastBitmap硬件指令优化
  - 零分配RingBuffer设计
  - 符号池化和缓存策略

## 📈 项目评估

- **[PROJECT_ASSESSMENT_REPORT.md](PROJECT_ASSESSMENT_REPORT.md)** - 项目综合评估
  - 业务功能完整性: 95%
  - 性能实现: ⭐⭐⭐⭐⭐
  - 文档质量: ⭐⭐⭐⭐⭐
  - 生产就绪度: 商业级

## 🗄️ 历史文档

旧版本的阶段性文档已归档至 `docs/archive/` 目录：
- 架构重构阶段文档 (Phase 2/3/4)
- 历史性能测试报告
- 代码质量评审报告
- 网络中间件设计文档
- HFT评估报告

这些文档保留用于参考历史决策和演进过程。

## 📁 目录结构

```
matching-engine/
├── README.md                           # 项目主文档
├── ARCHITECTURE.md                     # 架构设计
├── DEPLOYMENT.md                       # 部署指南
├── CONTRIBUTING.md                     # 贡献指南
├── PERFORMANCE_FINAL_REPORT.md         # 性能报告
├── PROJECT_ASSESSMENT_REPORT.md        # 项目评估
├── DOCS_INDEX.md                       # 本文档
├── docs/
│   ├── OPTIMIZATION_GUIDE.md           # 优化指南
│   └── archive/                        # 历史文档归档
└── src/                                # 源代码
```

## 🔍 快速查找

| 我想了解... | 请阅读... |
|------------|----------|
| 项目是什么，如何安装 | [README.md](README.md) |
| 系统架构和设计模式 | [ARCHITECTURE.md](ARCHITECTURE.md) |
| 如何部署到生产环境 | [DEPLOYMENT.md](DEPLOYMENT.md) |
| 如何参与开发贡献 | [CONTRIBUTING.md](CONTRIBUTING.md) |
| 性能表现如何 | [PERFORMANCE_FINAL_REPORT.md](PERFORMANCE_FINAL_REPORT.md) |
| 如何进行性能优化 | [docs/OPTIMIZATION_GUIDE.md](docs/OPTIMIZATION_GUIDE.md) |
| 项目功能完整度 | [PROJECT_ASSESSMENT_REPORT.md](PROJECT_ASSESSMENT_REPORT.md) |
| 订单授权验证 | `src/domain/orderbook/tick_based.rs:862-906` |
| 订单状态查询 | `src/domain/orderbook/tick_based.rs:908-1005` |
| WAL持久化测试 | `src/infrastructure/persistence/wal.rs:384-683` |
| 崩溃恢复测试 | `src/infrastructure/persistence/recovery.rs:329-675` |

## ✅ 测试覆盖

当前项目包含 **112个测试**，全部通过 ✅

- 单元测试: 92个
- 集成测试: 10个
- WAL持久化测试: 5个
- 崩溃恢复测试: 5个

运行测试：
```bash
# 运行所有测试
cargo test

# 运行性能基准测试
cargo bench
```

---

**最后更新**: 2025-11-19
**项目状态**: ✅ 商业级生产就绪
