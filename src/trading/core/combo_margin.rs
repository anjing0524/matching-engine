//! Leg-level margin ledger for M1 combo orders.
//!
//! A Combo Pair has no position or margin of its own. Entering a combo order
//! freezes an estimate on each leg Contract from the leg's prior settlement and
//! the pair's configured margin rate. A fill releases the frozen estimate for
//! the filled quantity and records actual trade-price margin on the owner's
//! side of each leg. Cancel releases whatever is still frozen.
//!
//! Margin is reported per account and leg. Hedged long and short positions on
//! the same leg are margined with the single-side-largest rule: the required
//! amount is the larger side, never their sum.

use crate::shared::collections::identity::{U64MixMap, U128MixMap};
use crate::trading::core::combo_pair::ComboPair;
use crate::trading::core::protocol::Side;
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// Basis-point denominator used by the configured margin rates.
const MARGIN_RATE_DENOMINATOR: u128 = 10_000;

/// Margin state for one account on one leg Contract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct LegMargin {
    /// Owning user account.
    pub account_id: u64,
    /// Leg Contract instrument id.
    pub instrument_id: u32,
    /// Estimated margin frozen for this account's resting combo orders.
    pub frozen: u128,
    /// Actual margin from filled long positions at their trade prices.
    pub long_trade_margin: u128,
    /// Actual margin from filled short positions at their trade prices.
    pub short_trade_margin: u128,
}

impl LegMargin {
    /// Required margin under the single-side-largest rule.
    #[must_use]
    pub const fn required(&self) -> u128 {
        if self.long_trade_margin > self.short_trade_margin {
            self.long_trade_margin
        } else {
            self.short_trade_margin
        }
    }
}

/// Margin frozen by one resting or filled Combo Order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboMarginFreeze {
    /// Exchange id of the Combo Order carrying the freeze.
    pub order_id: u64,
    /// Combo Pair traded by the order.
    pub pair_id: u32,
    /// Owning user account.
    pub account_id: u64,
    /// Quantity whose estimate is still frozen.
    pub remaining: u64,
    /// Estimated margin frozen on the first leg.
    pub leg1_frozen: u128,
    /// Estimated margin frozen on the second leg.
    pub leg2_frozen: u128,
}

/// Contract multiplier and basis-point rate governing one leg's margin.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LegMarginRate {
    /// Contract multiplier applied to leg prices.
    pub multiplier: u64,
    /// Margin rate in basis points of notional.
    pub rate_bps: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct LegMarginState {
    frozen: u128,
    long_trade_margin: u128,
    short_trade_margin: u128,
}

#[derive(Clone, Copy, Debug)]
struct Reservation {
    freeze: ComboMarginFreeze,
    leg1_instrument_id: u32,
    leg2_instrument_id: u32,
    multiplier: u64,
    leg1_rate_bps: u64,
    leg2_rate_bps: u64,
    leg1_prior_settlement: u64,
    leg2_prior_settlement: u64,
}

/// Serializable leg-level margin ledger state.
///
/// `leg_margins` carries actual trade-price margin and the frozen estimate per
/// account and leg Contract. `reservations` carries the freeze attached to each
/// Combo Order; the registry cross-checks it against the resting Combo Orders so
/// a partial snapshot cannot silently drop a reservation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ComboMarginSnapshot {
    /// Margin state per account and leg Contract.
    pub leg_margins: Vec<LegMargin>,
    /// Freezes carried by still-open Combo Orders.
    pub reservations: Vec<ComboMarginFreeze>,
}

/// Why a leg-level margin ledger snapshot was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComboMarginSnapshotError {
    /// The same account and leg Contract appeared twice.
    DuplicateLegMargin {
        /// Owning user account.
        account_id: u64,
        /// Leg Contract instrument id.
        instrument_id: u32,
    },
    /// Two freeze entries named the same Combo Order.
    DuplicateReservation {
        /// Exchange id of the Combo Order carrying the freeze.
        order_id: u64,
    },
    /// A reservation with no open quantity is not a valid ledger state.
    ZeroRemainingReservation {
        /// Exchange id of the Combo Order carrying the freeze.
        order_id: u64,
    },
    /// A reservation named a pair the snapshot does not define.
    UnknownPairReservation {
        /// Exchange id of the Combo Order carrying the freeze.
        order_id: u64,
        /// Combo Pair carried by the freeze.
        pair_id: u32,
    },
    /// A reservation amount disagrees with its quantity, pair, and settlement.
    ReservationAmountMismatch {
        /// Exchange id of the Combo Order carrying the freeze.
        order_id: u64,
    },
    /// Per-leg frozen totals do not match the sum of the order reservations.
    FrozenMismatch {
        /// Owning user account.
        account_id: u64,
        /// Leg Contract instrument id.
        instrument_id: u32,
        /// Frozen amount implied by the reservations.
        expected: u128,
        /// Frozen amount stored in the leg margin entry.
        actual: u128,
    },
}

