//! File-backed single-producer/single-consumer ring for gateway↔engine IPC.
//!
//! The ring is designed for two processes that map the same file. One side is
//! the only producer and the other the only consumer; that invariant is the
//! safety condition for the atomic head/tail protocol. It is deliberately
//! small and allocation-free on push; `try_pop_into` reuses the caller buffer.
use memmap2::{MmapMut, MmapOptions};
use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: u64 = 0x5350_5343_5249_4e47; // "SPSCRING"
const VERSION: u64 = 1;
const HEAD_OFFSET: usize = 64;
const TAIL_OFFSET: usize = 128;
const HEADER_LEN: usize = 192;
const SLOT_HEADER: usize = 4;

fn slot_stride(slot_size: usize) -> Option<usize> {
    let raw = slot_size.checked_add(SLOT_HEADER)?;
    Some(raw.checked_add(7)? & !7)
}

/// Error returned when a push cannot be completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushError {
    /// No free slot.
    Full,
    /// Payload does not fit in one slot.
    TooLarge,
}

/// A mapped SPSC ring.
pub struct SpscRing {
    mmap: MmapMut,
    base: *mut u8,
    capacity: u64,
    slot_size: usize,
    stride: usize,
}

// The SPSC protocol makes one producer and one consumer safe. The type is
// deliberately not `Sync`: sharing one mapping between threads through a safe
// API would allow two producers to write the same slot. Each side must own its
// own mapping (`create` / `open`).
unsafe impl Send for SpscRing {}

/// Touches every mapped page once so the first hot-path access does not take
/// a minor page fault. The mapping is zero-filled by the kernel; this is a
/// read-only prefault and does not change ring contents.
fn prefault(mmap: &MmapMut) {
    const PAGE: usize = 4096;
    let len = mmap.len();
    let base = mmap.as_ptr();
    let mut offset = 0;
    while offset < len {
        // SAFETY: `offset` is within the mapping and the read is volatile so
        // the compiler cannot remove the page touch.
        unsafe {
            std::ptr::read_volatile(base.add(offset));
        }
        offset += PAGE;
    }
}

impl SpscRing {
    /// Creates a new ring file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be created or mapped.
    pub fn create(path: &Path, capacity: u64, slot_size: usize) -> io::Result<Self> {
        assert!(capacity > 0, "capacity must be non-zero");
        assert!(slot_size > 0, "slot size must be non-zero");
        let stride = slot_stride(slot_size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "slot size overflow"))?;
        let total = u64::try_from(HEADER_LEN)
            .ok()
            .and_then(|header| header.checked_add(capacity.checked_mul(stride as u64)?))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ring size overflow"))?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        file.set_len(total)?;
        let mut mmap = unsafe { MmapOptions::new().map_mut(&file)? };
        prefault(&mmap);
        // Initialise the header before exposing the mapping.
        unsafe {
            let base = mmap.as_mut_ptr();
            ptr::write(base as *mut u64, MAGIC);
            ptr::write(base.add(8) as *mut u64, VERSION);
            ptr::write(base.add(16) as *mut u64, capacity);
            ptr::write(base.add(24) as *mut u64, slot_size as u64);
            ptr::write(base.add(HEAD_OFFSET) as *mut u64, 0);
            ptr::write(base.add(TAIL_OFFSET) as *mut u64, 0);
            let head = &*(base.add(HEAD_OFFSET) as *const AtomicU64);
            let tail = &*(base.add(TAIL_OFFSET) as *const AtomicU64);
            head.store(0, Ordering::Release);
            tail.store(0, Ordering::Release);
        }
        mmap.flush()?;
        Ok(Self {
            base: mmap.as_mut_ptr(),
            mmap,
            capacity,
            slot_size,
            stride,
        })
    }

    /// Opens an existing ring file.
    ///
    /// # Errors
    /// Returns an error if the file is malformed or cannot be mapped.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let len = file.metadata()?.len() as usize;
        if len < HEADER_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SPSC ring is truncated",
            ));
        }
        let mut mmap = unsafe { MmapOptions::new().map_mut(&file)? };
        prefault(&mmap);
        let base = mmap.as_mut_ptr();
        let (magic, version, capacity, slot_size) = unsafe {
            (
                ptr::read(base as *const u64),
                ptr::read(base.add(8) as *const u64),
                ptr::read(base.add(16) as *const u64),
                ptr::read(base.add(24) as *const u64) as usize,
            )
        };
        if magic != MAGIC || version != VERSION || slot_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SPSC ring header mismatch",
            ));
        }
        let stride = slot_stride(slot_size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "slot overflow"))?;
        let expected = HEADER_LEN
            .checked_add(
                usize::try_from(capacity)
                    .ok()
                    .and_then(|capacity| capacity.checked_mul(stride))
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ring overflow"))?,
            )
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ring overflow"))?;
        if len != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SPSC ring size mismatch",
            ));
        }
        Ok(Self {
            base,
            mmap,
            capacity,
            slot_size,
            stride,
        })
    }

    #[must_use]
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Attempts to push one payload.
    ///
    /// # Errors
    /// Returns [`PushError::Full`] or [`PushError::TooLarge`].
    pub fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        if payload.len() > self.slot_size {
            return Err(PushError::TooLarge);
        }
        let head = self.head().load(Ordering::Relaxed);
        let tail = self.tail().load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= self.capacity {
            return Err(PushError::Full);
        }
        let index = head % self.capacity;
        let slot = unsafe { self.slot_ptr(index) };
        unsafe {
            ptr::write(slot as *mut u32, payload.len() as u32);
            ptr::copy_nonoverlapping(payload.as_ptr(), slot.add(SLOT_HEADER), payload.len());
        }
        self.head().store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    #[cfg(test)]
    fn try_pop_into(&self, out: &mut Vec<u8>) -> bool {
        let tail = self.tail().load(Ordering::Relaxed);
        let head = self.head().load(Ordering::Acquire);
        if tail == head {
            return false;
        }
        let index = tail % self.capacity;
        let slot = unsafe { self.slot_ptr(index) };
        let len = unsafe { ptr::read(slot as *const u32) } as usize;
        if len > self.slot_size {
            self.tail().store(tail.wrapping_add(1), Ordering::Release);
            out.clear();
            return true;
        }
        out.resize(len, 0);
        unsafe {
            ptr::copy_nonoverlapping(slot.add(SLOT_HEADER), out.as_mut_ptr(), len);
        }
        self.tail().store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Pops one payload into a fixed stack buffer.
    ///
    /// Returns the stored payload length; the caller validates it against the
    /// expected slot size. Returns `None` when the ring is empty.
    #[must_use]
    pub fn try_pop_into_slice(&self, out: &mut [u8]) -> Option<usize> {
        let tail = self.tail().load(Ordering::Relaxed);
        let head = self.head().load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        let index = tail % self.capacity;
        let slot = unsafe { self.slot_ptr(index) };
        let len = unsafe { ptr::read(slot as *const u32) } as usize;
        if len > self.slot_size {
            self.tail().store(tail.wrapping_add(1), Ordering::Release);
            return Some(len);
        }
        let copy_len = len.min(out.len());
        unsafe {
            ptr::copy_nonoverlapping(slot.add(SLOT_HEADER), out.as_mut_ptr(), copy_len);
        }
        self.tail().store(tail.wrapping_add(1), Ordering::Release);
        Some(len)
    }

    /// Approximate occupancy.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.head()
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail().load(Ordering::Acquire))
            .min(self.capacity)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn head(&self) -> &AtomicU64 {
        unsafe { &*(self.base.add(HEAD_OFFSET) as *const AtomicU64) }
    }

    fn tail(&self) -> &AtomicU64 {
        unsafe { &*(self.base.add(TAIL_OFFSET) as *const AtomicU64) }
    }

    unsafe fn slot_ptr(&self, index: u64) -> *mut u8 {
        unsafe { self.base.add(HEADER_LEN + index as usize * self.stride) }
    }
}

