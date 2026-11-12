//! Single-threaded ownership and routing of ordinary Contract books.
//!
//! The registry is the one place that maps a wire instrument id to its
//! deterministic [`TradingCore`]. It owns every core, so matching stays on one
//! thread and two instruments can never share book or position state.
//!
//! Timing and control commands apply to the whole venue and are broadcast to
//! every registered core. New orders and cancels must name a registered
//! instrument before any book mutates.

use crate::domain::orderbook::ContractSpec;
use crate::shared::SymbolId;
use crate::shared::collections::identity::U64MixMap;
use crate::trading::core::combo_book::{
    ComboBook, ComboBookSnapshot, ComboCommandError, ComboEvent, ComboFill, ComboOrder,
    ComboOrderOwner, ComboPrivateFill, ImpliedComboFill, validate_new_combo_order,
};
use crate::trading::core::combo_margin::{
    ComboEntryMargin, ComboMarginError, ComboMarginFreeze, ComboMarginLedger, ComboMarginSnapshot,
    ComboMarginSnapshotError, LegMargin, LegMarginRate,
};
use crate::trading::core::combo_pair::{ComboLeg, ComboPair, ComboPairError};
use crate::trading::core::kernel::{CoreEvent, TradingCore};
use crate::trading::core::leg_liquidity::{
    ComboLegError, ComboLegExecution, LegExecutionContext, LegLiquidityError, LegLiquidityPlan,
};
use crate::trading::core::plugins::{NoPlugins, PreTradePlugin};
use crate::trading::core::private_events::PrivateEvent;
use crate::trading::core::protocol::{Command, CommandType, Side};
use crate::trading::core::public_events::{PublicEvent, PublicEventType};
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// Why the registry refused an instrument or a command.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegistryError {
    /// The command named an instrument id that is not registered.
    UnknownInstrument {
        /// Instrument id carried by the rejected command.
        instrument_id: u32,
    },
    /// An instrument id was registered more than once.
    DuplicateInstrument {
        /// Instrument id that was already present.
        instrument_id: u32,
    },
    /// The contract specification cannot build a valid book.
    InvalidSpec {
        /// Instrument id being registered.
        instrument_id: u32,
        /// Human-readable reason the spec is unusable.
        reason: &'static str,
    },
}

impl Display for RegistryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::UnknownInstrument { instrument_id } => {
                write!(formatter, "instrument {instrument_id} is not registered")
            }
            Self::DuplicateInstrument { instrument_id } => {
                write!(
                    formatter,
                    "instrument {instrument_id} is already registered"
                )
            }
            Self::InvalidSpec {
                instrument_id,
                reason,
            } => write!(
                formatter,
                "instrument {instrument_id} has an invalid contract spec: {reason}"
            ),
        }
    }
}

impl Error for RegistryError {}

/// Why a registry snapshot could not be restored.
///
/// Every variant is explicit and no state changes on error: a partial snapshot
/// (for example a pair without its book, a resting Combo Order without its
/// margin reservation, or a private sequence that does not advance) is rejected
/// instead of being silently repaired.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RegistrySnapshotError {
    /// The snapshot listed one instrument id twice.
    DuplicateInstrument {
        /// Repeated instrument id.
        instrument_id: u32,
    },
    /// An ordinary Contract core rejected its own snapshot.
    CoreRestore {
        /// Instrument whose state was being restored.
        instrument_id: u32,
        /// Reason reported by the core.
        reason: String,
    },
    /// The snapshot listed one instrument's prior settlement twice.
    DuplicatePriorSettlement {
        /// Repeated instrument id.
        instrument_id: u32,
    },
    /// A prior settlement named an instrument the snapshot does not contain.
    UnknownPriorSettlementInstrument {
        /// Instrument id without a snapshot.
        instrument_id: u32,
    },
    /// The snapshot listed one pair id twice.
    DuplicatePair {
        /// Repeated pair id.
        pair_id: u32,
    },
    /// A pair definition did not validate against the snapshot instruments.
    InvalidPair {
        /// Rejected pair id.
        pair_id: u32,
        /// Reason from the static-pair validator.
        error: ComboPairError,
    },
    /// A pair had no Combo Book state.
    MissingComboBook {
        /// Pair id missing its book state.
        pair_id: u32,
    },
    /// The snapshot listed one pair's Combo Book twice.
    DuplicateComboBook {
        /// Repeated pair id.
        pair_id: u32,
    },
    /// A Combo Book snapshot named a pair the snapshot does not define.
    UnknownComboBook {
        /// Pair id without a pair definition.
        pair_id: u32,
    },
    /// A Combo Book rejected its own snapshot.
    ComboBookRestore {
        /// Pair id whose book rejected the snapshot.
        pair_id: u32,
        /// Reason from the book.
        error: crate::trading::core::combo_book::ComboBookError,
    },
    /// One exchange Combo Order id rested in more than one pair book.
    DuplicateComboOrderId {
        /// Repeated exchange order id.
        order_id: u64,
    },
    /// The next Combo Order id would collide with a restored or referenced id.
    InvalidNextComboOrderId {
        /// Stored next Combo Order id.
        next_combo_order_id: u64,
        /// Highest exchange Combo Order id the snapshot references.
        max_assigned_order_id: u64,
    },
    /// A buffered Combo Private Fill named an unregistered pair.
    UnknownComboPrivatePair {
        /// Sequence of the buffered fill.
        seq: u64,
        /// Pair id carried by the fill.
        pair_id: u32,
    },
    /// Buffered Combo Private Fill sequences were not strictly increasing.
    InvalidComboPrivateSequence {
        /// Offending sequence.
        seq: u64,
    },
    /// The next Combo Private sequence would repeat a buffered fill.
    InvalidNextComboPrivateSeq {
        /// Stored next Combo Private sequence.
        next_combo_private_seq: u64,
        /// Highest buffered Combo Private sequence.
        max_buffered_seq: u64,
    },
    /// The next venue public sequence is not usable.
    InvalidNextPublicSeq {
        /// Stored next venue public sequence.
        next_public_seq: u64,
    },
    /// The leg margin ledger rejected its own snapshot.
    Margin(ComboMarginSnapshotError),
    /// A leg margin entry named an instrument the snapshot does not contain.
    UnknownLegMarginInstrument {
        /// Owning user account.
        account_id: u64,
        /// Leg Contract instrument id.
        instrument_id: u32,
    },
    /// A resting Combo Order had no margin reservation.
    MissingMarginReservation {
        /// Resting exchange Combo Order id.
        order_id: u64,
    },
    /// A margin reservation named an order that is not resting.
    UnknownMarginReservation {
        /// Reserved exchange Combo Order id.
        order_id: u64,
    },
    /// A reservation did not match its resting Combo Order.
    MarginReservationMismatch {
        /// Exchange Combo Order id whose reservation disagrees.
        order_id: u64,
    },
}

