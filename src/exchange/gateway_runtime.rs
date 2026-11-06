//! Connects gateway TCP events to middleware session and command handling.
//!
//! Login frames open a session keyed by the TCP connection id; command frames
//! are stamped with that session/member and submitted to the ordered command
//! ring. Private downlink is a later slice.

use crate::exchange::gateway::GatewayFrame;
use crate::exchange::gateway_server::{GatewayEvent, GatewayServerHandle};
use crate::exchange::middleware::MiddlewareSequencer;
use crate::exchange::session::SessionInfo;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Drains gateway events and applies them to middleware.
pub struct GatewayRuntime {
    middleware: MiddlewareSequencer,
    events: Receiver<GatewayEvent>,
    last_seen: HashMap<u64, Instant>,
    errors: u64,
}

impl GatewayRuntime {
    #[must_use]
    pub fn new(middleware: MiddlewareSequencer, events: Receiver<GatewayEvent>) -> Self {
        Self {
            middleware,
            events,
            last_seen: HashMap::new(),
            errors: 0,
        }
    }

    /// Drains all pending gateway events. Returns the number of frame/closed
    /// events processed.
    pub fn poll(&mut self) -> usize {
        let mut processed = 0;
        while let Ok(event) = self.events.try_recv() {
            match event {
                GatewayEvent::Frame {
                    connection_id,
                    frame,
                } => {
                    processed += 1;
                    self.last_seen.insert(connection_id, Instant::now());
                    self.handle_frame(connection_id, frame);
                }
                GatewayEvent::Closed { connection_id } => {
                    processed += 1;
                    self.last_seen.remove(&connection_id);
                    self.middleware.close_session(connection_id);
                }
            }
        }
        processed
    }

    fn handle_frame(&mut self, connection_id: u64, frame: GatewayFrame) {
        match frame {
            GatewayFrame::Login {
                member_id,
                user_id,
                seat_id,
                session_epoch: _,
                last_private_seq,
            } => {
                let info = SessionInfo {
                    session_id: connection_id,
                    gateway_id: 1,
                    member_id,
                    user_id,
                    seat_id,
                    last_private_seq,
                    active: true,
                };
                if !self.middleware.open_session(info) {
                    self.errors += 1;
                }
            }
            GatewayFrame::Command(mut command) => {
                command.session_id = connection_id;
                if let Some(session) = self.middleware.session(connection_id) {
                    command.member_id = session.member_id;
                }
                if self.middleware.submit_for_session(command).is_err() {
                    self.errors += 1;
                }
            }
            GatewayFrame::Heartbeat { .. } | GatewayFrame::PrivateEvent(_) => {}
        }
    }

    /// Closes sessions idle for at least `timeout`.
    ///
    /// Returns the connection ids that were expired. The caller should also
    /// close the corresponding TCP connection.
    pub fn expire_idle(&mut self, timeout: Duration) -> Vec<u64> {
        let now = Instant::now();
        let expired: Vec<u64> = self
            .last_seen
            .iter()
            .filter(|(_, seen)| now.duration_since(**seen) >= timeout)
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            self.last_seen.remove(id);
            self.middleware.close_session(*id);
        }
        expired
    }

    /// Routes private events to the session whose user matches the account.
    ///
    /// Returns how many events were handed to a gateway downlink.
    pub fn dispatch_private(
        &self,
        server: &GatewayServerHandle,
        events: impl IntoIterator<Item = crate::exchange::private_events::PrivateEvent>,
    ) -> usize {
        let mut sent = 0;
        for event in events {
            let user_id = u32::try_from(event.account_id).unwrap_or(u32::MAX);
            if let Some(session) = self.middleware.session_by_user(user_id)
                && server.send_private(session.session_id, &event).is_ok()
            {
                sent += 1;
            }
        }
        sent
    }

    #[must_use]
    pub fn errors(&self) -> u64 {
        self.errors
    }

    pub fn middleware_mut(&mut self) -> &mut MiddlewareSequencer {
        &mut self.middleware
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::gateway_server::start;
    use crate::exchange::protocol::{
        Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
        VolumeCondition,
    };
    use crate::exchange::transport::create;
    use std::io::Write;
    use std::net::TcpStream;

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
    fn idle_session_expires() {
        use std::time::Duration;
        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-idle-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let mut runtime = GatewayRuntime::new(middleware, events);
        let mut stream = TcpStream::connect(server.addr()).unwrap();
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
            runtime.poll();
            if runtime.middleware_mut().session_by_user(2).is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(runtime.expire_idle(Duration::from_millis(1)), vec![1]);
        assert!(runtime.middleware_mut().session_by_user(2).is_none());
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn private_event_routes_to_logged_in_gateway() {
        use crate::exchange::private_events::{PrivateEvent, PrivateStatus};
        use std::io::Read;

        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-private-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let mut runtime = GatewayRuntime::new(middleware, events);
        let mut stream = TcpStream::connect(server.addr()).unwrap();
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
            runtime.poll();
            if runtime.middleware_mut().session_by_user(2).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let event = PrivateEvent {
            seq: 1,
            order_id: 2,
            client_order_id: 3,
            account_id: 2,
            status: PrivateStatus::Accepted,
            filled_delta: 0,
            remaining: 5,
            reason: None,
        };
        assert_eq!(runtime.dispatch_private(&server, [event.clone()]), 1);
        let mut frame = [0u8; crate::exchange::gateway::GATEWAY_FRAME_SIZE];
        stream.read_exact(&mut frame).unwrap();
        assert_eq!(
            GatewayFrame::decode(&frame).unwrap(),
            GatewayFrame::PrivateEvent(event)
        );
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn login_and_command_reach_core() {
        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-runtime-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let mut runtime = GatewayRuntime::new(middleware, events);
        let mut stream = TcpStream::connect(server.addr()).unwrap();
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
        // Give the listener thread a moment to read both frames.
        for _ in 0..100 {
            runtime.poll();
            if let Some(Ok(event)) = consumer.recv() {
                assert_eq!(event.global_seq, 1);
                assert_eq!(event.session_id, 1);
                assert_eq!(event.member_id, 10);
                server.stop();
                let _ = std::fs::remove_file(&path);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("command did not reach the command ring");
    }
}
