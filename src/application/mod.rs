/// Application Layer - Use Cases and Services
///
/// This layer orchestrates domain logic to implement application-specific
/// business use cases. It depends on the domain layer but is independent
/// of infrastructure details (thanks to dependency injection).
///
/// ## Modules
/// - `use_cases`: High-level use case implementations
/// - `services`: Application services (MatchingService, PartitionedService)
/// - `dto`: Data Transfer Objects for cross-layer communication

pub mod use_cases;
pub mod services;
pub mod dto;

// Re-export key services
pub use services::{MatchingService, PartitionedService};
