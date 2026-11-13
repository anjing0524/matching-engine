//! Deterministic trading core kernel.
//!
//! The kernel owns one contract's book, active orders, positions, frozen close,
//! and auction staging. It has no network, shared memory,
//! wall clock, or background work; snapshots are explicit method calls.
//!
//! Supported command shapes include limit/market/FAK/FOK, cancel, and auction
//! staging/uncross. Combo execution lives in the registry combo path. The same
//! command sequence always produces the same private/public projections.

use crate::domain::orderbook::{ContractSpec, RestingOrder, TickBasedOrderBook};
use crate::shared::SymbolId;
use crate::shared::collections::identity::U64MixMap;
use crate::shared::protocol::{NewOrderRequest, OrderType};
use crate::trading::core::auction::{self, AuctionOrder, AuctionResult};
use crate::trading::core::leg_liquidity::{
    LegExecutionContext, LegFill, LegLiquidityError, LegLiquidityExecution, LegLiquidityPlan,
    PlannedLegFill,
};
use crate::trading::core::plugins::NoPlugins;
use crate::trading::core::plugins::{PreTradePlugin, RiskOrder, ValidationContext};
use crate::trading::core::positions::{CloseKey, FrozenClose, PositionKey, Positions};
use crate::trading::core::private_events::{PrivateEvent, PrivateStatus};
use crate::trading::core::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, VolumeCondition,
};
use crate::trading::core::public_events::{PublicEvent, PublicEventType, TradingPhase};

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

struct DiscardEvents;

impl Extend<CoreEvent> for DiscardEvents {
    fn extend<T: IntoIterator<Item = CoreEvent>>(&mut self, iter: T) {
        for _ in iter {}
    }
}

/// One accepted resting order before it is projected into API events.
#[derive(Clone, Copy, Debug)]
struct AcceptedOrder {
    order_id: u64,
    user_id: u64,
    side: OrderType,
    price_ticks: u64,
    remaining: u64,
}

/// One matching result before it is projected into public API events.
#[derive(Debug, Default)]
struct SubmitOutcome {
    trades: smallvec::SmallVec<[CoreEvent; 8]>,
    accepted: Option<AcceptedOrder>,
    rejected: Option<String>,
}

impl SubmitOutcome {
    fn events_into<E: Extend<CoreEvent>>(&self, client_order_id: u64, out: &mut E) {
        out.extend(self.trades.iter().cloned());
        if let Some(accepted) = self.accepted {
            out.extend([CoreEvent::Accepted {
                order_id: accepted.order_id,
                user_id: accepted.user_id,
                side: accepted.side,
                price_ticks: accepted.price_ticks,
                remaining_quantity: accepted.remaining,
            }]);
        }
        if let Some(reason) = &self.rejected {
            out.extend([CoreEvent::Rejected {
                client_order_id,
                reason: reason.clone(),
            }]);
        }
    }

    fn incoming_id(&self, side: Side) -> Option<u64> {
        self.accepted.map(|accepted| accepted.order_id).or_else(|| {
            self.trades.iter().find_map(|event| match event {
                CoreEvent::Trade {
                    buyer_order_id,
                    seller_order_id,
                    ..
                } => Some(match side {
                    Side::Buy => *buyer_order_id,
                    Side::Sell => *seller_order_id,
                }),
                _ => None,
            })
        })
    }
}

/// Lifecycle status of an trading order.
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
    /// True when this order was published as resting public state.
    pub public: bool,
}

/// In-memory core snapshot for the continuous book and business state.
///
/// Call-auction staging is included in this snapshot.
/// Ephemeral diagnostic event buffers are not snapshotted because they are
/// already consumed by middleware.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CoreSnapshot {
    pub spec: ContractSpec,
    pub symbol: SymbolId,
    pub resting_orders: Vec<crate::domain::orderbook::RestingOrder>,
    pub next_order_id: u64,
    pub orders: Vec<(u64, OrderMeta)>,
    pub positions: Vec<(PositionKey, crate::trading::core::positions::Position)>,
    pub pending_open: Vec<(PositionKey, crate::trading::core::positions::Position)>,
    pub frozen_close: Vec<(CloseKey, u64)>,
    pub auction_orders: Vec<AuctionOrder>,
    pub phase: TradingPhase,
    pub trading_day: u32,
    pub next_trade_id: u64,
    pub private_seq: u64,
    pub public_seq: u64,
    pub last_tick_seq: u64,
    pub last_trading_ts_ns: u64,
}

/// The deterministic core state machine for one contract.
pub struct TradingCore {
    book: TickBasedOrderBook,
    symbol: SymbolId,
    next_trade_id: u64,
    positions: Positions,
    pending_open: Positions,
    frozen_close: FrozenClose,
    orders: U64MixMap<OrderMeta>,
    private_seq: u64,
    private_events: Vec<PrivateEvent>,
    auction_orders: Vec<AuctionOrder>,
    public_seq: u64,
    public_events: Vec<PublicEvent>,
    phase: TradingPhase,
    trading_day: u32,
    last_tick_seq: u64,
    last_trading_ts_ns: u64,
}

