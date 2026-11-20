//! The execution time meter (wall time minus host calls; not OS CPU
//! time) read by the epoch deadline callback, and the memory
//! limiter (design D4).

use std::time::{Duration, Instant};

use wasmtime::{ResourceLimiter, UpdateDeadline};

use super::engine::TICK;

/// Which limit stopped a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Exceeded {
    #[error("execution time limit exceeded")]
    Cpu,
    #[error("memory limit exceeded")]
    Memory,
    /// The call ran longer than [`WALL_LIMIT`] past its CPU budget, host
    /// calls included.
    #[error("time limit exceeded")]
    Time,
}

/// How much longer than its CPU budget a call may take in wall time, host
/// calls (network, files) included. A host call is not interrupted; the
/// call traps when the plugin runs again.
pub const WALL_LIMIT: Duration = Duration::from_secs(60);

/// The time one call has spent running WebAssembly. Time spent in host
/// calls (network, files) is not counted against the budget, but the whole
/// call is bounded by the budget plus [`WALL_LIMIT`] of wall time.
#[derive(Clone, Copy, Debug)]
pub struct CpuMeter {
    budget: Duration,
    wall: Duration,
    started: Instant,
    in_host: Duration,
    exceeded: Option<Exceeded>,
}

impl CpuMeter {
    pub fn new(budget: Duration) -> Self {
        Self::with_wall(budget, budget.saturating_add(WALL_LIMIT))
    }

    /// A meter whose calls may take at most `wall` of wall time.
    pub fn with_wall(budget: Duration, wall: Duration) -> Self {
        Self {
            budget,
            wall,
            started: Instant::now(),
            in_host: Duration::ZERO,
            exceeded: None,
        }
    }

    /// Start metering a new call; returns the epoch ticks to the first
    /// check.
    pub fn start(&mut self) -> u64 {
        self.started = Instant::now();
        self.in_host = Duration::ZERO;
        self.exceeded = None;
        ticks(self.budget)
    }

    /// Count `elapsed` as time spent in the host.
    pub fn add_host_time(&mut self, elapsed: Duration) {
        self.in_host += elapsed;
    }

    pub fn used(&self) -> Duration {
        self.started.elapsed().saturating_sub(self.in_host)
    }

    /// The limit that stopped the current call, if any.
    pub fn exceeded(&self) -> Option<Exceeded> {
        self.exceeded
    }

    /// The epoch deadline was reached: trap when the budget (or the wall
    /// time limit) is spent, otherwise check again when the rest of either
    /// could be.
    pub fn on_deadline(&mut self) -> wasmtime::Result<UpdateDeadline> {
        let used = self.used();
        let elapsed = self.started.elapsed();
        let exceeded = if used >= self.budget {
            Some(Exceeded::Cpu)
        } else {
            (elapsed >= self.wall).then_some(Exceeded::Time)
        };
        if let Some(exceeded) = exceeded {
            self.exceeded = Some(exceeded);
            return Err(wasmtime::Error::new(exceeded));
        }
        let left = (self.budget - used).min(self.wall - elapsed);
        Ok(UpdateDeadline::Continue(ticks(left)))
    }
}

/// Epoch ticks covering `duration`, at least one.
fn ticks(duration: Duration) -> u64 {
    let tick = TICK.as_nanos();
    let ticks = duration.as_nanos().div_ceil(tick);
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
}

/// Caps the linear memory of all of an instance's memories together, and
/// its tables.
#[derive(Clone, Copy, Debug)]
pub struct MemoryLimiter {
    limit: usize,
    used: usize,
    exceeded: bool,
}

/// The most elements all tables of an instance may hold.
const MAX_TABLE_ELEMENTS: usize = 100_000;

impl MemoryLimiter {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            used: 0,
            exceeded: false,
        }
    }

    pub fn exceeded(&self) -> bool {
        self.exceeded
    }

    /// The limit, in bytes.
    pub fn limit(&self) -> usize {
        self.limit
    }
}

impl ResourceLimiter for MemoryLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let grown = self.used.saturating_add(desired.saturating_sub(current));
        if grown > self.limit {
            self.exceeded = true;
            return Err(wasmtime::Error::new(Exceeded::Memory));
        }
        self.used = grown;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= MAX_TABLE_ELEMENTS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_round_up() {
        assert_eq!(ticks(Duration::ZERO), 1);
        assert_eq!(ticks(Duration::from_millis(100)), 10);
        assert_eq!(ticks(Duration::from_millis(101)), 11);
    }

    #[test]
    fn host_time_is_not_counted() {
        let mut meter = CpuMeter::new(Duration::from_secs(1));
        meter.start();
        meter.add_host_time(Duration::from_secs(60));
        assert_eq!(meter.used(), Duration::ZERO);
        assert!(matches!(
            meter.on_deadline(),
            Ok(UpdateDeadline::Continue(_))
        ));
        let mut spent = CpuMeter::new(Duration::ZERO);
        spent.start();
        assert!(spent.on_deadline().is_err());
        assert_eq!(spent.exceeded(), Some(Exceeded::Cpu));
        spent.start();
        assert_eq!(spent.exceeded(), None);
    }

    /// Host calls do not spend the CPU budget, but a call that keeps the
    /// host busy past its wall time limit stops.
    #[test]
    fn host_time_counts_against_the_wall_limit() {
        let mut meter = CpuMeter::with_wall(Duration::from_secs(60), Duration::from_millis(5));
        meter.start();
        std::thread::sleep(Duration::from_millis(20));
        meter.add_host_time(Duration::from_millis(20));
        let Err(err) = meter.on_deadline() else {
            panic!("the wall time limit is spent");
        };
        assert_eq!(err.downcast_ref::<Exceeded>(), Some(&Exceeded::Time));
        assert_eq!(meter.exceeded(), Some(Exceeded::Time));
        assert_eq!(
            CpuMeter::new(Duration::from_millis(100)).wall,
            Duration::from_millis(100) + WALL_LIMIT
        );
    }

    #[test]
    fn memory_counts_every_memory() {
        let mut limiter = MemoryLimiter::new(100);
        assert!(limiter.memory_growing(0, 60, None).unwrap());
        assert!(limiter.memory_growing(0, 40, None).unwrap());
        assert!(limiter.memory_growing(40, 41, None).is_err());
        assert!(limiter.exceeded());
        assert!(
            !limiter
                .table_growing(0, MAX_TABLE_ELEMENTS + 1, None)
                .unwrap()
        );
    }
}
