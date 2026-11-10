//! Minimal end-to-end runtime: gateway TCP ↔ middleware ↔ core.
//!
//! One `tick()` drains gateway events into the middleware command ring, runs the
//! core command consumer, then routes core private events back to the matching
//! gateway session.

use crate::trading::combo::runtime::{ComboRuntime, ComboRuntimeSnapshot};
use crate::trading::core::plugins::PreTradePlugin;
use crate::trading::core::{CoreSnapshot, TradingCore};
use crate::trading::gateway::runtime::GatewayRuntime;
use crate::trading::gateway::server::GatewayServerHandle;
use crate::trading::market_data::MarketDataPublisher;
use crate::trading::market_data::metrics::TradingMetrics;
use crate::trading::market_data::repair::{NackRequest, RepairServer};
use crate::trading::runtime::event_fanout::EventFanout;
use crate::trading::transport::result_log::{PrivateResultLog, PublicResultLog};
use crate::trading::transport::runner::CoreRunner;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Full runtime snapshot: core state plus optional combo state.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TradingSnapshot {
    pub core: CoreSnapshot,
    pub combo: Option<ComboRuntimeSnapshot>,
}

const TRADING_SNAPSHOT_VERSION: u32 = 4;

impl TradingSnapshot {
    /// Writes a versioned, checksummed snapshot with atomic replacement.
    ///
    /// # Errors
    /// Returns an I/O error on serialization or file replacement.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        crate::trading::snapshot::snapshot_file::save(path, self, TRADING_SNAPSHOT_VERSION)
    }

    /// Loads a snapshot written by [`TradingSnapshot::save`].
    ///
    /// # Errors
    /// Returns an I/O error for read, JSON, version, or checksum failures.
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        crate::trading::snapshot::snapshot_file::load(path, TRADING_SNAPSHOT_VERSION)
    }
}

/// Optional collaborators for one runtime instance.
#[derive(Default)]
pub struct RuntimeOptions {
    pub public_log: Option<PublicResultLog>,
    pub private_log: Option<PrivateResultLog>,
    pub repair_buffer_capacity: Option<usize>,
    pub repair_server: Option<RepairServer>,
    pub market_data: Option<MarketDataPublisher>,
    pub combo: Option<ComboRuntime>,
    pub metrics_sink: Option<Arc<Mutex<TradingMetrics>>>,
    pub idle_timeout: Option<Duration>,
}

/// Composite gateway/middleware/core runtime for one process.
pub struct TradingRuntime<P> {
    gateway: GatewayRuntime,
    core: CoreRunner<P>,
    server: GatewayServerHandle,
    idle_timeout: Option<Duration>,
    last_expire: Instant,
    combo: Option<ComboRuntime>,
    fanout: EventFanout,
    metrics: TradingMetrics,
    metrics_sink: Option<Arc<Mutex<TradingMetrics>>>,
}

impl<P: PreTradePlugin> TradingRuntime<P> {
    #[must_use]
    pub fn new(gateway: GatewayRuntime, core: CoreRunner<P>, server: GatewayServerHandle) -> Self {
        Self {
            gateway,
            core,
            server,
            idle_timeout: None,
            last_expire: Instant::now(),
            combo: None,
            fanout: EventFanout::new(),
            metrics: TradingMetrics::new(),
            metrics_sink: None,
        }
    }

    #[must_use]
    pub fn with_options(mut self, options: RuntimeOptions) -> Self {
        if let Some(public) = options.public_log {
            self.fanout = self.fanout.with_public_log(public);
        }
        if let Some(private) = options.private_log {
            self.fanout = self.fanout.with_private_log(private);
        }
        if let Some(capacity) = options.repair_buffer_capacity {
            self.fanout = self.fanout.with_repair_buffer(capacity);
        }
        if let Some(server) = options.repair_server {
            self.fanout = self.fanout.with_repair_server(server);
        }
        if let Some(publisher) = options.market_data {
            self.fanout = self.fanout.with_market_data(publisher);
        }
        self.combo = options.combo;
        self.metrics_sink = options.metrics_sink;
        self.idle_timeout = options.idle_timeout;
        self
    }

