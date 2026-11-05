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
use crate::exchange::plugins::{PreTradePlugin, RiskOrder, ValidationContext};
use crate::exchange::positions::{CloseKey, FrozenClose, PositionKey, Positions};
use crate::exchange::private_events::{PrivateEvent, PrivateStatus};
use crate::exchange::protocol::{
    Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, VolumeCondition,
};
use crate::shared::SymbolId;
use crate::shared::protocol::{NewOrderRequest, OrderType};
use std::collections::HashMap;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug)]
pub struct OrderMeta {
    pub client_order_id: u64,
    pub account_id: u64,
    pub side: Side,
    pub offset: Offset,
    pub hedge: Hedge,
    pub remaining: u64,
    pub status: OrderStatus,
}

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
        }
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

    fn reject_private(&mut self, command: &Command, reason: String) -> Vec<CoreEvent> {
        self.push_private(
            0,
            command.client_order_id,
            u64::from(command.user_id),
            PrivateStatus::Rejected,
            0,
            0,
            Some(reason.clone()),
        );
        vec![CoreEvent::Rejected {
            client_order_id: command.client_order_id,
            reason,
        }]
    }

    /// Submits one limit order and returns every event it produced.
    ///
    /// The order book owns order-id allocation and price/tick validation; this
    /// kernel maps the command into the domain request and projects the result
    /// into protocol-neutral core events.
    pub fn submit_limit(&mut self, command: LimitOrderCommand) -> Vec<CoreEvent> {
        let request = NewOrderRequest {
            user_id: command.user_id,
            client_order_id: command.client_order_id,
            symbol: self.symbol,
            order_type: command.side,
            price: command.price_ticks,
            quantity: command.quantity,
        };

        match self.book.match_order(request) {
            crate::domain::orderbook::MatchOutcome::Filled { trades } => self.trade_events(trades),
            crate::domain::orderbook::MatchOutcome::Rested {
                trades,
                confirmation,
            } => {
                let mut events = self.trade_events(trades);
                events.push(CoreEvent::Accepted {
                    order_id: confirmation.order_id,
                    user_id: confirmation.user_id,
                    side: command.side,
                    price_ticks: command.price_ticks,
                    // `Rested` can also be a partial fill; recover the remainder
                    // from the book by asking for the order's remaining quantity.
                    remaining_quantity: self
                        .book
                        .get_order_status(confirmation.order_id)
                        .map_or(command.quantity, |status| status.remaining_quantity),
                });
                events
            }
            crate::domain::orderbook::MatchOutcome::Rejected { reason, trades } => {
                let mut events = self.trade_events(trades);
                events.push(CoreEvent::Rejected {
                    client_order_id: command.client_order_id,
                    reason,
                });
                events
            }
        }
    }

    /// Cancels a resting order. A missing/foreign order is rejected locally.
    pub fn cancel(&mut self, command: CancelCommand) -> CoreEvent {
        match self.book.cancel_order(command.order_id, command.user_id) {
            Ok(()) => {
                if let Some(meta) = self.orders.get_mut(&command.order_id) {
                    if meta.offset != Offset::Open {
                        let key = CloseKey::new(
                            PositionKey::new(meta.account_id, meta.side.opposite(), meta.hedge),
                            meta.offset,
                        );
                        self.frozen_close.release(key, meta.remaining);
                    }
                    let client_order_id = meta.client_order_id;
                    let account_id = meta.account_id;
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
        if command.command_type != CommandType::NewOrder || command.price_type != PriceType::Limit {
            return self.execute(command);
        }
        if command.price_ticks < 0 || command.quantity == 0 {
            return self.reject_private(&command, "invalid price or quantity".to_string());
        }
        if command.volume_condition == VolumeCondition::All {
            return self
                .reject_private(&command, "FOK is not implemented in this slice".to_string());
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
            return self.reject_private(&command, reason.to_string());
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
                return self
                    .reject_private(&command, format!("close reservation failed: {error:?}"));
            }
            Some(key)
        };

        let events = self.submit_limit(LimitOrderCommand {
            user_id: u64::from(command.user_id),
            client_order_id: command.client_order_id,
            side,
            price_ticks: command.price_ticks as u64,
            quantity: command.quantity,
        });

        let incoming_id = events.iter().find_map(|event| match event {
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
                    remaining: command.quantity,
                    status: OrderStatus::Accepted,
                },
            );
        }

        if let Some(CoreEvent::Rejected { reason, .. }) = events
            .iter()
            .find(|event| matches!(event, CoreEvent::Rejected { .. }))
        {
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
        if let Some(CoreEvent::Accepted {
            order_id,
            remaining_quantity,
            ..
        }) = events
            .iter()
            .find(|event| matches!(event, CoreEvent::Accepted { .. }))
        {
            self.push_private(
                *order_id,
                command.client_order_id,
                u64::from(command.user_id),
                PrivateStatus::Accepted,
                0,
                *remaining_quantity,
                None,
            );
        }

        let trades: Vec<(u64, u64, u64)> = events
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
        for (buyer_order_id, seller_order_id, quantity) in trades {
            self.apply_fill(buyer_order_id, quantity);
            self.apply_fill(seller_order_id, quantity);
        }

        let has_accepted = events
            .iter()
            .any(|event| matches!(event, CoreEvent::Accepted { .. }));
        if let Some(key) = close_key
            && !has_accepted
        {
            // No resting remainder: any unfilled quantity is released. Filled
            // quantity was already released by apply_fill.
            let filled = events
                .iter()
                .filter_map(|event| match event {
                    CoreEvent::Trade { quantity, .. } => Some(*quantity),
                    _ => None,
                })
                .sum::<u64>();
            self.frozen_close
                .release(key, command.quantity.saturating_sub(filled));
        }

        let mut events = events;
        if command.time_condition == TimeCondition::Ioc
            && let Some(order_id) = incoming_id
            && self
                .orders
                .get(&order_id)
                .is_some_and(|meta| meta.remaining > 0)
        {
            // FAK/IOC: the unmatched remainder is canceled immediately.
            events.push(self.cancel(CancelCommand {
                user_id: u64::from(command.user_id),
                order_id,
            }));
        }
        events
    }

    /// Applies one fill to an order's position and remaining quantity.
    fn apply_fill(&mut self, order_id: u64, quantity: u64) {
        let Some(mut meta) = self.orders.remove(&order_id) else {
            return;
        };
        let position_side = if meta.offset == Offset::Open {
            meta.side
        } else {
            meta.side.opposite()
        };
        let key = PositionKey::new(meta.account_id, position_side, meta.hedge);
        if meta.offset == Offset::Open {
            let _ = self.positions.apply_open(key, quantity);
        } else {
            let _ = self.positions.apply_close(key, meta.offset, quantity);
            self.frozen_close
                .release(CloseKey::new(key, meta.offset), quantity);
        }
        meta.remaining = meta.remaining.saturating_sub(quantity);
        meta.status = if meta.remaining == 0 {
            OrderStatus::Filled
        } else {
            OrderStatus::PartiallyFilled
        };
        self.push_private(
            order_id,
            meta.client_order_id,
            meta.account_id,
            if meta.remaining == 0 {
                PrivateStatus::Filled
            } else {
                PrivateStatus::PartiallyFilled
            },
            quantity,
            meta.remaining,
            None,
        );
        self.orders.insert(order_id, meta);
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

    fn trade_events(
        &mut self,
        trades: smallvec::SmallVec<[crate::shared::protocol::TradeNotification; 8]>,
    ) -> Vec<CoreEvent> {
        trades
            .into_iter()
            .map(|trade| {
                let trade_id = self.next_trade_id;
                self.next_trade_id += 1;
                CoreEvent::Trade {
                    trade_id,
                    price_ticks: trade.matched_price,
                    quantity: trade.matched_quantity,
                    buyer_user_id: trade.buyer_user_id,
                    buyer_order_id: trade.buyer_order_id,
                    seller_user_id: trade.seller_user_id,
                    seller_order_id: trade.seller_order_id,
                }
            })
            .collect()
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
        command.command_type = CommandType::TimeTick;
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
}
