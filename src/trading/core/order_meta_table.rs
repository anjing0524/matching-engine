//! Chunked order-metadata table.
//!
//! Order ids are dense and monotonic, so index chunks directly instead of
//! hashing. A chunked table never rehashes; growth appends one chunk pointer.

use crate::trading::core::kernel::OrderMeta;

const CHUNK: usize = 1 << 14;

/// Live order metadata indexed by order id.
pub(crate) struct OrderMetaTable {
    chunks: Vec<Box<[Option<OrderMeta>]>>,
    len: usize,
}

impl OrderMetaTable {
    pub(crate) fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    fn slot(&self, order_id: u64) -> Option<&Option<OrderMeta>> {
        let index = order_id as usize;
        self.chunks
            .get(index / CHUNK)
            .and_then(|chunk| chunk.get(index % CHUNK))
    }

    fn slot_mut(&mut self, order_id: u64) -> Option<&mut Option<OrderMeta>> {
        let index = order_id as usize;
        self.chunks
            .get_mut(index / CHUNK)
            .and_then(|chunk| chunk.get_mut(index % CHUNK))
    }

    pub(crate) fn insert(&mut self, order_id: u64, meta: OrderMeta) -> Option<OrderMeta> {
        let index = order_id as usize;
        let chunk = index / CHUNK;
        while self.chunks.len() <= chunk {
            self.chunks.push(vec![None; CHUNK].into_boxed_slice());
        }
        let slot = &mut self.chunks[chunk][index % CHUNK];
        let previous = slot.replace(meta);
        if previous.is_none() {
            self.len += 1;
        }
        previous
    }

    pub(crate) fn remove(&mut self, order_id: u64) -> Option<OrderMeta> {
        let removed = self.slot_mut(order_id).and_then(Option::take);
        if removed.is_some() {
            self.len -= 1;
        }
        removed
    }

    pub(crate) fn get(&self, order_id: u64) -> Option<&OrderMeta> {
        self.slot(order_id).and_then(Option::as_ref)
    }

    pub(crate) fn get_mut(&mut self, order_id: u64) -> Option<&mut OrderMeta> {
        self.slot_mut(order_id).and_then(Option::as_mut)
    }

    pub(crate) fn contains_key(&self, order_id: u64) -> bool {
        self.slot(order_id).is_some_and(Option::is_some)
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (u64, &OrderMeta)> {
        self.chunks
            .iter()
            .enumerate()
            .flat_map(|(chunk_idx, chunk)| {
                chunk.iter().enumerate().filter_map(move |(offset, slot)| {
                    slot.as_ref()
                        .map(|meta| ((chunk_idx * CHUNK + offset) as u64, meta))
                })
            })
    }
}

impl FromIterator<(u64, OrderMeta)> for OrderMetaTable {
    fn from_iter<T: IntoIterator<Item = (u64, OrderMeta)>>(iter: T) -> Self {
        let mut table = Self::new();
        for (order_id, meta) in iter {
            table.insert(order_id, meta);
        }
        table
    }
}
