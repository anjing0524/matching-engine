//! Minimal core command-consumer loop.
//!
//! The runner drains the shared-memory command transport, executes commands
//! through a static pre-trade plugin chain, and exposes the resulting private
//! and public events. It is deliberately single-threaded and has no networking.

use crate::trading::core::combo_book::{ComboCommandError, ComboEvent, ComboPrivateFill};
use crate::trading::core::plugins::PreTradePlugin;
use crate::trading::core::private_events::{PrivateEffect, PrivateEvent, PrivateStatus};
use crate::trading::core::protocol::{Command, CommandType};
use crate::trading::core::public_events::PublicEvent;
use crate::trading::core::registry::{InstrumentRegistry, RegistrySnapshot, RegistrySnapshotError};
use crate::trading::core::{CoreSnapshot, TradingCore};
use crate::trading::transport::{
    CommandConsumer, CommandTransportError, PrivateEventProducer, PublicEventProducer,
};
use std::collections::VecDeque;
use std::fmt::{Display, Formatter, Result as FmtResult};

fn flush_queue<T>(
    mut send: impl FnMut(&T) -> Result<(), CommandTransportError>,
    pending: &mut VecDeque<T>,
    events: Vec<T>,
) {
    pending.extend(events);
    while let Some(event) = pending.front() {
        match send(event) {
            Ok(()) => {
                pending.pop_front();
            }
            Err(CommandTransportError::Full) => break,
            Err(_) => {
                pending.pop_front();
            }
        }
    }
}

fn drain_commands(
    consumer: &CommandConsumer,
    decode_errors: &mut u64,
    mut execute: impl FnMut(Command),
) -> usize {
    let mut commands = 0;
    while let Some(result) = consumer.recv() {
        match result {
            Ok(command) => {
                execute(command);
                commands += 1;
            }
            Err(_) => *decode_errors += 1,
        }
    }
    commands
}

/// Snapshot restore failure returned by [`CoreRunner`].
#[derive(Debug)]
pub struct CoreRestoreError(String);

impl Display for CoreRestoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CoreRestoreError {}

/// One poll's core commands and projected events.
#[non_exhaustive]
pub struct CoreEvents {
    /// Number of commands executed during this poll.
    pub commands: usize,
    /// Private events produced by the poll.
    pub private: Vec<PrivateEvent>,
    /// Public events produced by the poll.
    pub public: Vec<PublicEvent>,
    /// Combo private fills produced by the poll.
    ///
    /// A plain [`CoreRunner`] has no combo source, so this is always empty.
    pub combo_private: Vec<ComboPrivateFill>,
    /// Ordered core/combo private effects for one per-user sequence.
    ///
    /// `RegistryRunner` fills this in command order; `CoreRunner` leaves it
    /// empty and uses `private`.
    pub ordered_private: Vec<PrivateEffect>,
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
        let events = self.core.drain_public_events();
        flush_queue(
            |event| producer.send(event),
            &mut self.pending_public,
            events,
        );
    }

    fn flush_private(&mut self) {
        let Some(producer) = &self.private_producer else {
            return;
        };
        let events = self.core.drain_private_events();
        flush_queue(
            |event| producer.send(event),
            &mut self.pending_private,
            events,
        );
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
    #[must_use]
    pub fn poll_events(&mut self) -> CoreEvents {
        let consumer = &self.consumer;
        let core = &mut self.core;
        let plugin = &self.plugin;
        let decode_errors = &mut self.decode_errors;
        let commands = drain_commands(consumer, decode_errors, |command| {
            core.execute_with_plugins_discard(command, plugin);
        });
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
            combo_private: Vec::new(),
            ordered_private: Vec::new(),
        }
    }

    /// Restores a durable core snapshot without exposing the kernel.
    ///
    /// # Errors
    /// Returns the core's snapshot compatibility error.
    pub fn restore_snapshot(&mut self, snapshot: CoreSnapshot) -> Result<(), CoreRestoreError> {
        self.core
            .restore_snapshot(snapshot)
            .map_err(CoreRestoreError)
    }

    #[must_use]
    pub fn core(&self) -> &TradingCore {
        &self.core
    }

    /// Mutable core access for snapshot restore at the runtime seam.
    pub fn core_mut(&mut self) -> &mut TradingCore {
        &mut self.core
    }

    #[must_use]
    pub fn decode_errors(&self) -> u64 {
        self.decode_errors
    }
}

