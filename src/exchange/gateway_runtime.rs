//! Connects gateway TCP events to middleware session and command handling.
//!
//! Login frames open a session keyed by the TCP connection id; command frames
//! are stamped with that session/member and submitted to the ordered command
//! ring. Private downlink is a later slice.

use crate::exchange::auth::GatewayCredentials;
use crate::exchange::combo_engine::ComboPrivateFill;
use crate::exchange::gateway::GatewayFrame;
use crate::exchange::gateway_server::{GatewayEvent, GatewayServerHandle};
use crate::exchange::middleware::MiddlewareSequencer;
use crate::exchange::private_events::PrivateEvent;
use crate::exchange::private_replay::{ComboPrivateReplayBuffer, PrivateReplayBuffer, ReplayEvent};
use crate::exchange::rate_limit::TokenBucket;
use crate::exchange::session::SessionInfo;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Drains gateway events and applies them to middleware.
pub struct GatewayRuntime {
    middleware: MiddlewareSequencer,
    events: Receiver<GatewayEvent>,
    last_seen: HashMap<u64, Instant>,
    private_replay: PrivateReplayBuffer,
    combo_replay: ComboPrivateReplayBuffer,
    next_private_seq: HashMap<u64, u64>,
    pending_replay: HashMap<u64, Vec<PrivateEvent>>,
    pending_combo_replay: HashMap<u64, Vec<ComboPrivateFill>>,
    rate_limit: Option<(f64, f64)>,
    buckets: HashMap<u64, TokenBucket>,
    auth_token: Option<u64>,
    credentials: Option<GatewayCredentials>,
    redirect_target: Option<String>,
    pending_redirects: HashMap<u64, String>,
    errors: u64,
}

impl GatewayRuntime {
    #[must_use]
    pub fn new(middleware: MiddlewareSequencer, events: Receiver<GatewayEvent>) -> Self {
        Self::with_replay_capacity(middleware, events, 1024)
    }

    #[must_use]
    pub fn with_replay_capacity(
        middleware: MiddlewareSequencer,
        events: Receiver<GatewayEvent>,
        replay_capacity: usize,
    ) -> Self {
        Self {
            middleware,
            events,
            last_seen: HashMap::new(),
            private_replay: PrivateReplayBuffer::new(replay_capacity),
            combo_replay: ComboPrivateReplayBuffer::new(replay_capacity),
            next_private_seq: HashMap::new(),
            pending_replay: HashMap::new(),
            pending_combo_replay: HashMap::new(),
            rate_limit: None,
            buckets: HashMap::new(),
            auth_token: None,
            credentials: None,
            redirect_target: None,
            pending_redirects: HashMap::new(),
            errors: 0,
        }
    }

    /// Requires every login to carry the given shared token.
    #[must_use]
    pub fn with_auth_token(mut self, token: u64) -> Self {
        self.auth_token = Some(token);
        self
    }