    pub(crate) fn combo_mut(&mut self) -> Option<&mut ComboRuntime> {
        self.combo.as_mut()
    }

    #[must_use]
    pub fn repair_sent(&self) -> u64 {
        self.fanout.repair_sent()
    }

    #[must_use]
    pub fn public_sent(&self) -> u64 {
        self.fanout.public_sent()
    }

    #[must_use]
    pub(crate) fn repair(
        &self,
        request: NackRequest,
    ) -> Vec<crate::trading::core::public_events::PublicEvent> {
        self.fanout.repair(request)
    }

    pub fn tick(&mut self) {
        let tick_start = Instant::now();
        let effects = self.gateway.poll_effects();
        self.metrics.gateway_events += effects.events as u64;
        self.metrics.active_sessions = effects.active_sessions as u64;
        self.metrics.closed_sessions = effects.closed_sessions as u64;
        for (connection_id, address) in effects.redirects {
            let _ = self.server.send_redirect(connection_id, &address);
            self.server.close(connection_id);
        }
        for (connection_id, replay) in effects.replays {
            for event in replay {
                let sent = match &event {
                    crate::trading::gateway::private_replay::ReplayEvent::Core(event) => {
                        self.server.send_private(connection_id, event)
                    }
                    crate::trading::gateway::private_replay::ReplayEvent::Combo(event) => {
                        self.server.send_combo_private(connection_id, event)
                    }
                };
                if sent.is_ok() {
                    self.metrics.replay_events_sent += 1;
                }
            }
        }
        let commands = self.core.poll();
        self.metrics.commands_executed += commands as u64;
        let private = self.core.drain_private();
        self.fanout.log_private(&private);
        let sent = self.gateway.dispatch_private(&self.server, private);
        self.metrics.private_events_sent += sent as u64;
        let public = self.core.drain_public();
        let public_before = self.fanout.public_sent();
        self.fanout.publish_public(&public);
        self.metrics.public_events_sent += self.fanout.public_sent() - public_before;
        if let Some(combo) = &mut self.combo {
            combo.poll_commands();
            let effects = combo.tick_effects();
            self.metrics.public_events_sent += effects.public_sent as u64;
            let command_events = self
                .gateway
                .dispatch_private(&self.server, effects.private_commands);
            let fill_events = self.gateway.dispatch_combo_private(
                &self.server,
                effects
                    .private_fills
                    .iter()
                    .map(crate::trading::combo::engine::ComboFillEvent::private_fill),
            );
            self.metrics.private_events_sent += (command_events + fill_events) as u64;
            self.metrics.combo_queue_orders = combo.engine().queue_len() as u64;
        }
        let repaired = self.fanout.poll_repair();
        self.metrics.repair_events_sent += repaired;
        if let Some(timeout) = self.idle_timeout
            && self.last_expire.elapsed() >= timeout / 2
        {
            for connection_id in self.gateway.expire_idle(timeout) {
                self.server.close(connection_id);
                self.metrics.idle_sessions_expired += 1;
            }
            self.last_expire = Instant::now();
        }
        self.metrics.fanout_errors = self.fanout.sink_errors();
        let elapsed_ns = u64::try_from(tick_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.metrics.tick_latency.observe(elapsed_ns);
        if let Some(sink) = &self.metrics_sink
            && let Ok(mut snapshot) = sink.lock()
        {
            *snapshot = self.metrics;
        }
    }

    #[must_use]
    pub fn metrics(&self) -> TradingMetrics {
        self.metrics
    }

    #[must_use]
    pub(crate) fn core(&self) -> &TradingCore {
        self.core.core()
    }

    #[must_use]
    pub fn snapshot(&self) -> TradingSnapshot {
        TradingSnapshot {
            core: self.core.core().snapshot(),
            combo: self.combo.as_ref().map(ComboRuntime::snapshot),
        }
    }

    /// Restores core and combo state.
    ///
    /// # Errors
    /// Returns an error for invalid core snapshot data or combo runtime mismatch.
    pub fn restore_snapshot(&mut self, snapshot: TradingSnapshot) -> Result<(), String> {
        if self.gateway.session_counts().0 > 0 {
            return Err("cannot restore a snapshot while sessions are active".to_string());
        }
        self.core.core_mut().restore_snapshot(snapshot.core)?;
        match (self.combo.as_mut(), snapshot.combo) {
            (Some(combo), Some(combo_snapshot)) => combo.restore_snapshot(combo_snapshot),
            (None, None) => Ok(()),
            _ => Err("snapshot combo state does not match runtime configuration".to_string()),
        }
    }

    #[cfg(test)]
    pub(crate) fn gateway_mut(&mut self) -> &mut GatewayRuntime {
        &mut self.gateway
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
    use crate::trading::gateway::GatewayFrame;
    use crate::trading::gateway::runtime::GatewayRuntime;
    use crate::trading::gateway::server::start;
    use crate::trading::transport::create;
    use crate::trading::transport::middleware::MiddlewareSequencer;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    fn command() -> Command {
        Command {
            global_seq: 0,
            gateway_seq: 1,
            ingress_ts_ns: 0,
            trading_ts_ns: 0,
            client_order_id: 1,
            session_id: 0,
            gateway_id: 1,
            member_id: 0,
            user_id: 2,
            seat_id: 3,
            instrument_id: 1,
            combo_id: 0,
            session_epoch: 1,
            command_type: CommandType::NewOrder,
            price_type: PriceType::Limit,
            time_condition: TimeCondition::Gfd,
            volume_condition: VolumeCondition::Any,
            side: Side::Buy,
            offset: Offset::Open,
            hedge: Hedge::Spec,
            flags: 0,
            leg2_ratio: 0,
            price_ticks: 50_000,
            quantity: 5,
            trigger_price_ticks: 0,
            leg2_instrument_id: 0,
            leg2_offset: Offset::Open,
            leg2_hedge: Hedge::Spec,
            trigger_type: TriggerType::None,
            cancel_order_id: 0,
        }
    }

    #[test]
    fn multiple_gateway_clients_fan_out_public_events() {
        use crate::trading::core::protocol::{Command, CommandType, Hedge, Offset, Side};
        use crate::trading::market_data::{MarketDataPublisher, MarketDataSubscriber};
        use std::io::Write;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-fanout-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 1024).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
        let mut runtime =
            TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
                market_data: Some(publisher),
                ..Default::default()
            });

