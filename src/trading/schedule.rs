//! Trading-session schedule.
//!
//! The schedule is reference data: ordered windows with a phase, start/end
//! nanosecond timestamps, and a trading day. Middleware uses it to turn a
//! sequenced time tick into a Control command; core still applies the command
//! deterministically.

use crate::trading::public_events::TradingPhase;

/// One scheduled trading window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionWindow {
    pub phase: TradingPhase,
    pub start_ns: u64,
    pub end_ns: u64,
    pub trading_day: u32,
}

/// Ordered schedule of trading windows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TradingSchedule {
    windows: Vec<SessionWindow>,
}

impl TradingSchedule {
    /// Builds a schedule. Windows must be non-empty, ordered, and non-overlapping.
    ///
    /// # Errors
    /// Returns a reason if the schedule is invalid.
    pub fn new(windows: Vec<SessionWindow>) -> Result<Self, &'static str> {
        if windows.is_empty() {
            return Err("schedule must contain at least one window");
        }
        for pair in windows.windows(2) {
            if pair[0].end_ns > pair[1].start_ns {
                return Err("schedule windows overlap or are out of order");
            }
        }
        if windows
            .iter()
            .any(|window| window.start_ns >= window.end_ns)
        {
            return Err("schedule window has invalid interval");
        }
        Ok(Self { windows })
    }

    #[must_use]
    pub fn phase_at(&self, ts_ns: u64) -> Option<TradingPhase> {
        self.windows
            .iter()
            .find(|window| window.start_ns <= ts_ns && ts_ns < window.end_ns)
            .map(|window| window.phase)
    }

    /// Returns the next window transition strictly after `ts_ns`.
    #[must_use]
    pub fn next_transition(&self, ts_ns: u64) -> Option<(u64, TradingPhase, u32)> {
        for window in &self.windows {
            if window.start_ns > ts_ns {
                return Some((window.start_ns, window.phase, window.trading_day));
            }
            if ts_ns < window.end_ns {
                return Some((window.end_ns, TradingPhase::Closed, window.trading_day));
            }
        }
        None
    }

    #[must_use]
    pub fn windows(&self) -> &[SessionWindow] {
        &self.windows
    }
}

/// Converts timed observations into phase-change decisions.
///
/// The middleware owns the wall clock; the driver only reports a change when
/// the observed phase differs from the last reported phase.
#[derive(Debug)]
pub struct ScheduleDriver {
    schedule: TradingSchedule,
    last_phase: Option<TradingPhase>,
}

impl ScheduleDriver {
    #[must_use]
    pub fn new(schedule: TradingSchedule) -> Self {
        Self {
            schedule,
            last_phase: None,
        }
    }

    /// Feeds one timestamp and returns `(phase, trading_day)` on a transition.
    pub fn observe(&mut self, ts_ns: u64) -> Option<(TradingPhase, u32)> {
        let phase = self
            .schedule
            .phase_at(ts_ns)
            .unwrap_or(TradingPhase::Closed);
        if self.last_phase == Some(phase) {
            return None;
        }
        self.last_phase = Some(phase);
        let trading_day = self
            .schedule
            .windows()
            .iter()
            .find(|window| window.start_ns <= ts_ns && ts_ns < window.end_ns)
            .map_or(1, |window| window.trading_day);
        Some((phase, trading_day))
    }

    #[must_use]
    pub fn schedule(&self) -> &TradingSchedule {
        &self.schedule
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule() -> TradingSchedule {
        TradingSchedule::new(vec![
            SessionWindow {
                phase: TradingPhase::OpeningAuction,
                start_ns: 100,
                end_ns: 200,
                trading_day: 1,
            },
            SessionWindow {
                phase: TradingPhase::Continuous,
                start_ns: 200,
                end_ns: 500,
                trading_day: 1,
            },
        ])
        .unwrap()
    }

    #[test]
    fn reports_phase_at_timestamp() {
        let schedule = schedule();
        assert_eq!(schedule.phase_at(50), None);
        assert_eq!(schedule.phase_at(100), Some(TradingPhase::OpeningAuction));
        assert_eq!(schedule.phase_at(200), Some(TradingPhase::Continuous));
        assert_eq!(schedule.phase_at(500), None);
    }

    #[test]
    fn reports_next_transition() {
        let schedule = schedule();
        assert_eq!(
            schedule.next_transition(50),
            Some((100, TradingPhase::OpeningAuction, 1))
        );
        assert_eq!(
            schedule.next_transition(150),
            Some((200, TradingPhase::Closed, 1))
        );
        assert_eq!(
            schedule.next_transition(250),
            Some((500, TradingPhase::Closed, 1))
        );
        assert_eq!(schedule.next_transition(500), None);
    }

    #[test]
    fn driver_reports_only_phase_changes() {
        let mut driver = ScheduleDriver::new(schedule());
        assert_eq!(driver.observe(100), Some((TradingPhase::OpeningAuction, 1)));
        assert_eq!(driver.observe(150), None);
        assert_eq!(driver.observe(200), Some((TradingPhase::Continuous, 1)));
        assert_eq!(driver.observe(500), Some((TradingPhase::Closed, 1)));
    }

    #[test]
    fn rejects_overlapping_windows() {
        assert!(
            TradingSchedule::new(vec![
                SessionWindow {
                    phase: TradingPhase::Continuous,
                    start_ns: 10,
                    end_ns: 20,
                    trading_day: 1,
                },
                SessionWindow {
                    phase: TradingPhase::Break,
                    start_ns: 15,
                    end_ns: 30,
                    trading_day: 1,
                },
            ])
            .is_err()
        );
    }
}
