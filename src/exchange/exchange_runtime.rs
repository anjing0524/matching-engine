//! Minimal end-to-end runtime: gateway TCP ↔ middleware ↔ core.
//!
//! One `tick()` drains gateway events into the middleware command ring, runs the
//! core command consumer, then routes core private events back to the matching
//! gateway session.

use crate::exchange::combo_runtime::{ComboRuntime, ComboRuntimeSnapshot};
use crate::exchange::config::ExchangeConfig;
use crate::exchange::core::{CoreSnapshot, ExchangeCore};
use crate::exchange::event_fanout::EventFanout;
use crate::exchange::gateway_runtime::GatewayRuntime;
use crate::exchange::gateway_server::GatewayServerHandle;
use crate::exchange::market_data::MarketDataPublisher;
use crate::exchange::metrics::ExchangeMetrics;
use crate::exchange::plugins::PreTradePlugin;
use crate::exchange::repair::{NackRequest, RepairServer};
use crate::exchange::result_log::{PrivateResultLog, PublicResultLog};
use crate::exchange::runner::CoreRunner;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Full runtime snapshot: core state plus optional combo state.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExchangeSnapshot {
    pub core: CoreSnapshot,
    pub combo: Option<ComboRuntimeSnapshot>,
}

const EXCHANGE_SNAPSHOT_VERSION: u32 = 2;

impl ExchangeSnapshot {
    /// Writes a versioned, checksummed snapshot with atomic replacement.
    ///
    /// # Errors
    /// Returns an I/O error on serialization or file replacement.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        crate::exchange::snapshot_file::save(path, self, EXCHANGE_SNAPSHOT_VERSION)
    }

    /// Loads a snapshot written by [`ExchangeSnapshot::save`].
    ///
    /// # Errors
    /// Returns an I/O error for read, JSON, version, or checksum failures.
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        crate::exchange::snapshot_file::load(path, EXCHANGE_SNAPSHOT_VERSION)
    }
}

/// Composite gateway/middleware/core runtime for one process.
pub struct ExchangeRuntime<P> {
    gateway: GatewayRuntime,
    core: CoreRunner<P>,
    server: GatewayServerHandle,
    idle_timeout: Option<Duration>,
    last_expire: Instant,
    combo: Option<ComboRuntime>,
    fanout: EventFanout,
    metrics: ExchangeMetrics,
    metrics_sink: Option<Arc<Mutex<ExchangeMetrics>>>,
}