        let clients = 4u64;
        let per_client = 25u64;
        let mut streams = Vec::new();
        for client in 0..clients {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .write_all(
                    &GatewayFrame::Login {
                        member_id: client as u32 + 1,
                        auth_token: 0,
                        user_id: client as u32 + 1,
                        seat_id: 1,
                        session_epoch: 1,
                        last_private_seq: 0,
                    }
                    .encode(),
                )
                .unwrap();
            for seq in 0..per_client {
                let mut command = Command::zeroed();
                command.command_type = CommandType::NewOrder;
                command.client_order_id = client * per_client + seq + 1;
                command.user_id = client as u32 + 1;
                command.side = Side::Buy;
                command.price_ticks = 50_000 + client as i64 * 100 + seq as i64;
                command.quantity = 1;
                command.offset = Offset::Open;
                command.hedge = Hedge::Spec;
                stream
                    .write_all(&GatewayFrame::Command(command).encode())
                    .unwrap();
            }
            streams.push(stream);
        }

        for _ in 0..2000 {
            runtime.tick();
            if runtime.public_sent() == clients * per_client {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(runtime.public_sent(), clients * per_client);
        for _ in 0..clients * per_client {
            subscriber.recv().unwrap();
        }
        drop(streams);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn combo_order_flows_gateway_to_public_transport() {
        use crate::shared::protocol::OrderType;
        use crate::trading::combo::engine::ComboEngine;
        use crate::trading::combo::market_data::{
            ComboMarketDataPublisher, ComboMarketDataSubscriber,
        };
        use crate::trading::combo::runtime::ComboRuntime;
        use crate::trading::core::protocol::{Command, CommandType, Hedge, Offset, Side};
        use std::io::Read;

        let core_path = std::env::temp_dir().join(format!(
            "trading-system-engine-combo-e2e-core-{}",
            std::process::id()
        ));
        let combo_path = std::env::temp_dir().join(format!(
            "trading-system-engine-combo-e2e-ring-{}",
            std::process::id()
        ));
        for path in [&core_path, &combo_path] {
            let _ = std::fs::remove_file(path);
        }
        let (core_producer, core_consumer) = create(&core_path, 16).unwrap();
        let (combo_producer, combo_consumer) = create(&combo_path, 16).unwrap();
        let middleware =
            MiddlewareSequencer::new(core_producer).with_combo_transport(combo_producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, core_consumer, NoPlugins);

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
        let subscriber = ComboMarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            ComboMarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap())
                .unwrap();
        let combo = ComboRuntime::new(engine)
            .with_command_consumer(combo_consumer)
            .with_publisher(publisher);
        let mut runtime =
            TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
                combo: Some(combo),
                ..Default::default()
            });

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    auth_token: 0,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        let mut combo_command = Command::zeroed();
        combo_command.command_type = CommandType::NewOrder;
        combo_command.combo_id = 7;
        combo_command.client_order_id = 42;
        combo_command.user_id = 2;
        combo_command.side = Side::Buy;
        combo_command.price_ticks = 49_900;
        combo_command.quantity = 5;
        combo_command.offset = Offset::Open;
        combo_command.hedge = Hedge::Spec;
        stream
            .write_all(&GatewayFrame::Command(combo_command).encode())
            .unwrap();

