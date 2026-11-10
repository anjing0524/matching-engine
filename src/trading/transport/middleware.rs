//! Minimal middleware sequencer producer.
//!
//! The full design puts global ordering, logging, session routing and multicast
//! in middleware. This slice implements only the ordered command producer:
//! assign `global_seq`, append the fixed 256-byte command to the shared ring,
//! and let the core runner consume it.

use crate::trading::transport::command_log::{CommandLogWriter, RotatingCommandLog};
use crate::trading::core::protocol::{Command, CommandType};
use crate::trading::core::schedule::{ScheduleDriver, TradingSchedule};
use crate::trading::gateway::session::{SessionInfo, SessionRegistry};
use crate::trading::transport::{CommandProducer, CommandTransportError};

/// Single-writer middleware command sequencer.
pub struct MiddlewareSequencer {
    producer: CommandProducer,
    combo_producer: Option<CommandProducer>,
    next_global_seq: u64,
    command_log: Option<CommandLogWriter>,
    rotating_log: Option<RotatingCommandLog>,
    schedule: Option<ScheduleDriver>,
    sessions: SessionRegistry,
}

impl MiddlewareSequencer {
    #[must_use]
    pub fn new(producer: CommandProducer) -> Self {
        Self {
            producer,
            combo_producer: None,
            next_global_seq: 1,
            command_log: None,
            rotating_log: None,
            schedule: None,
            sessions: SessionRegistry::new(),
        }
    }

    pub fn open_session(&mut self, info: SessionInfo) -> bool {
        self.sessions.open(info)
    }

    pub fn close_session(&mut self, session_id: u64) -> bool {
        self.sessions.close(session_id)
    }

    pub fn open_or_takeover(
        &mut self,
        info: SessionInfo,
    ) -> Result<Option<u64>, crate::trading::gateway::session::SessionOpenError> {
        self.sessions.open_or_takeover(info)
    }

    pub fn drain_gateway(&mut self, gateway_id: u32) -> Vec<u64> {
        self.sessions.drain_gateway(gateway_id)
    }

    pub fn prune_closed_sessions(&mut self, keep_closed: usize) -> usize {
        self.sessions.prune_closed(keep_closed)
    }

    #[must_use]
    pub fn session_counts(&self) -> (usize, usize) {
        (self.sessions.active_count(), self.sessions.closed_count())
    }

    #[must_use]
    pub fn session(&self, session_id: u64) -> Option<SessionInfo> {
        self.sessions.get(session_id)
    }

    #[must_use]
    pub fn session_by_user(&self, user_id: u32) -> Option<SessionInfo> {
        self.sessions.find_active_by_user(user_id)
    }

    pub fn acknowledge_private(&mut self, session_id: u64, private_seq: u64) -> bool {
        self.sessions.acknowledge_private(session_id, private_seq)
    }

    /// Submits only when the command's session is open and active.
    pub fn submit_for_session(&mut self, command: Command) -> Result<u64, CommandTransportError> {
        let session = self
            .sessions
            .get(command.session_id)
            .filter(|session| session.active)
            .ok_or(CommandTransportError::Unauthorized)?;
        if session.member_id != command.member_id {
            return Err(CommandTransportError::Unauthorized);
        }
        self.submit(command)
    }

    /// Appends every accepted command to a replay log segment.
    ///
    /// # Errors
    /// Returns an I/O error if the log cannot be created.
    pub fn with_command_log(mut self, path: &std::path::Path) -> std::io::Result<Self> {
        self.command_log = Some(CommandLogWriter::create(path)?);
        Ok(self)
    }

    /// Appends accepted commands to rotating segments.
    ///
    /// # Errors
    /// Returns an I/O error if the directory cannot be created.
    pub fn with_rotating_command_log(
        mut self,
        directory: &std::path::Path,
        prefix: &str,
        max_segment_bytes: u64,
        retain_segments: usize,
    ) -> std::io::Result<Self> {
        self.rotating_log = Some(RotatingCommandLog::open(
            directory,
            prefix,
            max_segment_bytes,
            retain_segments,
        )?);
        Ok(self)
    }

    /// Routes commands with `combo_id != 0` to a separate combo command ring.
    #[must_use]
    pub fn with_combo_transport(mut self, producer: CommandProducer) -> Self {
        self.combo_producer = Some(producer);
        self
    }