impl<P: PreTradePlugin> ExchangeRuntime<P> {
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
            metrics: ExchangeMetrics::new(),
            metrics_sink: None,
        }
    }

    #[must_use]
    pub fn with_result_logs(mut self, public: PublicResultLog, private: PrivateResultLog) -> Self {
        self.fanout = self
            .fanout
            .with_public_log(public)
            .with_private_log(private);
        self
    }

    #[must_use]
    pub fn with_public_result_log(mut self, public: PublicResultLog) -> Self {
        self.fanout = self.fanout.with_public_log(public);
        self
    }

    #[must_use]
    pub fn with_private_result_log(mut self, private: PrivateResultLog) -> Self {
        self.fanout = self.fanout.with_private_log(private);
        self
    }

    #[must_use]
    pub fn with_repair_server(mut self, server: RepairServer) -> Self {
        self.fanout = self.fanout.with_repair_server(server);
        self
    }

    #[must_use]
    pub fn repair_sent(&self) -> u64 {
        self.fanout.repair_sent()
    }

    #[must_use]
    pub fn with_combo_runtime(mut self, combo: ComboRuntime) -> Self {
        self.combo = Some(combo);
        self
    }

    pub fn combo_mut(&mut self) -> Option<&mut ComboRuntime> {
        self.combo.as_mut()
    }

    #[must_use]
    pub fn with_repair_buffer(mut self, capacity: usize) -> Self {
        self.fanout = self.fanout.with_repair_buffer(capacity);
        self
    }

    #[must_use]
    pub fn repair(&self, request: NackRequest) -> Vec<crate::exchange::public_events::PublicEvent> {
        self.fanout.repair(request)
    }

    #[must_use]
    pub fn with_market_data(mut self, publisher: MarketDataPublisher) -> Self {
        self.fanout = self.fanout.with_market_data(publisher);
        self
    }

    /// Switches the multicast publisher's local interface.
    ///
    /// # Errors
    /// Returns an I/O error when no multicast publisher exists or rebinding fails.
    pub fn switch_market_data_interface(
        &mut self,
        interface: std::net::Ipv4Addr,
    ) -> std::io::Result<()> {
        self.fanout.switch_interface(interface)
    }

    #[must_use]
    pub fn public_sent(&self) -> u64 {
        self.fanout.public_sent()
    }

    #[must_use]
    pub fn with_config(mut self, config: ExchangeConfig) -> Self {
        self.idle_timeout = config.idle_timeout;
        self.fanout = self
            .fanout
            .with_repair_buffer(config.repair_buffer_capacity);
        self
    }

    #[must_use]
    pub fn with_metrics_sink(mut self, sink: Arc<Mutex<ExchangeMetrics>>) -> Self {
        self.metrics_sink = Some(sink);
        self
    }

    #[must_use]
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// Runs one non-blocking iteration.
    ///
    /// Returns `(gateway_events, commands_executed, private_events_sent)`.
    pub fn tick(&mut self) -> (usize, usize, usize) {
        let tick_start = Instant::now();
        let gateway_events = self.gateway.poll();
        self.metrics.gateway_events += gateway_events as u64;
        if gateway_events > 0 {
            self.gateway.prune_closed_sessions(4096);
        }
        let (active_sessions, closed_sessions) = self.gateway.session_counts();
        self.metrics.active_sessions = active_sessions as u64;
        self.metrics.closed_sessions = closed_sessions as u64;
        for (connection_id, address) in self.gateway.take_redirects() {
            let _ = self.server.send_redirect(connection_id, &address);
            self.server.close(connection_id);
        }
        for (connection_id, replay) in self.gateway.take_replay_batches() {
            for event in replay {
                let sent = match &event {
                    crate::exchange::private_replay::ReplayEvent::Core(event) => {
                        self.server.send_private(connection_id, event)
                    }
                    crate::exchange::private_replay::ReplayEvent::Combo(event) => {
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
            let (combo_public, combo_private) =
                combo.tick_with_gateway(&mut self.gateway, &self.server);
            self.metrics.public_events_sent += combo_public as u64;
            self.metrics.private_events_sent += combo_private as u64;
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
        let elapsed_ns = u64::try_from(tick_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.metrics.tick_latency.observe(elapsed_ns);
        if let Some(sink) = &self.metrics_sink
            && let Ok(mut snapshot) = sink.lock()
        {
            *snapshot = self.metrics;
        }
        (gateway_events, commands, sent)
    }

    #[must_use]
    pub fn metrics(&self) -> ExchangeMetrics {
        self.metrics
    }

    #[must_use]
    pub fn core(&self) -> &ExchangeCore {
        self.core.core()
    }

    #[must_use]
    pub fn snapshot(&self) -> ExchangeSnapshot {
        ExchangeSnapshot {
            core: self.core.core().snapshot(),
            combo: self.combo.as_ref().map(ComboRuntime::snapshot),
        }
    }

    /// Restores core and combo state.
    ///
    /// # Errors
    /// Returns an error for invalid core snapshot data or combo runtime mismatch.
    pub fn restore_snapshot(&mut self, snapshot: ExchangeSnapshot) -> Result<(), String> {
        self.core.core_mut().restore_snapshot(snapshot.core)?;
        match (self.combo.as_mut(), snapshot.combo) {
            (Some(combo), Some(combo_snapshot)) => combo.restore_snapshot(combo_snapshot),
            (None, None) => Ok(()),
            _ => Err("snapshot combo state does not match runtime configuration".to_string()),
        }
    }

    pub fn gateway_mut(&mut self) -> &mut GatewayRuntime {
        &mut self.gateway
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::ContractSpec;
    use crate::exchange::gateway::GatewayFrame;
    use crate::exchange::gateway_runtime::GatewayRuntime;
    use crate::exchange::gateway_server::start;
    use crate::exchange::middleware::MiddlewareSequencer;
    use crate::exchange::plugins::NoPlugins;
    use crate::exchange::protocol::{
        Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
        VolumeCondition,
    };
    use crate::exchange::transport::create;
    use crate::shared::SymbolId;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    fn command() -> Command {
        Command {
            global_seq: 0,
            gateway_seq: 1,
            ingress_ts_ns: 0,
            exchange_ts_ns: 0,
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
        use crate::exchange::market_data::{MarketDataPublisher, MarketDataSubscriber};
        use crate::exchange::protocol::{Command, CommandType, Hedge, Offset, Side};
        use std::io::Write;

        let path =
            std::env::temp_dir().join(format!("matching-engine-fanout-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 1024).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
        let mut runtime = ExchangeRuntime::new(gateway, runner, server).with_market_data(publisher);

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
        use crate::exchange::combo_engine::ComboEngine;
        use crate::exchange::combo_market_data::{
            ComboMarketDataPublisher, ComboMarketDataSubscriber,
        };
        use crate::exchange::combo_runtime::ComboRuntime;
        use crate::exchange::protocol::{Command, CommandType, Hedge, Offset, Side};
        use crate::shared::protocol::OrderType;
        use std::io::Read;

        let core_path = std::env::temp_dir().join(format!(
            "matching-engine-combo-e2e-core-{}",
            std::process::id()
        ));
        let combo_path = std::env::temp_dir().join(format!(
            "matching-engine-combo-e2e-ring-{}",
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
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
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
        let mut runtime = ExchangeRuntime::new(gateway, runner, server).with_combo_runtime(combo);

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
            let mut frame = [0u8; crate::exchange::gateway::GATEWAY_FRAME_SIZE];
            if stream.read_exact(&mut frame).is_ok() {
                private = Some(GatewayFrame::decode(&frame).unwrap());
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(matches!(
            private,
            Some(GatewayFrame::PrivateEvent(event)) if event.status == crate::exchange::private_events::PrivateStatus::Accepted
        ));
        let fill = subscriber.recv().unwrap();
        assert_eq!(fill.quantity, 5);
        assert_eq!(fill.spread_ticks, 49_900);
        for path in [&core_path, &combo_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn combo_runtime_tick_is_driven_by_exchange_runtime() {
        use crate::exchange::combo_engine::ComboEngine;
        use crate::exchange::combo_market_data::{
            ComboMarketDataPublisher, ComboMarketDataSubscriber,
        };
        use crate::exchange::combo_runtime::ComboRuntime;
        use crate::exchange::protocol::{Hedge, Offset, Side};
        use crate::shared::protocol::OrderType;

        let path = std::env::temp_dir().join(format!(
            "matching-engine-exchange-runtime-combo-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
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
        let mut runtime = ExchangeRuntime::new(gateway, runner, server).with_combo_runtime(combo);
        runtime.tick();
        let fill = subscriber.recv().unwrap();
        assert_eq!(fill.quantity, 5);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn config_builder_sets_repair_capacity_and_timeout() {
        use crate::exchange::config::ExchangeConfig;

        let path = std::env::temp_dir().join(format!(
            "matching-engine-exchange-runtime-config-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let runtime = ExchangeRuntime::new(gateway, runner, server).with_config(ExchangeConfig {
            repair_buffer_capacity: 3,
            idle_timeout: None,
            ..ExchangeConfig::default()
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
        use crate::exchange::repair::NackRequest;

        let path = std::env::temp_dir().join(format!(
            "matching-engine-exchange-runtime-repair-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let mut runtime = ExchangeRuntime::new(gateway, runner, server).with_repair_buffer(16);

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
        use crate::exchange::market_data::{MarketDataPublisher, MarketDataSubscriber};
        use crate::exchange::market_data_tracker::NackClient;
        use crate::exchange::repair::{NackRequest, RepairServer};

        let path = std::env::temp_dir().join(format!(
            "matching-engine-exchange-runtime-repair-server-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
        let repair = RepairServer::bind("127.0.0.1:0", publisher, 16).unwrap();
        let repair_addr = repair.local_addr().unwrap();
        let mut runtime = ExchangeRuntime::new(gateway, runner, server).with_repair_server(repair);

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
        use crate::exchange::market_data::MarketDataSubscriber;

        let path = std::env::temp_dir().join(format!(
            "matching-engine-exchange-runtime-udp-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let subscriber = MarketDataSubscriber::bind("127.0.0.1:0").unwrap();
        let publisher =
            MarketDataPublisher::bind("127.0.0.1:0", subscriber.local_addr().unwrap()).unwrap();
        let mut runtime = ExchangeRuntime::new(gateway, runner, server).with_market_data(publisher);

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
            crate::exchange::public_events::PublicEventType::OrderRested
        );
        assert_eq!(runtime.public_sent(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn runtime_expires_idle_gateway() {
        let path = std::env::temp_dir().join(format!(
            "matching-engine-exchange-runtime-idle-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let mut runtime = ExchangeRuntime::new(gateway, runner, server)
            .with_idle_timeout(Duration::from_millis(30));

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
            "matching-engine-exchange-runtime-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 16).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let gateway = GatewayRuntime::new(middleware, events);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = ExchangeCore::new(spec, SymbolId::intern("BTCUSDT"));
        let runner = CoreRunner::new(core, consumer, NoPlugins);
        let metrics_sink = Arc::new(Mutex::new(ExchangeMetrics::new()));
        let mut runtime = ExchangeRuntime::new(gateway, runner, server)
            .with_metrics_sink(Arc::clone(&metrics_sink));

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
            let mut frame = [0u8; crate::exchange::gateway::GATEWAY_FRAME_SIZE];
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
    fn exchange_snapshot_file_roundtrip() {
        let core = ExchangeCore::new(
            ContractSpec::new("BTCUSDT", 1, 1, 1_000_000),
            SymbolId::intern("BTCUSDT"),
        );
        let snapshot = ExchangeSnapshot {
            core: core.snapshot(),
            combo: None,
        };
        let path = std::env::temp_dir().join(format!(
            "exchange-runtime-snapshot-{}.json",
            std::process::id()
        ));
        snapshot.save(&path).unwrap();
        let loaded = ExchangeSnapshot::load(&path).unwrap();
        assert_eq!(loaded.core, snapshot.core);
        assert!(loaded.combo.is_none());

        let corrupted = std::fs::read(&path).unwrap();
        let mut corrupted = corrupted;
        corrupted[20] ^= 0xff;
        std::fs::write(&path, corrupted).unwrap();
        assert!(ExchangeSnapshot::load(&path).is_err());

        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn full_snapshot_restores_combo_runtime() {
        use crate::exchange::combo_engine::ComboEngine;
        use crate::exchange::combo_runtime::ComboRuntime;

        let core_path = std::env::temp_dir().join(format!(
            "exchange-full-snapshot-core-{}",
            std::process::id()
        ));
        let combo_path = std::env::temp_dir().join(format!(
            "exchange-full-snapshot-combo-{}",
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
        let core = ExchangeCore::new(
            ContractSpec::new("BTCUSDT", 1, 1, 1_000_000),
            SymbolId::intern("BTCUSDT"),
        );
        let runner = CoreRunner::new(core, core_consumer, NoPlugins);
        let engine = ComboEngine::new(
            ContractSpec::new("LEG1", 1, 1, 100_000),
            ContractSpec::new("LEG2", 1, 1, 100_000),
        );
        let mut runtime = ExchangeRuntime::new(gateway, runner, server)
            .with_combo_runtime(ComboRuntime::new(engine));

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
        let snapshot: ExchangeSnapshot = serde_json::from_str(&json).unwrap();
        runtime.restore_snapshot(snapshot).unwrap();
        assert_eq!(runtime.combo_mut().unwrap().engine().queue_len(), 1);
        let _ = std::fs::remove_file(&core_path);
        let _ = std::fs::remove_file(&combo_path);
    }
}