impl Display for RegistrySnapshotError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::DuplicateInstrument { instrument_id } => {
                write!(formatter, "snapshot repeats instrument {instrument_id}")
            }
            Self::CoreRestore {
                instrument_id,
                reason,
            } => write!(
                formatter,
                "snapshot restore failed for instrument {instrument_id}: {reason}"
            ),
            Self::DuplicatePriorSettlement { instrument_id } => write!(
                formatter,
                "snapshot repeats prior settlement for instrument {instrument_id}"
            ),
            Self::UnknownPriorSettlementInstrument { instrument_id } => write!(
                formatter,
                "snapshot has prior settlement for unknown instrument {instrument_id}"
            ),
            Self::DuplicatePair { pair_id } => {
                write!(formatter, "snapshot repeats combo pair {pair_id}")
            }
            Self::InvalidPair { pair_id, error } => {
                write!(
                    formatter,
                    "snapshot combo pair {pair_id} is invalid: {error}"
                )
            }
            Self::MissingComboBook { pair_id } => {
                write!(formatter, "snapshot combo pair {pair_id} has no combo book")
            }
            Self::DuplicateComboBook { pair_id } => {
                write!(formatter, "snapshot repeats combo book for pair {pair_id}")
            }
            Self::UnknownComboBook { pair_id } => {
                write!(
                    formatter,
                    "snapshot has combo book for unknown pair {pair_id}"
                )
            }
            Self::ComboBookRestore { pair_id, error } => {
                write!(
                    formatter,
                    "snapshot combo book {pair_id} is invalid: {error}"
                )
            }
            Self::DuplicateComboOrderId { order_id } => {
                write!(formatter, "snapshot repeats combo order {order_id}")
            }
            Self::InvalidNextComboOrderId {
                next_combo_order_id,
                max_assigned_order_id,
            } => write!(
                formatter,
                "snapshot next combo order id {next_combo_order_id} does not advance past {max_assigned_order_id}"
            ),
            Self::UnknownComboPrivatePair { seq, pair_id } => write!(
                formatter,
                "snapshot combo private fill {seq} names unknown pair {pair_id}"
            ),
            Self::InvalidComboPrivateSequence { seq } => write!(
                formatter,
                "snapshot combo private sequence {seq} is not strictly increasing"
            ),
            Self::InvalidNextComboPrivateSeq {
                next_combo_private_seq,
                max_buffered_seq,
            } => write!(
                formatter,
                "snapshot next combo private sequence {next_combo_private_seq} does not advance past {max_buffered_seq}"
            ),
            Self::InvalidNextPublicSeq { next_public_seq } => write!(
                formatter,
                "snapshot next venue public sequence {next_public_seq} is not usable"
            ),
            Self::Margin(error) => Display::fmt(error, formatter),
            Self::UnknownLegMarginInstrument {
                account_id,
                instrument_id,
            } => write!(
                formatter,
                "snapshot has margin for account {account_id} on unknown leg {instrument_id}"
            ),
            Self::MissingMarginReservation { order_id } => write!(
                formatter,
                "snapshot resting combo order {order_id} has no margin reservation"
            ),
            Self::UnknownMarginReservation { order_id } => write!(
                formatter,
                "snapshot has margin reservation {order_id} with no resting order"
            ),
            Self::MarginReservationMismatch { order_id } => write!(
                formatter,
                "snapshot margin reservation {order_id} does not match its resting order"
            ),
        }
    }
}

impl Error for RegistrySnapshotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPair { error, .. } => Some(error),
            Self::ComboBookRestore { error, .. } => Some(error),
            Self::Margin(error) => Some(error),
            _ => None,
        }
    }
}

/// Serializable registry-backed combo state.
///
/// Captures the static Combo Pairs, every ordinary Contract core (shared leg
/// books and positions), each pair's base Combo Book (resting orders, arrival
/// sequence, last Combo trade price), the leg margin ledger reservations, the
/// registry-owned combo private buffer and sequence, and the one venue public
/// sequence. A restored registry matches deterministically from the same
/// command stream and continues publishing without repeating a sequence.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct RegistrySnapshot {
    /// Ordinary Contract state, sorted by instrument id.
    pub instruments: Vec<(u32, crate::trading::core::kernel::CoreSnapshot)>,
    /// Static Combo Pair definitions, sorted by pair id.
    pub pairs: Vec<ComboPair>,
    /// Prior settlement per instrument, sorted by instrument id.
    pub prior_settlements: Vec<(u32, u64)>,
    /// Base Combo Book state per pair, sorted by pair id.
    pub combo_books: Vec<ComboBookSnapshot>,
    /// Leg-level margin ledger state.
    pub margin: ComboMarginSnapshot,
    /// Undrained Combo Private Fills, oldest first.
    pub combo_private_events: Vec<ComboPrivateFill>,
    /// Exchange id the next accepted Combo Order will receive.
    pub next_combo_order_id: u64,
    /// Registry-owned sequence the next Combo Private Fill will receive.
    pub next_combo_private_seq: u64,
    /// Venue sequence the next public event will receive.
    ///
    /// Ordinary Contract events and Combo trades share this one monotonic
    /// domain; it advances when the registry drains public events.
    pub next_public_seq: u64,
}

/// Owns one ordinary Contract core per instrument id, the static Combo Pairs,
/// and one base Combo Order book per pair.
///
/// Register instruments and pairs before matching starts; the registry is
/// static for the lifetime of a trading day. Commands are routed by
/// [`Command::instrument_id`] and rejected when that id is unknown.
pub struct InstrumentRegistry {
    instruments: U64MixMap<TradingCore>,
    pairs: U64MixMap<ComboPair>,
    combo_books: U64MixMap<ComboBook>,
    prior_settlements: U64MixMap<u64>,
    margin_balances: U64MixMap<u64>,
    require_explicit_margin: bool,
    margin: ComboMarginLedger,
    combo_private_events: Vec<ComboPrivateFill>,
    combo_public_events: Vec<PublicEvent>,
    next_combo_order_id: u64,
    next_combo_private_seq: u64,
    next_public_seq: u64,
}

