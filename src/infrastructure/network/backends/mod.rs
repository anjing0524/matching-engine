/// Network backend implementations
///
/// Multiple high-performance network backends:
/// - tokio: Async I/O with Tokio runtime (baseline, development)
/// - io_uring: Linux zero-syscall async I/O (production)
/// - dpdk: Userspace network stack with Poll Mode Drivers (ultra-low latency)

pub mod tokio;

#[cfg(feature = "io-uring")]
pub mod io_uring;

#[cfg(feature = "dpdk")]
pub mod dpdk;

// Re-export backend implementations
pub use tokio::TokioTransport;

#[cfg(feature = "io-uring")]
pub use io_uring::{IoUringTransport, IoUringConfig};

#[cfg(feature = "dpdk")]
pub use dpdk::{DpdkTransport, DpdkConfig};
