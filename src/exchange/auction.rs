//! Call-auction (opening/closing) order collection and uncross.
//!
//! Scope: one contract, limit orders only, maximum-volume price selection with
//! deterministic tie-breaking. Market/arbitrage orders are rejected by the
//! caller before entering this book.

use crate::exchange::protocol::{Hedge, Offset, Side};

/// One order collected during the call auction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuctionOrder {
    pub order_id: u64,
    pub user_id: u64,
    pub side: Side,
    pub price_ticks: u64,
    pub quantity: u64,
    pub offset: Offset,
    pub hedge: Hedge,
    /// Global arrival sequence, used as the time-priority tie-breaker.
    pub arrival_seq: u64,
}

/// One auction execution at the single uncross price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuctionTrade {
    pub price_ticks: u64,
    pub quantity: u64,
    pub buyer_order_id: u64,
    pub seller_order_id: u64,
    pub buyer_user_id: u64,
    pub seller_user_id: u64,
}

/// Result of one uncross.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuctionResult {
    pub price_ticks: Option<u64>,
    pub volume: u64,
    pub trades: Vec<AuctionTrade>,
    /// Orders (with remaining quantities) that continue into continuous trading.
    pub remaining: Vec<AuctionOrder>,
}

/// Maximum-volume price with deterministic tie-break:
/// closest to previous settlement, then lower price.
#[must_use]
pub fn auction_price(orders: &[AuctionOrder], previous_settlement: u64) -> Option<u64> {
    let mut prices: Vec<u64> = orders
        .iter()
        .filter(|order| order.quantity > 0)
        .map(|order| order.price_ticks)
        .collect();
    prices.sort_unstable();
    prices.dedup();

    let mut best: Option<(u64, u64, u64)> = None; // (volume, distance, price)
    for price in prices {
        let demand: u64 = orders
            .iter()
            .filter(|order| order.side == Side::Buy && order.price_ticks >= price)
            .map(|order| order.quantity)
            .sum();
        let supply: u64 = orders
            .iter()
            .filter(|order| order.side == Side::Sell && order.price_ticks <= price)
            .map(|order| order.quantity)
            .sum();
        let volume = demand.min(supply);
        if volume == 0 {
            continue;
        }
        let distance = price.abs_diff(previous_settlement);
        let replace = match best {
            None => true,
            Some((best_volume, best_distance, best_price)) => {
                volume > best_volume
                    || (volume == best_volume
                        && (distance < best_distance
                            || (distance == best_distance && price < best_price)))
            }
        };
        if replace {
            best = Some((volume, distance, price));
        }
    }
    best.map(|(_, _, price)| price)
}