        let mut private = None;
        for _ in 0..500 {
            runtime.tick();
            let mut frame = [0u8; crate::trading::gateway::GATEWAY_FRAME_SIZE];
            if stream.read_exact(&mut frame).is_ok() {
                private = Some(GatewayFrame::decode(&frame).unwrap());
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(matches!(
            private,
            Some(GatewayFrame::PrivateEvent(event)) if event.status == crate::trading::core::private_events::PrivateStatus::Accepted
        ));
        let fill = subscriber.recv().unwrap();
        assert_eq!(fill.quantity, 5);
        assert_eq!(fill.spread_ticks, 49_900);
        for path in [&core_path, &combo_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn combo_runtime_tick_is_driven_by_runtime() {
        use crate::shared::protocol::OrderType;
        use crate::trading::combo::engine::ComboEngine;
        use crate::trading::combo::market_data::{
            ComboMarketDataPublisher, ComboMarketDataSubscriber,
        };
        use crate::trading::combo::runtime::ComboRuntime;
        use crate::trading::core::protocol::{Hedge, Offset, Side};

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-trading-runtime-combo-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);

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
        let combo = ComboRuntime::new(engine).with_publisher(publisher);
        let mut runtime =
            TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
                combo: Some(combo),
                ..Default::default()
            });
        runtime.tick();
        let fill = subscriber.recv().unwrap();
        assert_eq!(fill.quantity, 5);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn config_builder_sets_repair_capacity_and_timeout() {
        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-trading-runtime-config-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let runtime = TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
            repair_buffer_capacity: Some(3),
            ..RuntimeOptions::default()
        });
        assert_eq!(
            runtime
                .repair(NackRequest {
                    from_seq: 1,
                    to_seq: 1
                })
                .len(),
            0
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn runtime_public_events_fill_repair_buffer() {
        use crate::trading::market_data::repair::NackRequest;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-trading-runtime-repair-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let mut runtime =
            TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
                repair_buffer_capacity: Some(16),
                ..Default::default()
            });

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    auth_token: 0,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        stream
            .write_all(&GatewayFrame::Command(command()).encode())
            .unwrap();
        for _ in 0..100 {
            runtime.tick();
            if !runtime
                .repair(NackRequest {
                    from_seq: 1,
                    to_seq: 1,
                })
                .is_empty()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let repaired = runtime.repair(NackRequest {
            from_seq: 1,
            to_seq: 10,
        });
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].seq, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn runtime_repair_server_serves_nack() {
        use crate::trading::market_data::repair::{NackRequest, RepairServer};
        use crate::trading::market_data::tracker::NackClient;
        use crate::trading::market_data::{MarketDataPublisher, MarketDataSubscriber};

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-trading-runtime-repair-server-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
        let repair = RepairServer::bind("127.0.0.1:0", publisher, 16).unwrap();
        let repair_addr = repair.local_addr().unwrap();
        let mut runtime =
            TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
                repair_server: Some(repair),
                ..Default::default()
            });

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    auth_token: 0,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        stream
            .write_all(&GatewayFrame::Command(command()).encode())
            .unwrap();
        let mut saw_public = false;
        for _ in 0..100 {
            runtime.tick();
            if runtime.public_sent() > 0 || runtime.repair_sent() > 0 {
                saw_public = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // Public events are pushed into the repair server even without a normal
        // publisher, so use a NACK to pull the first event back.
        assert!(!saw_public);
        let client = NackClient::bind("127.0.0.1:0", repair_addr).unwrap();
        client
            .send(NackRequest {
                from_seq: 1,
                to_seq: 1,
            })
            .unwrap();
        for _ in 0..100 {
            runtime.tick();
            if runtime.repair_sent() > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(runtime.repair_sent(), 1);
        assert_eq!(subscriber.recv().unwrap().seq, 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn core_public_events_reach_udp_subscriber() {
        use crate::trading::market_data::MarketDataSubscriber;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-trading-runtime-udp-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
        let mut runtime =
            TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
                market_data: Some(publisher),
                ..Default::default()
            });

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    auth_token: 0,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        stream
            .write_all(&GatewayFrame::Command(command()).encode())
            .unwrap();
        for _ in 0..100 {
            runtime.tick();
            if runtime.public_sent() > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let event = subscriber.recv().unwrap();
        assert_eq!(
            event.event_type,
            crate::trading::core::public_events::PublicEventType::OrderRested
        );
        assert_eq!(runtime.public_sent(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn runtime_expires_idle_gateway() {
        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-trading-runtime-idle-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let mut runtime =
            TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
                idle_timeout: Some(Duration::from_millis(30)),
                ..Default::default()
            });

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    auth_token: 0,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        for _ in 0..50 {
            runtime.tick();
            if runtime
                .gateway_mut()
                .middleware_mut()
                .session_by_user(2)
                .is_some()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(40));
        runtime.tick();
        assert!(
            runtime
                .gateway_mut()
                .middleware_mut()
                .session_by_user(2)
                .is_none()
        );
        drop(stream);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn order_flows_gateway_to_core_and_private_back() {
        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-trading-runtime-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let metrics_sink = Arc::new(Mutex::new(TradingMetrics::new()));
        let mut runtime =
            TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
                metrics_sink: Some(Arc::clone(&metrics_sink)),
                ..Default::default()
            });

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    auth_token: 0,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                }
                .encode(),
            )
            .unwrap();
        stream
            .write_all(&GatewayFrame::Command(command()).encode())
            .unwrap();

        let mut received = None;
        for _ in 0..200 {
            runtime.tick();
            let mut frame = [0u8; crate::trading::gateway::GATEWAY_FRAME_SIZE];
            if stream.read_exact(&mut frame).is_ok() {
                received = Some(GatewayFrame::decode(&frame).unwrap());
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        match received {
            Some(GatewayFrame::PrivateEvent(event)) => {
                assert_eq!(event.account_id, 2);
                assert_eq!(event.remaining, 5);
            }
            other => panic!("expected private event, got {other:?}"),
        }
        let metrics = runtime.metrics();
        assert!(metrics.gateway_events > 0);
        assert_eq!(metrics.commands_executed, 1);
        assert_eq!(metrics.private_events_sent, 1);
        assert!(metrics.tick_latency.count() > 0);
        let shared = *metrics_sink.lock().unwrap();
        assert_eq!(shared.commands_executed, 1);
        let _ = std::fs::remove_file(&path);
    }
    #[test]
    fn trading_snapshot_file_roundtrip() {
        let core = TradingCore::new(
            ContractSpec::new("BTCUSDT", 1, 1, 1_000_000),
            SymbolId::intern("BTCUSDT"),
        );
        let snapshot = TradingSnapshot {
            core: core.snapshot(),
            combo: None,
        };
        let path = std::env::temp_dir().join(format!(
            "trading-runtime-snapshot-{}.json",
            std::process::id()
        ));
        snapshot.save(&path).unwrap();
        let loaded = TradingSnapshot::load(&path).unwrap();
        assert_eq!(loaded.core, snapshot.core);
        assert!(loaded.combo.is_none());

        let corrupted = std::fs::read(&path).unwrap();
        let mut corrupted = corrupted;
        corrupted[20] ^= 0xff;
        std::fs::write(&path, corrupted).unwrap();
        assert!(TradingSnapshot::load(&path).is_err());

        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn full_snapshot_restores_combo_runtime() {
        use crate::trading::combo::engine::ComboEngine;
        use crate::trading::combo::runtime::ComboRuntime;

        let core_path =
            std::env::temp_dir().join(format!("trading-full-snapshot-core-{}", std::process::id()));
        let combo_path = std::env::temp_dir().join(format!(
            "trading-full-snapshot-combo-{}",
            std::process::id()
        ));
        for path in [&core_path, &combo_path] {
            let _ = std::fs::remove_file(path);
        }
        let (core_producer, core_consumer) = create(&core_path, 16).unwrap();
        let (combo_producer, _combo_consumer) = create(&combo_path, 16).unwrap();
        let middleware =
            MiddlewareSequencer::new(core_producer).with_combo_transport(combo_producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let gateway = GatewayRuntime::new(middleware, events);
        let core = TradingCore::new(
            ContractSpec::new("BTCUSDT", 1, 1, 1_000_000),
            SymbolId::intern("BTCUSDT"),
        );
        let runner = CoreRunner::new(core, core_consumer, NoPlugins);
        let engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        );
        let mut runtime =
            TradingRuntime::new(gateway, runner, server).with_options(RuntimeOptions {
                combo: Some(ComboRuntime::new(engine)),
                ..Default::default()
            });

        let mut command = Command::zeroed();
        command.command_type = CommandType::NewOrder;
        command.combo_id = 7;
        command.client_order_id = 42;
        command.user_id = 1;
        command.side = Side::Buy;
        command.price_ticks = 49_900;
        command.quantity = 5;
        command.offset = Offset::Open;
        command.hedge = Hedge::Spec;
        assert!(runtime.combo_mut().unwrap().submit_command(&command));
        let snapshot = runtime.snapshot();

        let mut cancel = Command::zeroed();
        cancel.command_type = CommandType::Cancel;
        cancel.combo_id = 7;
        cancel.cancel_order_id = 42;
        assert!(runtime.combo_mut().unwrap().submit_command(&cancel));
        assert_eq!(runtime.combo_mut().unwrap().engine().queue_len(), 0);

        let json = serde_json::to_string(&snapshot).unwrap();
        let snapshot: TradingSnapshot = serde_json::from_str(&json).unwrap();
        runtime.restore_snapshot(snapshot).unwrap();
        assert_eq!(runtime.combo_mut().unwrap().engine().queue_len(), 1);
        let _ = std::fs::remove_file(&core_path);
        let _ = std::fs::remove_file(&combo_path);
    }
    #[test]
    fn restore_is_rejected_with_active_sessions() {
        use crate::trading::transport::session::SessionInfo;

        let path =
            std::env::temp_dir().join(format!("trading-restore-active-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let gateway = GatewayRuntime::new(middleware, events);
        let core = TradingCore::new(
            ContractSpec::new("BTCUSDT", 1, 1, 1_000_000),
            SymbolId::intern("BTCUSDT"),
        );
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let mut runtime = TradingRuntime::new(gateway, runner, server);
        runtime
            .gateway_mut()
            .middleware_mut()
            .open_or_takeover(SessionInfo {
                session_id: 1,
                gateway_id: 1,
                member_id: 1,
                user_id: 1,
                seat_id: 1,
                last_private_seq: 0,
                active: true,
            })
            .unwrap();
        let snapshot = TradingSnapshot {
            core: runtime.core().snapshot(),
            combo: None,
        };
        assert!(runtime.restore_snapshot(snapshot).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
