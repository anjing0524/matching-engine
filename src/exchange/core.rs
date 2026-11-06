//! Exchange core kernel skeleton.
//!
//! This module is the first runtime slice of the gateway/middleware/core design
//! in `docs/exchange-core-architecture.md`. It deliberately has no network,
//! no persistence, and no shared memory: the goal is a deterministic,
//! single-threaded command → matching → event kernel that later slices can wrap
//! with the middleware transport.
//!
//! Scope of this slice:
//! - one contract/order book;
//! - limit orders only (market/FAK/FOK and combos come later);
//! - explicit accepted / rejected / trade / canceled events;
//! - deterministic replay: same command sequence produces the same events.

use crate::domain::orderbook::{ContractSpec, TickBasedOrderBook};
use crate::exchange::auction::{self, AuctionOrder, AuctionResult};
use crate::exchange::combo::ComboQueue;
use crate::exchange::plugins::{PreTradePlugin, RiskOrder, ValidationContext};
use crate::exchange::positions::{CloseKey, FrozenClose, PositionKey, Positions};
use crate::exchange::private_events::{PrivateEvent, PrivateStatus};
use crate::exchange::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, VolumeCondition,
};
use crate::exchange::public_events::{ExchangePhase, PublicEvent, PublicEventType};
use crate::shared::SymbolId;
use crate::shared::protocol::{NewOrderRequest, OrderType};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

/// A new limit order submitted to the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitOrderCommand {
    pub user_id: u64,
    pub client_order_id: u64,
    pub side: OrderType,
    pub price_ticks: u64,
    pub quantity: u64,
}

/// A cancel command for a previously accepted order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelCommand {
    pub user_id: u64,
    pub order_id: u64,
}

/// Core output consumed by the private/public event projections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreEvent {
    Accepted {
        order_id: u64,
        user_id: u64,
        side: OrderType,
        price_ticks: u64,
        remaining_quantity: u64,
    },
    Rejected {
        client_order_id: u64,
        reason: String,
    },
    Trade {
        trade_id: u64,
        price_ticks: u64,
        quantity: u64,
        buyer_user_id: u64,
        buyer_order_id: u64,
        seller_user_id: u64,
        seller_order_id: u64,
    },
    Canceled {
        order_id: u64,
        user_id: u64,
    },
}

/// Lifecycle status of an exchange order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OrderStatus {
    Accepted,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

/// Metadata for an order accepted into the book.
///
/// The order book owns price/FIFO state; this table carries the business
/// attributes needed to update positions and release close reservations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OrderMeta {
    pub client_order_id: u64,
    pub account_id: u64,
    pub side: Side,
    pub offset: Offset,
    pub hedge: Hedge,
    pub price_ticks: u64,
    pub remaining: u64,
    pub status: OrderStatus,
}

/// In-memory core snapshot for the continuous book and business state.
///
/// Combo queues and call-auction staging are intentionally outside this slice;
/// callers must drain them before snapshotting. Ephemeral event buffers are not
/// snapshotted because they are already consumed by middleware.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoreSnapshot {
    pub spec: ContractSpec,
    pub symbol: SymbolId,
    pub resting_orders: Vec<crate::domain::orderbook::RestingOrder>,
    pub next_order_id: u64,
    pub orders: Vec<(u64, OrderMeta)>,
    pub positions: Vec<(PositionKey, crate::exchange::positions::Position)>,
    pub frozen_close: Vec<(CloseKey, u64)>,
    pub phase: ExchangePhase,
    pub trading_day: u32,
    pub next_trade_id: u64,
    pub private_seq: u64,
    pub public_seq: u64,
    pub last_tick_seq: u64,
    pub last_exchange_ts_ns: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotEnvelope {
    version: u32,
    checksum: u32,
    snapshot: CoreSnapshot,
}

/// Current on-disk snapshot format version.
pub const SNAPSHOT_VERSION: u32 = 1;

/// The deterministic core state machine for one contract.
pub struct ExchangeCore {
    book: TickBasedOrderBook,
    symbol: SymbolId,
    next_trade_id: u64,
    positions: Positions,
    frozen_close: FrozenClose,
    orders: HashMap<u64, OrderMeta>,
    private_seq: u64,
    private_events: Vec<PrivateEvent>,
    auction_orders: Vec<AuctionOrder>,
    auction_meta: HashMap<u64, (u64, Side, Offset, Hedge)>,
    auction_order_seq: u64,
    public_seq: u64,
    public_events: Vec<PublicEvent>,
    public_orders: HashSet<u64>,
    combo_queues: HashMap<u32, ComboQueue>,
    phase: ExchangePhase,
    trading_day: u32,
    last_tick_seq: u64,
    last_exchange_ts_ns: u64,
}

impl ExchangeCore {
    #[must_use]
    pub fn new(spec: ContractSpec, symbol: SymbolId) -> Self {
        Self {
            book: TickBasedOrderBook::new(spec),
            symbol,
            next_trade_id: 1,
            positions: Positions::new(),
            frozen_close: FrozenClose::new(),
            orders: HashMap::new(),
            private_seq: 1,
            private_events: Vec::new(),
            auction_orders: Vec::new(),
            auction_meta: HashMap::new(),
            auction_order_seq: 1,
            public_seq: 1,
            public_events: Vec::new(),
            public_orders: HashSet::new(),
            combo_queues: HashMap::new(),
            phase: ExchangePhase::Continuous,
            trading_day: 1,
            last_tick_seq: 0,
            last_exchange_ts_ns: 0,
        }
    }

    /// Captures continuous-book and business state.
    ///
    /// Call `snapshot` only when combo queues and auction staging are empty.
    #[must_use]
    pub fn snapshot(&self) -> CoreSnapshot {
        let mut orders: Vec<_> = self.orders.iter().map(|(id, meta)| (*id, *meta)).collect();
        orders.sort_by_key(|(id, _)| *id);
        let mut positions = self.positions.snapshot();
        positions.sort_by_key(|(key, _)| (key.account_id, key.side as u8, key.hedge as u8));
        let mut frozen_close = self.frozen_close.snapshot();
        frozen_close.sort_by_key(|(key, _)| {
            (
                key.position.account_id,
                key.position.side as u8,
                key.position.hedge as u8,
                key.offset as u8,
            )
        });
        CoreSnapshot {
            spec: self.book.spec().clone(),
            symbol: self.symbol,
            resting_orders: self.book.snapshot_orders(),
            next_order_id: self.book.next_order_id(),
            orders,
            positions,
            frozen_close,
            phase: self.phase,
            trading_day: self.trading_day,
            next_trade_id: self.next_trade_id,
            private_seq: self.private_seq,
            public_seq: self.public_seq,
            last_tick_seq: self.last_tick_seq,
            last_exchange_ts_ns: self.last_exchange_ts_ns,
        }
    }

    /// Restores a previously captured snapshot.
    ///
    /// # Errors
    /// Returns a reason when the snapshot's resting orders cannot be rebuilt.
    pub fn restore_snapshot(&mut self, snapshot: CoreSnapshot) -> Result<(), String> {
        let book = TickBasedOrderBook::restore(
            snapshot.spec.clone(),
            &snapshot.resting_orders,
            snapshot.next_order_id,
        )?;
        self.book = book;
        self.symbol = snapshot.symbol;
        self.orders = snapshot.orders.into_iter().collect();
        self.public_orders = self.orders.keys().copied().collect();
        self.positions.restore(snapshot.positions);
        self.frozen_close.restore(snapshot.frozen_close);
        self.phase = snapshot.phase;
        self.trading_day = snapshot.trading_day;
        self.next_trade_id = snapshot.next_trade_id;
        self.private_seq = snapshot.private_seq;
        self.public_seq = snapshot.public_seq;
        self.last_tick_seq = snapshot.last_tick_seq;
        self.last_exchange_ts_ns = snapshot.last_exchange_ts_ns;
        self.private_events.clear();
        self.public_events.clear();
        self.auction_orders.clear();
        self.auction_meta.clear();
        self.combo_queues.clear();
        Ok(())
    }

