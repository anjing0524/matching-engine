//! Minimal end-to-end runtime: gateway TCP ↔ middleware ↔ core.
//!
//! One `tick()` drains gateway events into the middleware command ring, runs the
//! core command consumer, then routes core private events back to the matching
//! gateway session.

use crate::exchange::config::ExchangeConfig;
use crate::exchange::core::ExchangeCore;
use crate::exchange::gateway_runtime::GatewayRuntime;
use crate::exchange::gateway_server::GatewayServerHandle;
use crate::exchange::market_data::MarketDataPublisher;
use crate::exchange::plugins::PreTradePlugin;
use crate::exchange::repair::{NackRequest, RepairBuffer, RepairServer};
use crate::exchange::runner::CoreRunner;
use std::time::{Duration, Instant};

/// Composite gateway/middleware/core runtime for one process.
pub struct ExchangeRuntime<P> {
    gateway: GatewayRuntime,
    core: CoreRunner<P>,
    server: GatewayServerHandle,
    idle_timeout: Option<Duration>,
    last_expire: Instant,
    market_data: Option<MarketDataPublisher>,
    repair_buffer: Option<RepairBuffer>,
    repair_server: Option<RepairServer>,
    public_sent: u64,
    repair_sent: u64,
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
            market_data: None,
            repair_buffer: None,
            repair_server: None,
            public_sent: 0,
            repair_sent: 0,
        }
    }

    #[must_use]
    pub fn with_repair_server(mut self, server: RepairServer) -> Self {
        self.repair_server = Some(server);
        self
    }

    #[must_use]
    pub fn repair_sent(&self) -> u64 {
        self.repair_sent
    }

    #[must_use]
    pub fn with_repair_buffer(mut self, capacity: usize) -> Self {
        self.repair_buffer = Some(RepairBuffer::new(capacity));
        self
    }

    #[must_use]
    pub fn repair(&self, request: NackRequest) -> Vec<crate::exchange::public_events::PublicEvent> {
        self.repair_buffer
            .as_ref()
            .map_or_else(Vec::new, |buffer| buffer.repair(request))
    }

    #[must_use]
    pub fn with_market_data(mut self, publisher: MarketDataPublisher) -> Self {
        self.market_data = Some(publisher);
        self
    }

    #[must_use]
    pub fn public_sent(&self) -> u64 {
        self.public_sent
    }

    #[must_use]
    pub fn with_config(mut self, config: ExchangeConfig) -> Self {
        self.idle_timeout = config.idle_timeout;
        self.repair_buffer = Some(RepairBuffer::new(config.repair_buffer_capacity));
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
        let gateway_events = self.gateway.poll();
        for (connection_id, replay) in self.gateway.take_all_replays() {
            for event in replay {
                let _ = self.server.send_private(connection_id, &event);
            }
        }
        let commands = self.core.poll();
        let private = self.core.drain_private();
        let sent = self.gateway.dispatch_private(&self.server, private);
        let public = self.core.drain_public();
        if let Some(buffer) = &mut self.repair_buffer {
            for event in &public {
                buffer.push(*event);
            }
        }
        if let Some(server) = &mut self.repair_server {
            for event in &public {
                server.push(*event);
            }
        }
        if let Some(publisher) = &self.market_data {
            for event in public {
                if publisher.send(&event).is_ok() {
                    self.public_sent += 1;
                }
            }
        }
        if let Some(server) = &self.repair_server {
            self.repair_sent += server.poll_once().unwrap_or(0) as u64;
        }
        if let Some(timeout) = self.idle_timeout
            && self.last_expire.elapsed() >= timeout / 2
        {
            for connection_id in self.gateway.expire_idle(timeout) {
                self.server.close(connection_id);
            }
            self.last_expire = Instant::now();
        }
        (gateway_events, commands, sent)
    }

    #[must_use]
    pub fn core(&self) -> &ExchangeCore {
        self.core.core()
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
        let mut runtime = ExchangeRuntime::new(gateway, runner, server);

        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
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
        let _ = std::fs::remove_file(&path);
    }
}