    /// Requires a per-member token; unknown members are rejected.
    #[must_use]
    pub fn with_credentials(mut self, credentials: GatewayCredentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Sets the address sent to clients displaced by a newer login.
    #[must_use]
    pub fn with_redirect_target(mut self, target: String) -> Self {
        self.redirect_target = Some(target);
        self
    }

    /// Enables a per-connection token bucket for command frames.
    #[must_use]
    pub fn with_rate_limit(mut self, capacity: f64, refill_per_sec: f64) -> Self {
        self.rate_limit = Some((capacity, refill_per_sec));
        self
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
                    self.buckets.remove(&connection_id);
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
                auth_token,
            } => {
                let token_ok = if let Some(credentials) = &self.credentials {
                    credentials.verify_login(member_id, auth_token, seat_id)
                } else {
                    self.auth_token
                        .is_none_or(|expected| expected == auth_token)
                };
                if !token_ok {
                    self.errors += 1;
                    return;
                }
                let info = SessionInfo {
                    session_id: connection_id,
                    gateway_id: 1,
                    member_id,
                    user_id,
                    seat_id,
                    last_private_seq,
                    active: true,
                };
                let replaced = match self.middleware.open_or_takeover(info) {
                    Ok(replaced) => replaced,
                    Err(_) => {
                        self.errors += 1;
                        return;
                    }
                };
                if let Some(old) = replaced
                    && let Some(target) = &self.redirect_target
                {
                    self.pending_redirects.insert(old, target.clone());
                }
                let user_id = u64::from(user_id);
                let replay = self.private_replay.replay(user_id, last_private_seq);
                if !replay.is_empty() {
                    self.pending_replay.insert(connection_id, replay);
                }
                let combo_replay = self.combo_replay.replay(user_id, last_private_seq);
                if !combo_replay.is_empty() {
                    self.pending_combo_replay
                        .insert(connection_id, combo_replay);
                }
            }
            GatewayFrame::Command(mut command) => {
                if let Some((capacity, refill)) = self.rate_limit {
                    let bucket = self
                        .buckets
                        .entry(connection_id)
                        .or_insert_with(|| TokenBucket::new(capacity, refill));
                    if !bucket.try_acquire() {
                        self.errors += 1;
                        return;
                    }
                }
                command.session_id = connection_id;
                if let Some(session) = self.middleware.session(connection_id) {
                    command.member_id = session.member_id;
                }
                if self.middleware.submit_for_session(command).is_err() {
                    self.errors += 1;
                }
            }
            GatewayFrame::Ack { private_seq } => {
                if !self
                    .middleware
                    .acknowledge_private(connection_id, private_seq)
                {
                    self.errors += 1;
                }
            }
            GatewayFrame::Heartbeat { .. }
            | GatewayFrame::PrivateEvent(_)
            | GatewayFrame::ComboPrivate(_)
            | GatewayFrame::Redirect { .. } => {}
        }
    }

    /// Takes queued redirects for displaced connections.
    #[must_use]
    pub fn session_counts(&self) -> (usize, usize) {
        self.middleware.session_counts()
    }

    /// Prunes old closed session records after connection churn.
    pub fn prune_closed_sessions(&mut self, keep_closed: usize) -> usize {
        self.middleware.prune_closed_sessions(keep_closed)
    }

    pub fn take_redirects(&mut self) -> HashMap<u64, String> {
        std::mem::take(&mut self.pending_redirects)
    }

    /// Drains all sessions owned by a gateway before planned failover.
    ///
    /// Returns the drained connection ids; callers should close their TCP
    /// connections. Retained private replay lets clients reconnect with
    /// `last_private_seq`.
    pub fn drain_gateway(&mut self, gateway_id: u32) -> Vec<u64> {
        let drained = self.middleware.drain_gateway(gateway_id);
        for connection_id in &drained {
            self.last_seen.remove(connection_id);
            self.buckets.remove(connection_id);
            if let Some(target) = &self.redirect_target {
                self.pending_redirects
                    .insert(*connection_id, target.clone());
            }
        }
        drained
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
            self.buckets.remove(id);
            self.pending_replay.remove(id);
            self.pending_combo_replay.remove(id);
            self.pending_redirects.remove(id);
            self.middleware.close_session(*id);
        }
        expired
    }

    /// Takes replay events queued for a newly logged-in connection.
    pub fn take_replay(&mut self, connection_id: u64) -> Option<Vec<PrivateEvent>> {
        self.pending_replay.remove(&connection_id)
    }

    /// Drains all queued replay batches.
    pub fn take_all_replays(&mut self) -> HashMap<u64, Vec<PrivateEvent>> {
        std::mem::take(&mut self.pending_replay)
    }

    /// Drains core and combo replay batches in per-user sequence order.
    #[must_use]
    pub fn take_replay_batches(&mut self) -> HashMap<u64, Vec<ReplayEvent>> {
        let core = std::mem::take(&mut self.pending_replay);
        let combo = std::mem::take(&mut self.pending_combo_replay);
        let mut merged: HashMap<u64, Vec<ReplayEvent>> = HashMap::new();
        for (connection_id, events) in core {
            merged
                .entry(connection_id)
                .or_default()
                .extend(events.into_iter().map(ReplayEvent::Core));
        }
        for (connection_id, events) in combo {
            merged
                .entry(connection_id)
                .or_default()
                .extend(events.into_iter().map(ReplayEvent::Combo));
        }
        for events in merged.values_mut() {
            events.sort_by_key(ReplayEvent::seq);
        }
        merged
    }

    fn assign_private_seq(&mut self, user_id: u64) -> u64 {
        let next = self.next_private_seq.entry(user_id).or_insert(1);
        let seq = *next;
        *next += 1;
        seq
    }

    /// Routes combo private fills to the session whose user matches the fill.
    ///
    /// Returns how many fills were handed to a gateway downlink.
    pub fn dispatch_combo_private(
        &mut self,
        server: &GatewayServerHandle,
        fills: impl IntoIterator<Item = ComboPrivateFill>,
    ) -> usize {
        let mut sent = 0;
        for mut fill in fills {
            fill.seq = self.assign_private_seq(fill.user_id);
            self.combo_replay.push(fill.user_id, fill);
            let user_id = u32::try_from(fill.user_id).unwrap_or(u32::MAX);
            if let Some(session) = self.middleware.session_by_user(user_id)
                && server.send_combo_private(session.session_id, &fill).is_ok()
            {
                sent += 1;
            }
        }
        sent
    }

    /// Routes private events to the session whose user matches the account.
    ///
    /// Returns how many events were handed to a gateway downlink.
    pub fn dispatch_private(
        &mut self,
        server: &GatewayServerHandle,
        events: impl IntoIterator<Item = crate::exchange::private_events::PrivateEvent>,
    ) -> usize {
        let mut sent = 0;
        for mut event in events {
            event.seq = self.assign_private_seq(event.account_id);
            let user_id = u32::try_from(event.account_id).unwrap_or(u32::MAX);
            self.private_replay.push(event.account_id, event.clone());
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
    fn combo_private_fill_routes_to_logged_in_gateway() {
        use crate::exchange::combo_engine::ComboPrivateFill;
        use crate::exchange::protocol::Side;
        use std::io::Read;

        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-combo-private-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let mut runtime = GatewayRuntime::new(middleware, events).with_rate_limit(10.0, 1.0);
        let mut stream = TcpStream::connect(server.addr()).unwrap();
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
            runtime.poll();
            if runtime.middleware_mut().session_by_user(2).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let fill = ComboPrivateFill {
            seq: 1,
            order_id: 2,
            user_id: 2,
            side: Side::Buy,
            quantity: 5,
            leg1_price: 50_000,
            leg2_price: 100,
            spread_ticks: 49_900,
        };
        assert_eq!(runtime.dispatch_combo_private(&server, [fill]), 1);
        let mut frame = [0u8; crate::exchange::gateway::GATEWAY_FRAME_SIZE];
        stream.read_exact(&mut frame).unwrap();
        assert_eq!(
            GatewayFrame::decode(&frame).unwrap(),
            GatewayFrame::ComboPrivate(fill)
        );
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn replay_capacity_evicts_oldest() {
        // Capacity is exercised through the replay buffer directly; the gateway
        // only wires the value through.
        let mut buffer = crate::exchange::private_replay::PrivateReplayBuffer::new(1);
        let event = |seq| PrivateEvent {
            seq,
            order_id: seq,
            client_order_id: seq,
            account_id: 2,
            status: crate::exchange::private_events::PrivateStatus::Accepted,
            filled_delta: 0,
            remaining: 1,
            reason: None,
        };
        buffer.push(2, event(1));
        buffer.push(2, event(2));
        let replay = buffer.replay(2, 0);
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].seq, 2);
    }

    #[test]
    fn second_gateway_login_takes_over() {
        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-takeover-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let mut runtime = GatewayRuntime::new(middleware, events);
        let mut first = TcpStream::connect(addr).unwrap();
        first
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                    auth_token: 0,
                }
                .encode(),
            )
            .unwrap();
        for _ in 0..50 {
            runtime.poll();
            if runtime.middleware_mut().session(1).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut second = TcpStream::connect(addr).unwrap();
        second
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                    auth_token: 0,
                }
                .encode(),
            )
            .unwrap();
        for _ in 0..50 {
            runtime.poll();
            if runtime.middleware_mut().session(2).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!runtime.middleware_mut().session(1).unwrap().active);
        assert!(runtime.middleware_mut().session(2).unwrap().active);
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn takeover_queues_redirect_for_old_connection() {
        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-redirect-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let mut runtime = GatewayRuntime::new(middleware, events)
            .with_redirect_target("127.0.0.1:9000".to_string());
        let login = GatewayFrame::Login {
            member_id: 10,
            user_id: 2,
            seat_id: 3,
            session_epoch: 1,
            last_private_seq: 0,
            auth_token: 0,
        };
        let mut first = TcpStream::connect(addr).unwrap();
        first.write_all(&login.clone().encode()).unwrap();
        for _ in 0..50 {
            runtime.poll();
            if runtime.middleware_mut().session(1).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut second = TcpStream::connect(addr).unwrap();
        second.write_all(&login.encode()).unwrap();
        for _ in 0..50 {
            runtime.poll();
            if runtime.middleware_mut().session(2).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let redirects = runtime.take_redirects();
        assert_eq!(
            redirects.get(&1).map(String::as_str),
            Some("127.0.0.1:9000")
        );
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn per_member_credentials_are_enforced() {
        use crate::exchange::auth::GatewayCredentials;

        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-credentials-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let mut credentials = GatewayCredentials::new();
        credentials.insert(1, 11);
        credentials.insert(2, 22);
        let mut runtime = GatewayRuntime::new(middleware, events).with_credentials(credentials);

        let mut wrong = TcpStream::connect(addr).unwrap();
        wrong
            .write_all(
                &GatewayFrame::Login {
                    member_id: 1,
                    user_id: 1,
                    seat_id: 1,
                    session_epoch: 1,
                    last_private_seq: 0,
                    auth_token: 22,
                }
                .encode(),
            )
            .unwrap();
        for _ in 0..50 {
            runtime.poll();
            if runtime.errors() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(runtime.errors() > 0);

        let mut right = TcpStream::connect(addr).unwrap();
        right
            .write_all(
                &GatewayFrame::Login {
                    member_id: 2,
                    user_id: 2,
                    seat_id: 1,
                    session_epoch: 1,
                    last_private_seq: 0,
                    auth_token: 22,
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
        assert!(runtime.middleware_mut().session_by_user(2).is_some());
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn login_requires_matching_auth_token() {
        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-auth-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let mut runtime = GatewayRuntime::new(middleware, events).with_auth_token(42);

        let mut wrong = TcpStream::connect(addr).unwrap();
        wrong
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                    auth_token: 0,
                }
                .encode(),
            )
            .unwrap();
        for _ in 0..50 {
            runtime.poll();
            if runtime.errors() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(runtime.errors() > 0);
        assert!(runtime.middleware_mut().session_by_user(2).is_none());

        let mut right = TcpStream::connect(addr).unwrap();
        right
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 0,
                    auth_token: 42,
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
        assert!(runtime.middleware_mut().session_by_user(2).is_some());
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rate_limit_drops_excess_commands() {
        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-rate-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let mut runtime = GatewayRuntime::new(middleware, events).with_rate_limit(1.0, 0.0);
        let mut stream = TcpStream::connect(server.addr()).unwrap();
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
        let mut first = command();
        first.client_order_id = 1;
        let mut second = command();
        second.client_order_id = 2;
        stream
            .write_all(&GatewayFrame::Command(first).encode())
            .unwrap();
        stream
            .write_all(&GatewayFrame::Command(second).encode())
            .unwrap();
        for _ in 0..50 {
            runtime.poll();
            if consumer.recv().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Drain any already-polled second command, then verify no third arrives.
        while consumer.recv().is_some() {}
        for _ in 0..50 {
            runtime.poll();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(consumer.recv().is_none());
        assert!(runtime.errors() >= 1);
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn client_ack_updates_session_private_seq() {
        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-ack-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let mut runtime = GatewayRuntime::new(middleware, events);
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
            runtime.poll();
            if runtime.middleware_mut().session_by_user(2).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        stream
            .write_all(&GatewayFrame::Ack { private_seq: 7 }.encode())
            .unwrap();
        for _ in 0..50 {
            runtime.poll();
            if runtime
                .middleware_mut()
                .session_by_user(2)
                .is_some_and(|session| session.last_private_seq == 7)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            runtime
                .middleware_mut()
                .session_by_user(2)
                .unwrap()
                .last_private_seq,
            7
        );
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reconnect_replays_private_events_after_ack() {
        use crate::exchange::private_events::{PrivateEvent, PrivateStatus};

        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-replay-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let mut runtime = GatewayRuntime::new(middleware, events);

        let mut first = TcpStream::connect(addr).unwrap();
        first
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
            runtime.poll();
            if runtime.middleware_mut().session_by_user(2).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let event = |seq| PrivateEvent {
            seq,
            order_id: seq,
            client_order_id: seq,
            account_id: 2,
            status: PrivateStatus::Accepted,
            filled_delta: 0,
            remaining: 1,
            reason: None,
        };
        assert_eq!(runtime.dispatch_private(&server, [event(1), event(2)]), 2);
        drop(first);
        for _ in 0..50 {
            runtime.poll();
            if runtime.middleware_mut().session_by_user(2).is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let mut second = TcpStream::connect(addr).unwrap();
        second
            .write_all(
                &GatewayFrame::Login {
                    member_id: 10,
                    auth_token: 0,
                    user_id: 2,
                    seat_id: 3,
                    session_epoch: 1,
                    last_private_seq: 1,
                }
                .encode(),
            )
            .unwrap();
        let mut replay = None;
        for _ in 0..50 {
            runtime.poll();
            if let Some(events) = runtime.take_replay(2) {
                replay = Some(events);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let replay = replay.expect("replay batch");
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].seq, 2);
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn replay_merges_core_and_combo_sequences() {
        use crate::exchange::combo_engine::ComboPrivateFill;
        use crate::exchange::private_events::{PrivateEvent, PrivateStatus};
        use crate::exchange::private_replay::ReplayEvent;
        use crate::exchange::protocol::Side;

        let path = std::env::temp_dir().join(format!(
            "matching-engine-gateway-combo-replay-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (mut server, events) = start("127.0.0.1:0").unwrap();
        let addr = server.addr();
        let mut runtime = GatewayRuntime::new(middleware, events);

        let login = GatewayFrame::Login {
            member_id: 10,
            auth_token: 0,
            user_id: 2,
            seat_id: 3,
            session_epoch: 1,
            last_private_seq: 0,
        };
        let mut first = TcpStream::connect(addr).unwrap();
        first.write_all(&login.encode()).unwrap();
        for _ in 0..50 {
            runtime.poll();
            if runtime.middleware_mut().session_by_user(2).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let core = PrivateEvent {
            seq: 99,
            order_id: 1,
            client_order_id: 1,
            account_id: 2,
            status: PrivateStatus::Accepted,
            filled_delta: 0,
            remaining: 1,
            reason: None,
        };
        let combo = ComboPrivateFill {
            seq: 99,
            order_id: 2,
            user_id: 2,
            side: Side::Buy,
            quantity: 1,
            leg1_price: 50_000,
            leg2_price: 100,
            spread_ticks: 49_900,
        };
        assert_eq!(runtime.dispatch_private(&server, [core]), 1);
        assert_eq!(runtime.dispatch_combo_private(&server, [combo]), 1);

        drop(first);
        for _ in 0..50 {
            runtime.poll();
            if runtime.middleware_mut().session_by_user(2).is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let mut second = TcpStream::connect(addr).unwrap();
        second
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
        let mut replay = None;
        for _ in 0..50 {
            runtime.poll();
            if let Some(events) = runtime.take_replay_batches().remove(&2) {
                replay = Some(events);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let replay = replay.expect("merged replay");
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].seq(), 1);
        assert_eq!(replay[1].seq(), 2);
        assert!(matches!(replay[0], ReplayEvent::Core(_)));
        assert!(matches!(replay[1], ReplayEvent::Combo(_)));
        server.stop();
        let _ = std::fs::remove_file(&path);
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
        let mut runtime = GatewayRuntime::new(middleware, events).with_rate_limit(10.0, 1.0);
        let mut stream = TcpStream::connect(server.addr()).unwrap();
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
            runtime.poll();
            if runtime.middleware_mut().session_by_user(2).is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        stream
            .write_all(
                &GatewayFrame::Command(crate::exchange::protocol::Command::zeroed()).encode(),
            )
            .unwrap();
        let mut bucket_id = None;
        for _ in 0..100 {
            runtime.poll();
            if let Some(id) = runtime.buckets.keys().next().copied() {
                bucket_id = Some(id);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let bucket_id = bucket_id.expect("rate-limit bucket");
        std::thread::sleep(Duration::from_millis(5));
        assert!(
            runtime
                .expire_idle(Duration::from_millis(1))
                .contains(&bucket_id)
        );
        assert!(!runtime.buckets.contains_key(&bucket_id));
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