impl Display for ComboMarginSnapshotError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::DuplicateLegMargin {
                account_id,
                instrument_id,
            } => write!(
                formatter,
                "margin snapshot repeats account {account_id} on leg {instrument_id}"
            ),
            Self::DuplicateReservation { order_id } => {
                write!(formatter, "margin snapshot repeats reservation {order_id}")
            }
            Self::ZeroRemainingReservation { order_id } => write!(
                formatter,
                "margin reservation {order_id} has no open quantity"
            ),
            Self::UnknownPairReservation { order_id, pair_id } => write!(
                formatter,
                "margin reservation {order_id} names unknown combo pair {pair_id}"
            ),
            Self::ReservationAmountMismatch { order_id } => write!(
                formatter,
                "margin reservation {order_id} disagrees with its quantity and settlement"
            ),
            Self::FrozenMismatch {
                account_id,
                instrument_id,
                expected,
                actual,
            } => write!(
                formatter,
                "margin snapshot has frozen {actual} on account {account_id} leg {instrument_id}, reservations imply {expected}"
            ),
        }
    }
}

impl Error for ComboMarginSnapshotError {}

/// Margin ledger keyed by account and leg Contract.
#[derive(Debug, Default)]
pub(crate) struct ComboMarginLedger {
    leg_margins: U128MixMap<u128, LegMarginState>,
    reservations: U64MixMap<Reservation>,
}

impl ComboMarginLedger {
    /// Freezes prior-settlement margin for a newly accepted Combo Order.
    pub(crate) fn freeze(
        &mut self,
        pair: &ComboPair,
        order_id: u64,
        account_id: u64,
        quantity: u64,
        leg1_prior_settlement: u64,
        leg2_prior_settlement: u64,
    ) -> ComboMarginFreeze {
        let leg1_frozen = margin_amount(
            leg1_prior_settlement,
            pair.margin.multiplier,
            pair.margin.leg1_rate_bps,
            quantity,
        );
        let leg2_frozen = margin_amount(
            leg2_prior_settlement,
            pair.margin.multiplier,
            pair.margin.leg2_rate_bps,
            quantity,
        );
        let leg1 = self.leg_state(account_id, pair.leg1_instrument_id);
        leg1.frozen = leg1.frozen.saturating_add(leg1_frozen);
        let leg2 = self.leg_state(account_id, pair.leg2_instrument_id);
        leg2.frozen = leg2.frozen.saturating_add(leg2_frozen);

        let freeze = ComboMarginFreeze {
            order_id,
            pair_id: pair.pair_id,
            account_id,
            remaining: quantity,
            leg1_frozen,
            leg2_frozen,
        };
        self.reservations.insert(
            order_id,
            Reservation {
                freeze,
                leg1_instrument_id: pair.leg1_instrument_id,
                leg2_instrument_id: pair.leg2_instrument_id,
                multiplier: pair.margin.multiplier,
                leg1_rate_bps: pair.margin.leg1_rate_bps,
                leg2_rate_bps: pair.margin.leg2_rate_bps,
                leg1_prior_settlement,
                leg2_prior_settlement,
            },
        );
        freeze
    }

    /// Releases the estimate for `filled` quantity of one Combo Order.
    ///
    /// The remaining estimate is recomputed from the unfilled quantity, so
    /// integer rounding never accumulates across partial fills.
    pub(crate) fn release_on_fill(&mut self, order_id: u64, filled: u64) {
        let Some(reservation) = self.reservations.get(&order_id).copied() else {
            return;
        };
        let remaining = reservation.freeze.remaining.saturating_sub(filled);
        let leg1_frozen = margin_amount(
            reservation.leg1_prior_settlement,
            reservation.multiplier,
            reservation.leg1_rate_bps,
            remaining,
        );
        let leg2_frozen = margin_amount(
            reservation.leg2_prior_settlement,
            reservation.multiplier,
            reservation.leg2_rate_bps,
            remaining,
        );
        self.release_frozen(
            reservation.freeze.account_id,
            reservation.leg1_instrument_id,
            reservation.freeze.leg1_frozen.saturating_sub(leg1_frozen),
        );
        self.release_frozen(
            reservation.freeze.account_id,
            reservation.leg2_instrument_id,
            reservation.freeze.leg2_frozen.saturating_sub(leg2_frozen),
        );
        let mut updated = reservation;
        updated.freeze.remaining = remaining;
        updated.freeze.leg1_frozen = leg1_frozen;
        updated.freeze.leg2_frozen = leg2_frozen;
        if remaining == 0 {
            self.reservations.remove(&order_id);
        } else {
            self.reservations.insert(order_id, updated);
        }
    }