impl InstrumentRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            instruments: U64MixMap::default(),
            pairs: U64MixMap::default(),
            combo_books: U64MixMap::default(),
            prior_settlements: U64MixMap::default(),
            margin_balances: U64MixMap::default(),
            require_explicit_margin: false,
            margin: ComboMarginLedger::default(),
            combo_private_events: Vec::new(),
            combo_public_events: Vec::new(),
            next_combo_order_id: 1,
            next_combo_private_seq: 1,
            next_public_seq: 1,
        }
    }

    /// Registers one ordinary Contract with its book specification.
    ///
    /// The instrument symbol is interned from [`ContractSpec::symbol`], so the
    /// core and its public events always agree on the symbol.
    ///
    /// # Errors
    /// Returns [`RegistryError::InvalidSpec`] when the spec cannot form a valid
    /// book and [`RegistryError::DuplicateInstrument`] when the id is taken.
    pub fn register(
        &mut self,
        instrument_id: u32,
        spec: ContractSpec,
    ) -> Result<(), RegistryError> {
        validate_spec(instrument_id, &spec)?;
        let key = u64::from(instrument_id);
        if self.instruments.contains_key(&key) {
            return Err(RegistryError::DuplicateInstrument { instrument_id });
        }
        let symbol = SymbolId::intern(&spec.symbol);
        self.instruments.insert(key, TradingCore::new(spec, symbol));
        Ok(())
    }

    /// Registers one static Combo Pair over two ordinary Contracts.
    ///
    /// Call during startup configuration. The registry exposes no command that
    /// creates or replaces a pair while matching runs.
    ///
    /// # Errors
    /// Returns [`ComboPairError`] when the definition is not a valid static
    /// pair. No pair is registered in that case.
    pub fn register_pair(&mut self, pair: ComboPair) -> Result<(), ComboPairError> {
        if self.pairs.contains_key(&u64::from(pair.pair_id)) {
            return Err(ComboPairError::DuplicatePairId {
                pair_id: pair.pair_id,
            });
        }
        validate_pair(&self.instruments, &pair)?;
        let key = u64::from(pair.pair_id);
        self.pairs.insert(key, pair);
        self.combo_books.insert(key, ComboBook::new(pair.pair_id));
        Ok(())
    }

    /// Sets the prior settlement price used to freeze leg margin at combo entry.
    ///
    /// Prior settlement is venue state supplied at startup or day roll, not a
    /// protocol command. Combo entries for an instrument without a prior
    /// settlement fail closed with [`ComboMarginError::MissingPriorSettlement`].
    ///
    /// # Errors
    /// Returns [`RegistryError::UnknownInstrument`] when `instrument_id` is not
    /// registered. The stored settlement is unchanged in that case.
    pub fn set_prior_settlement(
        &mut self,
        instrument_id: u32,
        price_ticks: u64,
    ) -> Result<(), RegistryError> {
        if !self.instruments.contains_key(&u64::from(instrument_id)) {
            return Err(RegistryError::UnknownInstrument { instrument_id });
        }
        self.prior_settlements
            .insert(u64::from(instrument_id), price_ticks);
        Ok(())
    }

    /// Returns the prior settlement configured for `instrument_id`.
    #[must_use]
    pub fn prior_settlement(&self, instrument_id: u32) -> Option<u64> {
        self.prior_settlements
            .get(&u64::from(instrument_id))
            .copied()
    }

    /// Configures one account's available margin balance.
    ///
    /// The balance is venue configuration, like prior settlement, and is not
    /// carried by a registry snapshot. It is denominated in the same
    /// `price * quantity * multiplier * rate` units the leg margin ledger
    /// reports. An account without a configured balance is not limited, so a
    /// venue that wants backing enforced must configure every trading account
    /// before matching starts.
    pub fn set_margin_balance(&mut self, account_id: u64, balance: u64) {
        self.margin_balances.insert(account_id, balance);
    }

    /// Returns the balance configured for `account_id`, if any.
    #[must_use]
    pub fn margin_balance(&self, account_id: u64) -> Option<u64> {
        self.margin_balances.get(&account_id).copied()
    }

    /// Requires every Combo Order account to have an explicit margin balance.
    ///
    /// The default is `false`, which preserves the registry's standalone
    /// behavior: an account without a configured balance is not limited.
    /// Production runtimes set this when a Combo Pair is configured, so an
    /// unconfigured account is rejected at entry instead of trading unlimited.
    pub fn set_require_explicit_margin(&mut self, required: bool) {
        self.require_explicit_margin = required;
    }

    /// Returns true when Combo Order entry requires an explicit balance.
    #[must_use]
    pub fn requires_explicit_margin(&self) -> bool {
        self.require_explicit_margin
    }

    /// Returns the exchange id the next accepted Combo Order will receive.
    #[must_use]
    pub fn next_combo_order_id(&self) -> u64 {
        self.next_combo_order_id
    }

    /// Returns the account's remaining margin after frozen estimates and
    /// single-side-largest actual margin.
    ///
    /// Returns `None` when the account has no configured balance, because no
    /// limit applies. The result saturates at zero when actual trade-price
    /// margin has already consumed the balance.
    #[must_use]
    pub fn available_margin(&self, account_id: u64) -> Option<u64> {
        let balance = self.margin_balance(account_id)?;
        Some(u64::try_from(self.margin.available(account_id, balance)).unwrap_or(0))
    }

    /// Returns the margin state for `account_id` on one leg Contract.
    #[must_use]
    pub fn leg_margin(&self, account_id: u64, instrument_id: u32) -> LegMargin {
        self.margin.leg_margin(account_id, instrument_id)
    }

    /// Returns the margin freeze carried by one Combo Order.
    #[must_use]
    pub fn combo_margin_freeze(&self, order_id: u64) -> Option<ComboMarginFreeze> {
        self.margin.freeze_for(order_id)
    }

    /// Looks up the Combo Pair for `pair_id`.
    #[must_use]
    pub fn pair(&self, pair_id: u32) -> Option<&ComboPair> {
        self.pairs.get(&u64::from(pair_id))
    }

    /// Returns true when `pair_id` resolves to a Combo Pair.
    #[must_use]
    pub fn contains_pair(&self, pair_id: u32) -> bool {
        self.pairs.contains_key(&u64::from(pair_id))
    }

    /// Returns the number of registered Combo Pairs.
    #[must_use]
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    /// Returns the base Combo Order book for `pair_id`.
    ///
    /// The book is a combo-only interface: ordinary Contract books never expose
    /// combo spreads or quantities.
    #[must_use]
    pub fn combo_book(&self, pair_id: u32) -> Option<&ComboBook> {
        self.combo_books.get(&u64::from(pair_id))
    }

    /// Returns Combo Private Fills produced since the last drain.
    ///
    /// Combo effects use a registry-owned private sequence so direct and
    /// implied-in fills stay ordered with each other. Ordinary leg fills remain
    /// in their Contract core's private sequence and are delivered separately.
    #[must_use]
    pub fn combo_private_events(&self) -> &[ComboPrivateFill] {
        &self.combo_private_events
    }

    /// Drains Combo Private Fills produced since the last drain.
    #[must_use]
    pub fn drain_combo_private_events(&mut self) -> Vec<ComboPrivateFill> {
        std::mem::take(&mut self.combo_private_events)
    }

    /// Returns the Combo Private Fill sequence the next fill will receive.
    #[must_use]
    pub fn next_combo_private_seq(&self) -> u64 {
        self.next_combo_private_seq
    }

    /// Drains ordinary private events from every Contract core.
    ///
    /// Cores are visited in ascending instrument id order so a command stream
    /// that touches more than one leg produces the same private event order on
    /// every run.
    #[must_use]
    pub fn drain_private_events(&mut self) -> Vec<PrivateEvent> {
        let mut events = Vec::new();
        for instrument_id in self.sorted_instrument_ids() {
            if let Some(core) = self.instruments.get_mut(&u64::from(instrument_id)) {
                events.extend(core.drain_private_events());
            }
        }
        events
    }

    /// Drains ordinary public events from every Contract core, then any Combo
    /// trade price statistics produced since the last drain.
    ///
    /// Cores are visited in ascending instrument id order for deterministic
    /// fan-out. Combo trade statistics follow the leg trades of the matching
    /// command, so a consumer sees both leg executions before the combo price.
    /// Every returned event is stamped with the registry's one venue public
    /// sequence, so a market-data publisher and tracker never see duplicate or
    /// regressing sequence numbers across cores and Combo trades.
    #[must_use]
    pub fn drain_public_events(&mut self) -> Vec<PublicEvent> {
        let mut events = Vec::new();
        for instrument_id in self.sorted_instrument_ids() {
            if let Some(core) = self.instruments.get_mut(&u64::from(instrument_id)) {
                events.extend(core.drain_public_events());
            }
        }
        events.append(&mut self.combo_public_events);
        for event in &mut events {
            event.seq = self.next_public_seq;
            if event.event_type == PublicEventType::ComboTrade {
                event.trade_id = event.seq;
            }
            self.next_public_seq = self.next_public_seq.saturating_add(1);
        }
        events
    }

    /// Returns the venue sequence the next public event will receive.
    #[must_use]
    pub fn next_public_seq(&self) -> u64 {
        self.next_public_seq
    }

    fn sorted_instrument_ids(&self) -> Vec<u32> {
        let mut instrument_ids: Vec<u32> = self
            .instruments
            .keys()
            .map(|instrument_id| *instrument_id as u32)
            .collect();
        instrument_ids.sort_unstable();
        instrument_ids
    }

    /// Replays buffered Combo Private Fills after `last_private_seq`.
    ///
    /// This is the registry-scoped reconnect seam: it returns, in sequence
    /// order, the undrained fills a member missed. A fill is replayed exactly
    /// when `seq > last_private_seq`. The buffer is drained by
    /// [`Self::drain_combo_private_events`]; replay therefore cannot recover
    /// fills that were already delivered. Full gateway log replay is outside
    /// this scope.
    #[must_use]
    pub fn replay_combo_private_after(&self, last_private_seq: u64) -> Vec<ComboPrivateFill> {
        self.combo_private_events
            .iter()
            .copied()
            .filter(|fill| fill.seq > last_private_seq)
            .collect()
    }

    /// Captures registry-backed combo state for restart or a new process.
    ///
    /// The captured snapshot is self-contained: restoring it rebuilds ordinary
    /// Contract books and positions, the static pairs, base Combo Order books,
    /// leg margin reservations, and the combo private sequence.
    #[must_use]
    pub fn snapshot(&self) -> RegistrySnapshot {
        let mut instruments: Vec<(u32, crate::trading::core::kernel::CoreSnapshot)> = self
            .instruments
            .iter()
            .map(|(instrument_id, core)| (*instrument_id as u32, core.snapshot()))
            .collect();
        instruments.sort_by_key(|(instrument_id, _)| *instrument_id);
        let mut pairs: Vec<ComboPair> = self.pairs.values().copied().collect();
        pairs.sort_by_key(|pair| pair.pair_id);
        let mut prior_settlements: Vec<(u32, u64)> = self
            .prior_settlements
            .iter()
            .map(|(instrument_id, price_ticks)| (*instrument_id as u32, *price_ticks))
            .collect();
        prior_settlements.sort_by_key(|(instrument_id, _)| *instrument_id);
        let mut combo_books: Vec<ComboBookSnapshot> =
            self.combo_books.values().map(ComboBook::snapshot).collect();
        combo_books.sort_by_key(|book| book.pair_id);
        RegistrySnapshot {
            instruments,
            pairs,
            prior_settlements,
            combo_books,
            margin: self.margin.snapshot(),
            combo_private_events: self.combo_private_events.clone(),
            next_combo_order_id: self.next_combo_order_id,
            next_combo_private_seq: self.next_combo_private_seq,
            next_public_seq: self.next_public_seq,
        }
    }

    /// Replaces all registry state with a captured snapshot.
    ///
    /// Restore is fail-closed and all-or-none. Every instrument, pair, book,
    /// reservation, and sequence is validated before any field changes, so an
    /// invalid or partial snapshot leaves the running registry untouched.
    ///
    /// # Errors
    /// Returns [`RegistrySnapshotError`] when the snapshot is internally
    /// inconsistent, references state it does not contain, or would make
    /// matching nondeterministic.
    pub fn restore_snapshot(
        &mut self,
        snapshot: RegistrySnapshot,
    ) -> Result<(), RegistrySnapshotError> {
        let RegistrySnapshot {
            instruments: instrument_snapshots,
            pairs: pair_definitions,
            prior_settlements: settlement_entries,
            combo_books: book_snapshots,
            margin: margin_snapshot,
            combo_private_events,
            next_combo_order_id,
            next_combo_private_seq,
            next_public_seq,
        } = snapshot;

        let mut instruments: U64MixMap<TradingCore> = U64MixMap::default();
        for (instrument_id, core_snapshot) in instrument_snapshots {
            if instruments.contains_key(&u64::from(instrument_id)) {
                return Err(RegistrySnapshotError::DuplicateInstrument { instrument_id });
            }
            let mut core = TradingCore::new(core_snapshot.spec.clone(), core_snapshot.symbol);
            core.restore_snapshot(core_snapshot).map_err(|reason| {
                RegistrySnapshotError::CoreRestore {
                    instrument_id,
                    reason,
                }
            })?;
            instruments.insert(u64::from(instrument_id), core);
        }

        let mut prior_settlements: U64MixMap<u64> = U64MixMap::default();
        for (instrument_id, price_ticks) in settlement_entries {
            if !instruments.contains_key(&u64::from(instrument_id)) {
                return Err(RegistrySnapshotError::UnknownPriorSettlementInstrument {
                    instrument_id,
                });
            }
            if prior_settlements
                .insert(u64::from(instrument_id), price_ticks)
                .is_some()
            {
                return Err(RegistrySnapshotError::DuplicatePriorSettlement { instrument_id });
            }
        }

        let mut pairs: U64MixMap<ComboPair> = U64MixMap::default();
        for pair in pair_definitions {
            if pairs.contains_key(&u64::from(pair.pair_id)) {
                return Err(RegistrySnapshotError::DuplicatePair {
                    pair_id: pair.pair_id,
                });
            }
            validate_pair(&instruments, &pair).map_err(|error| {
                RegistrySnapshotError::InvalidPair {
                    pair_id: pair.pair_id,
                    error,
                }
            })?;
            pairs.insert(u64::from(pair.pair_id), pair);
        }

        let mut resting_orders: Vec<ComboOrder> = book_snapshots
            .iter()
            .flat_map(|book| book.bids.iter().chain(book.asks.iter()).copied())
            .collect();
        let mut combo_books: U64MixMap<ComboBook> = U64MixMap::default();
        for book_snapshot in book_snapshots {
            let pair_id = book_snapshot.pair_id;
            if !pairs.contains_key(&u64::from(pair_id)) {
                return Err(RegistrySnapshotError::UnknownComboBook { pair_id });
            }
            if combo_books.contains_key(&u64::from(pair_id)) {
                return Err(RegistrySnapshotError::DuplicateComboBook { pair_id });
            }
            let mut book = ComboBook::new(pair_id);
            book.restore_snapshot(book_snapshot)
                .map_err(|error| RegistrySnapshotError::ComboBookRestore { pair_id, error })?;
            combo_books.insert(u64::from(pair_id), book);
        }
        for pair in pairs.values() {
            if !combo_books.contains_key(&u64::from(pair.pair_id)) {
                return Err(RegistrySnapshotError::MissingComboBook {
                    pair_id: pair.pair_id,
                });
            }
        }

        resting_orders.sort_by_key(|order| order.order_id);
        let max_assigned_order_id = resting_orders
            .iter()
            .map(|order| order.order_id)
            .chain(combo_private_events.iter().map(|fill| fill.order_id))
            .max()
            .unwrap_or(0);
        if next_combo_order_id == 0 || next_combo_order_id <= max_assigned_order_id {
            return Err(RegistrySnapshotError::InvalidNextComboOrderId {
                next_combo_order_id,
                max_assigned_order_id,
            });
        }
        let mut resting_order_ids: U64MixMap<u32> = U64MixMap::default();
        for order in &resting_orders {
            if resting_order_ids
                .insert(order.order_id, order.pair_id)
                .is_some()
            {
                return Err(RegistrySnapshotError::DuplicateComboOrderId {
                    order_id: order.order_id,
                });
            }
        }

        let mut previous_seq = 0;
        for fill in &combo_private_events {
            if fill.seq <= previous_seq {
                return Err(RegistrySnapshotError::InvalidComboPrivateSequence { seq: fill.seq });
            }
            previous_seq = fill.seq;
            if !pairs.contains_key(&u64::from(fill.pair_id)) {
                return Err(RegistrySnapshotError::UnknownComboPrivatePair {
                    seq: fill.seq,
                    pair_id: fill.pair_id,
                });
            }
        }
        if next_combo_private_seq == 0 || next_combo_private_seq <= previous_seq {
            return Err(RegistrySnapshotError::InvalidNextComboPrivateSeq {
                next_combo_private_seq,
                max_buffered_seq: previous_seq,
            });
        }
        if next_public_seq == 0 {
            return Err(RegistrySnapshotError::InvalidNextPublicSeq { next_public_seq });
        }

        for margin in &margin_snapshot.leg_margins {
            if !instruments.contains_key(&u64::from(margin.instrument_id)) {
                return Err(RegistrySnapshotError::UnknownLegMarginInstrument {
                    account_id: margin.account_id,
                    instrument_id: margin.instrument_id,
                });
            }
        }
        let reservation_order_ids: Vec<u64> = margin_snapshot
            .reservations
            .iter()
            .map(|freeze| freeze.order_id)
            .collect();
        let mut margin = ComboMarginLedger::default();
        margin
            .restore_snapshot(margin_snapshot, &pairs, &prior_settlements)
            .map_err(RegistrySnapshotError::Margin)?;

        for order in &resting_orders {
            let Some(freeze) = margin.freeze_for(order.order_id) else {
                return Err(RegistrySnapshotError::MissingMarginReservation {
                    order_id: order.order_id,
                });
            };
            if freeze.pair_id != order.pair_id
                || freeze.account_id != order.owner.user_id
                || freeze.remaining != order.remaining
            {
                return Err(RegistrySnapshotError::MarginReservationMismatch {
                    order_id: order.order_id,
                });
            }
        }
        for order_id in reservation_order_ids {
            if !resting_order_ids.contains_key(&order_id) {
                return Err(RegistrySnapshotError::UnknownMarginReservation { order_id });
            }
        }

        self.instruments = instruments;
        self.pairs = pairs;
        self.combo_books = combo_books;
        self.prior_settlements = prior_settlements;
        self.margin = margin;
        self.combo_private_events = combo_private_events;
        self.combo_public_events.clear();
        self.next_combo_order_id = next_combo_order_id;
        self.next_combo_private_seq = next_combo_private_seq;
        self.next_public_seq = next_public_seq;
        Ok(())
    }

    /// Atomically consumes matching resting liquidity from two leg books.
    ///
    /// This is the core-owned seam a combo matcher uses instead of touching an
    /// ordinary Contract book. `leg1_plan` and `leg2_plan` must come from
    /// [`TradingCore::plan_leg_liquidity`] on the named instruments. Both plans
    /// are validated before either book mutates, so a short, stale, mismatched,
    /// or same-sided plan leaves both books, positions, and event buffers
    /// unchanged. A successful call consumes the same quantity from both legs,
    /// updates both resting owners' positions and exposure, and emits one
    /// ordinary public leg trade per resting order.
    ///
    /// A Combo Pair need not be registered: the seam only coordinates two
    /// ordinary Contract books.
    ///
    /// # Errors
    /// Returns [`ComboLegError`] when an instrument is unknown, both legs name
    /// the same instrument, the plans select different quantities or taker
    /// sides, or either plan is stale or unusable. Nothing mutates on error.
    pub fn consume_combo_legs(
        &mut self,
        leg1_instrument_id: u32,
        leg2_instrument_id: u32,
        taker_order_id: u64,
        leg1_plan: &LegLiquidityPlan,
        leg2_plan: &LegLiquidityPlan,
    ) -> Result<ComboLegExecution, ComboLegError> {
        let leg1_key = u64::from(leg1_instrument_id);
        let leg2_key = u64::from(leg2_instrument_id);
        if leg1_key == leg2_key {
            return Err(ComboLegError::SameInstrument {
                instrument_id: leg1_instrument_id,
            });
        }
        if leg1_plan.quantity != leg2_plan.quantity {
            return Err(ComboLegError::QuantityMismatch {
                leg1_quantity: leg1_plan.quantity,
                leg2_quantity: leg2_plan.quantity,
            });
        }
        if leg1_plan.taker_side == leg2_plan.taker_side {
            return Err(ComboLegError::TakerSidesNotOpposite {
                leg1_side: leg1_plan.taker_side,
                leg2_side: leg2_plan.taker_side,
            });
        }
        if !self.instruments.contains_key(&leg1_key) {
            return Err(ComboLegError::UnknownInstrument {
                leg: ComboLeg::First,
                instrument_id: leg1_instrument_id,
            });
        }
        if !self.instruments.contains_key(&leg2_key) {
            return Err(ComboLegError::UnknownInstrument {
                leg: ComboLeg::Second,
                instrument_id: leg2_instrument_id,
            });
        }

        // Validate both books before mutating either one. Validation is
        // read-only, so this is the all-or-none barrier.
        self.instruments
            .get(&leg1_key)
            .expect("first combo leg instrument was checked")
            .validate_leg_liquidity(leg1_plan)
            .map_err(|error| ComboLegError::Plan {
                leg: ComboLeg::First,
                error,
            })?;
        self.instruments
            .get(&leg2_key)
            .expect("second combo leg instrument was checked")
            .validate_leg_liquidity(leg2_plan)
            .map_err(|error| ComboLegError::Plan {
                leg: ComboLeg::Second,
                error,
            })?;

        // Validation guarantees that both commits are infallible.
        let leg1 = self
            .instruments
            .get_mut(&leg1_key)
            .expect("first combo leg instrument was checked")
            .commit_leg_liquidity(
                leg1_plan,
                LegExecutionContext::new(leg1_instrument_id, taker_order_id),
            );
        let leg2 = self
            .instruments
            .get_mut(&leg2_key)
            .expect("second combo leg instrument was checked")
            .commit_leg_liquidity(
                leg2_plan,
                LegExecutionContext::new(leg2_instrument_id, taker_order_id),
            );
        Ok(ComboLegExecution {
            quantity: leg1_plan.quantity,
            taker_order_id,
            leg1,
            leg2,
        })
    }

    /// Executes one Combo Order command against its pair book.
    ///
    /// This is the command translation and direct-matching seam. It accepts
    /// only M1 base combo semantics: spread limit GFD new orders and cancels.
    /// Market, FOK/FAK, stop-trigger, zero-quantity, unknown-pair, and
    /// duplicate-cancel requests fail explicitly without mutating a book. A new
    /// order also fails, before any book mutates, when the account's configured
    /// balance cannot reserve the prior-settlement estimate on both legs.
    /// A new order matches opposite Combo Orders in price-time priority, then
    /// consumes standing leg liquidity as implied-in while its spread limit is
    /// satisfied, and finally rests any residual. Implied candidates are never
    /// inserted into the public combo book.
    ///
    /// # Errors
    /// Returns [`ComboCommandError`] when the command names an unknown pair,
    /// uses unsupported order semantics, or cancels a non-resting order.
    pub fn execute_combo(
        &mut self,
        command: Command,
    ) -> Result<Vec<ComboEvent>, ComboCommandError> {
        match command.command_type {
            CommandType::NewOrder => self.rest_combo_order(command),
            CommandType::Cancel => self.cancel_combo_order(command),
            command_type => Err(ComboCommandError::UnsupportedCommandType { command_type }),
        }
    }

    fn rest_combo_order(&mut self, command: Command) -> Result<Vec<ComboEvent>, ComboCommandError> {
        let phase = self
            .instruments
            .values()
            .next()
            .map(TradingCore::phase)
            .unwrap_or(crate::trading::core::public_events::TradingPhase::Continuous);
        if phase != crate::trading::core::public_events::TradingPhase::Continuous {
            return Err(ComboCommandError::OutsideContinuousTrading { phase });
        }
        let pair_id = command.combo_id;
        let Some(pair) = self.pairs.get(&u64::from(pair_id)).copied() else {
            return Err(ComboCommandError::UnknownPair { pair_id });
        };
        validate_new_combo_order(&command)?;

        let order_id = self.next_combo_order_id;
        let owner = ComboOrderOwner::new(
            u64::from(command.user_id),
            command.session_id,
            command.client_order_id,
        );
        let mut order = ComboOrder::new(
            order_id,
            pair_id,
            owner,
            command.side,
            command.price_ticks,
            command.quantity,
        );
        self.reserve_combo_margin(&pair, &order)?;

        // Direct combo matching always runs first. Only a residual taker may
        // consume leg liquidity through the implied-in path.
        let direct_leg_prices = self.direct_leg_reference(&pair, order.side);
        let mut events = self
            .combo_books
            .get_mut(&u64::from(pair_id))
            .ok_or(ComboCommandError::UnknownPair { pair_id })?
            .match_taker(&mut order)?;
        self.settle_direct_fills(&pair, &events, direct_leg_prices, command.trading_ts_ns);
        if order.remaining > 0 {
            events.extend(self.match_implied_in(&pair, &mut order, command.trading_ts_ns));
        }
        if order.remaining > 0 {
            let rested = self
                .combo_books
                .get_mut(&u64::from(pair_id))
                .ok_or(ComboCommandError::UnknownPair { pair_id })?
                .rest(order)?;
            events.push(ComboEvent::Rested { order: rested });
        }
        self.next_combo_order_id = self.next_combo_order_id.saturating_add(1);
        Ok(events)
    }

    /// Fills the residual of one Combo Order against standing leg liquidity.
    ///
    /// The M1 planner repeatedly inspects the best crossing price on each leg
    /// and consumes the smaller of the combo residual and both best-level
    /// available quantities while the realized spread satisfies the combo
    /// limit. Each round builds both leg plans before either book mutates, so a
    /// shortfall on either leg stops implied-in with no mutation. Implied
    /// candidates are never stored in the combo book.
    fn match_implied_in(
        &mut self,
        pair: &ComboPair,
        order: &mut ComboOrder,
        trading_ts_ns: u64,
    ) -> Vec<ComboEvent> {
        let mut events = Vec::new();
        while order.remaining > 0 {
            let (leg1_side, leg2_side) = match order.side {
                Side::Buy => (Side::Buy, Side::Sell),
                Side::Sell => (Side::Sell, Side::Buy),
            };
            let Some(leg1_price) = self.best_leg_price(pair.leg1_instrument_id, leg1_side) else {
                break;
            };
            let Some(leg2_price) = self.best_leg_price(pair.leg2_instrument_id, leg2_side) else {
                break;
            };
            let spread_price = leg_spread_price(leg1_price, leg2_price);
            if !spread_within_limit(order.side, order.spread_price, spread_price) {
                break;
            }
            let leg1_available = self.leg_available_at(
                pair.leg1_instrument_id,
                leg1_side,
                leg1_price,
                order.remaining,
            );
            let leg2_available = self.leg_available_at(
                pair.leg2_instrument_id,
                leg2_side,
                leg2_price,
                order.remaining,
            );
            let quantity = order.remaining.min(leg1_available).min(leg2_available);
            if quantity == 0 {
                break;
            }
            let Some(leg1_plan) =
                self.plan_leg_at(pair.leg1_instrument_id, leg1_side, leg1_price, quantity)
            else {
                break;
            };
            let Some(leg2_plan) =
                self.plan_leg_at(pair.leg2_instrument_id, leg2_side, leg2_price, quantity)
            else {
                break;
            };
            if self
                .consume_combo_legs(
                    pair.leg1_instrument_id,
                    pair.leg2_instrument_id,
                    order.order_id,
                    &leg1_plan,
                    &leg2_plan,
                )
                .is_err()
            {
                // The seam validated both plans before mutating either book.
                break;
            }

            order.remaining -= quantity;
            if let Some(book) = self.combo_books.get_mut(&u64::from(pair.pair_id)) {
                book.record_trade_price(spread_price);
            }
            let fill = ImpliedComboFill {
                pair_id: pair.pair_id,
                order_id: order.order_id,
                owner: order.owner,
                side: order.side,
                spread_price,
                quantity,
                leg1_price_ticks: leg1_price,
                leg2_price_ticks: leg2_price,
            };
            self.margin.release_on_fill(fill.order_id, fill.quantity);
            self.apply_combo_owner_fill(
                pair,
                fill.owner.user_id,
                fill.side,
                fill.quantity,
                (fill.leg1_price_ticks, fill.leg2_price_ticks),
            );
            let terms = ComboTradeTerms {
                pair_id: fill.pair_id,
                order_id: fill.order_id,
                side: Some(fill.side),
                buy_order_id: 0,
                sell_order_id: 0,
                quantity: fill.quantity,
                spread_price: fill.spread_price,
                leg1_price_ticks: fill.leg1_price_ticks,
                leg2_price_ticks: fill.leg2_price_ticks,
            };
            self.push_combo_private_fill(
                fill.order_id,
                fill.owner,
                fill.side,
                ComboFillTerms {
                    pair_id: fill.pair_id,
                    quantity: fill.quantity,
                    spread_price: fill.spread_price,
                    leg1_price_ticks: fill.leg1_price_ticks,
                    leg2_price_ticks: fill.leg2_price_ticks,
                },
            );
            self.push_combo_public_trade(terms, trading_ts_ns);
            events.push(ComboEvent::ImpliedFilled { fill });
        }
        events
    }

    /// Re-evaluates every Combo Order resting on a pair that names one of the
    /// command's legs as a taker against the current leg books.
    ///
    /// Ordinary leg matching has already run, so a base leg order keeps
    /// priority over an implied execution at the same price: only liquidity
    /// still available after the leg book settled can back an implied fill.
    /// Resting orders are visited in combined price-time priority, and each
    /// fill is applied to its book exactly once.
    fn revalue_resting_combos(&mut self, instrument_id: u32, trading_ts_ns: u64) {
        let mut pairs: Vec<ComboPair> = self
            .pairs
            .values()
            .filter(|pair| {
                pair.leg1_instrument_id == instrument_id || pair.leg2_instrument_id == instrument_id
            })
            .copied()
            .collect();
        pairs.sort_by_key(|pair| (pair.kind.priority(), pair.pair_id));
        for pair in pairs {
            self.match_resting_combos(&pair, trading_ts_ns);
        }
    }

    /// Executes one pair's resting Combo Orders against standing leg liquidity.
    ///
    /// Candidates are read before matching so the book is not borrowed while
    /// the shared leg books mutate. Each candidate is re-read from the book
    /// immediately before matching, so an order already consumed by an earlier
    /// candidate cannot fill twice.
    fn match_resting_combos(&mut self, pair: &ComboPair, trading_ts_ns: u64) {
        let key = u64::from(pair.pair_id);
        let Some(book) = self.combo_books.get(&key) else {
            return;
        };
        let candidates = book.orders_by_priority();
        for candidate in candidates {
            let Some(current) = self
                .combo_books
                .get(&key)
                .and_then(|book| book.order(candidate.order_id))
                .copied()
            else {
                continue;
            };
            let before = current.remaining;
            let mut order = current;
            self.match_implied_in(pair, &mut order, trading_ts_ns);
            let filled = before - order.remaining;
            if filled == 0 {
                continue;
            }
            if let Some(book) = self.combo_books.get_mut(&key) {
                let _filled_order = book
                    .apply_implied_fill(order.order_id, filled)
                    .expect("a matched resting combo order must still be resting");
            }
        }
    }

    /// Applies both leg sides of one Combo Fill to the owner's leg positions
    /// and records the actual trade-price margin on each leg.
    ///
    /// The Combo Pair has no position of its own. A buy spread opens long leg1
    /// and short leg2; a sell spread opens short leg1 and long leg2. Each leg
    /// Contract receives the owner's side exactly once.
    fn apply_combo_owner_fill(
        &mut self,
        pair: &ComboPair,
        account_id: u64,
        side: Side,
        quantity: u64,
        leg_prices: (u64, u64),
    ) {
        let (leg1_side, leg2_side) = leg_sides(side);
        self.instruments
            .get_mut(&u64::from(pair.leg1_instrument_id))
            .expect("a registered Combo Pair names a registered first leg")
            .apply_combo_fill_position(account_id, leg1_side, quantity);
        self.instruments
            .get_mut(&u64::from(pair.leg2_instrument_id))
            .expect("a registered Combo Pair names a registered second leg")
            .apply_combo_fill_position(account_id, leg2_side, quantity);
        self.margin.record_actual(
            account_id,
            pair.leg1_instrument_id,
            leg1_side,
            leg_prices.0,
            LegMarginRate {
                multiplier: pair.margin.multiplier,
                rate_bps: pair.margin.leg1_rate_bps,
            },
            quantity,
        );
        self.margin.record_actual(
            account_id,
            pair.leg2_instrument_id,
            leg2_side,
            leg_prices.1,
            LegMarginRate {
                multiplier: pair.margin.multiplier,
                rate_bps: pair.margin.leg2_rate_bps,
            },
            quantity,
        );
    }

    /// Reserves prior-settlement margin for a newly accepted Combo Order.
    ///
    /// Both legs must reserve from the account's configured balance before any
    /// estimate is recorded. This runs before any combo or leg book mutation,
    /// so a rejected entry consumes no order id and leaves matching state
    /// untouched.
    fn reserve_combo_margin(
        &mut self,
        pair: &ComboPair,
        order: &ComboOrder,
    ) -> Result<(), ComboMarginError> {
        let leg1_prior_settlement = self.prior_settlement(pair.leg1_instrument_id).ok_or(
            ComboMarginError::MissingPriorSettlement {
                instrument_id: pair.leg1_instrument_id,
            },
        )?;
        let leg2_prior_settlement = self.prior_settlement(pair.leg2_instrument_id).ok_or(
            ComboMarginError::MissingPriorSettlement {
                instrument_id: pair.leg2_instrument_id,
            },
        )?;
        let balance = self.margin_balance(order.owner.user_id);
        if balance.is_none() && self.require_explicit_margin {
            return Err(ComboMarginError::MissingMarginBalance {
                account_id: order.owner.user_id,
            });
        }
        self.margin.try_freeze(
            pair,
            ComboEntryMargin {
                order_id: order.order_id,
                account_id: order.owner.user_id,
                quantity: order.quantity,
                leg1_prior_settlement,
                leg2_prior_settlement,
                balance,
            },
        )?;
        Ok(())
    }

    /// Settles the effects of every direct Combo Fill in `events`.
    fn settle_direct_fills(
        &mut self,
        pair: &ComboPair,
        events: &[ComboEvent],
        leg_prices: (u64, u64),
        trading_ts_ns: u64,
    ) {
        for event in events {
            let ComboEvent::Filled { fill } = event else {
                continue;
            };
            self.settle_direct_fill(pair, fill, leg_prices, trading_ts_ns);
        }
    }

    fn settle_direct_fill(
        &mut self,
        pair: &ComboPair,
        fill: &ComboFill,
        leg_prices: (u64, u64),
        trading_ts_ns: u64,
    ) {
        self.margin
            .release_on_fill(fill.buy_order_id, fill.quantity);
        self.margin
            .release_on_fill(fill.sell_order_id, fill.quantity);
        self.apply_combo_owner_fill(
            pair,
            fill.buy_owner.user_id,
            Side::Buy,
            fill.quantity,
            leg_prices,
        );
        self.apply_combo_owner_fill(
            pair,
            fill.sell_owner.user_id,
            Side::Sell,
            fill.quantity,
            leg_prices,
        );
        let terms = ComboFillTerms {
            pair_id: fill.pair_id,
            quantity: fill.quantity,
            spread_price: fill.spread_price,
            leg1_price_ticks: leg_prices.0,
            leg2_price_ticks: leg_prices.1,
        };
        self.push_combo_private_fill(fill.buy_order_id, fill.buy_owner, Side::Buy, terms);
        self.push_combo_private_fill(fill.sell_order_id, fill.sell_owner, Side::Sell, terms);
        let trade = ComboTradeTerms {
            pair_id: fill.pair_id,
            order_id: 0,
            side: None,
            buy_order_id: fill.buy_order_id,
            sell_order_id: fill.sell_order_id,
            quantity: fill.quantity,
            spread_price: fill.spread_price,
            leg1_price_ticks: leg_prices.0,
            leg2_price_ticks: leg_prices.1,
        };
        self.push_combo_public_trade(trade, trading_ts_ns);
    }

    /// Returns the leg prices a direct Combo Fill reports.
    ///
    /// Direct matching consumes no leg liquidity, so there are no leg execution
    /// prices. The fill uses the best crossing reference on each leg and falls
    /// back to prior settlement, then zero, when a leg book has no quote. The
    /// spread remains the direct execution spread.
    fn direct_leg_reference(&self, pair: &ComboPair, taker_side: Side) -> (u64, u64) {
        let (leg1_side, leg2_side) = leg_sides(taker_side);
        let leg1 = self
            .best_leg_price(pair.leg1_instrument_id, leg1_side)
            .or_else(|| self.prior_settlement(pair.leg1_instrument_id))
            .unwrap_or(0);
        let leg2 = self
            .best_leg_price(pair.leg2_instrument_id, leg2_side)
            .or_else(|| self.prior_settlement(pair.leg2_instrument_id))
            .unwrap_or(0);
        (leg1, leg2)
    }

    fn push_combo_private_fill(
        &mut self,
        order_id: u64,
        owner: ComboOrderOwner,
        side: Side,
        terms: ComboFillTerms,
    ) {
        let seq = self.next_combo_private_seq;
        self.next_combo_private_seq = self.next_combo_private_seq.saturating_add(1);
        self.combo_private_events.push(ComboPrivateFill {
            seq,
            pair_id: terms.pair_id,
            order_id,
            client_order_id: owner.client_order_id,
            user_id: owner.user_id,
            session_id: owner.session_id,
            side,
            quantity: terms.quantity,
            spread_price: terms.spread_price,
            leg1_price_ticks: terms.leg1_price_ticks,
            leg2_price_ticks: terms.leg2_price_ticks,
        });
    }

    /// Publishes one public Combo trade price statistic.
    ///
    /// The event is the only public record of a combo execution. Implied
    /// orders themselves never receive a public identity, and a Combo Pair has
    /// no order book depth event. The registry stamps the one venue public
    /// sequence when the event is drained, together with every ordinary
    /// Contract event.
    fn push_combo_public_trade(&mut self, terms: ComboTradeTerms, trading_ts_ns: u64) {
        self.combo_public_events.push(PublicEvent {
            seq: 0,
            event_type: PublicEventType::ComboTrade,
            trading_ts_ns,
            phase: 0,
            instrument_id: 0,
            pair_id: terms.pair_id,
            order_id: terms.order_id,
            trade_id: 0,
            buy_order_id: terms.buy_order_id,
            sell_order_id: terms.sell_order_id,
            side: terms.side,
            price_ticks: 0,
            quantity: terms.quantity,
            volume: 0,
            spread_price: terms.spread_price,
            leg1_price_ticks: terms.leg1_price_ticks,
            leg2_price_ticks: terms.leg2_price_ticks,
        });
    }

    /// Returns the best price a combo taker would pay or receive on one leg.
    fn best_leg_price(&self, instrument_id: u32, taker_side: Side) -> Option<u64> {
        let core = self.instruments.get(&u64::from(instrument_id))?;
        match taker_side {
            Side::Buy => core.best_ask(),
            Side::Sell => core.best_bid(),
        }
    }

    /// Returns crossing quantity available at one leg price, capped by `wanted`.
    fn leg_available_at(
        &self,
        instrument_id: u32,
        taker_side: Side,
        price_ticks: u64,
        wanted: u64,
    ) -> u64 {
        let Some(core) = self.instruments.get(&u64::from(instrument_id)) else {
            return 0;
        };
        match core.plan_leg_liquidity(taker_side, price_ticks, wanted) {
            Ok(plan) => plan.quantity,
            Err(LegLiquidityError::InsufficientLiquidity { available, .. }) => available,
            Err(_) => 0,
        }
    }

    /// Builds one leg plan at an exact best price, if the book still covers it.
    fn plan_leg_at(
        &self,
        instrument_id: u32,
        taker_side: Side,
        price_ticks: u64,
        quantity: u64,
    ) -> Option<LegLiquidityPlan> {
        self.instruments
            .get(&u64::from(instrument_id))?
            .plan_leg_liquidity(taker_side, price_ticks, quantity)
            .ok()
    }

    fn cancel_combo_order(
        &mut self,
        command: Command,
    ) -> Result<Vec<ComboEvent>, ComboCommandError> {
        let pair_id = command.combo_id;
        if command.cancel_order_id == 0 {
            return Err(ComboCommandError::MissingCancelOrderId);
        }
        let order = {
            let book = self
                .combo_books
                .get_mut(&u64::from(pair_id))
                .ok_or(ComboCommandError::UnknownPair { pair_id })?;
            book.cancel_for(u64::from(command.user_id), command.cancel_order_id)?
        };
        self.margin.release_all(order.order_id);
        Ok(vec![ComboEvent::Canceled { order }])
    }

    /// Returns the number of registered instruments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.instruments.len()
    }

    /// Returns true when no instrument is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instruments.is_empty()
    }

    /// Returns true when `instrument_id` resolves to a book.
    #[must_use]
    pub fn contains(&self, instrument_id: u32) -> bool {
        self.instruments.contains_key(&u64::from(instrument_id))
    }

    /// Looks up the core for `instrument_id`.
    #[must_use]
    pub fn core(&self, instrument_id: u32) -> Option<&TradingCore> {
        self.instruments.get(&u64::from(instrument_id))
    }

    /// Looks up the core for `instrument_id` for state inspection or restore.
    pub fn core_mut(&mut self, instrument_id: u32) -> Option<&mut TradingCore> {
        self.instruments.get_mut(&u64::from(instrument_id))
    }

    /// Looks up the contract specification for `instrument_id`.
    #[must_use]
    pub fn spec(&self, instrument_id: u32) -> Option<&ContractSpec> {
        self.core(instrument_id).map(TradingCore::spec)
    }

    /// Executes a command through the default plugin chain.
    ///
    /// # Errors
    /// Returns [`RegistryError::UnknownInstrument`] when an order or cancel
    /// names an unregistered instrument.
    pub fn execute(&mut self, command: Command) -> Result<Vec<CoreEvent>, RegistryError> {
        self.execute_with_plugins(command, &NoPlugins)
    }

    /// Executes a command through a caller-supplied pre-trade plugin chain.
    ///
    /// # Errors
    /// Returns [`RegistryError::UnknownInstrument`] when an order or cancel
    /// names an unregistered instrument.
    pub fn execute_with_plugins<P: PreTradePlugin>(
        &mut self,
        command: Command,
        plugin: &P,
    ) -> Result<Vec<CoreEvent>, RegistryError> {
        let mut events = Vec::new();
        self.execute_with_plugins_into(command, plugin, &mut events)?;
        Ok(events)
    }

    /// Executes a command through the default plugin chain, discarding its
    /// projected `CoreEvent`s.
    ///
    /// Private and public events are still recorded in the owning Contract
    /// cores, so the caller drains them exactly as with the projecting variant.
    ///
    /// # Errors
    /// Returns [`RegistryError::UnknownInstrument`] when an order or cancel
    /// names an unregistered instrument.
    pub fn execute_with_plugins_discard<P: PreTradePlugin>(
        &mut self,
        command: Command,
        plugin: &P,
    ) -> Result<(), RegistryError> {
        self.execute_with_plugins_into(command, plugin, &mut DiscardEvents)
    }

    /// Routes a command and appends its events into `out`.
    ///
    /// New orders and cancels require a registered instrument. Timing and
    /// control commands update every core because they describe venue state.
    ///
    /// # Errors
    /// Returns [`RegistryError::UnknownInstrument`] when an order or cancel
    /// names an unregistered instrument. No book mutates in that case.
    pub fn execute_with_plugins_into<P: PreTradePlugin, E: Extend<CoreEvent>>(
        &mut self,
        command: Command,
        plugin: &P,
        out: &mut E,
    ) -> Result<(), RegistryError> {
        match command.command_type {
            CommandType::NewOrder | CommandType::Cancel => {
                let core = self
                    .instruments
                    .get_mut(&u64::from(command.instrument_id))
                    .ok_or(RegistryError::UnknownInstrument {
                        instrument_id: command.instrument_id,
                    })?;
                core.execute_with_plugins_into(command, plugin, out);
                if command.command_type == CommandType::NewOrder {
                    // A new leg order can create the second leg side a resting
                    // Combo Order needs. Ordinary matching has already run, so
                    // base leg orders keep priority over implied execution.
                    // A cancel can only remove liquidity, so it cannot make a
                    // resting combo executable.
                    self.revalue_resting_combos(command.instrument_id, command.trading_ts_ns);
                }
            }
            CommandType::TimeTick | CommandType::Control => {
                for core in self.instruments.values_mut() {
                    core.execute_with_plugins_into(command, plugin, out);
                }
            }
        }
        Ok(())
    }
}

