//! Combo execution → public market-data transport.
//!
//! Runs the combination queue/executor and forwards successful fill events to
//! the combo UDP publisher.

use crate::exchange::combo_engine::ComboEngine;
use crate::exchange::combo_market_data::ComboMarketDataPublisher;

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