impl TradingCore {
    #[must_use]
    pub fn new(spec: ContractSpec, symbol: SymbolId) -> Self {
        Self {
            book: TickBasedOrderBook::new(spec),
            symbol,
            next_trade_id: 1,
            positions: Positions::new(),
            pending_open: Positions::new(),
            frozen_close: FrozenClose::new(),
            orders: U64MixMap::default(),
            private_seq: 1,
            private_events: Vec::new(),
            auction_orders: Vec::new(),
            public_seq: 1,
            public_events: Vec::new(),
            phase: TradingPhase::Continuous,
            trading_day: 1,
            last_tick_seq: 0,
            last_trading_ts_ns: 0,
        }
    }

    /// Captures continuous-book and business state.
    ///
    /// Captures the full continuous, combo, and auction state.
    #[must_use]
    pub fn snapshot(&self) -> CoreSnapshot {
        let mut orders: Vec<_> = self.orders.iter().map(|(id, meta)| (*id, *meta)).collect();
        orders.sort_by_key(|(id, _)| *id);
        let mut positions = self.positions.snapshot();
        positions.sort_by_key(|(key, _)| (key.account_id, key.side as u8, key.hedge as u8));
        let mut pending_open = self.pending_open.snapshot();
        pending_open.sort_by_key(|(key, _)| (key.account_id, key.side as u8, key.hedge as u8));
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
            pending_open,
            frozen_close,
            auction_orders: self.auction_orders.clone(),
            phase: self.phase,
            trading_day: self.trading_day,
            next_trade_id: self.next_trade_id,
            private_seq: self.private_seq,
            public_seq: self.public_seq,
            last_tick_seq: self.last_tick_seq,
            last_trading_ts_ns: self.last_trading_ts_ns,
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
        self.positions.restore(snapshot.positions);
        self.pending_open.restore(snapshot.pending_open);
        self.frozen_close.restore(snapshot.frozen_close);
        self.auction_orders = snapshot.auction_orders;
        self.phase = snapshot.phase;
        self.trading_day = snapshot.trading_day;
        self.next_trade_id = snapshot.next_trade_id;
        self.private_seq = snapshot.private_seq;
        self.public_seq = snapshot.public_seq;
        self.last_tick_seq = snapshot.last_tick_seq;
        self.last_trading_ts_ns = snapshot.last_trading_ts_ns;
        self.private_events.clear();
        self.public_events.clear();
        Ok(())
    }

    #[must_use]
    pub fn positions(&self) -> &Positions {
        &self.positions
    }

    pub fn positions_mut(&mut self) -> &mut Positions {
        &mut self.positions
    }

    /// Applies the combo owner's side of one atomic Combo Fill to this Contract.
    ///
    /// Combo orders have no position of their own, so the leg Contract carries
    /// the owner's open position. M1 combo fills open spec positions and emit
    /// no ordinary private fill: the combo owner receives a Combo Fill instead.
    pub(crate) fn apply_combo_fill_position(&mut self, account_id: u64, side: Side, quantity: u64) {
        let key = PositionKey::new(account_id, side, Hedge::Spec);
        let _ = self.positions.apply_open(key, quantity);
    }

    #[must_use]
    pub fn pending_open(&self) -> &Positions {
        &self.pending_open
    }

    #[must_use]
    pub fn frozen_close(&self) -> &FrozenClose {
        &self.frozen_close
    }

