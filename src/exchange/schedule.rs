//! Trading-session schedule.
//!
//! The schedule is reference data: ordered windows with a phase, start/end
//! nanosecond timestamps, and a trading day. Middleware uses it to turn a
//! sequenced time tick into a Control command; core still applies the command
//! deterministically.

use crate::exchange::public_events::ExchangePhase;

/// One scheduled trading window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionWindow {
    pub phase: ExchangePhase,
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
    pub fn phase_at(&self, ts_ns: u64) -> Option<ExchangePhase> {
        self.windows
            .iter()
            .find(|window| window.start_ns <= ts_ns && ts_ns < window.end_ns)
            .map(|window| window.phase)
    }

    /// Returns the next window transition strictly after `ts_ns`.
    #[must_use]
    pub fn next_transition(&self, ts_ns: u64) -> Option<(u64, ExchangePhase, u32)> {
        for window in &self.windows {
            if window.start_ns > ts_ns {
                return Some((window.start_ns, window.phase, window.trading_day));
            }
            if ts_ns < window.end_ns {
                return Some((window.end_ns, ExchangePhase::Closed, window.trading_day));
            }
        }
        None
    }

    #[must_use]
    pub fn windows(&self) -> &[SessionWindow] {
        &self.windows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule() -> TradingSchedule {
        TradingSchedule::new(vec![
            SessionWindow {
                phase: ExchangePhase::OpeningAuction,
                start_ns: 100,
                end_ns: 200,
                trading_day: 1,
            },
            SessionWindow {
                phase: ExchangePhase::Continuous,
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
        assert_eq!(schedule.phase_at(100), Some(ExchangePhase::OpeningAuction));
        assert_eq!(schedule.phase_at(200), Some(ExchangePhase::Continuous));
        assert_eq!(schedule.phase_at(500), None);
    }

    #[test]
    fn reports_next_transition() {
        let schedule = schedule();
        assert_eq!(
            schedule.next_transition(50),
            Some((100, ExchangePhase::OpeningAuction, 1))
        );
        assert_eq!(
            schedule.next_transition(150),
            Some((200, ExchangePhase::Closed, 1))
        );
        assert_eq!(
            schedule.next_transition(250),
            Some((500, ExchangePhase::Closed, 1))
        );
        assert_eq!(schedule.next_transition(500), None);
    }

    #[test]
    fn rejects_overlapping_windows() {
        assert!(
            TradingSchedule::new(vec![
                SessionWindow {
                    phase: ExchangePhase::Continuous,
                    start_ns: 10,
                    end_ns: 20,
                    trading_day: 1,
                },
                SessionWindow {
                    phase: ExchangePhase::Break,
                    start_ns: 15,
                    end_ns: 30,
                    trading_day: 1,
                },
            ])
            .is_err()
        );
    }
}