    /// Serializes the snapshot with a format version.
    ///
    /// # Errors
    /// Returns a JSON error if serialization fails.
    pub fn snapshot_json(&self) -> Result<String, serde_json::Error> {
        let snapshot = self.snapshot();
        let payload = serde_json::to_vec(&snapshot)?;
        let checksum = crc32fast::hash(&payload);
        serde_json::to_string(&SnapshotEnvelope {
            version: SNAPSHOT_VERSION,
            checksum,
            snapshot,
        })
    }

    /// Restores a snapshot from versioned JSON.
    ///
    /// # Errors
    /// Returns a reason for invalid JSON, a version mismatch, or a restore error.
    pub fn restore_snapshot_json(&mut self, json: &str) -> Result<(), String> {
        let envelope: SnapshotEnvelope =
            serde_json::from_str(json).map_err(|error| error.to_string())?;
        if envelope.version != SNAPSHOT_VERSION {
            return Err(format!("unsupported snapshot version {}", envelope.version));
        }
        let payload = serde_json::to_vec(&envelope.snapshot).map_err(|error| error.to_string())?;
        if crc32fast::hash(&payload) != envelope.checksum {
            return Err("snapshot checksum mismatch".to_string());
        }
        self.restore_snapshot(envelope.snapshot)
    }