    /// Attaches the trading schedule used to emit SessionControl commands.
    #[must_use]
    pub fn with_schedule(mut self, schedule: TradingSchedule) -> Self {
        self.schedule = Some(ScheduleDriver::new(schedule));
        self
    }

    /// Assigns the next global sequence and appends the command.
    ///
    /// # Errors
    /// Returns `Full` when the command ring is full; the sequence is not
    /// consumed in that case.
    pub fn submit(&mut self, mut command: Command) -> Result<u64, CommandTransportError> {
        let global_seq = self.next_global_seq;
        command.global_seq = global_seq;
        let producer = if command.combo_id != 0 {
            self.combo_producer.as_ref().unwrap_or(&self.producer)
        } else {
            &self.producer
        };
        producer.send(&command)?;
        if let Some(log) = &mut self.command_log {
            log.append(&command).map_err(CommandTransportError::Io)?;
            let _ = log.sync();
        }
        if let Some(log) = &mut self.rotating_log {
            log.append(&command).map_err(CommandTransportError::Io)?;
        }
        self.next_global_seq += 1;
        Ok(global_seq)
    }

    /// Emits a TimeTick and, when the schedule changes phase, a SessionControl.
    ///
    /// Returns the global sequences of every command written.
    pub fn submit_time_tick(
        &mut self,
        trading_ts_ns: u64,
    ) -> Result<Vec<u64>, CommandTransportError> {
        let mut tick = Command::zeroed();
        tick.command_type = CommandType::TimeTick;
        tick.trading_ts_ns = trading_ts_ns;
        tick.ingress_ts_ns = trading_ts_ns;
        let mut sequences = vec![self.submit(tick)?];

        let transition = self
            .schedule
            .as_mut()
            .and_then(|driver| driver.observe(trading_ts_ns));
        if let Some((phase, trading_day)) = transition {
            let mut control = Command::zeroed();
            control.command_type = CommandType::Control;
            control.trading_ts_ns = trading_ts_ns;
            control.flags = phase.code();
            control.quantity = u64::from(trading_day);
            sequences.push(self.submit(control)?);
        }
        Ok(sequences)
    }

