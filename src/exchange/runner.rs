//! Minimal core command-consumer loop.
//!
//! The runner drains the shared-memory command transport, executes commands
//! through a static pre-trade plugin chain, and exposes the resulting private
//! and public events. It is deliberately single-threaded and has no networking.

use crate::exchange::core::{CoreEvent, ExchangeCore};
use crate::exchange::plugins::PreTradePlugin;
use crate::exchange::private_events::PrivateEvent;
use crate::exchange::public_events::PublicEvent;
use crate::exchange::transport::CommandConsumer;

/// Drains commands from a `CommandConsumer` and applies them to `ExchangeCore`.
pub struct CoreRunner<P> {
    core: ExchangeCore,
    consumer: CommandConsumer,
    plugin: P,
    events: Vec<CoreEvent>,
}

impl<P: PreTradePlugin> CoreRunner<P> {
    #[must_use]
    pub fn new(core: ExchangeCore, consumer: CommandConsumer, plugin: P) -> Self {
        Self {
            core,
            consumer,
            plugin,
            events: Vec::new(),
        }
    }

    /// Drains all currently available commands. Returns the number executed.
    pub fn poll(&mut self) -> usize {
        let mut executed = 0;
        while let Some(result) = self.consumer.recv() {
            match result {
                Ok(command) => {
                    let mut produced = self.core.execute_with_plugins(command, &self.plugin);
                    self.events.append(&mut produced);
                    executed += 1;
                }
                Err(error) => {
                    self.events.push(CoreEvent::Rejected {
                        client_order_id: 0,
                        reason: format!("command decode error: {error:?}"),
                    });
                }
            }
        }
        executed
    }

    #[must_use]
    pub fn core(&self) -> &ExchangeCore {
        &self.core
    }

    pub fn core_mut(&mut self) -> &mut ExchangeCore {
        &mut self.core
    }

    pub fn drain_events(&mut self) -> Vec<CoreEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn drain_private(&mut self) -> Vec<PrivateEvent> {
        self.core.drain_private_events()
    }

    pub fn drain_public(&mut self) -> Vec<PublicEvent> {
        self.core.drain_public_events()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::ContractSpec;
    use crate::exchange::plugins::NoPlugins;
    use crate::exchange::protocol::{
        Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
        VolumeCondition,
    };
    use crate::exchange::transport::create;
    use crate::shared::SymbolId;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "matching-engine-runner-{name}-{}",
            std::process::id()
        ))
    }

    fn command(seq: u64, side: Side, price: i64, quantity: u64) -> Command {
        Command {
            global_seq: seq,
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
    fn runner_drains_commands_into_core() {
        let path = temp_path("basic");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let mut runner = CoreRunner::new(core, consumer, NoPlugins);

        producer.send(&command(1, Side::Buy, 50_000, 5)).unwrap();
        producer.send(&command(2, Side::Sell, 50_000, 5)).unwrap();
        assert_eq!(runner.poll(), 2);
        let events = runner.drain_events();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::Trade { quantity: 5, .. }))
        );
        assert!(runner.core().best_bid().is_none());
        drop(producer);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn runner_can_execute_time_tick() {
        let path = temp_path("tick");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 4).unwrap();
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let mut runner = CoreRunner::new(core, consumer, NoPlugins);
        let mut tick = command(1, Side::Buy, 50_000, 1);
        tick.command_type = CommandType::TimeTick;
        tick.exchange_ts_ns = 99;
        producer.send(&tick).unwrap();
        assert_eq!(runner.poll(), 1);
        assert_eq!(runner.core().last_exchange_ts_ns(), 99);
        assert_eq!(runner.drain_public().len(), 1);
        drop(producer);
        let _ = std::fs::remove_file(&path);
    }
}
