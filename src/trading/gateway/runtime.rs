//! Connects gateway TCP events to middleware session and command handling.
//!
//! Login frames open a session keyed by the TCP connection id; command frames
//! are stamped with that session/member and submitted to the ordered command
//! ring. Private downlink is a later slice.

use crate::shared::collections::{U64MixMap, U64MixSet};
use crate::trading::combo::engine::ComboPrivateFill;
#[cfg(test)]
use crate::trading::core::private_events::PrivateEvent;
use crate::trading::gateway::GatewayFrame;
use crate::trading::gateway::auth::GatewayCredentials;
use crate::trading::gateway::private_replay::{
    ComboPrivateReplayBuffer, PrivateReplayBuffer, ReplayEvent,
};
use crate::trading::gateway::rate_limit::TokenBucket;
use crate::trading::gateway::server::{GatewayEvent, GatewayServerHandle};
use crate::trading::transport::middleware::MiddlewareSequencer;
use crate::trading::transport::session::SessionInfo;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Per-connection state owned by the gateway runtime.
struct ConnectionState {
    last_seen: Instant,
    bucket: Option<TokenBucket>,
    /// Already seq-ordered core/combo replay queue for this connection.
    replay: Vec<ReplayEvent>,
    redirect: Option<String>,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            last_seen: Instant::now(),
            bucket: None,
            replay: Vec::new(),
            redirect: None,
        }
    }
}

/// One poll's gateway effects, already drained from per-connection state.
pub struct GatewayEffects {
    pub events: usize,
    pub redirects: Vec<(u64, String)>,
    pub replays: Vec<(u64, Vec<ReplayEvent>)>,
    pub active_sessions: usize,
    pub closed_sessions: usize,
}

