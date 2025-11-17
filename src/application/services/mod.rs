/// Application Services
///
/// Services coordinate domain logic to implement application workflows.
/// These services orchestrate use cases and manage the lifecycle of domain objects.
///
/// ## Available Services
/// - `MatchingService`: Single-threaded matching engine for sequential order processing
/// - `PartitionedService`: Multi-threaded partitioned engine for high-throughput scenarios

pub mod matching_service;
pub mod partitioned_service;

// Re-export main types
pub use matching_service::{MatchingService, EngineCommand, EngineOutput};
pub use partitioned_service::{PartitionedService, PartitionConfig, OrderRequest, OrderResponse, PartitionStats};
