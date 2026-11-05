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
use crate::shared::SymbolId;
use crate::shared::protocol::{NewOrderRequest, OrderType};

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

/// The deterministic core state machine for one contract.
pub struct ExchangeCore {
    book: TickBasedOrderBook,
    symbol: SymbolId,
    next_trade_id: u64,
}

impl ExchangeCore {
    #[must_use]
    pub fn new(spec: ContractSpec, symbol: SymbolId) -> Self {
        Self {
            book: TickBasedOrderBook::new(spec),
            symbol,
            next_trade_id: 1,
        }
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
            Ok(()) => CoreEvent::Canceled {
                order_id: command.order_id,
                user_id: command.user_id,
            },
            Err(reason) => CoreEvent::Rejected {
                client_order_id: 0,
                reason,
            },
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
