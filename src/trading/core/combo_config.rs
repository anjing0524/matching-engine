//! Static combo venue configuration: pairs, leg index, settlements, balances.
//!
//! This is the venue's combo policy state. Matching and event projection read
//! it through this module; `InstrumentRegistry` no longer owns five parallel
//! maps for the same configuration.

use crate::shared::collections::identity::U64MixMap;
use crate::trading::core::combo_pair::ComboPair;

/// Static combo configuration and per-account margin balances.
pub(crate) struct ComboConfig {
    pairs: U64MixMap<ComboPair>,
    pairs_by_instrument: U64MixMap<Vec<u32>>,
    prior_settlements: U64MixMap<u64>,
    margin_balances: U64MixMap<u64>,
    require_explicit_margin: bool,
}

impl ComboConfig {
    pub(crate) fn new() -> Self {
        Self {
            pairs: U64MixMap::default(),
            pairs_by_instrument: U64MixMap::default(),
            prior_settlements: U64MixMap::default(),
            margin_balances: U64MixMap::default(),
            require_explicit_margin: false,
        }
    }

    pub(crate) fn contains_pair(&self, pair_id: u32) -> bool {
        self.pairs.contains_key(&u64::from(pair_id))
    }

    pub(crate) fn pair(&self, pair_id: u32) -> Option<&ComboPair> {
        self.pairs.get(&u64::from(pair_id))
    }

    pub(crate) fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    pub(crate) fn insert_pair(&mut self, pair: ComboPair) {
        self.pairs.insert(u64::from(pair.pair_id), pair);
        for instrument_id in [pair.leg1_instrument_id, pair.leg2_instrument_id] {
            self.pairs_by_instrument
                .entry(u64::from(instrument_id))
                .or_default()
                .push(pair.pair_id);
        }
    }

    pub(crate) fn pair_ids_for_instrument(&self, instrument_id: u32) -> Vec<u32> {
        self.pairs_by_instrument
            .get(&u64::from(instrument_id))
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn pairs(&self) -> Vec<ComboPair> {
        self.pairs.values().copied().collect()
    }

    pub(crate) fn set_prior_settlement(&mut self, instrument_id: u32, price_ticks: u64) {
        self.prior_settlements
            .insert(u64::from(instrument_id), price_ticks);
    }

    pub(crate) fn prior_settlement(&self, instrument_id: u32) -> Option<u64> {
        self.prior_settlements
            .get(&u64::from(instrument_id))
            .copied()
    }

    pub(crate) fn prior_settlements(&self) -> Vec<(u32, u64)> {
        self.prior_settlements
            .iter()
            .map(|(instrument_id, price_ticks)| (*instrument_id as u32, *price_ticks))
            .collect()
    }

    pub(crate) fn set_margin_balance(&mut self, account_id: u64, balance: u64) {
        self.margin_balances.insert(account_id, balance);
    }

    pub(crate) fn margin_balance(&self, account_id: u64) -> Option<u64> {
        self.margin_balances.get(&account_id).copied()
    }

    pub(crate) fn set_require_explicit_margin(&mut self, required: bool) {
        self.require_explicit_margin = required;
    }

    pub(crate) fn require_explicit_margin(&self) -> bool {
        self.require_explicit_margin
    }

    pub(crate) fn restore_pairs_and_settlements(
        &mut self,
        pairs: Vec<ComboPair>,
        prior_settlements: Vec<(u32, u64)>,
    ) {
        self.pairs.clear();
        self.pairs_by_instrument.clear();
        self.prior_settlements.clear();
        for pair in pairs {
            self.insert_pair(pair);
        }
        for (instrument_id, price_ticks) in prior_settlements {
            self.set_prior_settlement(instrument_id, price_ticks);
        }
    }
}