/// Price, quantity, and leg-price terms of one public Combo trade statistic.
#[derive(Clone, Copy)]
struct ComboTradeTerms {
    pair_id: u32,
    order_id: u64,
    side: Option<Side>,
    buy_order_id: u64,
    sell_order_id: u64,
    quantity: u64,
    spread_price: i64,
    leg1_price_ticks: u64,
    leg2_price_ticks: u64,
}

/// Price, quantity, and leg-price terms shared by the private fills of one
/// atomic Combo Fill.
#[derive(Clone, Copy)]
struct ComboFillTerms {
    pair_id: u32,
    quantity: u64,
    spread_price: i64,
    leg1_price_ticks: u64,
    leg2_price_ticks: u64,
}

/// Returns the leg sides a Combo Order owner takes for one spread direction.
///
/// A buy spread buys leg1 and sells leg2; a sell spread does the opposite.
fn leg_sides(combo_side: Side) -> (Side, Side) {
    match combo_side {
        Side::Buy => (Side::Buy, Side::Sell),
        Side::Sell => (Side::Sell, Side::Buy),
    }
}

const MAX_MARGIN_RATE_BPS: u64 = 10_000;

/// Returns `leg1 - leg2` as a signed spread price, saturating at the `i64` ends.
fn leg_spread_price(leg1_price: u64, leg2_price: u64) -> i64 {
    let leg1 = i64::try_from(leg1_price).unwrap_or(i64::MAX);
    let leg2 = i64::try_from(leg2_price).unwrap_or(i64::MIN);
    leg1.saturating_sub(leg2)
}

