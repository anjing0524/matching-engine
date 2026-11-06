//! Combo execution → public market-data transport.
//!
//! Runs the combination queue/executor and forwards successful fill events to
//! the combo UDP publisher.

use crate::exchange::combo_engine::ComboEngine;
use crate::exchange::combo_market_data::ComboMarketDataPublisher;
use crate::exchange::gateway_runtime::GatewayRuntime;
use crate::exchange::gateway_server::GatewayServerHandle;
use crate::exchange::protocol::{Command, CommandType, Side};

/// Composite combo engine plus optional public transport.
pub struct ComboRuntime {
    engine: ComboEngine,
    publisher: Option<ComboMarketDataPublisher>,
    public_sent: u64,
}

impl ComboRuntime {
    #[must_use]
    pub fn new(engine: ComboEngine) -> Self {
        Self {
            engine,
            publisher: None,
            public_sent: 0,
        }
    }

    #[must_use]
    pub fn with_publisher(mut self, publisher: ComboMarketDataPublisher) -> Self {
        self.publisher = Some(publisher);
        self
    }

    /// Routes one gateway command into this combo engine.
    ///
    /// Combo new orders use `client_order_id` as the combo order id and
    /// `price_ticks` as the spread. Returns `false` for non-combo commands or
    /// rejected queue/cancel requests.
    pub fn submit_command(&mut self, command: &Command) -> bool {
        if command.combo_id == 0 {
            return false;
        }
        match command.command_type {
            CommandType::NewOrder => {
                if command.price_ticks < 0 {
                    return false;
                }
                let side = match command.side {
                    crate::exchange::protocol::Side::Buy => Side::Buy,
                    crate::exchange::protocol::Side::Sell => Side::Sell,
                };
                self.engine.queue(
                    command.client_order_id.max(1),
                    u64::from(command.user_id),
                    side,
                    command.price_ticks as u64,
                    command.quantity,
                    command.offset,
                    command.hedge,
                )
            }
            CommandType::Cancel => {
                let order_id = if command.cancel_order_id == 0 {
                    command.client_order_id
                } else {
                    command.cancel_order_id
                };
                self.engine.cancel(order_id)
            }
            _ => false,
        }
    }

    /// Publishes fill events produced since the last tick.
    pub fn tick(&mut self) -> usize {
        let Some(publisher) = &self.publisher else {
            return 0;
        };
        let events = self.engine.drain_events();
        let mut sent = 0;
        for event in events {
            if publisher.send(&event.public_fill()).is_ok() {
                sent += 1;
                self.public_sent += 1;
            }
        }
        sent
    }

    /// Publishes public fills and routes private fills to logged-in gateways.
    ///
    /// Returns `(public_sent, private_sent)`.
    pub fn tick_with_gateway(
        &mut self,
        gateway: &GatewayRuntime,
        server: &GatewayServerHandle,
    ) -> (usize, usize) {
        let events = self.engine.drain_events();
        let mut public = 0;
        if let Some(publisher) = &self.publisher {
            for event in &events {
                if publisher.send(&event.public_fill()).is_ok() {
                    public += 1;
                    self.public_sent += 1;
                }
            }
        }
        let private = gateway.dispatch_combo_private(
            server,
            events
                .iter()
                .map(crate::exchange::combo_engine::ComboFillEvent::private_fill),
        );
        (public, private)
    }

    #[must_use]
    pub fn engine(&self) -> &ComboEngine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut ComboEngine {
        &mut self.engine
    }

    #[must_use]
    pub fn public_sent(&self) -> u64 {
        self.public_sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::ContractSpec;
    use crate::exchange::combo_market_data::ComboMarketDataSubscriber;
    use crate::exchange::protocol::{Hedge, Offset, Side};
    use crate::shared::protocol::OrderType;

    #[test]
    fn combo_command_routes_into_engine() {
        use crate::exchange::protocol::{Command, CommandType};

        let engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        );
        let mut runtime = ComboRuntime::new(engine);
        let mut command = Command::zeroed();
        command.command_type = CommandType::NewOrder;
        command.combo_id = 7;
        command.client_order_id = 42;
        command.user_id = 1;
        command.side = Side::Buy;
        command.price_ticks = 49_900;
        command.quantity = 5;
        assert!(runtime.submit_command(&command));
        assert_eq!(runtime.engine().queue_len(), 1);

        let mut cancel = Command::zeroed();
        cancel.command_type = CommandType::Cancel;
        cancel.combo_id = 7;
        cancel.cancel_order_id = 42;
        assert!(runtime.submit_command(&cancel));
        assert_eq!(runtime.engine().queue_len(), 0);
    }

    #[test]
    fn combo_fill_is_published_after_execution() {
        let mut engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        );
        engine
            .executor_mut()
            .leg1
            .insert_resting_order(2, OrderType::Sell, 50_000, 5, 5, 1)
            .unwrap();
        engine
            .executor_mut()
            .leg2
            .insert_resting_order(3, OrderType::Buy, 100, 5, 5, 2)
            .unwrap();
        assert!(engine.queue(10, 1, Side::Buy, 49_900, 5, Offset::Open, Hedge::Spec));
        assert!(engine.try_execute_best(Side::Buy).unwrap().is_ok());

        let subscriber = ComboMarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            ComboMarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap())
                .unwrap();
        let mut runtime = ComboRuntime::new(engine).with_publisher(publisher);
        assert_eq!(runtime.tick(), 1);
        assert_eq!(runtime.public_sent(), 1);
        let fill = subscriber.recv().unwrap();
        assert_eq!(fill.quantity, 5);
        assert_eq!(fill.spread_ticks, 49_900);
    }
}
