//! Data feeding (load-testing "Data feeding"): which row of `--data` each
//! iteration of each VU uses. Rows are read with the shared
//! `rustman_core::data_rows` reader, so a data file means the same in
//! `rustman run` and `rustman bench`; its values become variables at the
//! data-row layer, and secret columns are redacted as in a run.
//!
//! - `sequential` (default): one cursor shared by every VU, so each row is
//!   used once per pass.
//! - `per-vu`: VU `n` (from 1) always uses row `n`.
//! - `random`: a uniformly random row per iteration.
//!
//! At the end of the rows, `recycle` (default) starts over and `stop` ends
//! the VU (`sequential`), or leaves the VUs without a row idle (`per-vu`).
//! `random` never runs out.

use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use rustman_core::run::DataRun;

/// `--data-mode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DataMode {
    #[default]
    Sequential,
    PerVu,
    Random,
}

/// `--data-exhaust`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Exhaust {
    Stop,
    #[default]
    Recycle,
}

impl FromStr for DataMode {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "sequential" => Ok(Self::Sequential),
            "per-vu" => Ok(Self::PerVu),
            "random" => Ok(Self::Random),
            _ => Err(format!(
                "unknown data mode '{text}' (use sequential, per-vu or random)"
            )),
        }
    }
}

impl FromStr for Exhaust {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "stop" => Ok(Self::Stop),
            "recycle" => Ok(Self::Recycle),
            _ => Err(format!(
                "unknown data exhaust '{text}' (use stop or recycle)"
            )),
        }
    }
}

impl fmt::Display for DataMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sequential => "sequential",
            Self::PerVu => "per-vu",
            Self::Random => "random",
        })
    }
}

impl fmt::Display for Exhaust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stop => "stop",
            Self::Recycle => "recycle",
        })
    }
}

/// The data of a bench: the rows (with their secret columns) and how VUs
/// pick them.
#[derive(Clone, Debug)]
pub struct DataFeed {
    pub run: DataRun,
    pub mode: DataMode,
    pub exhaust: Exhaust,
}

/// Hands out rows to VUs.
#[derive(Debug)]
pub struct Feeder {
    rows: u64,
    mode: DataMode,
    exhaust: Exhaust,
    cursor: AtomicU64,
}

impl Feeder {
    pub fn new(rows: usize, mode: DataMode, exhaust: Exhaust) -> Self {
        Self {
            rows: u64::try_from(rows).unwrap_or(u64::MAX).max(1),
            mode,
            exhaust,
            cursor: AtomicU64::new(0),
        }
    }

    pub fn for_feed(feed: &DataFeed) -> Self {
        Self::new(feed.run.rows.len(), feed.mode, feed.exhaust)
    }

    /// The 1-based row of the next iteration of VU `vu` (0-based), or
    /// `None` when its rows ran out under `stop`.
    pub fn next(&self, vu: usize) -> Option<u32> {
        let index = match self.mode {
            DataMode::Sequential => self.cursor.fetch_add(1, Ordering::SeqCst),
            DataMode::PerVu => u64::try_from(vu).unwrap_or(u64::MAX),
            DataMode::Random => rand::random_range(0..self.rows),
        };
        let index = match self.exhaust {
            Exhaust::Stop if index >= self.rows => return None,
            _ => index % self.rows,
        };
        u32::try_from(index + 1).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(feeder: &Feeder, vu: usize, n: usize) -> Vec<Option<u32>> {
        (0..n).map(|_| feeder.next(vu)).collect()
    }

    #[test]
    fn sequential_shares_one_cursor_and_recycles() {
        let feeder = Feeder::new(3, DataMode::Sequential, Exhaust::Recycle);
        let rows = [
            feeder.next(0),
            feeder.next(1),
            feeder.next(0),
            feeder.next(1),
        ];
        assert_eq!(rows, [Some(1), Some(2), Some(3), Some(1)]);
    }

    #[test]
    fn sequential_stops_at_the_end() {
        let feeder = Feeder::new(2, DataMode::Sequential, Exhaust::Stop);
        assert_eq!(take(&feeder, 0, 3), [Some(1), Some(2), None]);
        assert_eq!(feeder.next(1), None);
    }

    #[test]
    fn per_vu_gives_each_vu_its_own_row() {
        let feeder = Feeder::new(3, DataMode::PerVu, Exhaust::Recycle);
        for vu in 0..3 {
            let row = Some(u32::try_from(vu).unwrap() + 1);
            assert_eq!(take(&feeder, vu, 3), [row; 3]);
        }
        assert_eq!(feeder.next(3), Some(1));
        let stop = Feeder::new(3, DataMode::PerVu, Exhaust::Stop);
        assert_eq!(stop.next(2), Some(3));
        assert_eq!(stop.next(3), None);
    }

    #[test]
    fn random_stays_in_range_and_never_runs_out() {
        for exhaust in [Exhaust::Stop, Exhaust::Recycle] {
            let feeder = Feeder::new(4, DataMode::Random, exhaust);
            let rows = take(&feeder, 0, 400);
            assert!(rows.iter().all(|r| matches!(r, Some(1..=4))), "{rows:?}");
            for row in 1..=4 {
                assert!(rows.contains(&Some(row)), "row {row} never picked");
            }
        }
    }

    #[test]
    fn modes_parse() {
        for mode in [DataMode::Sequential, DataMode::PerVu, DataMode::Random] {
            assert_eq!(mode.to_string().parse::<DataMode>().unwrap(), mode);
        }
        for exhaust in [Exhaust::Stop, Exhaust::Recycle] {
            assert_eq!(exhaust.to_string().parse::<Exhaust>().unwrap(), exhaust);
        }
        assert!("shuffle".parse::<DataMode>().is_err());
        assert!("wrap".parse::<Exhaust>().is_err());
    }
}