/// Returns true when a realized spread satisfies a combo taker's limit.
fn spread_within_limit(taker_side: Side, limit: i64, realized: i64) -> bool {
    match taker_side {
        Side::Buy => realized <= limit,
        Side::Sell => realized >= limit,
    }
}

struct DiscardEvents;

impl Extend<CoreEvent> for DiscardEvents {
    fn extend<T: IntoIterator<Item = CoreEvent>>(&mut self, iter: T) {
        for _ in iter {}
    }
}

impl Default for InstrumentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_pair(
    instruments: &U64MixMap<TradingCore>,
    pair: &ComboPair,
) -> Result<(), ComboPairError> {
    if pair.pair_id == 0 {
        return Err(ComboPairError::InvalidPairId { pair_id: 0 });
    }
    if pair.leg1_instrument_id == pair.leg2_instrument_id {
        return Err(ComboPairError::IdenticalLegs {
            pair_id: pair.pair_id,
            instrument_id: pair.leg1_instrument_id,
        });
    }
    for (leg, instrument_id) in [
        (ComboLeg::First, pair.leg1_instrument_id),
        (ComboLeg::Second, pair.leg2_instrument_id),
    ] {
        if !instruments.contains_key(&u64::from(instrument_id)) {
            return Err(ComboPairError::UnknownLegInstrument {
                pair_id: pair.pair_id,
                leg,
                instrument_id,
            });
        }
    }
    let leg1_tick = instruments
        .get(&u64::from(pair.leg1_instrument_id))
        .map(|core| core.spec().tick_size);
    let leg2_tick = instruments
        .get(&u64::from(pair.leg2_instrument_id))
        .map(|core| core.spec().tick_size);
    if let (Some(leg1_tick), Some(leg2_tick)) = (leg1_tick, leg2_tick)
        && leg1_tick != leg2_tick
    {
        return Err(ComboPairError::TickMismatch {
            pair_id: pair.pair_id,
            leg1_tick,
            leg2_tick,
        });
    }
    if pair.spread_tick == 0 {
        return Err(ComboPairError::InvalidSpreadTick {
            pair_id: pair.pair_id,
            spread_tick: pair.spread_tick,
        });
    }
    let margin = pair.margin;
    let reason = if margin.multiplier == 0 {
        "multiplier must be positive"
    } else if margin.leg1_rate_bps > MAX_MARGIN_RATE_BPS {
        "first-leg margin rate must not exceed 10000 bps"
    } else if margin.leg2_rate_bps > MAX_MARGIN_RATE_BPS {
        "second-leg margin rate must not exceed 10000 bps"
    } else {
        return Ok(());
    };
    Err(ComboPairError::InvalidMargin {
        pair_id: pair.pair_id,
        reason,
    })
}

fn validate_spec(instrument_id: u32, spec: &ContractSpec) -> Result<(), RegistryError> {
    let reason = if spec.tick_size == 0 {
        "tick size must be positive"
    } else if spec.max_price < spec.min_price {
        "max price must not be below min price"
    } else if !(spec.max_price - spec.min_price).is_multiple_of(spec.tick_size) {
        "price range must be divisible by tick size"
    } else {
        return Ok(());
    };
    Err(RegistryError::InvalidSpec {
        instrument_id,
        reason,
    })
}
