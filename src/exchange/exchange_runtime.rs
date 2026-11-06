//! Minimal end-to-end runtime: gateway TCP ↔ middleware ↔ core.
//!
//! One `tick()` drains gateway events into the middleware command ring, runs the
//! core command consumer, then routes core private events back to the matching
//! gateway session.

use crate::exchange::core::ExchangeCore;
use crate::exchange::gateway_runtime::GatewayRuntime;
use crate::exchange::gateway_server::GatewayServerHandle;
use crate::exchange::plugins::PreTradePlugin;
use crate::exchange::runner::CoreRunner;

/// Composite gateway/middleware/core runtime for one process.
pub struct ExchangeRuntime<P> {
    gateway: GatewayRuntime,
    core: CoreRunner<P>,
    server: GatewayServerHandle,
}

impl<P: PreTradePlugin> ExchangeRuntime<P> {
    #[must_use]
    pub fn new(gateway: GatewayRuntime, core: CoreRunner<P>, server: GatewayServerHandle) -> Self {
        Self {
            gateway,
            core,
            server,
        }
    }

    /// Runs one non-blocking iteration.
    ///
    /// Returns `(gateway_events, commands_executed, private_events_sent)`.
    pub fn tick(&mut self) -> (usize, usize, usize) {
        let gateway_events = self.gateway.poll();
        let commands = self.core.poll();
        let private = self.core.drain_private();
        let sent = self.gateway.dispatch_private(&self.server, private);
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
