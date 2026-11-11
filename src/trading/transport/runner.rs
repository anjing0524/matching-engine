//! Minimal core command-consumer loop.
//!
//! The runner drains the shared-memory command transport, executes commands
//! through a static pre-trade plugin chain, and exposes the resulting private
//! and public events. It is deliberately single-threaded and has no networking.

use crate::trading::core::TradingCore;
use crate::trading::core::plugins::PreTradePlugin;
use crate::trading::core::private_events::PrivateEvent;
use crate::trading::core::public_events::PublicEvent;
use crate::trading::transport::{
    CommandConsumer, CommandTransportError, PrivateEventProducer, PublicEventProducer,
};
use std::collections::VecDeque;

/// One poll's core commands and projected events.
pub struct CoreEvents {
    pub commands: usize,
    pub private: Vec<PrivateEvent>,
    pub public: Vec<PublicEvent>,
}

/// Drains commands from a `CommandConsumer` and applies them to `TradingCore`.
pub struct CoreRunner<P> {
    core: TradingCore,
    consumer: CommandConsumer,
    plugin: P,
    decode_errors: u64,
    public_producer: Option<PublicEventProducer>,
    private_producer: Option<PrivateEventProducer>,
    pending_public: VecDeque<PublicEvent>,
    pending_private: VecDeque<PrivateEvent>,
}

impl<P: PreTradePlugin> CoreRunner<P> {
    #[must_use]
    pub fn new(core: TradingCore, consumer: CommandConsumer, plugin: P) -> Self {
        Self {
            core,
            consumer,
            plugin,
            decode_errors: 0,
            public_producer: None,
            private_producer: None,
            pending_public: VecDeque::new(),
            pending_private: VecDeque::new(),
        }
    }

    /// Attaches shared-memory public/private event producers.
    #[must_use]
    pub fn with_event_transports(
        mut self,
        public: PublicEventProducer,
        private: PrivateEventProducer,
    ) -> Self {
        self.public_producer = Some(public);
        self.private_producer = Some(private);
        self
    }

    fn flush_public(&mut self) {
        let Some(producer) = &self.public_producer else {
            return;
        };
        self.pending_public.extend(self.core.drain_public_events());
        while let Some(event) = self.pending_public.front() {
            match producer.send(event) {
                Ok(()) => {
                    self.pending_public.pop_front();
                }
                Err(CommandTransportError::Full) => break,
                Err(_) => {
                    self.pending_public.pop_front();
                }
            }
        }
    }

