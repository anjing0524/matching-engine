//! Combo execution → public market-data transport.
//!
//! Runs the combination queue/executor and forwards successful fill events to
//! the combo UDP publisher.

use crate::trading::combo::engine::{ComboEngine, ComboEngineSnapshot};
use crate::trading::combo::market_data::ComboMarketDataPublisher;
use crate::trading::combo::{ComboExecError, ComboFill};
use crate::trading::core::private_events::{PrivateEvent, PrivateStatus};
use crate::trading::core::protocol::{Command, CommandType, Side};
use crate::trading::transport::CommandConsumer;

/// Private fills and command statuses produced by one combo tick.
pub struct ComboEffects {
    pub public_sent: usize,
    pub private_commands: Vec<PrivateEvent>,
    pub private_fills: Vec<crate::trading::combo::engine::ComboFillEvent>,
}

/// Serializable combo runtime state.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ComboRuntimeSnapshot {
    pub engine: ComboEngineSnapshot,
    pub next_private_seq: u64,
    pub public_sent: u64,
}

/// Composite combo engine plus optional public transport.
pub struct ComboRuntime {
    engine: ComboEngine,
    publisher: Option<ComboMarketDataPublisher>,
    commands: Option<CommandConsumer>,
    command_events: Vec<PrivateEvent>,
    next_private_seq: u64,
    public_sent: u64,
}

impl ComboRuntime {
    #[must_use]
    pub fn new(engine: ComboEngine) -> Self {
        Self {
            engine,
            publisher: None,
            commands: None,
            command_events: Vec::new(),
            next_private_seq: 1,
            public_sent: 0,
        }
    }

    #[must_use]
    pub fn with_publisher(mut self, publisher: ComboMarketDataPublisher) -> Self {
        self.publisher = Some(publisher);
        self
    }

    #[must_use]
    pub fn with_command_consumer(mut self, consumer: CommandConsumer) -> Self {
        self.commands = Some(consumer);
        self
    }

    /// Drains combo commands from the middleware combo ring.
    pub fn poll_commands(&mut self) -> usize {
        let Some(consumer) = self.commands.take() else {
            return 0;
        };
        let mut executed = 0;
        while let Some(Ok(command)) = consumer.recv() {
            if self.submit_command(&command) {
                executed += 1;
            }
        }
        self.commands = Some(consumer);
        executed
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
        let mut reject_reason = None;
        let accepted = match command.command_type {
            CommandType::NewOrder => {
                if command.price_ticks < 0 {
                    reject_reason = Some("invalid combo spread");
                    false
                } else {
                    match self.engine.queue_checked(
                        command.client_order_id.max(1),
                        u64::from(command.user_id),
                        command.side,
                        command.price_ticks as u64,
                        command.quantity,
                        command.offset,
                        command.hedge,
                    ) {
                        Ok(()) => true,
                        Err(reason) => {
                            reject_reason = Some(reason.as_str());
                            false
                        }
                    }
                }
            }
            CommandType::Cancel => {
                let order_id = if command.cancel_order_id == 0 {
                    command.client_order_id
                } else {
                    command.cancel_order_id
                };
                if self.engine.cancel(order_id) {
                    true
                } else {
                    reject_reason = Some("combo order not found");
                    false
                }
            }
            _ => return false,
        };
        let (status, remaining) = match (command.command_type, accepted) {
            (CommandType::NewOrder, true) => (PrivateStatus::Accepted, command.quantity),
            (CommandType::NewOrder, false) => (PrivateStatus::Rejected, 0),
            (CommandType::Cancel, true) => (PrivateStatus::Canceled, 0),
            (CommandType::Cancel, false) => (PrivateStatus::Rejected, 0),
            _ => return false,
        };
        let seq = self.next_private_seq;
        self.next_private_seq += 1;
        self.command_events.push(PrivateEvent {
            seq,
            order_id: command.client_order_id.max(1),
            client_order_id: command.client_order_id,
            account_id: u64::from(command.user_id),
            status,
            filled_delta: 0,
            remaining,
            reason: reject_reason.map(str::to_string),
        });
        accepted
    }

    /// Drains private command-status events.
    pub fn drain_command_events(&mut self) -> Vec<PrivateEvent> {
        std::mem::take(&mut self.command_events)
    }