/// Drains gateway events and applies them to middleware.
pub struct GatewayRuntime {
    middleware: MiddlewareSequencer,
    events: Receiver<GatewayEvent>,
    connections: U64MixMap<ConnectionState>,
    private_replay: PrivateReplayBuffer,
    combo_replay: ComboPrivateReplayBuffer,
    next_private_seq: U64MixMap<u64>,
    replay_pending: U64MixSet,
    redirect_pending: U64MixSet,
    rate_limit: Option<(f64, f64)>,
    auth_token: Option<u64>,
    credentials: Option<GatewayCredentials>,
    redirect_target: Option<String>,
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
            connections: U64MixMap::default(),
            private_replay: PrivateReplayBuffer::new(replay_capacity),
            combo_replay: ComboPrivateReplayBuffer::new(replay_capacity),
            next_private_seq: U64MixMap::default(),
            replay_pending: U64MixSet::default(),
            redirect_pending: U64MixSet::default(),
            rate_limit: None,
            auth_token: None,
            credentials: None,
            redirect_target: None,
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
                    self.connections
                        .entry(connection_id)
                        .or_insert_with(ConnectionState::new)
                        .last_seen = Instant::now();
                    self.handle_frame(connection_id, frame);
                }
                GatewayEvent::Closed { connection_id } => {
                    processed += 1;
                    self.connections.remove(&connection_id);
                    self.replay_pending.remove(&connection_id);
                    self.redirect_pending.remove(&connection_id);
                    self.middleware.close_session(connection_id);
                }
            }
        }
        processed
    }

    /// Polls frames and returns every lifecycle effect in one value.
    pub fn poll_effects(&mut self) -> GatewayEffects {
        let events = self.poll();
        if events > 0 {
            self.prune_closed_sessions(4096);
        }
        let redirects = self.take_redirects();
        let replays = self.take_replay_batches();
        let (active_sessions, closed_sessions) = self.session_counts();
        GatewayEffects {
            events,
            redirects,
            replays,
            active_sessions,
            closed_sessions,
        }
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
                    self.connections
                        .entry(old)
                        .or_insert_with(ConnectionState::new)
                        .redirect = Some(target.clone());
                    self.redirect_pending.insert(old);
                }
                let user_id = u64::from(user_id);
                let state = self
                    .connections
                    .entry(connection_id)
                    .or_insert_with(ConnectionState::new);
                let mut replay: Vec<ReplayEvent> = self
                    .private_replay
                    .replay(user_id, last_private_seq)
                    .into_iter()
                    .map(ReplayEvent::Core)
                    .collect();
                replay.extend(
                    self.combo_replay
                        .replay(user_id, last_private_seq)
                        .into_iter()
                        .map(ReplayEvent::Combo),
                );
                replay.sort_by_key(ReplayEvent::seq);
                if !replay.is_empty() {
                    self.replay_pending.insert(connection_id);
                }
                state.replay = replay;
            }
            GatewayFrame::Command(mut command) => {
                if let Some((capacity, refill)) = self.rate_limit {
                    let state = self
                        .connections
                        .entry(connection_id)
                        .or_insert_with(ConnectionState::new);
                    let bucket = state
                        .bucket
                        .get_or_insert_with(|| TokenBucket::new(capacity, refill));
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

    pub fn take_redirects(&mut self) -> Vec<(u64, String)> {
        let mut redirects = Vec::new();
        for connection_id in self.redirect_pending.iter().copied().collect::<Vec<_>>() {
            let Some(state) = self.connections.get_mut(&connection_id) else {
                continue;
            };
            if let Some(target) = state.redirect.take() {
                redirects.push((connection_id, target));
            }
        }
        self.redirect_pending.clear();
        redirects
    }

    /// Drains all sessions owned by a gateway before planned failover.
    ///
    /// Returns the drained connection ids; callers should close their TCP
    /// connections. Retained private replay lets clients reconnect with
    /// `last_private_seq`.
    pub fn drain_gateway(&mut self, gateway_id: u32) -> Vec<u64> {
        let drained = self.middleware.drain_gateway(gateway_id);
        for connection_id in &drained {
            let redirect = self.redirect_target.clone();
            let mut state = ConnectionState::new();
            state.redirect = redirect;
            self.connections.insert(*connection_id, state);
            self.redirect_pending.insert(*connection_id);
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
            .connections
            .iter()
            .filter(|(_, state)| now.duration_since(state.last_seen) >= timeout)
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            self.connections.remove(id);
            self.middleware.close_session(*id);
        }
        expired
    }

    /// Drains core and combo replay batches in per-user sequence order.
    #[must_use]
    pub fn take_replay_batches(&mut self) -> Vec<(u64, Vec<ReplayEvent>)> {
        let mut batches = Vec::new();
        for connection_id in self.replay_pending.iter().copied().collect::<Vec<_>>() {
            let Some(state) = self.connections.get_mut(&connection_id) else {
                continue;
            };
            if state.replay.is_empty() {
                continue;
            }
            let mut events = std::mem::take(&mut state.replay);
            events.sort_by_key(ReplayEvent::seq);
            batches.push((connection_id, events));
        }
        self.replay_pending.clear();
        batches
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
        events: impl IntoIterator<Item = crate::trading::core::private_events::PrivateEvent>,
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
    use crate::trading::core::protocol::{
        Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
        VolumeCondition,
    };
    use crate::trading::gateway::server::start;
    use crate::trading::transport::create;
    use std::io::Write;
    use std::net::TcpStream;

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
    fn combo_private_fill_routes_to_logged_in_gateway() {
        use crate::trading::combo::engine::ComboPrivateFill;
        use crate::trading::core::protocol::Side;
        use std::io::Read;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-gateway-combo-private-{}",
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
        let mut frame = [0u8; crate::trading::gateway::GATEWAY_FRAME_SIZE];
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
        let mut buffer = crate::trading::gateway::private_replay::PrivateReplayBuffer::new(1);
        let event = |seq| PrivateEvent {
            seq,
            order_id: seq,
            client_order_id: seq,
            account_id: 2,
            status: crate::trading::core::private_events::PrivateStatus::Accepted,
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
            "trading-system-engine-gateway-takeover-{}",
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
            "trading-system-engine-gateway-redirect-{}",
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
            redirects
                .iter()
                .find(|(id, _)| *id == 1)
                .map(|(_, target)| target.as_str()),
            Some("127.0.0.1:9000")
        );
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn per_member_credentials_are_enforced() {
        use crate::trading::gateway::auth::GatewayCredentials;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-gateway-credentials-{}",
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
            "trading-system-engine-gateway-auth-{}",
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
            "trading-system-engine-gateway-rate-{}",
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
            "trading-system-engine-gateway-ack-{}",
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
        use crate::trading::core::private_events::{PrivateEvent, PrivateStatus};

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-gateway-replay-{}",
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
            if let Some(events) = runtime
                .take_replay_batches()
                .into_iter()
                .find(|(id, _)| *id == 2)
                .map(|(_, events)| events)
            {
                replay = Some(events);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let replay = replay.expect("replay batch");
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].seq(), 2);
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn replay_merges_core_and_combo_sequences() {
        use crate::trading::combo::engine::ComboPrivateFill;
        use crate::trading::core::private_events::{PrivateEvent, PrivateStatus};
        use crate::trading::core::protocol::Side;
        use crate::trading::gateway::private_replay::ReplayEvent;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-gateway-combo-replay-{}",
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
            if let Some(events) = runtime
                .take_replay_batches()
                .into_iter()
                .find(|(id, _)| *id == 2)
                .map(|(_, events)| events)
            {
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
            "trading-system-engine-gateway-idle-{}",
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
                &GatewayFrame::Command(crate::trading::core::protocol::Command::zeroed()).encode(),
            )
            .unwrap();
        let mut bucket_id = None;
        for _ in 0..100 {
            runtime.poll();
            if let Some(id) = runtime
                .connections
                .iter()
                .find_map(|(id, state)| state.bucket.as_ref().map(|_| *id))
            {
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
        assert!(!runtime.connections.contains_key(&bucket_id));
        assert!(runtime.middleware_mut().session_by_user(2).is_none());
        server.stop();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn private_event_routes_to_logged_in_gateway() {
        use crate::trading::core::private_events::{PrivateEvent, PrivateStatus};
        use std::io::Read;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-gateway-private-{}",
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
        let mut frame = [0u8; crate::trading::gateway::GATEWAY_FRAME_SIZE];
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
            "trading-system-engine-gateway-runtime-{}",
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
    #[test]
    #[ignore = "64k connection profile; run explicitly"]
    fn idle_expiry_64k_profile() {
        use std::sync::mpsc::channel;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-idle-profile-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (_sender, events) = channel();
        let mut runtime = GatewayRuntime::new(middleware, events);
        for id in 1..=64_000u64 {
            let mut state = ConnectionState::new();
            state.last_seen = Instant::now();
            runtime.connections.insert(id, state);
        }
        let start = Instant::now();
        let expired = runtime.expire_idle(Duration::from_secs(3600));
        println!(
            "idle_expiry_64k: {} connections in {:?}",
            runtime.connections.len(),
            start.elapsed()
        );
        assert!(expired.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