impl Drop for SpscRing {
    fn drop(&mut self) {
        // Best-effort flush: unmap happens when `mmap` drops. Durability is
        // provided by the explicit log/snapshot paths, not by the ring.
        let _ = self.mmap.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use tempfile::TempDir;

    #[test]
    fn test_ring_roundtrip_and_wrap() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ring");
        let ring = SpscRing::create(&path, 4, 32).unwrap();
        let mut out = Vec::new();
        for i in 0..16u8 {
            ring.try_push(&[i]).unwrap();
            assert!(ring.try_pop_into(&mut out));
            assert_eq!(out, vec![i]);
        }
        assert!(ring.is_empty());
    }

    #[test]
    fn test_ring_pop_into_stack_slice() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ring");
        let ring = SpscRing::create(&path, 4, 8).unwrap();
        ring.try_push(&[1, 2, 3, 4]).unwrap();
        let mut out = [0u8; 8];
        assert_eq!(ring.try_pop_into_slice(&mut out), Some(4));
        assert_eq!(&out[..4], &[1, 2, 3, 4]);
        assert_eq!(ring.try_pop_into_slice(&mut out), None);
    }

    #[test]
    fn test_ring_odd_slot_size_alignment() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ring");
        let ring = SpscRing::create(&path, 4, 3).unwrap();
        let mut out = Vec::new();
        for i in 0..20u8 {
            ring.try_push(&[i, i + 1, i + 2][..3]).unwrap();
            assert!(ring.try_pop_into(&mut out));
            assert_eq!(out, vec![i, i + 1, i + 2]);
        }
    }

    #[test]
    fn test_ring_full_and_too_large() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ring");
        let ring = SpscRing::create(&path, 2, 8).unwrap();
        assert_eq!(ring.try_push(&[0; 9]), Err(PushError::TooLarge));
        ring.try_push(b"a").unwrap();
        ring.try_push(b"b").unwrap();
        assert_eq!(ring.try_push(b"c"), Err(PushError::Full));
    }

    #[test]
    fn test_two_mappings_concurrent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ring");
        let producer = SpscRing::create(&path, 1024, 64).unwrap();
        let consumer = SpscRing::open(&path).unwrap();
        let n = 200_000u64;
        let p = thread::spawn(move || {
            for i in 0..n {
                let bytes = i.to_le_bytes();
                loop {
                    match producer.try_push(&bytes) {
                        Ok(()) => break,
                        Err(PushError::Full) => std::hint::spin_loop(),
                        Err(PushError::TooLarge) => panic!("too large"),
                    }
                }
            }
        });
        let mut out = Vec::new();
        let mut expected = 0u64;
        while expected < n {
            if consumer.try_pop_into(&mut out) {
                assert_eq!(out, expected.to_le_bytes());
                expected += 1;
            } else {
                std::hint::spin_loop();
            }
        }
        p.join().unwrap();
    }
}