    /// Runs marketable combo orders, publishes fills, and returns private effects.
    pub fn tick_effects(&mut self) -> ComboEffects {
        let private_commands = self.drain_command_events();
        let _ = self.engine.run();
        let events = self.engine.drain_events();
        let mut public_sent = 0;
        if let Some(publisher) = &self.publisher {
            for event in &events {
                if publisher.send(&event.public_fill()).is_ok() {
                    public_sent += 1;
                    self.public_sent += 1;
                }
            }
        }
        ComboEffects {
            public_sent,
            private_commands,
            private_fills: events,
        }
    }

    /// Number of queued combo orders.
    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.engine.queue_len()
    }

    /// Open combination quantity for `user_id`.
    #[must_use]
    pub fn open_qty(&self, user_id: u64) -> u64 {
        self.engine.open_qty(user_id)
    }

    /// Executes the best marketable combo order for `side`.
    pub fn try_execute_best(&mut self, side: Side) -> Option<Result<ComboFill, ComboExecError>> {
        self.engine.try_execute_best(side)
    }

    #[must_use]
    pub fn next_private_seq(&self) -> u64 {
        self.next_private_seq
    }

    #[must_use]
    pub fn snapshot(&self) -> ComboRuntimeSnapshot {
        ComboRuntimeSnapshot {
            engine: self.engine.snapshot(),
            next_private_seq: self.next_private_seq,
            public_sent: self.public_sent,
        }
    }

    /// Restores combo engine and private sequence state.
    ///
    /// # Errors
    /// Returns an error when the saved leg books are invalid.
    pub fn restore_snapshot(&mut self, snapshot: ComboRuntimeSnapshot) -> Result<(), String> {
        self.engine.restore(snapshot.engine)?;
        self.next_private_seq = snapshot.next_private_seq;
        self.public_sent = snapshot.public_sent;
        self.command_events.clear();
        Ok(())
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
    use crate::shared::protocol::OrderType;
    use crate::trading::combo::market_data::ComboMarketDataSubscriber;
    use crate::trading::core::protocol::{Hedge, Offset, Side};

    #[test]
    fn combo_command_emits_private_status() {
        use crate::trading::core::private_events::PrivateStatus;
        use crate::trading::core::protocol::{Command, CommandType};

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
        let events = runtime.drain_command_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, PrivateStatus::Accepted);
        assert_eq!(events[0].remaining, 5);

        assert!(!runtime.submit_command(&command));
        let rejected = runtime.drain_command_events();
        assert_eq!(rejected[0].status, PrivateStatus::Rejected);
    }

    #[test]
    fn combo_command_ring_reaches_engine() {
        use crate::trading::transport::create;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-combo-commands-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        );
        let mut runtime = ComboRuntime::new(engine).with_command_consumer(consumer);
        let mut command = Command::zeroed();
        command.command_type = CommandType::NewOrder;
        command.combo_id = 7;
        command.client_order_id = 42;
        command.user_id = 1;
        command.side = Side::Buy;
        command.price_ticks = 49_900;
        command.quantity = 5;
        producer.send(&command).unwrap();
        assert_eq!(runtime.poll_commands(), 1);
        assert_eq!(runtime.queue_len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn combo_command_routes_into_engine() {
        use crate::trading::core::protocol::{Command, CommandType};

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
        assert_eq!(runtime.queue_len(), 1);

        let mut cancel = Command::zeroed();
        cancel.command_type = CommandType::Cancel;
        cancel.combo_id = 7;
        cancel.cancel_order_id = 42;
        assert!(runtime.submit_command(&cancel));
        assert_eq!(runtime.queue_len(), 0);
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
    #[test]
    fn runtime_snapshot_restores_queue_and_sequence() {
        let mut engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        )
        .with_max_open_qty_per_user(10);
        assert!(
            engine
                .queue_checked(42, 1, Side::Buy, 49_900, 5, Offset::Open, Hedge::Spec)
                .is_ok()
        );
        let runtime = ComboRuntime::new(engine);
        let snapshot = runtime.snapshot();
        let mut restored = ComboRuntime::new(ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1_000, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        ));
        restored.restore_snapshot(snapshot).unwrap();
        assert_eq!(restored.queue_len(), 1);
        assert_eq!(restored.next_private_seq(), 1);
    }
    #[test]
    fn tick_without_publisher_still_drains_fills() {
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
        let mut runtime = ComboRuntime::new(engine);
        assert_eq!(runtime.tick_effects().public_sent, 0);
        runtime = runtime.with_publisher(publisher);
        assert_eq!(runtime.tick_effects().public_sent, 0);
    }
}
