/// Network backend implementations
///
/// Multiple high-performance network backends:
/// - tokio: Async I/O with Tokio runtime (baseline, development)
/// - io_uring: Linux zero-syscall async I/O (currently disabled due to compatibility issues)
/// - dpdk: Userspace network stack with Poll Mode Drivers (ultra-low latency)
pub mod tokio;

// io_uring backend is temporarily disabled due to incompatibility with Send requirements
// TODO: Refactor to support tokio_uring or use alternative io_uring library
// #[cfg(feature = "io-uring")]
// pub mod io_uring;

#[cfg(feature = "dpdk")]
pub mod dpdk;

// Re-export backend implementations
pub use tokio::TokioTransport;

// #[cfg(feature = "io-uring")]
// pub use io_uring::{IoUringConfig, IoUringTransport};

#[cfg(feature = "dpdk")]
pub use dpdk::{DpdkConfig, DpdkTransport};
