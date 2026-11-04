//! Application services.
//!
//! - [`matching_runtime::MatchingRuntime`]: single-threaded matching actor with
//!   group commit, reply ordering, and lifecycle control.
//! - [`persistent_service::PersistentMatchingService`]: single-book matching
//!   with synchronous WAL persistence and crash recovery.
pub mod matching_runtime;
pub mod persistent_service;

pub use matching_runtime::{
    CancelTicket, MatchingHandle, MatchingRuntime, OrderOutcome, OrderTicket, RuntimeConfig,
    RuntimeError,
};
pub use persistent_service::{PersistentMatchingService, ServiceError, ServiceResult};