/// Uncrosses collected orders at one maximum-volume price.
#[must_use]
pub fn uncross(orders: &[AuctionOrder], previous_settlement: u64) -> AuctionResult {
    let Some(price) = auction_price(orders, previous_settlement) else {
        return AuctionResult {
            price_ticks: None,
            volume: 0,
            trades: Vec::new(),
            remaining: orders.to_vec(),
        };
    };

    let mut buys: Vec<(usize, AuctionOrder)> = orders
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, order)| order.side == Side::Buy && order.price_ticks >= price)
        .collect();
    let mut sells: Vec<(usize, AuctionOrder)> = orders
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, order)| order.side == Side::Sell && order.price_ticks <= price)
        .collect();

    // Price priority first, then arrival time.
    buys.sort_by(|a, b| {
        b.1.price_ticks
            .cmp(&a.1.price_ticks)
            .then(a.1.arrival_seq.cmp(&b.1.arrival_seq))
    });
    sells.sort_by(|a, b| {
        a.1.price_ticks
            .cmp(&b.1.price_ticks)
            .then(a.1.arrival_seq.cmp(&b.1.arrival_seq))
    });

    let mut remaining = orders.to_vec();
    let mut trades = Vec::new();
    let mut buy_index = 0;
    let mut sell_index = 0;
    while buy_index < buys.len() && sell_index < sells.len() {
        let buy_pos = buys[buy_index].0;
        let sell_pos = sells[sell_index].0;
        let (buyer_order_id, seller_order_id, buyer_user_id, seller_user_id, quantity) = {
            let buy = &mut buys[buy_index].1;
            let sell = &mut sells[sell_index].1;
            let quantity = buy.quantity.min(sell.quantity);
            if quantity == 0 {
                break;
            }
            buy.quantity -= quantity;
            sell.quantity -= quantity;
            (
                buy.order_id,
                sell.order_id,
                buy.user_id,
                sell.user_id,
                quantity,
            )
        };
        trades.push(AuctionTrade {
            price_ticks: price,
            quantity,
            buyer_order_id,
            seller_order_id,
            buyer_user_id,
            seller_user_id,
        });
        remaining[buy_pos].quantity = buys[buy_index].1.quantity;
        remaining[sell_pos].quantity = sells[sell_index].1.quantity;
        if buys[buy_index].1.quantity == 0 {
            buy_index += 1;
        }
        if sells[sell_index].1.quantity == 0 {
            sell_index += 1;
        }
    }

    let volume: u64 = trades.iter().map(|trade| trade.quantity).sum();
    remaining.retain(|order| order.quantity > 0);

    AuctionResult {
        price_ticks: Some(price),
        volume,
        trades,
        remaining,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(order_id: u64, side: Side, price: u64, quantity: u64, seq: u64) -> AuctionOrder {
        AuctionOrder {
            order_id,
            user_id: order_id,
            side,
            price_ticks: price,
            quantity,
            offset: Offset::Open,
            hedge: Hedge::Spec,
            arrival_seq: seq,
        }
    }

    #[test]
    fn picks_maximum_volume_price() {
        let orders = [
            order(1, Side::Buy, 50_000, 10, 1),
            order(2, Side::Sell, 49_900, 8, 2),
        ];
        assert_eq!(auction_price(&orders, 50_000), Some(50_000));
        let result = uncross(&orders, 50_000);
        assert_eq!(result.price_ticks, Some(50_000));
        assert_eq!(result.volume, 8);
        assert_eq!(result.trades.len(), 1);
        assert_eq!(result.remaining.len(), 1);
        assert_eq!(result.remaining[0].order_id, 1);
        assert_eq!(result.remaining[0].quantity, 2);
    }

    #[test]
    fn time_priority_allocates_at_marginal_price() {
        let orders = [
            order(1, Side::Buy, 50_000, 5, 1),
            order(2, Side::Buy, 50_000, 5, 2),
            order(3, Side::Sell, 50_000, 7, 3),
        ];
        let result = uncross(&orders, 50_000);
        assert_eq!(result.volume, 7);
        assert_eq!(result.trades[0].buyer_order_id, 1);
        assert_eq!(result.trades[1].buyer_order_id, 2);
        assert_eq!(result.remaining[0].order_id, 2);
        assert_eq!(result.remaining[0].quantity, 3);
    }

    #[test]
    fn tie_breaks_to_previous_settlement_then_lower_price() {
        let orders = [
            order(1, Side::Buy, 49_900, 5, 1),
            order(2, Side::Sell, 49_900, 5, 2),
            order(3, Side::Buy, 50_100, 5, 3),
            order(4, Side::Sell, 50_100, 5, 4),
        ];
        assert_eq!(auction_price(&orders, 50_000), Some(49_900));
    }

    #[test]
    fn one_sided_book_does_not_uncross() {
        let orders = [order(1, Side::Buy, 50_000, 5, 1)];
        let result = uncross(&orders, 50_000);
        assert_eq!(result.price_ticks, None);
        assert_eq!(result.volume, 0);
        assert_eq!(result.remaining, orders.to_vec());
    }
}
