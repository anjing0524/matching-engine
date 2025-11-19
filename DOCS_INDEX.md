# 文档索引 📚

本文档提供了Matching Engine项目所有文档的完整索引和导航。

> **最后更新**: 2025-11-19
> **架构版本**: v4.0 (DDD + Hexagonal Architecture)
> **测试覆盖**: 98个测试全通过 ✅

---

## 📋 目录

- [快速导航](#快速导航)
- [核心文档](#核心文档)
- [架构文档](#架构文档)
- [性能报告](#性能报告)
- [代码质量报告](#代码质量报告)
- [专题文档](#专题文档)
- [历史文档](#历史文档)

---

## 🚀 快速导航

### 我想要...

**开始使用项目**
→ [README.md](README.md) - 项目概览和快速开始

**部署到生产环境**
→ [DEPLOYMENT.md](DEPLOYMENT.md) - 完整部署指南 (789行)

**贡献代码**
→ [CONTRIBUTING.md](CONTRIBUTING.md) - 贡献者指南 (722行)

**理解系统架构**
→ [ARCHITECTURE.md](ARCHITECTURE.md) - 架构设计文档 (v4.0)

**了解性能优化**
→ [PERFORMANCE_FINAL_REPORT.md](PERFORMANCE_FINAL_REPORT.md) - 性能最终报告

**从旧版本迁移**
→ [MIGRATION_GUIDE.md](MIGRATION_GUIDE.md) - 架构迁移指南

---

## 📘 核心文档

### [README.md](README.md)
**项目主文档** - 必读

- ✅ 项目概述和核心特性
- ✅ 性能指标 (9.34M orders/sec)
- ✅ 快速开始指南
- ✅ 项目结构说明
- ✅ 测试运行方法
- ✅ 文档索引

### [ARCHITECTURE.md](ARCHITECTURE.md)
**架构设计文档** - v4.0

- ✅ DDD分层架构详解
- ✅ Hexagonal/Onion Architecture设计
- ✅ 五层架构 + Legacy层
- ✅ 依赖注入机制
- ✅ 核心数据结构设计
- ✅ 架构演进历程

**相关**: [ARCHITECTURE_REFACTORING_SUMMARY.md](ARCHITECTURE_REFACTORING_SUMMARY.md) - 重构总结

### [DEPLOYMENT.md](DEPLOYMENT.md)
**生产部署指南** - 789行完整指南

- ✅ 硬件需求（最低/推荐/HFT配置）
- ✅ 4种构建方法
- ✅ 配置管理
- ✅ 3种运行模式（开发/systemd/Kubernetes）
- ✅ 监控和告警（Prometheus/Grafana）
- ✅ 故障排查
- ✅ 系统调优和安全加固

### [CONTRIBUTING.md](CONTRIBUTING.md)
**贡献者指南** - 722行开发规范

- ✅ 代码规范（Rust风格指南）
- ✅ Git工作流（Conventional Commits）
- ✅ 测试要求（>80%覆盖率）
- ✅ Code Review检查清单
- ✅ 开发环境设置
- ✅ Bug报告和功能请求模板

### [MIGRATION_GUIDE.md](MIGRATION_GUIDE.md)
**架构迁移指南** - 新旧API对照

- ✅ Legacy API → 新架构映射
- ✅ 代码迁移示例
- ✅ 废弃功能列表
- ✅ 兼容性说明
- ✅ 迁移检查清单

---

## 🏗️ 架构文档

### 架构演进系列

| 文档 | 说明 | 状态 |
|------|------|------|
| [ARCHITECTURE.md](ARCHITECTURE.md) | 当前架构 (v4.0) | ✅ 最新 |
| [ARCHITECTURE_PHASE2_SUMMARY.md](ARCHITECTURE_PHASE2_SUMMARY.md) | 阶段2: RingBuffer优化 | 📚 历史参考 |
| [ARCHITECTURE_PHASE3_SUMMARY.md](ARCHITECTURE_PHASE3_SUMMARY.md) | 阶段3: FastBitmap优化 | 📚 历史参考 |
| [ARCHITECTURE_PHASE4_SUMMARY.md](ARCHITECTURE_PHASE4_SUMMARY.md) | 阶段4: 五层架构重构 | 📚 历史参考 |
| [ARCHITECTURE_REFACTORING_PLAN.md](ARCHITECTURE_REFACTORING_PLAN.md) | 重构计划 | 📚 已完成 |
| [ARCHITECTURE_REFACTORING_SUMMARY.md](ARCHITECTURE_REFACTORING_SUMMARY.md) | 重构总结 | ✅ 参考价值 |

### 专题架构文档

- **[NETWORK_MIDDLEWARE_DESIGN.md](NETWORK_MIDDLEWARE_DESIGN.md)** - 网络中间件设计
- **[NETWORK_MIDDLEWARE_SUMMARY.md](NETWORK_MIDDLEWARE_SUMMARY.md)** - 网络中间件总结
- **[NETWORK_INTEGRATION_COMPLETE.md](NETWORK_INTEGRATION_COMPLETE.md)** - 网络集成完成报告

---

## 📊 性能报告

### 性能分析系列

| 文档 | 说明 | 关键数据 |
|------|------|----------|
| **[PERFORMANCE_FINAL_REPORT.md](PERFORMANCE_FINAL_REPORT.md)** | **最终性能报告** | **9.34M ops/s** |
| [PERFORMANCE_REPORT.md](PERFORMANCE_REPORT.md) | 早期性能报告 | 2.71M → 3.59M |
| [PERFORMANCE_TEST_REPORT.md](PERFORMANCE_TEST_REPORT.md) | 性能测试报告 | 基准测试详情 |
| [BENCHMARK_SUMMARY.md](BENCHMARK_SUMMARY.md) | 基准测试总结 | 对比数据 |

### HFT评估系列

- **[docs/HFT_EVALUATION_SUMMARY.md](docs/HFT_EVALUATION_SUMMARY.md)** - HFT高频交易评估总结
- **[docs/HFT_PERFORMANCE_EVALUATION.md](docs/HFT_PERFORMANCE_EVALUATION.md)** - HFT性能评估
- **[docs/OPTIMIZATION_GUIDE.md](docs/OPTIMIZATION_GUIDE.md)** - 优化指南

---

## ✅ 代码质量报告

### 工程质量系列

| 文档 | 说明 | 状态 |
|------|------|------|
| **[ENGINEERING_QUALITY_REPORT.md](ENGINEERING_QUALITY_REPORT.md)** | **工程质量报告** | ✅ 最新 |
| [CODE_QUALITY_REPORT.md](CODE_QUALITY_REPORT.md) | 代码质量报告 | ✅ 参考 |
| [CODE_REVIEW_REPORT.md](CODE_REVIEW_REPORT.md) | Code Review报告 | ✅ 参考 |

**工程质量报告包含**:
- ✅ 阶段3: 文档完善（DEPLOYMENT.md, CONTRIBUTING.md）
- ✅ 阶段4: 质量提升（Clippy修复，混沌测试，健康检查）
- ✅ 架构统一（Legacy代码隔离）
- ✅ 测试覆盖（98个测试）

---

## 🔧 专题文档

### 崩溃恢复

- **[CRASH_RECOVERY_ANALYSIS.md](CRASH_RECOVERY_ANALYSIS.md)** - 崩溃恢复分析
  - WAL (Write-Ahead Log) 设计
  - Snapshot快照机制
  - 恢复流程分析

### 网络架构

- **[NETWORK_MIDDLEWARE_DESIGN.md](NETWORK_MIDDLEWARE_DESIGN.md)** - 网络中间件设计
  - Tokio / io_uring / DPDK 后端
  - 零拷贝设计
  - 性能对比

---

## 📚 历史文档 (Archive)

以下文档记录了项目的演进历程，供参考：

### 架构演进
- [ARCHITECTURE_PHASE2_SUMMARY.md](ARCHITECTURE_PHASE2_SUMMARY.md) - RingBuffer优化阶段
- [ARCHITECTURE_PHASE3_SUMMARY.md](ARCHITECTURE_PHASE3_SUMMARY.md) - FastBitmap优化阶段
- [ARCHITECTURE_PHASE4_SUMMARY.md](ARCHITECTURE_PHASE4_SUMMARY.md) - 五层架构重构阶段
- [ARCHITECTURE_REFACTORING_PLAN.md](ARCHITECTURE_REFACTORING_PLAN.md) - 重构计划

### 性能演进
- [PERFORMANCE_REPORT.md](PERFORMANCE_REPORT.md) - 早期性能报告
- [PERFORMANCE_TEST_REPORT.md](PERFORMANCE_TEST_REPORT.md) - 性能测试报告

> **说明**: 历史文档保留用于追溯项目演进过程，但不保证与最新代码完全一致。请以最新的核心文档为准。

---

## 📖 文档更新日志

### 2025-11-19
- ✅ 创建 DOCS_INDEX.md（本文档）
- ✅ 更新 README.md（测试数量、架构、文档引用）
- ✅ 完成 CONTRIBUTING.md（722行）
- ✅ 完成 DEPLOYMENT.md（789行）
- ✅ 创建 MIGRATION_GUIDE.md（架构迁移指南）

### 2025-11-13
- ✅ 完成 ARCHITECTURE.md v4.0（五层架构）
- ✅ 完成架构统一（Legacy代码隔离）
- ✅ 添加混沌工程测试
- ✅ 完成健康检查指标

### 早期
- ✅ PERFORMANCE_FINAL_REPORT.md（9.34M ops/s）
- ✅ 各阶段架构文档
- ✅ 性能和质量报告

---

## 🎯 推荐阅读路径

### 新手入门
1. [README.md](README.md) - 项目概览
2. [ARCHITECTURE.md](ARCHITECTURE.md) - 架构理解
3. [CONTRIBUTING.md](CONTRIBUTING.md) - 开发规范
4. 运行测试和基准测试

### 生产部署
1. [README.md](README.md) - 系统要求
2. [DEPLOYMENT.md](DEPLOYMENT.md) - 部署配置
3. [PERFORMANCE_FINAL_REPORT.md](PERFORMANCE_FINAL_REPORT.md) - 性能预期
4. [docs/OPTIMIZATION_GUIDE.md](docs/OPTIMIZATION_GUIDE.md) - 调优指南

### 架构研究
1. [ARCHITECTURE.md](ARCHITECTURE.md) - 当前架构
2. [ARCHITECTURE_REFACTORING_SUMMARY.md](ARCHITECTURE_REFACTORING_SUMMARY.md) - 演进历程
3. [ARCHITECTURE_PHASE*.md](#架构演进系列) - 各阶段细节
4. [MIGRATION_GUIDE.md](MIGRATION_GUIDE.md) - 迁移指南

### 性能优化
1. [PERFORMANCE_FINAL_REPORT.md](PERFORMANCE_FINAL_REPORT.md) - 最终报告
2. [docs/HFT_PERFORMANCE_EVALUATION.md](docs/HFT_PERFORMANCE_EVALUATION.md) - HFT评估
3. [docs/OPTIMIZATION_GUIDE.md](docs/OPTIMIZATION_GUIDE.md) - 优化技巧
4. [BENCHMARK_SUMMARY.md](BENCHMARK_SUMMARY.md) - 基准对比

---

## 📞 获取帮助

### 文档问题
- 如果发现文档错误或过时信息，请提交Issue
- 欢迎通过PR改进文档

### 技术支持
- 通过GitHub Issues提问
- 查看[CONTRIBUTING.md](CONTRIBUTING.md)了解Bug报告规范

---

**最后更新**: 2025-11-19
**维护者**: Matching Engine Team
**架构版本**: v4.0 (DDD + Hexagonal + Legacy兼容)