/// Drains commands into an [`InstrumentRegistry`].
///
/// This is the single-process production command seam. Commands with a
/// non-zero `combo_id` are routed to the registry's static Combo Pair book;
/// every other command is routed to the ordinary Contract core named by
/// `instrument_id`. Because both routes share one registry, combo matching
/// consumes the same Contract books as ordinary leg orders and never starts a
/// second combo engine.
pub struct RegistryRunner<P> {
    registry: InstrumentRegistry,
    consumer: CommandConsumer,
    plugin: P,
    decode_errors: u64,
}

fn execute_combo_command(
    registry: &mut InstrumentRegistry,
    command: Command,
    statuses: &mut Vec<PrivateEvent>,
) {
    let next_order_id = registry.next_combo_order_id();
    match registry.execute_combo(command) {
        Ok(events) => {
            if command.command_type == CommandType::Cancel {
                if let Some(ComboEvent::Canceled { order }) = events.first() {
                    statuses.push(PrivateEvent {
                        seq: 0,
                        order_id: order.order_id.get(),
                        client_order_id: order.owner.client_order_id.get(),
                        account_id: order.owner.user_id.get(),
                        status: PrivateStatus::Canceled,
                        filled_delta: 0,
                        remaining: 0,
                        reason: None,
                    });
                }
                return;
            }
            let filled = taker_filled_quantity(next_order_id, &events);
            statuses.push(PrivateEvent {
                seq: 0,
                order_id: next_order_id,
                client_order_id: command.client_order_id,
                account_id: u64::from(command.user_id),
                status: PrivateStatus::Accepted,
                filled_delta: 0,
                remaining: command.quantity.saturating_sub(filled),
                reason: None,
            });
        }
        Err(error) => {
            statuses.push(combo_rejection(command, error));
        }
    }
}

impl<P: PreTradePlugin> RegistryRunner<P> {
    /// Creates a runner over an already configured registry.
    ///
    /// A registry with Combo Pairs fails closed: every account must have an
    /// explicit margin balance before it can enter a Combo Order.
    #[must_use]
    pub fn new(mut registry: InstrumentRegistry, consumer: CommandConsumer, plugin: P) -> Self {
        registry.set_require_explicit_margin(registry.pair_count() > 0);
        Self {
            registry,
            consumer,
            plugin,
            decode_errors: 0,
        }
    }

    /// Drains commands and returns core private/public and Combo Private effects.
    ///
    /// Combo command failures become ordinary private rejections, so a member
    /// always learns that an unsupported combo order or cancel was refused.
    #[must_use]
    pub fn poll_events(&mut self) -> CoreEvents {
        let mut ordered_private = Vec::new();
        let mut core_private = Vec::new();
        let mut combo_private = Vec::new();
        let consumer = &self.consumer;
        let registry = &mut self.registry;
        let plugin = &self.plugin;
        let decode_errors = &mut self.decode_errors;
        let commands = drain_commands(consumer, decode_errors, |command| {
            if command.combo_id != 0 {
                let mut statuses = Vec::new();
                execute_combo_command(registry, command, &mut statuses);
                for status in statuses {
                    ordered_private.push(PrivateEffect::Core(status.clone()));
                    core_private.push(status);
                }
            } else {
                let _ = registry.execute_with_plugins_discard(command, plugin);
                for event in registry.drain_private_events() {
                    ordered_private.push(PrivateEffect::Core(event.clone()));
                    core_private.push(event);
                }
            }
            for fill in registry.drain_combo_private_events() {
                ordered_private.push(PrivateEffect::Combo(fill));
                combo_private.push(fill);
            }
        });
        if commands == 0 {
            return CoreEvents {
                commands,
                private: Vec::new(),
                public: Vec::new(),
                combo_private: Vec::new(),
                ordered_private: Vec::new(),
            };
        }
        CoreEvents {
            commands,
            private: core_private,
            public: self.registry.drain_public_events(),
            combo_private,
            ordered_private,
        }
    }

    /// Restores registry-backed combo state without exposing the registry.
    ///
    /// # Errors
    /// Returns the registry's fail-closed snapshot error.
    pub fn restore_snapshot(
        &mut self,
        snapshot: RegistrySnapshot,
    ) -> Result<(), RegistrySnapshotError> {
        self.registry.restore_snapshot(snapshot)?;
        // A restored venue with Combo Pairs fails closed: balances are
        // configuration and are not carried by the snapshot, so every account
        // must be configured explicitly before it can enter a Combo Order.
        self.registry
            .set_require_explicit_margin(self.registry.pair_count() > 0);
        Ok(())
    }