    /// Atomically writes a versioned snapshot to `path`.
    ///
    /// # Errors
    /// Returns an I/O error for serialization or file replacement failure.
    pub fn save_snapshot(&self, path: &Path) -> io::Result<()> {
        let json = self
            .snapshot_json()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, json)?;
        std::fs::rename(temporary, path)
    }

    /// Loads a versioned snapshot from `path`.
    ///
    /// # Errors
    /// Returns an I/O error for read/JSON/version failures.
    pub fn load_snapshot(path: &Path) -> io::Result<CoreSnapshot> {
        let json = std::fs::read_to_string(path)?;
        let envelope: SnapshotEnvelope = serde_json::from_str(&json)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if envelope.version != SNAPSHOT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported snapshot version {}", envelope.version),
            ));
        }
        let payload = serde_json::to_vec(&envelope.snapshot)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if crc32fast::hash(&payload) != envelope.checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot checksum mismatch",
            ));
        }
        Ok(envelope.snapshot)
    }

    #[must_use]
    pub fn positions(&self) -> &Positions {
        &self.positions
    }

    pub fn positions_mut(&mut self) -> &mut Positions {
        &mut self.positions
    }

    #[must_use]
    pub fn frozen_close(&self) -> &FrozenClose {
        &self.frozen_close
    }

    /// Drains all private events produced since the last drain.
    pub fn drain_private_events(&mut self) -> Vec<PrivateEvent> {
        std::mem::take(&mut self.private_events)
    }

    #[must_use]
    pub fn private_events(&self) -> &[PrivateEvent] {
        &self.private_events
    }

    #[allow(clippy::too_many_arguments)]
    fn push_private(
        &mut self,
        order_id: u64,
        client_order_id: u64,
        account_id: u64,
        status: PrivateStatus,
        filled_delta: u64,
        remaining: u64,
        reason: Option<String>,
    ) {
        let seq = self.private_seq;
        self.private_seq += 1;
        self.private_events.push(PrivateEvent {
            seq,
            order_id,
            client_order_id,
            account_id,
            status,
            filled_delta,
            remaining,
            reason,
        });
    }

    /// Conservative all-or-none liquidity check against the current book.
    ///
    /// FOK is rare; using the public snapshot keeps the core book API simple.
    /// A production version should use a dedicated depth scan without allocating.
    fn can_fill(&self, side: Side, price_ticks: u64, quantity: u64) -> bool {
        let mut available = 0u64;
        for order in self.book.snapshot_orders() {
            let crosses = match side {
                Side::Buy => order.side == OrderType::Sell && order.price <= price_ticks,
                Side::Sell => order.side == OrderType::Buy && order.price >= price_ticks,
            };
            if crosses {
                available = available.saturating_add(order.quantity);
                if available >= quantity {
                    return true;
                }
            }
        }
        false
    }

    /// Drains public events produced since the last drain.
    pub fn drain_public_events(&mut self) -> Vec<PublicEvent> {
        std::mem::take(&mut self.public_events)
    }

    #[must_use]
    pub fn public_events(&self) -> &[PublicEvent] {
        &self.public_events
    }

    fn push_public_event(&mut self, mut event: PublicEvent) {
        event.seq = self.public_seq;
        self.public_seq += 1;
        self.public_events.push(event);
    }

    fn push_public(&mut self, event_type: PublicEventType, price_ticks: u64, volume: u64) {
        self.push_public_event(PublicEvent {
            seq: 0,
            event_type,
            exchange_ts_ns: 0,
            phase: 0,
            instrument_id: 0,
            order_id: 0,
            trade_id: 0,
            buy_order_id: 0,
            sell_order_id: 0,
            side: None,
            price_ticks,
            quantity: 0,
            volume,
        });
    }

    fn reject_private_into(
        &mut self,
        command: &Command,
        reason: String,
        events: &mut Vec<CoreEvent>,
    ) {
        self.push_private(
            0,
            command.client_order_id,
            u64::from(command.user_id),
            PrivateStatus::Rejected,
            0,
            0,
            Some(reason.clone()),
        );
        events.push(CoreEvent::Rejected {
            client_order_id: command.client_order_id,
            reason,
        });
    }

    #[must_use]
    pub fn phase(&self) -> ExchangePhase {
        self.phase
    }

    #[must_use]
    pub fn trading_day(&self) -> u32 {
        self.trading_day
    }

    #[must_use]
    pub fn last_tick_seq(&self) -> u64 {
        self.last_tick_seq
    }

    #[must_use]
    pub fn last_exchange_ts_ns(&self) -> u64 {
        self.last_exchange_ts_ns
    }

    /// Switches the trading phase and emits a public SessionSwitch event.
    pub fn set_phase(&mut self, phase: ExchangePhase, trading_day: u32) {
        self.phase = phase;
        self.trading_day = trading_day;
        self.push_public_event(PublicEvent {
            seq: 0,
            event_type: PublicEventType::SessionSwitch,
            exchange_ts_ns: 0,
            phase: phase.code(),
            instrument_id: 0,
            order_id: 0,
            trade_id: 0,
            buy_order_id: 0,
            sell_order_id: 0,
            side: None,
            price_ticks: 0,
            quantity: 0,
            volume: 0,
        });
    }

    /// Queues a combination order in the logical spread-price/FIFO queue.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_combo_order(
        &mut self,
        combo_id: u32,
        user_id: u64,
        side: Side,
        spread_price_ticks: u64,
        quantity: u64,
        offset: Offset,
        hedge: Hedge,
    ) -> Option<u64> {
        let order_id = self.book.reserve_order_id();
        let queue = self.combo_queues.entry(combo_id).or_default();
        queue
            .push(
                order_id,
                user_id,
                side,
                spread_price_ticks,
                quantity,
                offset,
                hedge,
            )
            .then_some(order_id)
    }

    /// Cancels a combination order by id.
    pub fn cancel_combo_order(&mut self, combo_id: u32, order_id: u64) -> bool {
        self.combo_queues
            .get_mut(&combo_id)
            .is_some_and(|queue| queue.cancel(order_id).is_some())
    }

    #[must_use]
    pub fn combo_queue_len(&self, combo_id: u32) -> usize {
        self.combo_queues.get(&combo_id).map_or(0, ComboQueue::len)
    }

    /// Clears the current auction collection.
    pub fn begin_auction(&mut self) {
        self.auction_orders.clear();
    }

    /// Queues one limit order into the call auction and returns its assigned id.
    ///
    /// The caller is responsible for rejecting market/arbitrage orders; this
    /// slice only models collected limit orders.
    pub fn queue_auction_order(
        &mut self,
        user_id: u64,
        side: Side,
        price_ticks: u64,
        quantity: u64,
        offset: Offset,
        hedge: Hedge,
    ) -> u64 {
        let order_id = self.book.reserve_order_id();
        self.auction_order_seq += 1;
        self.auction_orders.push(AuctionOrder {
            order_id,
            user_id,
            side,
            price_ticks,
            quantity,
            offset,
            hedge,
            arrival_seq: order_id,
        });
        self.auction_meta
            .insert(order_id, (user_id, side, offset, hedge));
        order_id
    }

    /// Uncrosses the collected auction orders at the maximum-volume price.
    pub fn uncross_auction(&mut self, previous_settlement: u64) -> AuctionResult {
        let orders = std::mem::take(&mut self.auction_orders);
        let result = auction::uncross(&orders, previous_settlement);
        for trade in &result.trades {
            self.apply_auction_fill(trade.buyer_order_id, trade.quantity);
            self.apply_auction_fill(trade.seller_order_id, trade.quantity);
            self.push_private(
                trade.buyer_order_id,
                0,
                self.auction_meta
                    .get(&trade.buyer_order_id)
                    .map_or(0, |meta| meta.0),
                PrivateStatus::Filled,
                trade.quantity,
                0,
                None,
            );
            self.push_private(
                trade.seller_order_id,
                0,
                self.auction_meta
                    .get(&trade.seller_order_id)
                    .map_or(0, |meta| meta.0),
                PrivateStatus::Filled,
                trade.quantity,
                0,
                None,
            );
        }
        for order in &result.remaining {
            let side = match order.side {
                Side::Buy => OrderType::Buy,
                Side::Sell => OrderType::Sell,
            };
            if self
                .book
                .insert_resting_order(
                    order.user_id,
                    side,
                    order.price_ticks,
                    order.quantity,
                    order.quantity,
                    order.order_id,
                )
                .is_ok()
            {
                self.orders.insert(
                    order.order_id,
                    OrderMeta {
                        client_order_id: 0,
                        account_id: order.user_id,
                        side: order.side,
                        offset: order.offset,
                        hedge: order.hedge,
                        price_ticks: order.price_ticks,
                        remaining: order.quantity,
                        status: OrderStatus::Accepted,
                    },
                );
            }
        }
        self.auction_meta.clear();
        self.push_public(
            PublicEventType::AuctionResult,
            result.price_ticks.unwrap_or(0),
            result.volume,
        );
        result
    }

    #[must_use]
    pub fn auction_order_count(&self) -> usize {
        self.auction_orders.len()
    }

    /// Submits one limit order and returns every event it produced.
    ///
    /// The order book owns order-id allocation and price/tick validation; this
    /// kernel maps the command into the domain request and projects the result
    /// into protocol-neutral core events.
    pub fn submit_limit(&mut self, command: LimitOrderCommand) -> Vec<CoreEvent> {
        let mut events = Vec::new();
        self.submit_limit_into(command, &mut events);
        events
    }

    fn submit_limit_into(&mut self, command: LimitOrderCommand, events: &mut Vec<CoreEvent>) {
        let request = NewOrderRequest {
            user_id: command.user_id,
            client_order_id: command.client_order_id,
            symbol: self.symbol,
            order_type: command.side,
            price: command.price_ticks,
            quantity: command.quantity,
        };

        match self.book.match_order(request) {
            crate::domain::orderbook::MatchOutcome::Filled { trades } => {
                self.trade_events_into(trades, events);
            }
            crate::domain::orderbook::MatchOutcome::Rested {
                trades,
                confirmation,
            } => {
                self.trade_events_into(trades, events);
                events.push(CoreEvent::Accepted {
                    order_id: confirmation.order_id,
                    user_id: confirmation.user_id,
                    side: command.side,
                    price_ticks: command.price_ticks,
                    remaining_quantity: self
                        .book
                        .get_order_status(confirmation.order_id)
                        .map_or(command.quantity, |status| status.remaining_quantity),
                });
            }
            crate::domain::orderbook::MatchOutcome::Rejected { reason, trades } => {
                self.trade_events_into(trades, events);
                events.push(CoreEvent::Rejected {
                    client_order_id: command.client_order_id,
                    reason,
                });
            }
        }
    }

    /// Cancels a resting order. A missing/foreign order is rejected locally.
    pub fn cancel(&mut self, command: CancelCommand) -> CoreEvent {
        match self.book.cancel_order(command.order_id, command.user_id) {
            Ok(()) => {
                if let Some(mut meta) = self.orders.remove(&command.order_id) {
                    if meta.offset != Offset::Open {
                        let key = CloseKey::new(
                            PositionKey::new(meta.account_id, meta.side.opposite(), meta.hedge),
                            meta.offset,
                        );
                        self.frozen_close.release(key, meta.remaining);
                    }
                    let canceled_quantity = meta.remaining;
                    let client_order_id = meta.client_order_id;
                    let account_id = meta.account_id;
                    let price_ticks = meta.price_ticks;
                    let side = meta.side;
                    meta.remaining = 0;
                    meta.status = OrderStatus::Canceled;
                    self.push_private(
                        command.order_id,
                        client_order_id,
                        account_id,
                        PrivateStatus::Canceled,
                        0,
                        0,
                        None,
                    );
                    if self.public_orders.remove(&command.order_id) {
                        self.push_public_event(PublicEvent {
                            seq: 0,
                            event_type: PublicEventType::OrderCanceled,
                            exchange_ts_ns: 0,
                            phase: 0,
                            instrument_id: 0,
                            order_id: command.order_id,
                            trade_id: 0,
                            buy_order_id: 0,
                            sell_order_id: 0,
                            side: Some(side),
                            price_ticks,
                            quantity: canceled_quantity,
                            volume: 0,
                        });
                    }
                    self.orders.insert(command.order_id, meta);
                }
                CoreEvent::Canceled {
                    order_id: command.order_id,
                    user_id: command.user_id,
                }
            }
            Err(reason) => CoreEvent::Rejected {
                client_order_id: 0,
                reason,
            },
        }
    }

    /// Executes a command through a static pre-trade plugin chain.
    ///
    /// Close orders reserve `frozen_close` before reaching the book. A fully
    /// filled or rejected order releases the reservation immediately; a
    /// resting order keeps it until it fills or is canceled. Fills do not yet
    /// mutate positions in this slice.
    pub fn execute_with_plugins<P: PreTradePlugin>(
        &mut self,
        command: Command,
        plugin: &P,
    ) -> Vec<CoreEvent> {
        let mut events = Vec::new();
        self.execute_with_plugins_into(command, plugin, &mut events);
        events
    }

    /// Appends the command's events into `out` without a per-command Vec.
    #[allow(clippy::too_many_lines)]
    pub fn execute_with_plugins_into<P: PreTradePlugin>(
        &mut self,
        command: Command,
        plugin: &P,
        out: &mut Vec<CoreEvent>,
    ) {
        let start = out.len();
        if command.command_type != CommandType::NewOrder {
            out.extend(self.execute(command));
            return;
        }
        if !matches!(
            self.phase,
            ExchangePhase::Continuous | ExchangePhase::OpeningAuction
        ) {
            self.reject_private_into(
                &command,
                "trading phase rejects new orders".to_string(),
                out,
            );
            return;
        }
        if !matches!(command.price_type, PriceType::Limit | PriceType::Market) {
            out.extend(self.execute(command));
            return;
        }
        if command.price_ticks < 0 || command.quantity == 0 {
            self.reject_private_into(&command, "invalid price or quantity".to_string(), out);
            return;
        }
        let matching_price = match command.price_type {
            PriceType::Limit => command.price_ticks as u64,
            PriceType::Market => match command.side {
                Side::Buy => self.book.spec().max_price,
                Side::Sell => self.book.spec().min_price,
            },
            PriceType::AdvancedMarket => {
                self.reject_private_into(
                    &command,
                    "advanced market orders are not implemented".to_string(),
                    out,
                );
                return;
            }
        };
        if command.volume_condition == VolumeCondition::All
            && !self.can_fill(command.side, matching_price, command.quantity)
        {
            self.reject_private_into(&command, "FOK cannot be fully filled".to_string(), out);
            return;
        }

        let side = match command.side {
            Side::Buy => OrderType::Buy,
            Side::Sell => OrderType::Sell,
        };
        let risk_order = RiskOrder {
            account_id: u64::from(command.user_id),
            side: command.side,
            offset: command.offset,
            hedge: command.hedge,
            quantity: command.quantity,
        };
        let context = ValidationContext {
            positions: &self.positions,
            frozen_close: &self.frozen_close,
        };
        if let Err(reason) = plugin.check(&risk_order, &context) {
            self.reject_private_into(&command, reason.to_string(), out);
            return;
        }

        let close_key = if command.offset == Offset::Open {
            None
        } else {
            let key = CloseKey::new(
                PositionKey::new(
                    u64::from(command.user_id),
                    command.side.opposite(),
                    command.hedge,
                ),
                command.offset,
            );
            if let Err(error) = self
                .frozen_close
                .reserve(key, &self.positions, command.quantity)
            {
                self.reject_private_into(
                    &command,
                    format!("close reservation failed: {error:?}"),
                    out,
                );
                return;
            }
            Some(key)
        };

        self.submit_limit_into(
            LimitOrderCommand {
                user_id: u64::from(command.user_id),
                client_order_id: command.client_order_id,
                side,
                price_ticks: matching_price,
                quantity: command.quantity,
            },
            out,
        );

        let incoming_id = out[start..].iter().find_map(|event| match event {
            CoreEvent::Accepted { order_id, .. } => Some(*order_id),
            CoreEvent::Trade {
                buyer_order_id,
                seller_order_id,
                ..
            } => Some(match command.side {
                Side::Buy => *buyer_order_id,
                Side::Sell => *seller_order_id,
            }),
            _ => None,
        });
        if let Some(order_id) = incoming_id {
            self.orders.insert(
                order_id,
                OrderMeta {
                    client_order_id: command.client_order_id,
                    account_id: u64::from(command.user_id),
                    side: command.side,
                    offset: command.offset,
                    hedge: command.hedge,
                    price_ticks: command.price_ticks as u64,
                    remaining: command.quantity,
                    status: OrderStatus::Accepted,
                },
            );
        }

        if let Some(reason) = out[start..].iter().find_map(|event| match event {
            CoreEvent::Rejected { reason, .. } => Some(reason.clone()),
            _ => None,
        }) {
            self.push_private(
                0,
                command.client_order_id,
                u64::from(command.user_id),
                PrivateStatus::Rejected,
                0,
                0,
                Some(reason),
            );
        }
        if let Some((order_id, remaining_quantity)) =
            out[start..].iter().find_map(|event| match event {
                CoreEvent::Accepted {
                    order_id,
                    remaining_quantity,
                    ..
                } => Some((*order_id, *remaining_quantity)),
                _ => None,
            })
        {
            self.push_private(
                order_id,
                command.client_order_id,
                u64::from(command.user_id),
                PrivateStatus::Accepted,
                0,
                remaining_quantity,
                None,
            );
        }

        let fills: smallvec::SmallVec<[(u64, u64, u64); 8]> = out[start..]
            .iter()
            .filter_map(|event| match event {
                CoreEvent::Trade {
                    buyer_order_id,
                    seller_order_id,
                    quantity,
                    ..
                } => Some((*buyer_order_id, *seller_order_id, *quantity)),
                _ => None,
            })
            .collect();
        for (buyer_order_id, seller_order_id, quantity) in fills {
            self.apply_fill(buyer_order_id, quantity);
            self.apply_fill(seller_order_id, quantity);
        }

        let has_accepted = out[start..]
            .iter()
            .any(|event| matches!(event, CoreEvent::Accepted { .. }));
        if let Some(key) = close_key
            && !has_accepted
        {
            let filled = out[start..]
                .iter()
                .filter_map(|event| match event {
                    CoreEvent::Trade { quantity, .. } => Some(*quantity),
                    _ => None,
                })
                .sum::<u64>();
            self.frozen_close
                .release(key, command.quantity.saturating_sub(filled));
        }

        if let Some((order_id, remaining_quantity)) =
            out[start..].iter().find_map(|event| match event {
                CoreEvent::Accepted {
                    order_id,
                    remaining_quantity,
                    ..
                } => Some((*order_id, *remaining_quantity)),
                _ => None,
            })
            && command.price_type == PriceType::Limit
            && command.time_condition != TimeCondition::Ioc
        {
            self.push_public_event(PublicEvent {
                seq: 0,
                event_type: PublicEventType::OrderRested,
                exchange_ts_ns: command.exchange_ts_ns,
                phase: 0,
                instrument_id: command.instrument_id,
                order_id,
                trade_id: 0,
                buy_order_id: 0,
                sell_order_id: 0,
                side: Some(command.side),
                price_ticks: command.price_ticks as u64,
                quantity: remaining_quantity,
                volume: 0,
            });
            self.public_orders.insert(order_id);
        }
        for event in &out[start..] {
            if let CoreEvent::Trade {
                price_ticks,
                quantity,
                trade_id,
                buyer_order_id,
                seller_order_id,
                ..
            } = event
            {
                self.push_public_event(PublicEvent {
                    seq: 0,
                    event_type: PublicEventType::Trade,
                    exchange_ts_ns: command.exchange_ts_ns,
                    phase: 0,
                    instrument_id: command.instrument_id,
                    order_id: 0,
                    trade_id: *trade_id,
                    buy_order_id: *buyer_order_id,
                    sell_order_id: *seller_order_id,
                    side: Some(command.side),
                    price_ticks: *price_ticks,
                    quantity: *quantity,
                    volume: 0,
                });
            }
        }
        if (command.price_type == PriceType::Market || command.time_condition == TimeCondition::Ioc)
            && let Some(order_id) = incoming_id
            && self
                .orders
                .get(&order_id)
                .is_some_and(|meta| meta.remaining > 0)
        {
            let canceled = self.cancel(CancelCommand {
                user_id: u64::from(command.user_id),
                order_id,
            });
            out.push(canceled);
        }
    }

    fn apply_auction_fill(&mut self, order_id: u64, quantity: u64) {
        let Some((account_id, side, offset, hedge)) = self.auction_meta.get(&order_id).copied()
        else {
            return;
        };
        let position_side = if offset == Offset::Open {
            side
        } else {
            side.opposite()
        };
        let key = PositionKey::new(account_id, position_side, hedge);
        if offset == Offset::Open {
            let _ = self.positions.apply_open(key, quantity);
        } else {
            let _ = self.positions.apply_close(key, offset, quantity);
        }
    }

    /// Applies one fill to an order's position and remaining quantity.
    fn apply_fill(&mut self, order_id: u64, quantity: u64) {
        let Some((client_order_id, account_id, side, offset, hedge, remaining)) = ({
            let Some(meta) = self.orders.get_mut(&order_id) else {
                return;
            };
            let updated = meta.remaining.saturating_sub(quantity);
            meta.remaining = updated;
            meta.status = if updated == 0 {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            };
            Some((
                meta.client_order_id,
                meta.account_id,
                meta.side,
                meta.offset,
                meta.hedge,
                updated,
            ))
        }) else {
            return;
        };
        let position_side = if offset == Offset::Open {
            side
        } else {
            side.opposite()
        };
        let key = PositionKey::new(account_id, position_side, hedge);
        if offset == Offset::Open {
            let _ = self.positions.apply_open(key, quantity);
        } else {
            let _ = self.positions.apply_close(key, offset, quantity);
            self.frozen_close
                .release(CloseKey::new(key, offset), quantity);
        }
        self.push_private(
            order_id,
            client_order_id,
            account_id,
            if remaining == 0 {
                PrivateStatus::Filled
            } else {
                PrivateStatus::PartiallyFilled
            },
            quantity,
            remaining,
            None,
        );
    }

    /// Returns the lifecycle status of an order accepted through the business
    /// path, or `None` before it was accepted.
    #[must_use]
    pub fn order_status(&self, order_id: u64) -> Option<OrderStatus> {
        self.orders.get(&order_id).map(|meta| meta.status)
    }

    /// Executes one fixed-size command-log record.
    ///
    /// This is the only entry point the middleware consumer needs. Unsupported
    /// commands are rejected explicitly instead of being silently ignored.
    pub fn execute(&mut self, command: Command) -> Vec<CoreEvent> {
        match command.command_type {
            CommandType::TimeTick => {
                self.last_tick_seq = command.global_seq;
                self.last_exchange_ts_ns = command.exchange_ts_ns;
                self.push_public_event(PublicEvent {
                    seq: 0,
                    event_type: PublicEventType::TimeTick,
                    exchange_ts_ns: command.exchange_ts_ns,
                    phase: self.phase.code(),
                    instrument_id: 0,
                    order_id: 0,
                    trade_id: 0,
                    buy_order_id: 0,
                    sell_order_id: 0,
                    side: None,
                    price_ticks: 0,
                    quantity: 0,
                    volume: 0,
                });
                Vec::new()
            }
            CommandType::Control => {
                if let Some(phase) = ExchangePhase::from_code(command.flags) {
                    self.set_phase(phase, command.quantity as u32);
                }
                Vec::new()
            }
            CommandType::NewOrder if command.price_type == PriceType::Limit => {
                if command.price_ticks < 0 || command.quantity == 0 {
                    return vec![CoreEvent::Rejected {
                        client_order_id: command.client_order_id,
                        reason: "invalid price or quantity".to_string(),
                    }];
                }
                self.submit_limit(LimitOrderCommand {
                    user_id: u64::from(command.user_id),
                    client_order_id: command.client_order_id,
                    side: match command.side {
                        Side::Buy => OrderType::Buy,
                        Side::Sell => OrderType::Sell,
                    },
                    price_ticks: command.price_ticks as u64,
                    quantity: command.quantity,
                })
            }
            CommandType::Cancel => vec![self.cancel(CancelCommand {
                user_id: u64::from(command.user_id),
                order_id: command.cancel_order_id,
            })],
            other => vec![CoreEvent::Rejected {
                client_order_id: command.client_order_id,
                reason: format!("unsupported command in this slice: {other:?}"),
            }],
        }
    }

    #[must_use]
    pub fn best_bid(&self) -> Option<u64> {
        self.book.best_bid()
    }

    #[must_use]
    pub fn best_ask(&self) -> Option<u64> {
        self.book.best_ask()
    }

    fn trade_events_into(
        &mut self,
        trades: smallvec::SmallVec<[crate::shared::protocol::TradeNotification; 8]>,
        events: &mut Vec<CoreEvent>,
    ) {
        for trade in trades {
            let trade_id = self.next_trade_id;
            self.next_trade_id += 1;
            events.push(CoreEvent::Trade {
                trade_id,
                price_ticks: trade.matched_price,
                quantity: trade.matched_quantity,
                buyer_user_id: trade.buyer_user_id,
                buyer_order_id: trade.buyer_order_id,
                seller_user_id: trade.seller_user_id,
                seller_order_id: trade.seller_order_id,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> ExchangeCore {
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"))
    }

    fn order(user_id: u64, side: OrderType, price: u64, quantity: u64) -> LimitOrderCommand {
        LimitOrderCommand {
            user_id,
            client_order_id: 0,
            side,
            price_ticks: price,
            quantity,
        }
    }

    #[test]
    fn resting_buy_is_reported_as_accepted() {
        let mut core = core();
        let events = core.submit_limit(order(1, OrderType::Buy, 50_000, 10));
        assert_eq!(events.len(), 1);
        match &events[0] {
            CoreEvent::Accepted {
                user_id,
                remaining_quantity,
                ..
            } => {
                assert_eq!(*user_id, 1);
                assert_eq!(*remaining_quantity, 10);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(core.best_bid(), Some(50_000));
        assert_eq!(core.best_ask(), None);
    }

    #[test]
    fn crossing_sell_produces_trade_and_clears_book() {
        let mut core = core();
        core.submit_limit(order(1, OrderType::Buy, 50_000, 10));
        let events = core.submit_limit(order(2, OrderType::Sell, 50_000, 10));
        assert_eq!(events.len(), 1);
        match &events[0] {
            CoreEvent::Trade {
                price_ticks,
                quantity,
                buyer_user_id,
                seller_user_id,
                ..
            } => {
                assert_eq!(*price_ticks, 50_000);
                assert_eq!(*quantity, 10);
                assert_eq!(*buyer_user_id, 1);
                assert_eq!(*seller_user_id, 2);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(core.best_bid(), None);
        assert_eq!(core.best_ask(), None);
    }

    #[test]
    fn partial_fill_rests_remainder() {
        let mut core = core();
        core.submit_limit(order(1, OrderType::Buy, 50_000, 10));
        let events = core.submit_limit(order(2, OrderType::Sell, 50_000, 4));
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], CoreEvent::Trade { quantity: 4, .. }));
        assert_eq!(core.best_bid(), Some(50_000));
    }

    #[test]
    fn cancel_removes_liquidity() {
        let mut core = core();
        let events = core.submit_limit(order(1, OrderType::Buy, 50_000, 10));
        let order_id = match events[0] {
            CoreEvent::Accepted { order_id, .. } => order_id,
            _ => panic!("expected accepted"),
        };
        assert_eq!(
            core.cancel(CancelCommand {
                user_id: 1,
                order_id
            }),
            CoreEvent::Canceled {
                order_id,
                user_id: 1
            }
        );
        assert_eq!(core.best_bid(), None);
    }

    fn wire_command(side: Side, price: i64, quantity: u64) -> Command {
        Command {
            global_seq: 1,
            gateway_seq: 1,
            ingress_ts_ns: 0,
            exchange_ts_ns: 0,
            client_order_id: 1,
            session_id: 1,
            gateway_id: 1,
            member_id: 1,
            user_id: 1,
            seat_id: 1,
            instrument_id: 1,
            combo_id: 0,
            session_epoch: 1,
            command_type: CommandType::NewOrder,
            price_type: PriceType::Limit,
            time_condition: crate::exchange::protocol::TimeCondition::Gfd,
            volume_condition: crate::exchange::protocol::VolumeCondition::Any,
            side,
            offset: crate::exchange::protocol::Offset::Open,
            hedge: crate::exchange::protocol::Hedge::Spec,
            flags: 0,
            leg2_ratio: 0,
            price_ticks: price,
            quantity,
            trigger_price_ticks: 0,
            leg2_instrument_id: 0,
            leg2_offset: crate::exchange::protocol::Offset::Open,
            leg2_hedge: crate::exchange::protocol::Hedge::Spec,
            trigger_type: crate::exchange::protocol::TriggerType::None,
            cancel_order_id: 0,
        }
    }

    #[test]
    fn wire_command_enters_kernel() {
        let mut core = core();
        let events = core.execute(wire_command(Side::Buy, 50_000, 5));
        assert!(matches!(
            events.as_slice(),
            [CoreEvent::Accepted {
                remaining_quantity: 5,
                ..
            }]
        ));
        assert_eq!(core.best_bid(), Some(50_000));
    }

    #[test]
    fn unsupported_wire_command_is_rejected() {
        let mut core = core();
        let mut command = wire_command(Side::Buy, 50_000, 5);
        command.price_type = PriceType::AdvancedMarket;
        let events = core.execute(command);
        assert!(matches!(events.as_slice(), [CoreEvent::Rejected { .. }]));
    }

    #[test]
    fn close_order_reserves_and_releases_position() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::positions::PositionKey;
        use crate::exchange::protocol::Offset;

        let mut core = core();
        core.positions_mut()
            .apply_open(
                PositionKey::new(1, Side::Buy, crate::exchange::protocol::Hedge::Spec),
                10,
            )
            .unwrap();
        let mut command = wire_command(Side::Sell, 50_000, 5);
        command.offset = Offset::CloseToday;
        let events = core.execute_with_plugins(command, &AvailableClosePlugin);
        assert!(matches!(events.as_slice(), [CoreEvent::Accepted { .. }]));
        let key = crate::exchange::positions::CloseKey::new(
            PositionKey::new(1, Side::Buy, crate::exchange::protocol::Hedge::Spec),
            Offset::CloseToday,
        );
        assert_eq!(core.frozen_close().available(key, core.positions()), 5);

        let mut too_large = wire_command(Side::Sell, 50_000, 6);
        too_large.offset = Offset::CloseToday;
        let events = core.execute_with_plugins(too_large, &AvailableClosePlugin);
        assert!(matches!(events.as_slice(), [CoreEvent::Rejected { .. }]));
    }

    #[test]
    fn fills_update_both_sides_positions() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::positions::PositionKey;
        use crate::exchange::protocol::{Hedge, Offset};

        let mut core = core();
        let mut buy = wire_command(Side::Buy, 50_000, 5);
        buy.offset = Offset::Open;
        core.execute_with_plugins(buy, &AvailableClosePlugin);
        let mut sell = wire_command(Side::Sell, 50_000, 5);
        sell.user_id = 2;
        sell.offset = Offset::Open;
        core.execute_with_plugins(sell, &AvailableClosePlugin);

        let long = core
            .positions()
            .get(PositionKey::new(1, Side::Buy, Hedge::Spec));
        let short = core
            .positions()
            .get(PositionKey::new(2, Side::Sell, Hedge::Spec));
        assert_eq!(long.today, 5);
        assert_eq!(short.today, 5);
    }

    #[test]
    fn close_fill_reduces_position_and_releases_freeze() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::positions::{CloseKey, PositionKey};
        use crate::exchange::protocol::{Hedge, Offset};

        let mut core = core();
        core.positions_mut()
            .apply_open(PositionKey::new(1, Side::Buy, Hedge::Spec), 10)
            .unwrap();
        // Resting buy from another account provides the close liquidity.
        let mut buy = wire_command(Side::Buy, 50_000, 5);
        buy.user_id = 2;
        core.execute_with_plugins(buy, &AvailableClosePlugin);

        let mut close = wire_command(Side::Sell, 50_000, 5);
        close.offset = Offset::CloseToday;
        core.execute_with_plugins(close, &AvailableClosePlugin);

        let key = PositionKey::new(1, Side::Buy, Hedge::Spec);
        assert_eq!(core.positions().get(key).today, 5);
        let close_key = CloseKey::new(key, Offset::CloseToday);
        assert_eq!(
            core.frozen_close().available(close_key, core.positions()),
            5
        );
    }

    #[test]
    fn order_status_tracks_accept_fill_and_cancel() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::Offset;

        let mut core = core();
        let mut buy = wire_command(Side::Buy, 50_000, 5);
        buy.offset = Offset::Open;
        let events = core.execute_with_plugins(buy, &AvailableClosePlugin);
        let order_id = match events[0] {
            CoreEvent::Accepted { order_id, .. } => order_id,
            _ => panic!("expected accepted"),
        };
        assert_eq!(core.order_status(order_id), Some(OrderStatus::Accepted));

        let mut sell = wire_command(Side::Sell, 50_000, 2);
        sell.user_id = 2;
        sell.offset = Offset::Open;
        core.execute_with_plugins(sell, &AvailableClosePlugin);
        assert_eq!(
            core.order_status(order_id),
            Some(OrderStatus::PartiallyFilled)
        );

        core.cancel(CancelCommand {
            user_id: 1,
            order_id,
        });
        assert_eq!(core.order_status(order_id), Some(OrderStatus::Canceled));
    }

    #[test]
    fn fully_filled_order_is_marked_filled() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::Offset;

        let mut core = core();
        let mut buy = wire_command(Side::Buy, 50_000, 5);
        buy.offset = Offset::Open;
        core.execute_with_plugins(buy, &AvailableClosePlugin);
        let mut sell = wire_command(Side::Sell, 50_000, 5);
        sell.user_id = 2;
        sell.offset = Offset::Open;
        let events = core.execute_with_plugins(sell, &AvailableClosePlugin);
        let incoming_id = match events
            .iter()
            .find(|event| matches!(event, CoreEvent::Trade { .. }))
        {
            Some(CoreEvent::Trade {
                seller_order_id, ..
            }) => *seller_order_id,
            _ => panic!("expected trade"),
        };
        assert_eq!(core.order_status(incoming_id), Some(OrderStatus::Filled));
    }

    #[test]
    fn private_events_follow_accept_partial_and_cancel() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::private_events::PrivateStatus;
        use crate::exchange::protocol::Offset;

        let mut core = core();
        let mut buy = wire_command(Side::Buy, 50_000, 5);
        buy.offset = Offset::Open;
        let events = core.execute_with_plugins(buy, &AvailableClosePlugin);
        let order_id = match events[0] {
            CoreEvent::Accepted { order_id, .. } => order_id,
            _ => panic!("expected accepted"),
        };
        let private = core.drain_private_events();
        assert_eq!(private.len(), 1);
        assert_eq!(private[0].status, PrivateStatus::Accepted);
        assert_eq!(private[0].order_id, order_id);
        assert_eq!(private[0].remaining, 5);

        let mut sell = wire_command(Side::Sell, 50_000, 2);
        sell.user_id = 2;
        core.execute_with_plugins(sell, &AvailableClosePlugin);
        let private = core.drain_private_events();
        assert!(private.iter().any(|event| {
            event.order_id == order_id
                && event.status == PrivateStatus::PartiallyFilled
                && event.remaining == 3
        }));

        core.cancel(CancelCommand {
            user_id: 1,
            order_id,
        });
        let private = core.drain_private_events();
        assert!(private.iter().any(|event| {
            event.order_id == order_id && event.status == PrivateStatus::Canceled
        }));
    }

    #[test]
    fn invalid_quantity_emits_private_reject() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::private_events::PrivateStatus;
        use crate::exchange::protocol::Offset;

        let mut core = core();
        let mut command = wire_command(Side::Buy, 50_000, 0);
        command.offset = Offset::Open;
        let events = core.execute_with_plugins(command, &AvailableClosePlugin);
        assert!(matches!(events.as_slice(), [CoreEvent::Rejected { .. }]));
        let private = core.drain_private_events();
        assert_eq!(private.len(), 1);
        assert_eq!(private[0].status, PrivateStatus::Rejected);
    }

    #[test]
    fn fak_cancels_unmatched_remainder() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::{Offset, TimeCondition, VolumeCondition};

        let mut core = core();
        let mut sell = wire_command(Side::Sell, 50_000, 5);
        sell.user_id = 2;
        sell.offset = Offset::Open;
        core.execute_with_plugins(sell, &AvailableClosePlugin);

        let mut buy = wire_command(Side::Buy, 50_000, 10);
        buy.offset = Offset::Open;
        buy.time_condition = TimeCondition::Ioc;
        buy.volume_condition = VolumeCondition::Minimum;
        let events = core.execute_with_plugins(buy, &AvailableClosePlugin);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::Trade { quantity: 5, .. }))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::Canceled { .. }))
        );
        assert_eq!(core.best_bid(), None);
        assert_eq!(core.best_ask(), None);
    }

    #[test]
    fn fok_is_rejected_before_trading_in_this_slice() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::{Offset, TimeCondition, VolumeCondition};

        let mut core = core();
        let mut sell = wire_command(Side::Sell, 50_000, 5);
        sell.user_id = 2;
        sell.offset = Offset::Open;
        core.execute_with_plugins(sell, &AvailableClosePlugin);

        let mut buy = wire_command(Side::Buy, 50_000, 10);
        buy.offset = Offset::Open;
        buy.time_condition = TimeCondition::Ioc;
        buy.volume_condition = VolumeCondition::All;
        let events = core.execute_with_plugins(buy, &AvailableClosePlugin);
        assert!(matches!(events.as_slice(), [CoreEvent::Rejected { .. }]));
        assert_eq!(core.best_ask(), Some(50_000));
    }

    #[test]
    fn fok_fills_atomically_when_liquidity_is_sufficient() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::{Offset, TimeCondition, VolumeCondition};

        let mut core = core();
        let mut sell = wire_command(Side::Sell, 50_000, 10);
        sell.user_id = 2;
        sell.offset = Offset::Open;
        core.execute_with_plugins(sell, &AvailableClosePlugin);

        let mut buy = wire_command(Side::Buy, 50_000, 10);
        buy.offset = Offset::Open;
        buy.time_condition = TimeCondition::Ioc;
        buy.volume_condition = VolumeCondition::All;
        let events = core.execute_with_plugins(buy, &AvailableClosePlugin);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::Trade { quantity: 10, .. })),
            "unexpected events: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, CoreEvent::Canceled { .. }))
        );
        assert_eq!(core.best_ask(), None);
    }

    #[test]
    fn market_buy_sweeps_multiple_price_levels() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::{Offset, PriceType};

        let mut core = core();
        let mut sell1 = wire_command(Side::Sell, 50_000, 5);
        sell1.user_id = 2;
        sell1.offset = Offset::Open;
        core.execute_with_plugins(sell1, &AvailableClosePlugin);
        let mut sell2 = wire_command(Side::Sell, 50_100, 5);
        sell2.user_id = 3;
        sell2.offset = Offset::Open;
        core.execute_with_plugins(sell2, &AvailableClosePlugin);

        let mut buy = wire_command(Side::Buy, 0, 8);
        buy.offset = Offset::Open;
        buy.price_type = PriceType::Market;
        let events = core.execute_with_plugins(buy, &AvailableClosePlugin);
        let traded: u64 = events
            .iter()
            .filter_map(|event| match event {
                CoreEvent::Trade { quantity, .. } => Some(*quantity),
                _ => None,
            })
            .sum();
        assert_eq!(traded, 8);
        assert_eq!(core.best_ask(), Some(50_100));
    }

    #[test]
    fn market_order_cancels_unfilled_remainder() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::{Offset, PriceType};

        let mut core = core();
        let mut sell = wire_command(Side::Sell, 50_000, 5);
        sell.user_id = 2;
        sell.offset = Offset::Open;
        core.execute_with_plugins(sell, &AvailableClosePlugin);

        let mut buy = wire_command(Side::Buy, 0, 8);
        buy.offset = Offset::Open;
        buy.price_type = PriceType::Market;
        let events = core.execute_with_plugins(buy, &AvailableClosePlugin);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::Canceled { .. }))
        );
        assert_eq!(core.best_ask(), None);
        assert_eq!(core.best_bid(), None);
    }

    #[test]
    fn auction_collects_and_uncrosses() {
        let mut core = core();
        core.begin_auction();
        core.queue_auction_order(1, Side::Buy, 50_000, 10, Offset::Open, Hedge::Spec);
        core.queue_auction_order(2, Side::Sell, 49_900, 8, Offset::Open, Hedge::Spec);
        assert_eq!(core.auction_order_count(), 2);
        let result = core.uncross_auction(50_000);
        assert_eq!(result.price_ticks, Some(50_000));
        assert_eq!(result.volume, 8);
        assert_eq!(core.auction_order_count(), 0);
        assert_eq!(core.best_bid(), Some(50_000));
        assert_eq!(core.best_ask(), None);
        assert_eq!(
            core.positions()
                .get(PositionKey::new(1, Side::Buy, Hedge::Spec))
                .today,
            8
        );
        assert_eq!(
            core.positions()
                .get(PositionKey::new(2, Side::Sell, Hedge::Spec))
                .today,
            8
        );
        let public = core.drain_public_events();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].event_type, PublicEventType::AuctionResult);
        assert_eq!(public[0].price_ticks, 50_000);
        assert_eq!(public[0].volume, 8);
    }

    #[test]
    fn public_events_include_rested_order_and_trade() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::Offset;

        let mut core = core();
        let mut buy = wire_command(Side::Buy, 50_000, 5);
        buy.offset = Offset::Open;
        core.execute_with_plugins(buy, &AvailableClosePlugin);
        let public = core.drain_public_events();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].event_type, PublicEventType::OrderRested);
        assert_eq!(public[0].quantity, 5);

        let mut sell = wire_command(Side::Sell, 50_000, 5);
        sell.user_id = 2;
        sell.offset = Offset::Open;
        core.execute_with_plugins(sell, &AvailableClosePlugin);
        let public = core.drain_public_events();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].event_type, PublicEventType::Trade);
        assert_eq!(public[0].quantity, 5);
        assert_eq!(public[0].price_ticks, 50_000);
        assert_eq!(public[0].event_type, PublicEventType::Trade);
        assert_ne!(public[0].buy_order_id, 0);
        assert_ne!(public[0].sell_order_id, 0);
    }

    #[test]
    fn public_order_canceled_is_emitted_for_rested_orders() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::Offset;

        let mut core = core();
        let mut buy = wire_command(Side::Buy, 50_000, 5);
        buy.offset = Offset::Open;
        let events = core.execute_with_plugins(buy, &AvailableClosePlugin);
        let order_id = match events[0] {
            CoreEvent::Accepted { order_id, .. } => order_id,
            _ => panic!("expected accepted"),
        };
        core.drain_public_events();
        core.cancel(CancelCommand {
            user_id: 1,
            order_id,
        });
        let public = core.drain_public_events();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].event_type, PublicEventType::OrderCanceled);
        assert_eq!(public[0].order_id, order_id);
        assert_eq!(public[0].quantity, 5);
    }

    #[test]
    fn combo_queue_orders_by_spread_price() {
        let mut core = core();
        let a = core
            .submit_combo_order(7, 1, Side::Buy, 100, 5, Offset::Open, Hedge::Spec)
            .unwrap();
        let b = core
            .submit_combo_order(7, 1, Side::Buy, 110, 5, Offset::Open, Hedge::Spec)
            .unwrap();
        assert_eq!(core.combo_queue_len(7), 2);
        let queue = core.combo_queues.get_mut(&7).unwrap();
        assert_eq!(queue.best_bid(), Some(110));
        assert_eq!(queue.pop_best(Side::Buy).unwrap().order_id, b);
        assert_eq!(queue.best_bid(), Some(100));
        assert_eq!(queue.pop_best(Side::Buy).unwrap().order_id, a);
    }

    #[test]
    fn combo_order_can_be_canceled() {
        let mut core = core();
        let order_id = core
            .submit_combo_order(7, 1, Side::Sell, 90, 5, Offset::Open, Hedge::Spec)
            .unwrap();
        assert!(core.cancel_combo_order(7, order_id));
        assert_eq!(core.combo_queue_len(7), 0);
    }

    #[test]
    fn session_switch_emits_public_event() {
        let mut core = core();
        core.set_phase(ExchangePhase::Break, 2);
        assert_eq!(core.phase(), ExchangePhase::Break);
        assert_eq!(core.trading_day(), 2);
        let public = core.drain_public_events();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].event_type, PublicEventType::SessionSwitch);
        assert_eq!(public[0].phase, ExchangePhase::Break.code());
    }

    #[test]
    fn closed_phase_rejects_new_orders() {
        use crate::exchange::plugins::AvailableClosePlugin;
        use crate::exchange::protocol::Offset;

        let mut core = core();
        core.set_phase(ExchangePhase::Closed, 1);
        core.drain_public_events();
        let mut buy = wire_command(Side::Buy, 50_000, 5);
        buy.offset = Offset::Open;
        let events = core.execute_with_plugins(buy, &AvailableClosePlugin);
        assert!(matches!(events.as_slice(), [CoreEvent::Rejected { .. }]));
    }

    #[test]
    fn time_tick_advances_public_time() {
        let mut core = core();
        let mut command = wire_command(Side::Buy, 50_000, 1);
        command.command_type = CommandType::TimeTick;
        command.global_seq = 42;
        command.exchange_ts_ns = 123;
        let events = core.execute(command);
        assert!(events.is_empty());
        assert_eq!(core.last_tick_seq(), 42);
        assert_eq!(core.last_exchange_ts_ns(), 123);
        let public = core.drain_public_events();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].event_type, PublicEventType::TimeTick);
        assert_eq!(public[0].exchange_ts_ns, 123);
    }

    #[test]
    fn control_command_switches_phase() {
        let mut core = core();
        let mut command = wire_command(Side::Buy, 50_000, 1);
        command.command_type = CommandType::Control;
        command.flags = ExchangePhase::Break.code();
        command.quantity = 3;
        core.execute(command);
        assert_eq!(core.phase(), ExchangePhase::Break);
        assert_eq!(core.trading_day(), 3);
        let public = core.drain_public_events();
        assert_eq!(public[0].event_type, PublicEventType::SessionSwitch);
    }

    #[test]
    fn same_command_sequence_is_deterministic() {
        let commands = [
            order(1, OrderType::Buy, 50_000, 7),
            order(2, OrderType::Sell, 50_100, 5),
            order(3, OrderType::Buy, 50_100, 5),
            order(4, OrderType::Sell, 49_900, 9),
        ];

        let run = || {
            let mut core = core();
            commands
                .into_iter()
                .flat_map(|command| core.submit_limit(command))
                .collect::<Vec<_>>()
        };

        assert_eq!(run(), run());
    }
    #[test]
    fn snapshot_restores_book_orders_and_positions() {
        use crate::exchange::positions::PositionKey;

        let mut core = core();
        let resting = core.submit_limit(order(1, OrderType::Buy, 49_000, 5));
        let resting_id = resting
            .iter()
            .find_map(|event| match event {
                CoreEvent::Accepted { order_id, .. } => Some(*order_id),
                _ => None,
            })
            .unwrap();
        let long_key = PositionKey::new(1, Side::Buy, Hedge::Spec);
        core.positions_mut().apply_open(long_key, 3).unwrap();

        let snapshot = core.snapshot();
        let _ = core.submit_limit(order(1, OrderType::Buy, 48_000, 5));
        core.positions_mut().apply_open(long_key, 2).unwrap();
        assert_eq!(core.positions().get(long_key).today, 5);

        core.restore_snapshot(snapshot.clone()).unwrap();
        assert_eq!(core.positions().get(long_key).today, 3);
        assert_eq!(core.phase(), snapshot.phase);
        assert_eq!(core.snapshot(), snapshot);
        let _ = resting_id;
    }
    #[test]
    fn snapshot_json_and_file_roundtrip() {
        use crate::exchange::positions::PositionKey;

        let mut state = core();
        let _ = state.submit_limit(order(1, OrderType::Buy, 49_000, 5));
        state
            .positions_mut()
            .apply_open(PositionKey::new(1, Side::Buy, Hedge::Spec), 2)
            .unwrap();
        let expected = state.snapshot();
        let json = state.snapshot_json().unwrap();
        let mut restored = core();
        restored.restore_snapshot_json(&json).unwrap();
        assert_eq!(restored.snapshot(), expected);

        let path = std::env::temp_dir().join(format!(
            "matching-engine-snapshot-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        state.save_snapshot(&path).unwrap();
        let loaded = ExchangeCore::load_snapshot(&path).unwrap();
        assert_eq!(loaded, expected);

        let mut tampered: serde_json::Value = serde_json::from_str(&json).unwrap();
        tampered["checksum"] = serde_json::Value::from(0);
        let mut target = core();
        assert!(
            target
                .restore_snapshot_json(&tampered.to_string())
                .unwrap_err()
                .contains("checksum")
        );
        let _ = std::fs::remove_file(&path);
    }
}
