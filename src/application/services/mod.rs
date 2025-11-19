pub mod high_performance_service;
/// Application Services
///
/// Services coordinate domain logic to implement application workflows.
/// These services orchestrate use cases and manage the lifecycle of domain objects.
///
/// ## Available Services
/// - `MatchingService`: Single-threaded matching engine for sequential order processing
/// - `PartitionedService`: Multi-threaded partitioned engine for high-throughput scenarios
/// - `PersistentMatchingService`: 持久化匹配服务（同步WAL，高可靠性）
/// - `HighPerformanceService`: 高性能匹配服务（异步WAL，百万级吞吐量）
pub mod matching_service;
pub mod partitioned_service;
pub mod persistent_service;

// Re-export main types
pub use high_performance_service::{HighPerformanceConfig, HighPerformanceService};
pub use matching_service::{EngineCommand, EngineOutput, MatchingService};
pub use partitioned_service::{
    OrderRequest, OrderResponse, PartitionConfig, PartitionStats, PartitionedService,
};
pub use persistent_service::PersistentMatchingService;