    /// Looks up the registry for startup configuration and state inspection.
    #[must_use]
    pub fn registry(&self) -> &InstrumentRegistry {
        &self.registry
    }

    /// Mutable registry access for startup configuration.
    pub fn registry_mut(&mut self) -> &mut InstrumentRegistry {
        &mut self.registry
    }

    /// Returns the number of commands that failed to decode.
    #[must_use]
    pub fn decode_errors(&self) -> u64 {
        self.decode_errors
    }
}

fn taker_filled_quantity(order_id: u64, events: &[ComboEvent]) -> u64 {
    events
        .iter()
        .filter_map(|event| match event {
            ComboEvent::Filled { fill }
                if fill.buy_order_id == order_id || fill.sell_order_id == order_id =>
            {
                Some(fill.quantity)
            }
            ComboEvent::ImpliedFilled { fill } if fill.order_id == order_id => Some(fill.quantity),
            _ => None,
        })
        .sum()
}

fn combo_rejection(command: Command, error: ComboCommandError) -> PrivateEvent {
    let order_id = if command.command_type == CommandType::Cancel {
        command.cancel_order_id
    } else {
        command.client_order_id
    };
    PrivateEvent {
        seq: 0,
        order_id,
        client_order_id: command.client_order_id,
        account_id: u64::from(command.user_id),
        status: PrivateStatus::Rejected,
        filled_delta: 0,
        remaining: 0,
        reason: Some(error.to_string()),
    }
}

/// The one runner seam for a runtime.
///
/// The target is still one of two execution bodies while Phase 3 is in
/// progress, but callers own a `VenueRunner`, not a runtime-local enum. This
/// lets the target implementations collapse without changing the runtime
/// composition root again.
pub enum VenueRunner<P> {
    /// Single Contract core, used by split deployment and focused tests.
    Core(Box<CoreRunner<P>>),
    /// Registry-backed venue, used by single-process production.
    Registry(Box<RegistryRunner<P>>),
}

impl<P: PreTradePlugin> VenueRunner<P> {
    /// Wraps a single-core runner.
    #[must_use]
    pub fn core(runner: CoreRunner<P>) -> Self {
        Self::Core(Box::new(runner))
    }

    /// Wraps a registry-backed runner.
    #[must_use]
    pub fn registry(runner: RegistryRunner<P>) -> Self {
        Self::Registry(Box::new(runner))
    }

    /// Drains one poll's commands and events.
    #[must_use]
    pub fn poll_events(&mut self) -> CoreEvents {
        match self {
            Self::Core(runner) => runner.poll_events(),
            Self::Registry(runner) => runner.poll_events(),
        }
    }

    /// Returns the single core, if this is the core target.
    #[must_use]
    pub fn as_core(&self) -> Option<&TradingCore> {
        match self {
            Self::Core(runner) => Some(runner.core()),
            Self::Registry(_) => None,
        }
    }

    /// Returns the single core mutably, if this is the core target.
    pub fn as_core_mut(&mut self) -> Option<&mut TradingCore> {
        match self {
            Self::Core(runner) => Some(runner.core_mut()),
            Self::Registry(_) => None,
        }
    }

    /// Returns the registry, if this is the registry target.
    #[must_use]
    pub fn as_registry(&self) -> Option<&InstrumentRegistry> {
        match self {
            Self::Core(_) => None,
            Self::Registry(runner) => Some(runner.registry()),
        }
    }

    /// Returns the registry mutably, if this is the registry target.
    pub fn as_registry_mut(&mut self) -> Option<&mut InstrumentRegistry> {
        match self {
            Self::Core(_) => None,
            Self::Registry(runner) => Some(runner.registry_mut()),
        }
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
        assert_eq!(runner.poll_events().commands, 1);
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
        assert_eq!(runner.poll_events().commands, 2);
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
        assert_eq!(runner.poll_events().commands, 2);
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
        let events = runner.poll_events();
        assert_eq!(events.commands, 1);
        assert_eq!(runner.core().last_trading_ts_ns(), 99);
        assert_eq!(events.public.len(), 1);
        drop(producer);
        let _ = std::fs::remove_file(&path);
    }
}