    fn flush_private(&mut self) {
        let Some(producer) = &self.private_producer else {
            return;
        };
        self.pending_private
            .extend(self.core.drain_private_events());
        while let Some(event) = self.pending_private.front() {
            match producer.send(event) {
                Ok(()) => {
                    self.pending_private.pop_front();
                }
                Err(CommandTransportError::Full) => break,
                Err(_) => {
                    self.pending_private.pop_front();
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn flush_pending(&mut self) {
        self.flush_public();
        self.flush_private();
    }

    #[cfg(test)]
    pub(crate) fn pending_event_count(&self) -> usize {
        self.pending_public.len() + self.pending_private.len()
    }

    /// Drains commands and returns the resulting events through one seam.
    pub fn poll_events(&mut self) -> CoreEvents {
        let mut commands = 0;
        while let Some(result) = self.consumer.recv() {
            match result {
                Ok(command) => {
                    self.core
                        .execute_with_plugins_discard(command, &self.plugin);
                    commands += 1;
                }
                Err(_) => {
                    self.decode_errors += 1;
                }
            }
        }
        let private = if self.private_producer.is_some() {
            self.flush_private();
            Vec::new()
        } else {
            self.core.drain_private_events()
        };
        let public = if self.public_producer.is_some() {
            self.flush_public();
            Vec::new()
        } else {
            self.core.drain_public_events()
        };
        CoreEvents {
            commands,
            private,
            public,
        }
    }

    pub fn poll(&mut self) -> usize {
        self.poll_events().commands
    }

    /// Restores a durable core snapshot without exposing the kernel.
    ///
    /// # Errors
    /// Returns the core's snapshot compatibility error.
    pub fn restore_snapshot(
        &mut self,
        snapshot: crate::trading::core::CoreSnapshot,
    ) -> Result<(), String> {
        self.core.restore_snapshot(snapshot)
    }

    #[must_use]
    pub fn core(&self) -> &TradingCore {
        &self.core
    }

    #[must_use]
    pub fn decode_errors(&self) -> u64 {
        self.decode_errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::ContractSpec;
    use crate::shared::SymbolId;
    use crate::trading::core::plugins::NoPlugins;
    use crate::trading::core::protocol::{
        Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
        VolumeCondition,
    };
    use crate::trading::transport::{
        create, create_private_event_transport, create_public_event_transport,
    };

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "trading-system-engine-runner-{name}-{}",
            std::process::id()
        ))
    }

    fn command(seq: u64, side: Side, price: i64, quantity: u64) -> Command {
        Command {
            global_seq: seq,
            gateway_seq: seq,
            ingress_ts_ns: 0,
            trading_ts_ns: 0,
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
    fn runner_flushes_events_to_transports() {
        let cmd_path = temp_path("flush-cmd");
        let public_path = temp_path("flush-public");
        let private_path = temp_path("flush-private");
        for path in [&cmd_path, &public_path, &private_path] {
            let _ = std::fs::remove_file(path);
        }
        let (producer, consumer) = create(&cmd_path, 8).unwrap();
        let (public_producer, public_consumer) =
            create_public_event_transport(&public_path, 8).unwrap();
        let (private_producer, private_consumer) =
            create_private_event_transport(&private_path, 8).unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let mut runner = CoreRunner::new(core, consumer, NoPlugins)
            .with_event_transports(public_producer, private_producer);

        producer.send(&command(1, Side::Buy, 50_000, 5)).unwrap();
        assert_eq!(runner.poll(), 1);
        let public = public_consumer.recv().unwrap().unwrap();
        assert_eq!(
            public.event_type,
            crate::trading::core::public_events::PublicEventType::OrderRested
        );
        let private = private_consumer.recv().unwrap().unwrap();
        assert_eq!(
            private.status,
            crate::trading::core::private_events::PrivateStatus::Accepted
        );
        drop(producer);
        for path in [&cmd_path, &public_path, &private_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn full_event_ring_retains_pending_events() {
        let cmd_path = temp_path("retry-cmd");
        let public_path = temp_path("retry-public");
        let private_path = temp_path("retry-private");
        for path in [&cmd_path, &public_path, &private_path] {
            let _ = std::fs::remove_file(path);
        }
        let (producer, consumer) = create(&cmd_path, 8).unwrap();
        let (public_producer, public_consumer) =
            create_public_event_transport(&public_path, 1).unwrap();
        let (private_producer, _private_consumer) =
            create_private_event_transport(&private_path, 8).unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let mut runner = CoreRunner::new(core, consumer, NoPlugins)
            .with_event_transports(public_producer, private_producer);

        producer.send(&command(1, Side::Buy, 50_000, 5)).unwrap();
        producer.send(&command(2, Side::Buy, 50_100, 5)).unwrap();
        assert_eq!(runner.poll(), 2);
        assert_eq!(runner.pending_event_count(), 1);
        let first = public_consumer.recv().unwrap().unwrap();
        assert_eq!(first.quantity, 5);
        runner.flush_pending();
        assert_eq!(runner.pending_event_count(), 0);
        let second = public_consumer.recv().unwrap().unwrap();
        assert_eq!(second.price_ticks, 50_100);
        drop(producer);
        for path in [&cmd_path, &public_path, &private_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn runner_drains_commands_into_core() {
        let path = temp_path("basic");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let mut runner = CoreRunner::new(core, consumer, NoPlugins);

        producer.send(&command(1, Side::Buy, 50_000, 5)).unwrap();
        producer.send(&command(2, Side::Sell, 50_000, 5)).unwrap();
        assert_eq!(runner.poll(), 2);
        assert!(runner.core().best_bid().is_none());
        assert!(!runner.core().positions().snapshot().is_empty());
        drop(producer);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn runner_can_execute_time_tick() {
        let path = temp_path("tick");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 4).unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let mut runner = CoreRunner::new(core, consumer, NoPlugins);
        let mut tick = command(1, Side::Buy, 50_000, 1);
        tick.command_type = CommandType::TimeTick;
        tick.trading_ts_ns = 99;
        producer.send(&tick).unwrap();
        assert_eq!(runner.poll(), 1);
        assert_eq!(runner.core().last_trading_ts_ns(), 99);
        assert_eq!(runner.poll_events().public.len(), 1);
        drop(producer);
        let _ = std::fs::remove_file(&path);
    }
}