    #[must_use]
    pub fn next_global_seq(&self) -> u64 {
        self.next_global_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::orderbook::ContractSpec;
    use crate::shared::SymbolId;
    use crate::trading::core::TradingCore;
    use crate::trading::core::plugins::NoPlugins;
    use crate::trading::core::protocol::{
        Command, CommandType, Hedge, Offset, PriceType, Side, TimeCondition, TriggerType,
        VolumeCondition,
    };
    use crate::trading::transport::runner::CoreRunner;
    use crate::trading::transport::create;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "trading-system-engine-middleware-{name}-{}",
            std::process::id()
        ))
    }

    fn command(seq: u64, side: Side, price: i64, quantity: u64) -> Command {
        Command {
            global_seq: 0,
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
    fn session_gate_checks_open_and_member() {
        use crate::trading::gateway::session::SessionInfo;

        let path = temp_path("session-gate");
        let _ = std::fs::remove_file(&path);
        let (producer, _consumer) = create(&path, 8).unwrap();
        let mut middleware = MiddlewareSequencer::new(producer);
        assert!(middleware.open_session(SessionInfo {
            session_id: 1,
            gateway_id: 1,
            member_id: 10,
            user_id: 20,
            seat_id: 30,
            last_private_seq: 0,
            active: true,
        }));
        let mut order = command(1, Side::Buy, 50_000, 1);
        order.session_id = 1;
        order.member_id = 10;
        assert_eq!(middleware.submit_for_session(order).unwrap(), 1);

        let mut wrong_member = command(2, Side::Buy, 50_000, 1);
        wrong_member.session_id = 1;
        wrong_member.member_id = 11;
        assert!(matches!(
            middleware.submit_for_session(wrong_member),
            Err(CommandTransportError::Unauthorized)
        ));

        middleware.close_session(1);
        let mut after_close = command(3, Side::Buy, 50_000, 1);
        after_close.session_id = 1;
        after_close.member_id = 10;
        assert!(matches!(
            middleware.submit_for_session(after_close),
            Err(CommandTransportError::Unauthorized)
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn time_tick_emits_control_on_phase_change() {
        use crate::trading::core::public_events::TradingPhase;
        use crate::trading::core::schedule::{SessionWindow, TradingSchedule};

        let path = temp_path("schedule");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let schedule = TradingSchedule::new(vec![SessionWindow {
            phase: TradingPhase::Continuous,
            start_ns: 100,
            end_ns: 200,
            trading_day: 7,
        }])
        .unwrap();
        let mut middleware = MiddlewareSequencer::new(producer).with_schedule(schedule);

        assert_eq!(middleware.submit_time_tick(100).unwrap(), vec![1, 2]);
        assert_eq!(middleware.submit_time_tick(150).unwrap(), vec![3]);
        assert_eq!(middleware.next_global_seq(), 4);

        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let mut runner = CoreRunner::new(core, consumer, NoPlugins);
        assert_eq!(runner.poll(), 3);
        assert_eq!(runner.core().phase(), TradingPhase::Continuous);
        assert_eq!(runner.core().trading_day(), 7);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn accepted_commands_are_logged() {
        use crate::trading::transport::command_log::read_segment;

        let ring_path = temp_path("log-ring");
        let log_path = temp_path("command-log");
        let _ = std::fs::remove_file(&ring_path);
        let _ = std::fs::remove_file(&log_path);
        let (producer, _consumer) = create(&ring_path, 8).unwrap();
        let mut middleware = MiddlewareSequencer::new(producer)
            .with_command_log(&log_path)
            .unwrap();
        middleware.submit(command(1, Side::Buy, 50_000, 1)).unwrap();
        middleware
            .submit(command(2, Side::Sell, 50_000, 1))
            .unwrap();
        let logged = read_segment(&log_path).unwrap();
        assert_eq!(logged.len(), 2);
        assert_eq!(logged[0].global_seq, 1);
        assert_eq!(logged[1].global_seq, 2);
        let _ = std::fs::remove_file(&ring_path);
        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn combo_commands_use_separate_ring() {
        let core_path = temp_path("route-core");
        let combo_path = temp_path("route-combo");
        for path in [&core_path, &combo_path] {
            let _ = std::fs::remove_file(path);
        }
        let (core_producer, core_consumer) = create(&core_path, 8).unwrap();
        let (combo_producer, combo_consumer) = create(&combo_path, 8).unwrap();
        let mut middleware =
            MiddlewareSequencer::new(core_producer).with_combo_transport(combo_producer);

        let mut normal = command(1, Side::Buy, 50_000, 1);
        normal.session_id = 1;
        middleware.submit(normal).unwrap();
        let mut combo = command(2, Side::Buy, 49_900, 5);
        combo.combo_id = 7;
        middleware.submit(combo).unwrap();

        let core = core_consumer.recv().unwrap().unwrap();
        assert_eq!(core.combo_id, 0);
        let combo = combo_consumer.recv().unwrap().unwrap();
        assert_eq!(combo.combo_id, 7);
        assert!(core_consumer.recv().is_none());
        assert!(combo_consumer.recv().is_none());
        let _ = std::fs::remove_file(&core_path);
        let _ = std::fs::remove_file(&combo_path);
    }

    #[test]
    fn middleware_assigns_global_sequence_and_core_trades() {
        let path = temp_path("sequence");
        let _ = std::fs::remove_file(&path);
        let (producer, consumer) = create(&path, 8).unwrap();
        let mut middleware = MiddlewareSequencer::new(producer);
        let spec = ContractSpec::new("BTCUSDT", 1, 40_000, 70_000);
        let core = TradingCore::new(spec, SymbolId::intern("BTCUSDT"));
        let mut runner = CoreRunner::new(core, consumer, NoPlugins);

        assert_eq!(
            middleware.submit(command(1, Side::Buy, 50_000, 5)).unwrap(),
            1
        );
        assert_eq!(
            middleware
                .submit(command(2, Side::Sell, 50_000, 5))
                .unwrap(),
            2
        );
        assert_eq!(runner.poll(), 2);
        assert!(!runner.core().positions().snapshot().is_empty());
        assert_eq!(middleware.next_global_seq(), 3);
        let _ = std::fs::remove_file(&path);
    }
}
