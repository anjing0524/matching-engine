//! High-performance collections for matching engine
//!
//! - FastBitmap: Hardware-accelerated bitmap with POPCNT/TZCNT

pub mod fast_bitmap;

pub use fast_bitmap::FastBitmap;
