/// Legacy implementations (向后兼容层)
///
/// 此模块包含旧的实现，仅为了向后兼容保留。
/// 新代码应该使用 `crate::domain` 和 `crate::application` 中的实现。
///
/// # 迁移指南
///
/// - `legacy::orderbook::OrderBook` → `domain::orderbook::TickBasedOrderBook`
/// - `legacy::engine::MatchingEngine` → `application::services::MatchingService`
/// - `legacy::partitioned_engine::PartitionedEngine` → `application::services::PartitionedService`
/// - `legacy::network` → `infrastructure::network`
#[deprecated(note = "使用 crate::domain::orderbook::TickBasedOrderBook 代替")]
pub mod orderbook;

#[deprecated(note = "已废弃，使用 crate::domain::orderbook::TickBasedOrderBook 代替")]
pub mod orderbook_v2;

#[deprecated(note = "使用 crate::application::services::MatchingService 代替")]
pub mod engine;

#[deprecated(note = "使用 crate::application::services::PartitionedService 代替")]
pub mod partitioned_engine;

#[deprecated(note = "使用 crate::infrastructure::network 代替")]
pub mod network;
