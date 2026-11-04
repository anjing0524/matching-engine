//! High-performance network layer.
//!
//! The transport is a concrete blocking TCP implementation; protocol framing and
//! encoding live in [`crate::shared::wire`]. There is no async runtime on the
//! data path and no per-message dynamic dispatch.
pub mod tcp;

pub use tcp::{TcpConnection, TcpTransport};