    /// Releases every remaining estimate carried by one Combo Order.
    pub(crate) fn release_all(&mut self, order_id: u64) {
        let Some(reservation) = self.reservations.remove(&order_id) else {
            return;
        };
        self.release_frozen(
            reservation.freeze.account_id,
            reservation.leg1_instrument_id,
            reservation.freeze.leg1_frozen,
        );
        self.release_frozen(
            reservation.freeze.account_id,
            reservation.leg2_instrument_id,
            reservation.freeze.leg2_frozen,
        );
    }

    /// Records actual trade-price margin on one side of a leg Contract.
    pub(crate) fn record_actual(
        &mut self,
        account_id: u64,
        instrument_id: u32,
        side: Side,
        price_ticks: u64,
        rate: LegMarginRate,
        quantity: u64,
    ) {
        let amount = margin_amount(price_ticks, rate.multiplier, rate.rate_bps, quantity);
        let state = self.leg_state(account_id, instrument_id);
        match side {
            Side::Buy => {
                state.long_trade_margin = state.long_trade_margin.saturating_add(amount);
            }
            Side::Sell => {
                state.short_trade_margin = state.short_trade_margin.saturating_add(amount);
            }
        }
    }

    /// Returns the margin state for one account and leg Contract.
    pub(crate) fn leg_margin(&self, account_id: u64, instrument_id: u32) -> LegMargin {
        let state = self
            .leg_margins
            .get(&leg_key(account_id, instrument_id))
            .copied()
            .unwrap_or_default();
        LegMargin {
            account_id,
            instrument_id,
            frozen: state.frozen,
            long_trade_margin: state.long_trade_margin,
            short_trade_margin: state.short_trade_margin,
        }
    }

    /// Returns the freeze carried by one Combo Order.
    pub(crate) fn freeze_for(&self, order_id: u64) -> Option<ComboMarginFreeze> {
        self.reservations
            .get(&order_id)
            .map(|reservation| reservation.freeze)
    }

    /// Captures leg margin state and order reservations.
    #[must_use]
    pub(crate) fn snapshot(&self) -> ComboMarginSnapshot {
        let mut leg_margins: Vec<LegMargin> = self
            .leg_margins
            .iter()
            .map(|(key, state)| {
                let (account_id, instrument_id) = leg_key_parts(*key);
                LegMargin {
                    account_id,
                    instrument_id,
                    frozen: state.frozen,
                    long_trade_margin: state.long_trade_margin,
                    short_trade_margin: state.short_trade_margin,
                }
            })
            .collect();
        leg_margins.sort_by_key(|margin| (margin.instrument_id, margin.account_id));
        let mut reservations: Vec<ComboMarginFreeze> = self
            .reservations
            .values()
            .map(|reservation| reservation.freeze)
            .collect();
        reservations.sort_by_key(|freeze| freeze.order_id);
        ComboMarginSnapshot {
            leg_margins,
            reservations,
        }
    }

