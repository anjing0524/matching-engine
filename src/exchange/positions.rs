//! Position accounting for the exchange core kernel.
//!
//! Positions are gross and split by side, hedge type, and today/yesterday
//! bucket. Orders are deliberately not embedded in positions; this module only
//! owns the counters that fills and risk plugins read.

use crate::exchange::protocol::{Hedge, Offset, Side};
use std::collections::HashMap;

/// One side of a gross position, split by trading day bucket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub today: u64,
    pub yesterday: u64,
}

impl Position {
    #[must_use]
    pub fn total(self) -> u64 {
        self.today.saturating_add(self.yesterday)
    }
}

/// Key for one account's position bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PositionKey {
    pub account_id: u64,
    pub side: Side,
    pub hedge: Hedge,
}

impl PositionKey {
    #[must_use]
    pub const fn new(account_id: u64, side: Side, hedge: Hedge) -> Self {
        Self {
            account_id,
            side,
            hedge,
        }
    }
}

/// Key for close-quantity reservations made by accepted close orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CloseKey {
    pub position: PositionKey,
    pub offset: Offset,
}

impl CloseKey {
    #[must_use]
    pub const fn new(position: PositionKey, offset: Offset) -> Self {
        Self { position, offset }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionError {
    InvalidOffset,
    InsufficientPosition { available: u64, requested: u64 },
    Overflow,
}

/// Gross position store.
#[derive(Debug, Default)]
pub struct Positions {
    positions: HashMap<PositionKey, Position>,
}

impl Positions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<(PositionKey, Position)> {
        self.positions
            .iter()
            .map(|(key, value)| (*key, *value))
            .collect()
    }

    pub fn restore(&mut self, entries: impl IntoIterator<Item = (PositionKey, Position)>) {
        self.positions = entries.into_iter().collect();
    }

    #[must_use]
    pub fn get(&self, key: PositionKey) -> Position {
        self.positions.get(&key).copied().unwrap_or_default()
    }

    /// Adds filled open quantity to today's bucket.
    pub fn apply_open(&mut self, key: PositionKey, quantity: u64) -> Result<(), PositionError> {
        let position = self.positions.entry(key).or_default();
        position.today = position
            .today
            .checked_add(quantity)
            .ok_or(PositionError::Overflow)?;
        Ok(())
    }

    /// Applies a close fill to exactly one position bucket.
    pub fn apply_close(
        &mut self,
        key: PositionKey,
        offset: Offset,
        quantity: u64,
    ) -> Result<(), PositionError> {
        let position = self.positions.entry(key).or_default();
        let bucket = match offset {
            Offset::Open => return Err(PositionError::InvalidOffset),
            Offset::Close => &mut position.yesterday,
            Offset::CloseToday => &mut position.today,
        };
        if *bucket < quantity {
            return Err(PositionError::InsufficientPosition {
                available: *bucket,
                requested: quantity,
            });
        }
        *bucket -= quantity;
        Ok(())
    }

    /// Closeable quantity for one offset instruction, before frozen orders.
    #[must_use]
    pub fn available_close(&self, key: PositionKey, offset: Offset) -> u64 {
        let position = self.get(key);
        match offset {
            Offset::Open => 0,
            Offset::Close => position.yesterday,
            Offset::CloseToday => position.today,
        }
    }
}

/// Close-quantity reserved by accepted but not yet filled close orders.
///
/// This is a risk counter, not an ownership relation between an order and a
/// position. A fill/cancel releases its reservation.
#[derive(Debug, Default)]
pub struct FrozenClose {
    reserved: HashMap<CloseKey, u64>,
}

impl FrozenClose {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<(CloseKey, u64)> {
        self.reserved
            .iter()
            .map(|(key, value)| (*key, *value))
            .collect()
    }

    pub fn restore(&mut self, entries: impl IntoIterator<Item = (CloseKey, u64)>) {
        self.reserved = entries.into_iter().collect();
    }

    #[must_use]
    pub fn get(&self, key: CloseKey) -> u64 {
        self.reserved.get(&key).copied().unwrap_or_default()
    }

    /// Reserve close quantity; fails if the position bucket cannot cover it
    /// together with already frozen orders.
    pub fn reserve(
        &mut self,
        key: CloseKey,
        positions: &Positions,
        quantity: u64,
    ) -> Result<(), PositionError> {
        let available = positions.available_close(key.position, key.offset);
        let frozen = self.get(key);
        let remaining = available.saturating_sub(frozen);
        if remaining < quantity {
            return Err(PositionError::InsufficientPosition {
                available: remaining,
                requested: quantity,
            });
        }
        let entry = self.reserved.entry(key).or_default();
        *entry = entry.checked_add(quantity).ok_or(PositionError::Overflow)?;
        Ok(())
    }

    /// Release a reservation on fill or cancel. Saturates at zero so a
    /// duplicate release cannot underflow the counter.
    pub fn release(&mut self, key: CloseKey, quantity: u64) {
        if let Some(entry) = self.reserved.get_mut(&key) {
            *entry = entry.saturating_sub(quantity);
            if *entry == 0 {
                self.reserved.remove(&key);
            }
        }
    }

    #[must_use]
    pub fn available(&self, key: CloseKey, positions: &Positions) -> u64 {
        positions
            .available_close(key.position, key.offset)
            .saturating_sub(self.get(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(side: Side) -> PositionKey {
        PositionKey::new(1, side, Hedge::Spec)
    }

    #[test]
    fn open_adds_to_today() {
        let mut positions = Positions::new();
        positions.apply_open(key(Side::Buy), 10).unwrap();
        let position = positions.get(key(Side::Buy));
        assert_eq!(position.today, 10);
        assert_eq!(position.yesterday, 0);
    }

    #[test]
    fn close_only_reduces_yesterday() {
        let mut positions = Positions::new();
        positions.apply_open(key(Side::Buy), 10).unwrap();
        let error = positions
            .apply_close(key(Side::Buy), Offset::Close, 1)
            .unwrap_err();
        assert_eq!(
            error,
            PositionError::InsufficientPosition {
                available: 0,
                requested: 1
            }
        );
        positions
            .apply_close(key(Side::Buy), Offset::CloseToday, 3)
            .unwrap();
        assert_eq!(positions.get(key(Side::Buy)).today, 7);
    }

    #[test]
    fn frozen_close_prevents_multiple_orders_exceeding_position() {
        let mut positions = Positions::new();
        let mut frozen = FrozenClose::new();
        positions.apply_open(key(Side::Buy), 10).unwrap();
        let close_key = CloseKey::new(key(Side::Buy), Offset::CloseToday);
        frozen.reserve(close_key, &positions, 8).unwrap();
        assert!(frozen.reserve(close_key, &positions, 3).is_err());
        assert_eq!(frozen.available(close_key, &positions), 2);
        frozen.release(close_key, 8);
        assert_eq!(frozen.available(close_key, &positions), 10);
    }
}
