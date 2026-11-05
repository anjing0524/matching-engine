//! Application layer: services that orchestrate the domain.
//!
//! - [`services`]: matching runtime and persistent single-book engine
pub mod services;

pub use services::PersistentMatchingService;
