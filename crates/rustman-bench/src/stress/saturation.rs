//! The client-saturation guard (stress-testing "Client saturation guard"):
//! when the scheduler lags more than 100 ms behind its schedule for 3
//! consecutive ticks (seconds), the client, not the target, is the
//! bottleneck. Windows holding those ticks are marked `client-saturated`
//! and never count as server breaks.

use std::time::Duration;

/// Lag beyond this is the client falling behind.
pub const LAG_LIMIT: Duration = Duration::from_millis(100);
/// Ticks in a row over the limit that mark saturation.
pub const LAG_STREAK: u32 = 3;

/// Tracks the scheduler's lag tick by tick.
#[derive(Clone, Debug, Default)]
pub struct SaturationGuard {
    /// Ticks in a row over the limit, up to now.
    streak: u32,
    /// Whether the open window holds a saturated tick.
    window_saturated: bool,
    /// Ticks marked saturated in the whole run.
    saturated_ticks: u64,
}

impl SaturationGuard {
    /// Account for one tick whose worst scheduling lag was `lag`.
    pub fn tick(&mut self, lag: Duration) {
        if lag <= LAG_LIMIT {
            self.streak = 0;
            return;
        }
        self.streak += 1;
        if self.streak == LAG_STREAK {
            // The streak marks every lagging tick of it, earlier ones too.
            self.saturated_ticks += u64::from(LAG_STREAK);
            self.window_saturated = true;
        } else if self.streak > LAG_STREAK {
            self.saturated_ticks += 1;
            self.window_saturated = true;
        }
    }

    /// Close the open window: whether it held a saturated tick. A streak
    /// still short of [`LAG_STREAK`] carries into the next window.
    pub fn close_window(&mut self) -> bool {
        std::mem::take(&mut self.window_saturated)
    }

    /// Whether any tick of the run was saturated.
    pub fn saw_saturation(&self) -> bool {
        self.saturated_ticks > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn three_lagging_ticks_in_a_row_saturate_the_window() {
        let mut guard = SaturationGuard::default();
        for lag in [150, 300, 120] {
            guard.tick(ms(lag));
        }
        assert!(guard.close_window());
        assert!(guard.saw_saturation());
    }

    #[test]
    fn short_or_broken_streaks_do_not() {
        let mut guard = SaturationGuard::default();
        for lag in [150, 300, 50, 200, 101, 100] {
            guard.tick(ms(lag));
        }
        assert!(!guard.close_window());
        assert!(!guard.saw_saturation());
    }

    #[test]
    fn a_streak_spanning_windows_marks_the_window_where_it_completes_and_on() {
        let mut guard = SaturationGuard::default();
        guard.tick(ms(10));
        guard.tick(ms(500));
        assert!(!guard.close_window());
        guard.tick(ms(500));
        guard.tick(ms(500));
        assert!(guard.close_window());
        // Still lagging: the next window is saturated too.
        guard.tick(ms(500));
        assert!(guard.close_window());
        guard.tick(ms(5));
        assert!(!guard.close_window());
    }
}
