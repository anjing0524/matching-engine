//! Static-dispatch pre-trade plugin chain.
//!
//! Plugins only read authoritative core state and return Accept/Reject. They
//! cannot mutate positions or orders; state transitions stay in the kernel.
//! The chain is generic and monomorphized at compile time (no `dyn Trait`).

use crate::exchange::positions::{CloseKey, FrozenClose, PositionKey, Positions};
use crate::exchange::protocol::{Hedge, Offset, Side};

/// Minimal order view exposed to pre-trade plugins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RiskOrder {
    pub account_id: u64,
    pub side: Side,
    pub offset: Offset,
    pub hedge: Hedge,
    pub quantity: u64,
}

/// Read-only state a plugin may inspect.
pub struct ValidationContext<'a> {
    pub positions: &'a Positions,
    pub frozen_close: &'a FrozenClose,
}

/// A pre-trade plugin. Reject is a stable reason string for private reporting.
pub trait PreTradePlugin {
    fn check(&self, order: &RiskOrder, context: &ValidationContext<'_>)
    -> Result<(), &'static str>;
}

/// Static dispatch pipeline around one plugin or a tuple of plugins.
pub struct ValidationPipeline<P> {
    plugin: P,
}

impl<P> ValidationPipeline<P> {
    #[must_use]
    pub const fn new(plugin: P) -> Self {
        Self { plugin }
    }
}

impl<P: PreTradePlugin> PreTradePlugin for ValidationPipeline<P> {
    fn check(
        &self,
        order: &RiskOrder,
        context: &ValidationContext<'_>,
    ) -> Result<(), &'static str> {
        self.plugin.check(order, context)
    }
}

/// Runs plugins left-to-right and stops at the first rejection.
impl<A: PreTradePlugin, B: PreTradePlugin> PreTradePlugin for (A, B) {
    fn check(
        &self,
        order: &RiskOrder,
        context: &ValidationContext<'_>,
    ) -> Result<(), &'static str> {
        self.0.check(order, context)?;
        self.1.check(order, context)
    }
}

/// Rejects a close order that exceeds the position bucket after accounting for
/// already accepted close orders.
#[derive(Clone, Copy, Debug, Default)]
pub struct AvailableClosePlugin;

impl PreTradePlugin for AvailableClosePlugin {
    fn check(
        &self,
        order: &RiskOrder,
        context: &ValidationContext<'_>,
    ) -> Result<(), &'static str> {
        if order.offset == Offset::Open {
            return Ok(());
        }
        let position_side = if order.offset == Offset::Open {
            order.side
        } else {
            order.side.opposite()
        };
        let key = CloseKey::new(
            PositionKey::new(order.account_id, position_side, order.hedge),
            order.offset,
        );
        if context.frozen_close.available(key, context.positions) < order.quantity {
            return Err("insufficient closeable position");
        }
        Ok(())
    }
}

/// Simple upper bound used as an example plugin and for tests.
#[derive(Clone, Copy, Debug)]
pub struct MaxOrderQuantityPlugin {
    pub max_quantity: u64,
}

impl PreTradePlugin for MaxOrderQuantityPlugin {
    fn check(
        &self,
        order: &RiskOrder,
        _context: &ValidationContext<'_>,
    ) -> Result<(), &'static str> {
        if order.quantity > self.max_quantity {
            return Err("order quantity exceeds limit");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(offset: Offset, quantity: u64) -> RiskOrder {
        RiskOrder {
            account_id: 1,
            side: if offset == Offset::Open {
                Side::Buy
            } else {
                Side::Sell
            },
            offset,
            hedge: Hedge::Spec,
            quantity,
        }
    }

    #[test]
    fn close_plugin_checks_frozen_quantity() {
        let mut positions = Positions::new();
        let mut frozen = FrozenClose::new();
        positions
            .apply_open(PositionKey::new(1, Side::Buy, Hedge::Spec), 10)
            .unwrap();
        let close_key = CloseKey::new(
            PositionKey::new(1, Side::Buy, Hedge::Spec),
            Offset::CloseToday,
        );
        frozen.reserve(close_key, &positions, 8).unwrap();
        let context = ValidationContext {
            positions: &positions,
            frozen_close: &frozen,
        };
        let plugin = AvailableClosePlugin;
        assert!(
            plugin
                .check(&order(Offset::CloseToday, 3), &context)
                .is_err()
        );
        assert!(
            plugin
                .check(&order(Offset::CloseToday, 2), &context)
                .is_ok()
        );
    }

    #[test]
    fn tuple_chain_is_static_and_ordered() {
        let positions = Positions::new();
        let frozen = FrozenClose::new();
        let context = ValidationContext {
            positions: &positions,
            frozen_close: &frozen,
        };
        let chain = (
            MaxOrderQuantityPlugin { max_quantity: 5 },
            AvailableClosePlugin,
        );
        assert_eq!(
            chain.check(&order(Offset::CloseToday, 6), &context),
            Err("order quantity exceeds limit")
        );
        assert!(
            chain
                .check(&order(Offset::CloseToday, 5), &context)
                .is_err()
        );
        assert!(chain.check(&order(Offset::Open, 5), &context).is_ok());
    }
}
