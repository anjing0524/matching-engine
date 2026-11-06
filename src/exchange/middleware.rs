//! Minimal middleware sequencer producer.
//!
//! The full design puts global ordering, logging, session routing and multicast
//! in middleware. This slice implements only the ordered command producer:
//! assign `global_seq`, append the fixed 256-byte command to the shared ring,
//! and let the core runner consume it.

use crate::exchange::protocol::Command;
use crate::exchange::transport::{CommandProducer, CommandTransportError};

/// Single-writer middleware command sequencer.
pub struct MiddlewareSequencer {
    producer: CommandProducer,
    next_global_seq: u64,
}

impl MiddlewareSequencer {
    #[must_use]
    pub fn new(producer: CommandProducer) -> Self {
        Self {
            producer,
            next_global_seq: 1,
        }
    }

    /// Assigns the next global sequence and appends the command.
    ///
    /// # Errors
    /// Returns `Full` when the command ring is full; the sequence is not
    /// consumed in that case.
    pub fn submit(&mut self, mut command: Command) -> Result<u64, CommandTransportError> {
        let global_seq = self.next_global_seq;
        command.global_seq = global_seq;
        self.producer.send(&command)?;
        self.next_global_seq += 1;
        Ok(global_seq)
    }

    #[must_use]
    pub fn next_global_seq(&self) -> u64 {
        self.next_global_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::ContractSpec;
    use crate::exchange::core::{CoreEvent, ExchangeCore};
    use crate::exchange::plugins::NoPlugins;
    use crate::exchange::protocol::{
        Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
        VolumeCondition,
    };
    use crate::exchange::runner::CoreRunner;
    use crate::exchange::transport::create;
    use crate::shared::SymbolId;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "matching-engine-middleware-{name}-{}",
            std::process::id()
        ))
    }

    fn command(seq: u64, side: Side, price: i64, quantity: u64) -> Command {
        Command {
            global_seq: 0,
            gateway_seq: seq,
            ingress_ts_ns: 0,
            exchange_ts_ns: 0,
            client_order_id: seq,
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
            time_condition: TimeCondition::Gfd,
            volume_condition: VolumeCondition::Any,
            side,
            offset: Offset::Open,
            hedge: Hedge::Spec,
            flags: 0,
            leg2_ratio: 0,
            price_ticks: price,
            quantity,
            trigger_price_ticks: 0,
            leg2_instrument_id: 0,
            leg2_offset: Offset::Open,
            leg2_hedge: Hedge::Spec,
            trigger_type: TriggerType::None,
            cancel_order_id: 0,
        }
    }

    #[test]
    fn middleware_assigns_global_sequence_and_core_trades() {
        let path = temp_path("sequence");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let mut middleware = MiddlewareSequencer::new(producer);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let mut runner = CoreRunner::new(core, consumer, NoPlugins);

        assert_eq!(
            middleware.submit(command(1, Side::Buy, 50_000, 5)).unwrap(),
            1
        );
        assert_eq!(
            middleware
                .submit(command(2, Side::Sell, 50_000, 5))
                .unwrap(),
            2
        );
        assert_eq!(runner.poll(), 2);
        let events = runner.drain_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::Trade { quantity: 5, .. }))
        );
        assert_eq!(middleware.next_global_seq(), 3);
        let _ = std::fs::remove_file(&path);
    }
}