    /// Drains all private events produced since the last drain.
    /// Drains private events into a reusable buffer.
    pub fn drain_private_events_into(&mut self, out: &mut Vec<PrivateEvent>) {
        out.clear();
        out.append(&mut self.private_events);
    }

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
        let taker_side = match side {
            Side::Buy => OrderType::Buy,
            Side::Sell => OrderType::Sell,
        };
        self.book.executable_quantity(taker_side, price_ticks) >= quantity
    }

    /// Drains public events into a reusable buffer.
    pub fn drain_public_events_into(&mut self, out: &mut Vec<PublicEvent>) {
        out.clear();
        out.append(&mut self.public_events);
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
            trading_ts_ns: 0,
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
            pair_id: 0,
            spread_price: 0,
            leg1_price_ticks: 0,
            leg2_price_ticks: 0,
        });
    }

    fn reject_private_into<E: Extend<CoreEvent>>(
        &mut self,
        command: &Command,
        reason: String,
        events: &mut E,
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
        events.extend([CoreEvent::Rejected {
            client_order_id: command.client_order_id,
            reason,
        }]);
    }

    #[must_use]
    pub fn phase(&self) -> TradingPhase {
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
    pub fn last_trading_ts_ns(&self) -> u64 {
        self.last_trading_ts_ns
    }

    /// Returns the contract specification this core was built with.
    #[must_use]
    pub fn spec(&self) -> &ContractSpec {
        self.book.spec()
    }

    /// Switches the trading phase and emits a public SessionSwitch event.
    pub fn set_phase(&mut self, phase: TradingPhase, trading_day: u32) {
        self.phase = phase;
        self.trading_day = trading_day;
        self.push_public_event(PublicEvent {
            seq: 0,
            event_type: PublicEventType::SessionSwitch,
            trading_ts_ns: 0,
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
            pair_id: 0,
            spread_price: 0,
            leg1_price_ticks: 0,
            leg2_price_ticks: 0,
        });
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
        order_id
    }

    /// Uncrosses the collected auction orders at the maximum-volume price.
    pub fn uncross_auction(&mut self, previous_settlement: u64) -> AuctionResult {
        let orders = std::mem::take(&mut self.auction_orders);
        let metadata: U64MixMap<(u64, Side, Offset, Hedge)> = orders
            .iter()
            .map(|order| {
                (
                    order.order_id,
                    (order.user_id, order.side, order.offset, order.hedge),
                )
            })
            .collect();
        let result = auction::uncross(&orders, previous_settlement);
        for trade in &result.trades {
            let buyer = metadata.get(&trade.buyer_order_id).copied();
            let seller = metadata.get(&trade.seller_order_id).copied();
            self.apply_auction_fill(buyer, trade.quantity);
            self.apply_auction_fill(seller, trade.quantity);
            self.push_private(
                trade.buyer_order_id,
                0,
                buyer.map_or(0, |meta| meta.0),
                PrivateStatus::Filled,
                trade.quantity,
                0,
                None,
            );
            self.push_private(
                trade.seller_order_id,
                0,
                seller.map_or(0, |meta| meta.0),
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
                        public: true,
                    },
                );
            }
        }
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
    #[cfg(test)]
    pub fn submit_limit(&mut self, command: LimitOrderCommand) -> Vec<CoreEvent> {
        let outcome = self.submit_limit_outcome(command);
        let mut events = Vec::new();
        outcome.events_into(command.client_order_id, &mut events);
        events
    }

    fn submit_limit_outcome(&mut self, command: LimitOrderCommand) -> SubmitOutcome {
        let request = NewOrderRequest {
            user_id: command.user_id,
            client_order_id: command.client_order_id,
            symbol: self.symbol,
            order_type: command.side,
            price: command.price_ticks,
            quantity: command.quantity,
        };

        let mut outcome = SubmitOutcome::default();
        match self.book.match_order(request) {
            crate::domain::orderbook::MatchOutcome::Filled { trades } => {
                self.trade_events_into(trades, &mut outcome.trades);
            }
            crate::domain::orderbook::MatchOutcome::Rested {
                trades,
                confirmation,
            } => {
                self.trade_events_into(trades, &mut outcome.trades);
                let remaining = self
                    .book
                    .get_order_status(confirmation.order_id)
                    .map_or(command.quantity, |status| status.remaining_quantity);
                outcome.accepted = Some(AcceptedOrder {
                    order_id: confirmation.order_id,
                    user_id: command.user_id,
                    side: command.side,
                    price_ticks: command.price_ticks,
                    remaining,
                });
            }
            crate::domain::orderbook::MatchOutcome::Rejected { reason, trades } => {
                self.trade_events_into(trades, &mut outcome.trades);
                outcome.rejected = Some(reason);
            }
        }
        outcome
    }

    /// Cancels a resting order.    /// Cancels a resting order. A missing/foreign order is rejected locally.
    pub fn cancel(&mut self, command: CancelCommand) -> CoreEvent {
        match self.book.cancel_order(command.order_id, command.user_id) {
            Ok(()) => {
                if let Some(mut meta) = self.orders.remove(&command.order_id) {
                    if meta.offset == Offset::Open {
                        self.pending_open.release_open(
                            PositionKey::new(meta.account_id, meta.side, meta.hedge),
                            meta.remaining,
                        );
                    }
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
                    let public = meta.public;
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
                    if public {
                        self.push_public_event(PublicEvent {
                            seq: 0,
                            event_type: PublicEventType::OrderCanceled,
                            trading_ts_ns: 0,
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
                            pair_id: 0,
                            spread_price: 0,
                            leg1_price_ticks: 0,
                            leg2_price_ticks: 0,
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
    /// resting order keeps it until it fills or is canceled. Fills update
    /// positions and pending-open exposure.
    pub fn execute_with_plugins<P: PreTradePlugin>(
        &mut self,
        command: Command,
        plugin: &P,
    ) -> Vec<CoreEvent> {
        let mut events = Vec::new();
        self.execute_with_plugins_into(command, plugin, &mut events);
        events
    }

    /// Executes a command without materializing its `CoreEvent` view.
    pub fn execute_with_plugins_discard<P: PreTradePlugin>(
        &mut self,
        command: Command,
        plugin: &P,
    ) {
        self.execute_with_plugins_into(command, plugin, &mut DiscardEvents);
    }

    /// Appends the command's events into `out` without a per-command Vec.
    #[allow(clippy::too_many_lines)]
    pub fn execute_with_plugins_into<P: PreTradePlugin, E: Extend<CoreEvent>>(
        &mut self,
        command: Command,
        plugin: &P,
        out: &mut E,
    ) {
        if command.command_type != CommandType::NewOrder {
            out.extend(self.execute(command));
            return;
        }
        if !matches!(
            self.phase,
            TradingPhase::Continuous | TradingPhase::OpeningAuction
        ) {
            self.reject_private_into(
                &command,
                "trading phase rejects new orders".to_string(),
                out,
            );
            return;
        }
        if self.phase == TradingPhase::OpeningAuction
            && (command.price_type == PriceType::Market
                || command.time_condition == TimeCondition::Ioc
                || command.volume_condition == VolumeCondition::All)
        {
            self.reject_private_into(
                &command,
                "auction rejects market/FAK/FOK orders".to_string(),
                out,
            );
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
            pending_open: &self.pending_open,
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

        let outcome = self.submit_limit_outcome(LimitOrderCommand {
            user_id: u64::from(command.user_id),
            client_order_id: command.client_order_id,
            side,
            price_ticks: matching_price,
            quantity: command.quantity,
        });
        outcome.events_into(command.client_order_id, out);

        let incoming_id = outcome.incoming_id(command.side);
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
                    public: false,
                },
            );
        }

        if let Some(reason) = &outcome.rejected {
            self.push_private(
                0,
                command.client_order_id,
                u64::from(command.user_id),
                PrivateStatus::Rejected,
                0,
                0,
                Some(reason.clone()),
            );
        }
        if let Some(accepted) = outcome.accepted {
            self.push_private(
                accepted.order_id,
                command.client_order_id,
                u64::from(command.user_id),
                PrivateStatus::Accepted,
                0,
                accepted.remaining,
                None,
            );
            if command.offset == Offset::Open {
                let _ = self.pending_open.apply_open(
                    PositionKey::new(u64::from(command.user_id), command.side, command.hedge),
                    accepted.remaining,
                );
            }
        }

        for event in &outcome.trades {
            if let CoreEvent::Trade {
                buyer_order_id,
                seller_order_id,
                quantity,
                ..
            } = event
            {
                self.apply_fill(*buyer_order_id, *quantity);
                self.apply_fill(*seller_order_id, *quantity);
            }
        }

        if let Some(key) = close_key
            && outcome.accepted.is_none()
        {
            let filled = outcome
                .trades
                .iter()
                .filter_map(|event| match event {
                    CoreEvent::Trade { quantity, .. } => Some(*quantity),
                    _ => None,
                })
                .sum::<u64>();
            self.frozen_close
                .release(key, command.quantity.saturating_sub(filled));
        }

        if let Some(accepted) = outcome.accepted
            && command.price_type == PriceType::Limit
            && command.time_condition != TimeCondition::Ioc
        {
            self.push_public_event(PublicEvent {
                seq: 0,
                event_type: PublicEventType::OrderRested,
                trading_ts_ns: command.trading_ts_ns,
                phase: 0,
                instrument_id: command.instrument_id,
                order_id: accepted.order_id,
                trade_id: 0,
                buy_order_id: 0,
                sell_order_id: 0,
                side: Some(command.side),
                price_ticks: command.price_ticks as u64,
                quantity: accepted.remaining,
                volume: 0,
                pair_id: 0,
                spread_price: 0,
                leg1_price_ticks: 0,
                leg2_price_ticks: 0,
            });
            if let Some(meta) = self.orders.get_mut(&accepted.order_id) {
                meta.public = true;
            }
        }
        for event in &outcome.trades {
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
                    trading_ts_ns: command.trading_ts_ns,
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
                    pair_id: 0,
                    spread_price: 0,
                    leg1_price_ticks: 0,
                    leg2_price_ticks: 0,
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
            out.extend([canceled]);
        }
    }

    fn apply_auction_fill(&mut self, metadata: Option<(u64, Side, Offset, Hedge)>, quantity: u64) {
        let Some((account_id, side, offset, hedge)) = metadata else {
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
            self.pending_open.release_open(key, quantity);
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
        if remaining == 0 {
            self.orders.remove(&order_id);
        }
        let position_side = if offset == Offset::Open {
            side
        } else {
            side.opposite()
        };
        let key = PositionKey::new(account_id, position_side, hedge);
        if offset == Offset::Open {
            let _ = self.positions.apply_open(key, quantity);
            self.pending_open.release_open(key, quantity);
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
                self.last_trading_ts_ns = command.trading_ts_ns;
                self.push_public_event(PublicEvent {
                    seq: 0,
                    event_type: PublicEventType::TimeTick,
                    trading_ts_ns: command.trading_ts_ns,
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
                    pair_id: 0,
                    spread_price: 0,
                    leg1_price_ticks: 0,
                    leg2_price_ticks: 0,
                });
                Vec::new()
            }
            CommandType::Control => {
                if let Some(phase) = TradingPhase::from_code(command.flags) {
                    self.set_phase(phase, command.quantity as u32);
                }
                Vec::new()
            }
            CommandType::NewOrder => self.execute_with_plugins(command, &NoPlugins),
            CommandType::Cancel => vec![self.cancel(CancelCommand {
                user_id: u64::from(command.user_id),
                order_id: command.cancel_order_id,
            })],
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

    /// Plans an all-or-none consumption of resting liquidity on this Contract.
    ///
    /// The plan walks orders that cross `limit_price_ticks` for `taker_side` in
    /// price then time priority and selects exactly `quantity`. It never mutates
    /// the book. A combo matcher uses it to inspect leg liquidity without
    /// gaining access to the underlying book.
    ///
    /// # Errors
    /// Returns [`LegLiquidityError::ZeroQuantity`] for a zero request and
    /// [`LegLiquidityError::InsufficientLiquidity`] when the crossing book
    /// cannot cover the full quantity.
    pub fn plan_leg_liquidity(
        &self,
        taker_side: Side,
        limit_price_ticks: u64,
        quantity: u64,
    ) -> Result<LegLiquidityPlan, LegLiquidityError> {
        if quantity == 0 {
            return Err(LegLiquidityError::ZeroQuantity);
        }
        let mut fills = Vec::new();
        let mut remaining = quantity;
        let mut available = 0_u64;
        for order in self.crossing_orders(taker_side, limit_price_ticks) {
            available = available.saturating_add(order.quantity);
            if remaining == 0 {
                continue;
            }
            let take = remaining.min(order.quantity);
            fills.push(PlannedLegFill {
                order_id: order.order_id,
                account_id: order.user_id,
                price_ticks: order.price,
                quantity: take,
            });
            remaining -= take;
        }
        if remaining > 0 {
            return Err(LegLiquidityError::InsufficientLiquidity {
                requested: quantity,
                available,
            });
        }
        Ok(LegLiquidityPlan {
            taker_side,
            limit_price_ticks,
            quantity,
            fills,
        })
    }

    /// Validates that `plan` still matches the resting book without mutating it.
    ///
    /// Consumption re-plans the same request and requires identical order ids,
    /// prices, and quantities. A stale or tampered plan is rejected, so the
    /// two-leg registry seam can validate both legs before touching either.
    ///
    /// # Errors
    /// Returns a [`LegLiquidityError`] when the plan is zero-sized, stale, or
    /// its fills do not sum to its declared quantity.
    pub fn validate_leg_liquidity(&self, plan: &LegLiquidityPlan) -> Result<(), LegLiquidityError> {
        let selected = plan
            .fills
            .iter()
            .fold(0_u64, |total, fill| total.saturating_add(fill.quantity));
        if selected != plan.quantity {
            return Err(LegLiquidityError::QuantityMismatch {
                planned: plan.quantity,
                selected,
            });
        }
        if plan
            .fills
            .iter()
            .any(|fill| !self.orders.contains_key(&fill.order_id))
        {
            return Err(LegLiquidityError::StalePlan);
        }
        let current =
            self.plan_leg_liquidity(plan.taker_side, plan.limit_price_ticks, plan.quantity)?;
        if current.fills != plan.fills {
            return Err(LegLiquidityError::StalePlan);
        }
        Ok(())
    }

    /// Atomically consumes one validated leg-liquidity plan.
    ///
    /// The book is reduced, the resting owner receives the ordinary private
    /// fill and position/margin update, and one public leg trade is emitted per
    /// resting order. Nothing mutates when validation rejects the plan.
    ///
    /// # Errors
    /// Returns a [`LegLiquidityError`] when the plan is stale or unusable. In
    /// that case the book, positions, and event buffers are unchanged.
    pub fn consume_leg_liquidity(
        &mut self,
        plan: &LegLiquidityPlan,
        context: LegExecutionContext,
    ) -> Result<LegLiquidityExecution, LegLiquidityError> {
        self.validate_leg_liquidity(plan)?;
        Ok(self.commit_leg_liquidity(plan, context))
    }

    /// Applies a plan that a caller already validated against this book.
    ///
    /// The registry uses this after validating both legs, which is what makes a
    /// two-leg execution atomic. It must only run while the validated book is
    /// unchanged.
    pub(crate) fn commit_leg_liquidity(
        &mut self,
        plan: &LegLiquidityPlan,
        context: LegExecutionContext,
    ) -> LegLiquidityExecution {
        let mut fills = Vec::with_capacity(plan.fills.len());
        for planned in &plan.fills {
            let resting = self
                .book
                .fill_resting_order(planned.order_id, planned.quantity)
                .expect("validated leg-liquidity plan must remain consumable");
            let resting_side = match resting.side {
                OrderType::Buy => Side::Buy,
                OrderType::Sell => Side::Sell,
            };
            let trade_id = self.next_trade_id;
            self.next_trade_id = self.next_trade_id.saturating_add(1);
            self.apply_fill(resting.order_id, resting.quantity);
            let (buy_order_id, sell_order_id) = match resting_side {
                Side::Buy => (resting.order_id, context.taker_order_id),
                Side::Sell => (context.taker_order_id, resting.order_id),
            };
            self.push_public_event(PublicEvent {
                seq: 0,
                event_type: PublicEventType::Trade,
                trading_ts_ns: self.last_trading_ts_ns,
                phase: self.phase.code(),
                instrument_id: context.instrument_id,
                order_id: 0,
                trade_id,
                buy_order_id,
                sell_order_id,
                side: Some(plan.taker_side),
                price_ticks: resting.price,
                quantity: resting.quantity,
                volume: 0,
                pair_id: 0,
                spread_price: 0,
                leg1_price_ticks: 0,
                leg2_price_ticks: 0,
            });
            fills.push(LegFill {
                order_id: resting.order_id,
                account_id: resting.user_id,
                side: resting_side,
                price_ticks: resting.price,
                quantity: resting.quantity,
                remaining: resting.remaining,
                trade_id,
            });
        }
        LegLiquidityExecution {
            instrument_id: context.instrument_id,
            taker_order_id: context.taker_order_id,
            taker_side: plan.taker_side,
            quantity: plan.quantity,
            fills,
        }
    }

    /// Returns crossing orders in best-price, FIFO execution order.
    fn crossing_orders(&self, taker_side: Side, limit_price_ticks: u64) -> Vec<RestingOrder> {
        let mut orders = self.book.snapshot_orders();
        match taker_side {
            Side::Buy => {
                orders.retain(|order| {
                    order.side == OrderType::Sell && order.price <= limit_price_ticks
                });
            }
            Side::Sell => {
                let mut bids: Vec<(usize, RestingOrder)> = orders
                    .into_iter()
                    .enumerate()
                    .filter(|(_, order)| {
                        order.side == OrderType::Buy && order.price >= limit_price_ticks
                    })
                    .collect();
                // Snapshot order is ascending price with FIFO inside a level.
                // Sorting by descending price and the original index keeps the
                // FIFO sequence inside each price level.
                bids.sort_by_key(|(index, order)| (std::cmp::Reverse(order.price), *index));
                orders = bids.into_iter().map(|(_, order)| order).collect();
            }
        }
        orders
    }

    fn trade_events_into<E: Extend<CoreEvent>>(
        &mut self,
        trades: smallvec::SmallVec<[crate::shared::protocol::TradeNotification; 8]>,
        events: &mut E,
    ) {
        for trade in trades {
            let trade_id = self.next_trade_id;
            self.next_trade_id += 1;
            events.extend([CoreEvent::Trade {
                trade_id,
                price_ticks: trade.matched_price,
                quantity: trade.matched_quantity,
                buyer_user_id: trade.buyer_user_id,
                buyer_order_id: trade.buyer_order_id,
                seller_user_id: trade.seller_user_id,
                seller_order_id: trade.seller_order_id,
            }]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> TradingCore {
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        TradingCore::new(spec, SymbolId::intern("BTCUSDT"))
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
            trading_ts_ns: 0,
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
            time_condition: crate::trading::core::protocol::TimeCondition::Gfd,
            volume_condition: crate::trading::core::protocol::VolumeCondition::Any,
            side,
            offset: crate::trading::core::protocol::Offset::Open,
            hedge: crate::trading::core::protocol::Hedge::Spec,
            flags: 0,
            leg2_ratio: 0,
            price_ticks: price,
            quantity,
            trigger_price_ticks: 0,
            leg2_instrument_id: 0,
            leg2_offset: crate::trading::core::protocol::Offset::Open,
            leg2_hedge: crate::trading::core::protocol::Hedge::Spec,
            trigger_type: crate::trading::core::protocol::TriggerType::None,
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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::positions::PositionKey;
        use crate::trading::core::protocol::Offset;

        let mut core = core();
        core.positions_mut()
            .apply_open(
                PositionKey::new(1, Side::Buy, crate::trading::core::protocol::Hedge::Spec),
                10,
            )
            .unwrap();
        let mut command = wire_command(Side::Sell, 50_000, 5);
        command.offset = Offset::CloseToday;
        let events = core.execute_with_plugins(command, &AvailableClosePlugin);
        assert!(matches!(events.as_slice(), [CoreEvent::Accepted { .. }]));
        let key = crate::trading::core::positions::CloseKey::new(
            PositionKey::new(1, Side::Buy, crate::trading::core::protocol::Hedge::Spec),
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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::positions::PositionKey;
        use crate::trading::core::protocol::{Hedge, Offset};

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::positions::{CloseKey, PositionKey};
        use crate::trading::core::protocol::{Hedge, Offset};

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::Offset;

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::Offset;

        let mut core = core();
        let mut buy = wire_command(Side::Buy, 50_000, 5);
        buy.offset = Offset::Open;
        core.execute_with_plugins(buy, &AvailableClosePlugin);
        let mut sell = wire_command(Side::Sell, 50_000, 5);
        sell.user_id = 2;
        sell.offset = Offset::Open;
        let events = core.execute_with_plugins(sell, &AvailableClosePlugin);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::Trade { .. }))
        );
        let private = core.drain_private_events();
        assert!(private.iter().any(|event| {
            event.status == crate::trading::core::private_events::PrivateStatus::Filled
        }));
    }

    #[test]
    fn private_events_follow_accept_partial_and_cancel() {
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::private_events::PrivateStatus;
        use crate::trading::core::protocol::Offset;

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::private_events::PrivateStatus;
        use crate::trading::core::protocol::Offset;

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::{Offset, TimeCondition, VolumeCondition};

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::{Offset, TimeCondition, VolumeCondition};

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::{Offset, TimeCondition, VolumeCondition};

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::{Offset, PriceType};

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::{Offset, PriceType};

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::Offset;

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
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::Offset;

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
    fn session_switch_emits_public_event() {
        let mut core = core();
        core.set_phase(TradingPhase::Break, 2);
        assert_eq!(core.phase(), TradingPhase::Break);
        assert_eq!(core.trading_day(), 2);
        let public = core.drain_public_events();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].event_type, PublicEventType::SessionSwitch);
        assert_eq!(public[0].phase, TradingPhase::Break.code());
    }

    #[test]
    fn closed_phase_rejects_new_orders() {
        use crate::trading::core::plugins::AvailableClosePlugin;
        use crate::trading::core::protocol::Offset;

        let mut core = core();
        core.set_phase(TradingPhase::Closed, 1);
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
        command.trading_ts_ns = 123;
        let events = core.execute(command);
        assert!(events.is_empty());
        assert_eq!(core.last_tick_seq(), 42);
        assert_eq!(core.last_trading_ts_ns(), 123);
        let public = core.drain_public_events();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].event_type, PublicEventType::TimeTick);
        assert_eq!(public[0].trading_ts_ns, 123);
    }

    #[test]
    fn control_command_switches_phase() {
        let mut core = core();
        let mut command = wire_command(Side::Buy, 50_000, 1);
        command.command_type = CommandType::Control;
        command.flags = TradingPhase::Break.code();
        command.quantity = 3;
        core.execute(command);
        assert_eq!(core.phase(), TradingPhase::Break);
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
        use crate::trading::core::positions::PositionKey;

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
    fn pending_open_exposure_tracks_accept_and_cancel() {
        use crate::trading::core::plugins::NoPlugins;

        let mut core = core();
        let mut command = Command::zeroed();
        command.command_type = CommandType::NewOrder;
        command.client_order_id = 1;
        command.session_id = 1;
        command.member_id = 1;
        command.user_id = 1;
        command.seat_id = 1;
        command.instrument_id = 1;
        command.price_type = PriceType::Limit;
        command.time_condition = TimeCondition::Gfd;
        command.volume_condition = VolumeCondition::Any;
        command.side = Side::Buy;
        command.offset = Offset::Open;
        command.hedge = Hedge::Spec;
        command.price_ticks = 50_000;
        command.quantity = 5;
        let _ = core.execute_with_plugins(command, &NoPlugins);
        let key = PositionKey::new(1, Side::Buy, Hedge::Spec);
        assert_eq!(core.pending_open().get(key).total(), 5);

        let mut cancel = Command::zeroed();
        cancel.command_type = CommandType::Cancel;
        cancel.client_order_id = 2;
        cancel.session_id = 1;
        cancel.member_id = 1;
        cancel.user_id = 1;
        cancel.seat_id = 1;
        cancel.instrument_id = 1;
        cancel.cancel_order_id = 1;
        let _ = core.execute_with_plugins(cancel, &NoPlugins);
        assert_eq!(core.pending_open().get(key).total(), 0);
    }
    #[test]
    fn auction_rejects_market_fak_and_fok() {
        use crate::trading::core::plugins::NoPlugins;

        let mut core = core();
        let mut control = Command::zeroed();
        control.command_type = CommandType::Control;
        control.flags = TradingPhase::OpeningAuction.code();
        core.execute(control);
        assert_eq!(core.phase(), TradingPhase::OpeningAuction);

        for adjust in [
            |command: &mut Command| command.price_type = PriceType::Market,
            |command: &mut Command| command.time_condition = TimeCondition::Ioc,
            |command: &mut Command| command.volume_condition = VolumeCondition::All,
        ] {
            let mut command = wire_command(Side::Buy, 50_000, 5);
            adjust(&mut command);
            let events = core.execute_with_plugins(command, &NoPlugins);
            assert!(events.iter().any(|event| {
                matches!(event, CoreEvent::Rejected { reason, .. } if reason.contains("auction"))
            }));
        }
    }
    #[test]
    fn snapshot_roundtrips_auction_staging() {
        let mut trading = core();
        trading.begin_auction();
        trading.queue_auction_order(1, Side::Buy, 50_000, 3, Offset::Open, Hedge::Spec);
        trading.queue_auction_order(2, Side::Sell, 50_100, 2, Offset::Open, Hedge::Spec);
        let snapshot = trading.snapshot();
        let mut restored = core();
        let _ = restored.restore_snapshot(snapshot.clone());
        assert_eq!(restored.auction_order_count(), 2);
        assert_eq!(restored.snapshot(), snapshot);
    }
    #[test]
    fn execute_uses_business_accounting() {
        let mut core = core();
        let events = core.execute(wire_command(Side::Buy, 50_000, 5));
        let order_id = match events.as_slice() {
            [CoreEvent::Accepted { order_id, .. }] => *order_id,
            other => panic!("expected accepted, got {other:?}"),
        };
        let key = PositionKey::new(1, Side::Buy, Hedge::Spec);
        assert_eq!(core.order_status(order_id), Some(OrderStatus::Accepted));
        assert_eq!(core.pending_open().get(key).total(), 5);
    }
    #[test]
    #[ignore = "single-core latency profile; run explicitly"]
    fn core_latency_profile() {
        use crate::trading::core::plugins::NoPlugins;
        use std::hint::black_box;
        use std::time::Instant;

        let iterations = std::env::var("CORE_LATENCY_ITERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1_000_000);
        let warmup = iterations / 10;
        let mut core = core();
        let mut command = Command::zeroed();
        command.command_type = CommandType::NewOrder;
        command.price_type = PriceType::Limit;
        command.time_condition = TimeCondition::Gfd;
        command.volume_condition = VolumeCondition::Any;
        command.offset = Offset::Open;
        command.hedge = Hedge::Spec;
        command.price_ticks = 50_000;
        command.quantity = 1;
        command.instrument_id = 1;
        let plugin = NoPlugins;
        let mut samples = Vec::with_capacity(iterations);
        for index in 0..iterations + warmup {
            command.client_order_id = index as u64 + 1;
            command.user_id = if index % 2 == 0 { 1 } else { 2 };
            command.side = if index % 2 == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            let start = Instant::now();
            core.execute_with_plugins_discard(command, &plugin);
            let elapsed = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            if index >= warmup {
                samples.push(elapsed);
            }
        }
        samples.sort_unstable();
        let percentile = |p: f64| {
            let index = ((samples.len().saturating_sub(1) as f64) * p / 100.0) as usize;
            samples[index]
        };
        println!(
            "core_latency: n={} p50={}ns p99={}ns p999={}ns max={}ns",
            samples.len(),
            percentile(50.0),
            percentile(99.0),
            percentile(99.9),
            samples.last().copied().unwrap_or(0)
        );
        black_box(core);
    }
    #[test]
    #[ignore = "multi-contract latency profile; run explicitly"]
    fn multi_contract_latency_profile() {
        use crate::trading::core::plugins::NoPlugins;
        use std::hint::black_box;
        use std::time::Instant;

        let contracts = std::env::var("CORE_MULTI_CONTRACTS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(3000);
        let iterations = std::env::var("CORE_LATENCY_ITERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1_000_000);
        let spec = ContractSpec::new("MC", 1, 50_000, 50_100);
        let mut cores: Vec<TradingCore> = (0..contracts)
            .map(|index| TradingCore::new(spec.clone(), SymbolId::intern(&format!("MC{index}"))))
            .collect();
        let mut command = Command::zeroed();
        command.command_type = CommandType::NewOrder;
        command.price_type = PriceType::Limit;
        command.time_condition = TimeCondition::Gfd;
        command.volume_condition = VolumeCondition::Any;
        command.offset = Offset::Open;
        command.hedge = Hedge::Spec;
        command.price_ticks = 50_000;
        command.quantity = 1;
        let plugin = NoPlugins;
        let mut samples = Vec::with_capacity(iterations);
        for index in 0..iterations {
            let instrument = index % contracts;
            command.client_order_id = index as u64 + 1;
            command.user_id = if index % 2 == 0 { 1 } else { 2 };
            command.side = if index % 2 == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            command.instrument_id = u32::try_from(instrument).unwrap_or(u32::MAX);
            let start = Instant::now();
            cores[instrument].execute_with_plugins_discard(command, &plugin);
            samples.push(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX));
        }
        samples.sort_unstable();
        let percentile = |p: f64| {
            let index = ((samples.len().saturating_sub(1) as f64) * p / 100.0) as usize;
            samples[index]
        };
        let rss = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("VmRSS:"))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        println!(
            "multi_contract: contracts={contracts} n={} p50={}ns p99={}ns p999={}ns max={}ns {rss}",
            samples.len(),
            percentile(50.0),
            percentile(99.0),
            percentile(99.9),
            samples.last().copied().unwrap_or(0)
        );
        black_box(cores);
    }
    #[test]
    fn every_new_order_has_a_private_client_id() {
        use crate::trading::core::plugins::NoPlugins;

        let mut core = core();
        let mut missing = Vec::new();
        for index in 1..=100_000u64 {
            let mut command = wire_command(Side::Buy, 50_000, 1);
            command.client_order_id = index;
            command.user_id = if index.is_multiple_of(2) { 1 } else { 2 };
            command.side = if index.is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            };
            core.execute_with_plugins_discard(command, &NoPlugins);
            let events = core.drain_private_events();
            if !events.iter().any(|event| event.client_order_id == index) {
                missing.push(index);
            }
        }
        assert!(
            missing.is_empty(),
            "missing private client ids: {missing:?}"
        );
    }
}
