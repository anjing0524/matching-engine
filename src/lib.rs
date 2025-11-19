// 全局内存分配器：使用 jemalloc 提升性能
// jemalloc 在高并发场景下比系统分配器快 8-15%
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

// ===================================================================
// 新分层架构 (Layered Architecture)
// ===================================================================
//
// 采用六边形/洋葱架构，清晰分层，依赖单向流动：
// interfaces → application → domain ← infrastructure
//
// 详见: ARCHITECTURE_REFACTORING_PLAN.md

/// Domain Layer - 核心业务逻辑（无外部依赖）
pub mod domain;

/// Application Layer - 用例编排和服务
pub mod application;

/// Infrastructure Layer - 技术实现（网络、持久化、遥测）
pub mod infrastructure;

/// Shared - 跨层共享的工具和类型
pub mod shared;

/// Interfaces - 外部接口（CLI、API）
pub mod interfaces;

/// Legacy - 历史实现（向后兼容）
pub mod legacy;

// ===================================================================
// 向后兼容层 (Backward Compatibility Layer)
// ===================================================================
//
// 为了不破坏现有的测试、示例和基准测试，提供旧模块路径的别名
// 实际实现已移至 src/legacy/

#[deprecated(note = "使用 crate::domain::orderbook::TickBasedOrderBook 代替")]
pub use legacy::orderbook;

#[deprecated(note = "已废弃，使用 crate::domain::orderbook::TickBasedOrderBook 代替")]
pub use legacy::orderbook_v2;

#[deprecated(note = "使用 crate::application::services 代替")]
pub use legacy::engine;

#[deprecated(note = "使用 crate::application::services::PartitionedService 代替")]
pub use legacy::partitioned_engine;

#[deprecated(note = "使用 crate::infrastructure::network 代替")]
pub use legacy::network;

#[deprecated(note = "使用 crate::shared::timestamp 代替")]
pub use shared::timestamp;

#[deprecated(note = "使用 crate::shared::collections::ringbuffer 代替")]
pub use shared::collections::ringbuffer;

#[deprecated(note = "使用 crate::shared::collections::fast_bitmap 代替")]
pub use shared::collections::fast_bitmap;

// #[deprecated(note = "使用 crate::infrastructure::network 代替")]
// pub mod network_middleware;  // 已删除 - 使用 crate::infrastructure::network 代替

// ===================================================================
// 便捷的重新导出 (Convenience Re-exports)
// ===================================================================

// 域层核心类型和 trait
pub use domain::orderbook::{OrderBook, TickBasedOrderBook, ContractSpec};

// 应用层服务
// Note: MatchingService is now generic over OrderBook implementation
pub use application::services::{MatchingService, PartitionedService};

// 基础设施
pub use infrastructure::network::{NetworkTransport, Connection, ZeroCopyBuffer};

// 共享协议
pub use shared::protocol::{
    ClientMessage, ServerMessage,
    NewOrderRequest, CancelOrderRequest,
    OrderType, OrderConfirmation, TradeNotification,
};
