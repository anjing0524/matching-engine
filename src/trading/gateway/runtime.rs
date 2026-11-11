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
use crate::trading::gateway::private_downlink::PrivateDownlink;
use crate::trading::gateway::private_replay::ReplayEvent;
use crate::trading::gateway::rate_limit::TokenBucket;
use crate::trading::gateway::server::{GatewayEvent, GatewayServerHandle};
use crate::trading::gateway::session_policy::SessionPolicy;
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

/// Counts returned after one gateway effect application.
pub struct AppliedGatewayEffects {
    pub events: usize,
    pub redirects: usize,
    pub replays: usize,
    pub expired: usize,
    pub pruned: usize,
    pub active_sessions: usize,
    pub closed_sessions: usize,
}

/// Drains gateway events and applies them to middleware.
pub struct GatewayRuntime {
    middleware: MiddlewareSequencer,
    events: Receiver<GatewayEvent>,
    connections: U64MixMap<ConnectionState>,
    downlink: PrivateDownlink,
    replay_pending: U64MixSet,
    redirect_pending: U64MixSet,
    policy: SessionPolicy,
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
            downlink: PrivateDownlink::new(replay_capacity),
            replay_pending: U64MixSet::default(),
            redirect_pending: U64MixSet::default(),
            policy: SessionPolicy::default(),
            errors: 0,
        }
    }

    /// Requires every login to carry the given shared token.
    #[must_use]
    pub fn with_auth_token(mut self, token: u64) -> Self {
        self.policy = self.policy.with_auth_token(token);
        self
    }

    /// Requires a per-member token; unknown members are rejected.
    #[must_use]
    pub fn with_credentials(mut self, credentials: GatewayCredentials) -> Self {
        self.policy = self.policy.with_credentials(credentials);
        self
    }

    /// Sets the address sent to clients displaced by a newer login.
    #[must_use]
    pub fn with_redirect_target(mut self, target: String) -> Self {
        self.policy = self.policy.with_redirect_target(target);
        self
    }

    /// Enables a per-connection token bucket for command frames.
    #[must_use]
    pub fn with_rate_limit(mut self, capacity: f64, refill_per_sec: f64) -> Self {
        self.policy = self.policy.with_rate_limit(capacity, refill_per_sec);
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
                    self.middleware.sessions_mut().close(connection_id);
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

    /// Applies lifecycle effects to the server in one fixed order.
    ///
    /// Redirects are sent before replay, ended connections are closed, idle
    /// sessions are expired, and closed session records are pruned. Both
    /// drivers use this operation instead of composing the drain accessors.
    pub fn apply_effects(
        &mut self,
        server: &GatewayServerHandle,
        idle_timeout: Option<Duration>,
    ) -> AppliedGatewayEffects {
        let effects = self.poll_effects();
        let redirects = effects.redirects.len();
        for (connection_id, address) in effects.redirects {
            let _ = server.send(
                connection_id,
                GatewayFrame::Redirect {
                    address: address.clone(),
                },
            );
            server.close(connection_id);
        }
        let mut replays = 0;
        for (connection_id, events) in effects.replays {
            for event in events {
                let frame = match event {
                    ReplayEvent::Core(event) => GatewayFrame::PrivateEvent(event),
                    ReplayEvent::Combo(fill) => GatewayFrame::ComboPrivate(fill),
                };
                if server.send(connection_id, frame).is_ok() {
                    replays += 1;
                }
            }
        }
        let mut expired = 0;
        if let Some(timeout) = idle_timeout {
            for connection_id in self.expire_idle(timeout) {
                server.close(connection_id);
                expired += 1;
            }
        }
        let pruned = self.prune_closed_sessions(4096);
        AppliedGatewayEffects {
            events: effects.events,
            redirects,
            replays,
            expired,
            pruned,
            active_sessions: effects.active_sessions,
            closed_sessions: effects.closed_sessions,
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
                if !self.policy.login_allowed(member_id, seat_id, auth_token) {
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
                let replaced = match self.middleware.sessions_mut().open_or_takeover(info) {
                    Ok(replaced) => replaced,
                    Err(_) => {
                        self.errors += 1;
                        return;
                    }
                };
                if let Some(old) = replaced
                    && let Some(target) = self.policy.redirect_target()
                {
                    self.connections
                        .entry(old)
                        .or_insert_with(ConnectionState::new)
                        .redirect = Some(target.to_string());
                    self.redirect_pending.insert(old);
                }
                let user_id = u64::from(user_id);
                let state = self
                    .connections
                    .entry(connection_id)
                    .or_insert_with(ConnectionState::new);
                let replay = self.downlink.replay_for(user_id, last_private_seq);
                if !replay.is_empty() {
                    self.replay_pending.insert(connection_id);
                }
                state.replay = replay;
            }
            GatewayFrame::Command(command) => {
                if let Some((capacity, refill)) = self.policy.rate_limit() {
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
                if self.middleware.admit(connection_id, command).is_err() {
                    self.errors += 1;
                }
            }
            GatewayFrame::Ack { private_seq } => {
                if !self
                    .middleware
                    .sessions_mut()
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
        (
            self.middleware.sessions().active_count(),
            self.middleware.sessions().closed_count(),
        )
    }

    /// Prunes old closed session records after connection churn.
    pub fn prune_closed_sessions(&mut self, keep_closed: usize) -> usize {
        self.middleware.sessions_mut().prune_closed(keep_closed)
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
            self.middleware.sessions_mut().close(*id);
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

    /// Routes core private events through the private downlink.
    pub fn dispatch_private(
        &mut self,
        server: &GatewayServerHandle,
        events: impl IntoIterator<Item = crate::trading::core::private_events::PrivateEvent>,
    ) -> usize {
        self.downlink
            .dispatch_private(self.middleware.sessions(), server, events)
    }

    /// Routes combo private fills through the private downlink.
    pub fn dispatch_combo_private(
        &mut self,
        server: &GatewayServerHandle,
        fills: impl IntoIterator<Item = ComboPrivateFill>,
    ) -> usize {
        self.downlink
            .dispatch_combo_private(self.middleware.sessions(), server, fills)
    }

    #[must_use]
    pub fn errors(&self) -> u64 {
        self.errors
    }

    #[must_use]
    pub fn private_unrouted(&self) -> u64 {
        self.downlink.unrouted()
    }

    #[must_use]
    pub fn private_send_errors(&self) -> u64 {
        self.downlink.send_errors()
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
    use std::io::{Read, Write};
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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
            {
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
            if runtime.middleware_mut().sessions().get(1).is_some() {
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
            if runtime.middleware_mut().sessions().get(2).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!runtime.middleware_mut().sessions().get(1).unwrap().active);
        assert!(runtime.middleware_mut().sessions().get(2).unwrap().active);
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
            if runtime.middleware_mut().sessions().get(1).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut second = TcpStream::connect(addr).unwrap();
        second.write_all(&login.encode()).unwrap();
        for _ in 0..50 {
            runtime.poll();
            if runtime.middleware_mut().sessions().get(2).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let effects = runtime.apply_effects(&server, None);
        assert_eq!(effects.redirects, 1);
        let mut frame = [0u8; crate::trading::gateway::GATEWAY_FRAME_SIZE];
        first.read_exact(&mut frame).unwrap();
        assert_eq!(
            GatewayFrame::decode(&frame).unwrap(),
            GatewayFrame::Redirect {
                address: "127.0.0.1:9000".to_string()
            }
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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
        );
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
        assert!(
            runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_none()
        );

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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
        );
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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
            {
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
                .sessions_mut()
                .find_active_by_user(2)
                .is_some_and(|session| session.last_private_seq == 7)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
            {
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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_none()
            {
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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
            {
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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_none()
            {
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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
            {
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
        assert!(
            runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_none()
        );
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
            if runtime
                .middleware_mut()
                .sessions_mut()
                .find_active_by_user(2)
                .is_some()
            {
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
    #[test]
    #[ignore = "64k gateway profile; run explicitly"]
    fn gateway_64k_profile() {
        use crate::trading::core::private_events::{PrivateEvent, PrivateStatus};
        use crate::trading::transport::session::SessionInfo;
        use std::hint::black_box;
        use std::sync::mpsc::channel;

        let path = std::env::temp_dir().join(format!(
            "trading-system-engine-gateway-profile-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let middleware = MiddlewareSequencer::new(producer);
        let (_sender, gateway_events) = channel::<GatewayEvent>();
        let (mut server, _server_events) = start("127.0.0.1:0").unwrap();
        let mut runtime = GatewayRuntime::new(middleware, gateway_events);
        for id in 1..=64_000u64 {
            runtime.middleware_mut().sessions_mut().open(SessionInfo {
                session_id: id,
                gateway_id: 1,
                member_id: 1,
                user_id: u32::try_from(id).unwrap(),
                seat_id: 1,
                last_private_seq: 0,
                active: true,
            });
            runtime.connections.insert(id, ConnectionState::new());
        }
        runtime.middleware_mut().sessions_mut().prune_closed(0);

        let start = Instant::now();
        for id in 1..=64_000u64 {
            black_box(
                runtime
                    .middleware_mut()
                    .sessions_mut()
                    .find_active_by_user(u32::try_from(id).unwrap()),
            );
        }
        let lookup = start.elapsed();

        let start = Instant::now();
        for _ in 0..1_000 {
            black_box(runtime.poll_effects());
        }
        let poll = start.elapsed();

        let start = Instant::now();
        runtime.dispatch_private(
            &server,
            (1..=64_000u64).map(|id| PrivateEvent {
                seq: 0,
                order_id: id,
                client_order_id: id,
                account_id: id,
                status: PrivateStatus::Accepted,
                filled_delta: 0,
                remaining: 1,
                reason: None,
            }),
        );
        let dispatch = start.elapsed();

        let event = PrivateEvent {
            seq: 1,
            order_id: 1,
            client_order_id: 1,
            account_id: 1,
            status: PrivateStatus::Accepted,
            filled_delta: 0,
            remaining: 1,
            reason: None,
        };
        for id in 1..=64_000u64 {
            if let Some(state) = runtime.connections.get_mut(&id) {
                state.replay = vec![ReplayEvent::Core(event.clone())];
            }
            runtime.replay_pending.insert(id);
        }
        let start = Instant::now();
        let batches = runtime.take_replay_batches();
        let replay = start.elapsed();
        assert_eq!(batches.len(), 64_000);

        println!(
            "gateway_64k: lookup={lookup:?} poll_1000={poll:?} dispatch={dispatch:?} replay={replay:?}"
        );
        server.stop();
        let _ = std::fs::remove_file(&path);
    }
}