    /// Replaces the ledger with a captured snapshot.
    ///
    /// Freeze entries are rebuilt from the static pair definition and prior
    /// settlement so a later partial fill releases the same estimate. Restore is
    /// all-or-none: the ledger changes only after every entry is validated.
    ///
    /// # Errors
    /// Returns [`ComboMarginSnapshotError`] for duplicate entries, a reservation
    /// with no open quantity or an unknown pair, or per-leg frozen totals that
    /// do not match the reservations.
    pub(crate) fn restore_snapshot(
        &mut self,
        snapshot: ComboMarginSnapshot,
        pairs: &U64MixMap<ComboPair>,
        prior_settlements: &U64MixMap<u64>,
    ) -> Result<(), ComboMarginSnapshotError> {
        let mut leg_margins: U128MixMap<u128, LegMarginState> = U128MixMap::default();
        for margin in snapshot.leg_margins {
            let key = leg_key(margin.account_id, margin.instrument_id);
            if leg_margins.contains_key(&key) {
                return Err(ComboMarginSnapshotError::DuplicateLegMargin {
                    account_id: margin.account_id,
                    instrument_id: margin.instrument_id,
                });
            }
            leg_margins.insert(
                key,
                LegMarginState {
                    frozen: margin.frozen,
                    long_trade_margin: margin.long_trade_margin,
                    short_trade_margin: margin.short_trade_margin,
                },
            );
        }

        let mut expected: U128MixMap<u128, u128> = U128MixMap::default();
        let mut reservations: U64MixMap<Reservation> = U64MixMap::default();
        for freeze in snapshot.reservations {
            if freeze.remaining == 0 {
                return Err(ComboMarginSnapshotError::ZeroRemainingReservation {
                    order_id: freeze.order_id,
                });
            }
            if reservations.contains_key(&freeze.order_id) {
                return Err(ComboMarginSnapshotError::DuplicateReservation {
                    order_id: freeze.order_id,
                });
            }
            let Some(pair) = pairs.get(&u64::from(freeze.pair_id)).copied() else {
                return Err(ComboMarginSnapshotError::UnknownPairReservation {
                    order_id: freeze.order_id,
                    pair_id: freeze.pair_id,
                });
            };
            let leg1_prior_settlement = prior_settlements
                .get(&u64::from(pair.leg1_instrument_id))
                .copied()
                .unwrap_or(0);
            let leg2_prior_settlement = prior_settlements
                .get(&u64::from(pair.leg2_instrument_id))
                .copied()
                .unwrap_or(0);
            let leg1_frozen = margin_amount(
                leg1_prior_settlement,
                pair.margin.multiplier,
                pair.margin.leg1_rate_bps,
                freeze.remaining,
            );
            let leg2_frozen = margin_amount(
                leg2_prior_settlement,
                pair.margin.multiplier,
                pair.margin.leg2_rate_bps,
                freeze.remaining,
            );
            if freeze.leg1_frozen != leg1_frozen || freeze.leg2_frozen != leg2_frozen {
                return Err(ComboMarginSnapshotError::ReservationAmountMismatch {
                    order_id: freeze.order_id,
                });
            }
            let leg1_key = leg_key(freeze.account_id, pair.leg1_instrument_id);
            let leg2_key = leg_key(freeze.account_id, pair.leg2_instrument_id);
            let leg1_expected = expected.entry(leg1_key).or_default();
            *leg1_expected = leg1_expected.saturating_add(freeze.leg1_frozen);
            let leg2_expected = expected.entry(leg2_key).or_default();
            *leg2_expected = leg2_expected.saturating_add(freeze.leg2_frozen);
            reservations.insert(
                freeze.order_id,
                Reservation {
                    freeze,
                    leg1_instrument_id: pair.leg1_instrument_id,
                    leg2_instrument_id: pair.leg2_instrument_id,
                    multiplier: pair.margin.multiplier,
                    leg1_rate_bps: pair.margin.leg1_rate_bps,
                    leg2_rate_bps: pair.margin.leg2_rate_bps,
                    leg1_prior_settlement,
                    leg2_prior_settlement,
                },
            );
        }

        let mut keys: Vec<u128> = leg_margins.keys().chain(expected.keys()).copied().collect();
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            let actual = leg_margins.get(&key).map_or(0, |state| state.frozen);
            let expected_frozen = expected.get(&key).copied().unwrap_or(0);
            if actual != expected_frozen {
                let (account_id, instrument_id) = leg_key_parts(key);
                return Err(ComboMarginSnapshotError::FrozenMismatch {
                    account_id,
                    instrument_id,
                    expected: expected_frozen,
                    actual,
                });
            }
        }

        self.leg_margins = leg_margins;
        self.reservations = reservations;
        Ok(())
    }

    fn leg_state(&mut self, account_id: u64, instrument_id: u32) -> &mut LegMarginState {
        self.leg_margins
            .entry(leg_key(account_id, instrument_id))
            .or_default()
    }

    fn release_frozen(&mut self, account_id: u64, instrument_id: u32, amount: u128) {
        let Some(state) = self
            .leg_margins
            .get_mut(&leg_key(account_id, instrument_id))
        else {
            return;
        };
        state.frozen = state.frozen.saturating_sub(amount);
    }
}

/// Returns margin for `quantity` at `price_ticks`, saturating on overflow.
fn margin_amount(price_ticks: u64, multiplier: u64, rate_bps: u64, quantity: u64) -> u128 {
    u128::from(price_ticks)
        .saturating_mul(u128::from(multiplier))
        .saturating_mul(u128::from(quantity))
        .saturating_mul(u128::from(rate_bps))
        / MARGIN_RATE_DENOMINATOR
}

fn leg_key(account_id: u64, instrument_id: u32) -> u128 {
    (u128::from(instrument_id) << 64) | u128::from(account_id)
}

fn leg_key_parts(key: u128) -> (u64, u32) {
    let account_id = (key & u128::from(u64::MAX)) as u64;
    let instrument_id = (key >> 64) as u32;
    (account_id, instrument_id)
}
