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
    ComboBook, ComboCommandError, ComboEvent, ComboOrder, ComboOrderOwner, validate_new_combo_order,
};
use crate::trading::core::combo_pair::{ComboLeg, ComboPair, ComboPairError};
use crate::trading::core::kernel::{CoreEvent, TradingCore};
use crate::trading::core::plugins::{NoPlugins, PreTradePlugin};
use crate::trading::core::protocol::{Command, CommandType};
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// Why the registry refused an instrument or a command.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    next_combo_order_id: u64,
}

impl InstrumentRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            instruments: U64MixMap::default(),
            pairs: U64MixMap::default(),
            combo_books: U64MixMap::default(),
            next_combo_order_id: 1,
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

    /// Iterates over registered Combo Pairs in unspecified order.
    pub fn pairs(&self) -> impl Iterator<Item = &ComboPair> {
        self.pairs.values()
    }

    /// Returns the base Combo Order book for `pair_id`.
    ///
    /// The book is a combo-only interface: ordinary Contract books never expose
    /// combo spreads or quantities.
    #[must_use]
    pub fn combo_book(&self, pair_id: u32) -> Option<&ComboBook> {
        self.combo_books.get(&u64::from(pair_id))
    }

    /// Returns the base Combo Order book for `pair_id` for state inspection or
    /// restore.
    pub fn combo_book_mut(&mut self, pair_id: u32) -> Option<&mut ComboBook> {
        self.combo_books.get_mut(&u64::from(pair_id))
    }

    /// Executes one Combo Order command against its pair book.
    ///
    /// This is the command translation and direct-matching seam. It accepts
    /// only M1 base combo semantics: spread limit GFD new orders and cancels.
    /// Market, FOK/FAK, stop-trigger, zero-quantity, unknown-pair, and
    /// duplicate-cancel requests fail explicitly without mutating a book. A new
    /// order matches opposite Combo Orders in price-time priority before its
    /// residual rests. Implied-order insertion is intentionally absent.
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
        let pair_id = command.combo_id;
        if !self.combo_books.contains_key(&u64::from(pair_id)) {
            return Err(ComboCommandError::UnknownPair { pair_id });
        }
        validate_new_combo_order(&command)?;

        let order_id = self.next_combo_order_id;
        let owner = ComboOrderOwner::new(
            u64::from(command.user_id),
            command.session_id,
            command.client_order_id,
        );
        let order = ComboOrder::new(
            order_id,
            pair_id,
            owner,
            command.side,
            command.price_ticks,
            command.quantity,
        );
        let book = self
            .combo_books
            .get_mut(&u64::from(pair_id))
            .ok_or(ComboCommandError::UnknownPair { pair_id })?;
        let events = book.submit(order)?;
        self.next_combo_order_id = self.next_combo_order_id.saturating_add(1);
        Ok(events)
    }

    fn cancel_combo_order(
        &mut self,
        command: Command,
    ) -> Result<Vec<ComboEvent>, ComboCommandError> {
        let pair_id = command.combo_id;
        if command.cancel_order_id == 0 {
            return Err(ComboCommandError::MissingCancelOrderId);
        }
        let book = self
            .combo_books
            .get_mut(&u64::from(pair_id))
            .ok_or(ComboCommandError::UnknownPair { pair_id })?;
        let order = book.cancel_for(u64::from(command.user_id), command.cancel_order_id)?;
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

const MAX_MARGIN_RATE_BPS: u64 = 10_000;

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
