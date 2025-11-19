pub mod fast_bitmap;
/// High-performance collections for matching engine
///
/// - RingBuffer: Lock-free, zero-allocation circular buffer
/// - FastBitmap: Hardware-accelerated bitmap with POPCNT/TZCNT
pub mod ringbuffer;

pub use fast_bitmap::FastBitmap;
pub use ringbuffer::RingBuffer;
