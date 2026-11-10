//! High-performance collections for matching engine
//!
//! - FastBitmap: Hardware-accelerated bitmap with POPCNT/TZCNT
//! - U64 identity hashing for dense numeric IDs

pub mod fast_bitmap;
pub mod identity;

pub use fast_bitmap::FastBitmap;
pub use identity::{
    U64IdentityHasher, U64IdentityMap, U64IdentitySet, U64MixHasher, U64MixMap, U64MixSet,
};
