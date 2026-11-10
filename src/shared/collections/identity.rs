//! Identity hashing for dense, monotonically increasing numeric IDs.
//!
//! Order IDs are assigned by one book and are dense, so hashing them with
//! SipHash adds cost without improving distribution. These aliases use the
//! value itself as the hash.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// Hasher that uses a `u64` key as its own hash.
#[derive(Default)]
pub struct U64IdentityHasher(u64);

impl Hasher for U64IdentityHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // This hasher is only used for `u64` keys; keep a deterministic
        // fallback for the generic `Hash` path.
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

/// Hasher that mixes a `u64` key to avoid clustered high bits.
#[derive(Default)]
pub struct U64MixHasher(u64);

impl Hasher for U64MixHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }

    fn write_u64(&mut self, value: u64) {
        // splitmix64 finalizer: dense sequential IDs become well-distributed
        // bucket and control bits.
        let mut value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        self.0 = value ^ (value >> 31);
    }
}

/// `HashMap<u64, V>` with mixed hashing.
pub type U64MixMap<V> = HashMap<u64, V, BuildHasherDefault<U64MixHasher>>;

/// `HashSet<u64>` with mixed hashing.
pub type U64MixSet = HashSet<u64, BuildHasherDefault<U64MixHasher>>;

/// `HashMap<u64, V>` with identity hashing.
pub type U64IdentityMap<V> = HashMap<u64, V, BuildHasherDefault<U64IdentityHasher>>;

/// `HashSet<u64>` with identity hashing.
pub type U64IdentitySet = HashSet<u64, BuildHasherDefault<U64IdentityHasher>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_and_set_roundtrip_dense_ids() {
        let mut map: U64IdentityMap<u64> = U64IdentityMap::default();
        map.insert(7, 11);
        assert_eq!(map.get(&7), Some(&11));
        let mut set = U64IdentitySet::default();
        set.insert(9);
        assert!(set.contains(&9));
    }
}
