//! Load testing for Rustman (`rustman bench`, add-load-testing): request
//! files and collections run as virtual-user (VU) iterations, with the same
//! resolution, captures and assertions as `rustman run`.
//!
//! - **Reuse, not a second path** (design D1): each VU is a
//!   [`rustman_http::runner::Runner`] of its own, so it has its own
//!   variable and capture scope, cookie jar and connection pool, and sends
//!   every request through [`rustman_http::runner::Runner::send`], the
//!   executor of `rustman run`.
//! - **Safety** ([`safety`]): the allowlist is the `--target` origin only,
//!   method gating and the production guard apply, every request is checked
//!   before load starts, and every request (and redirect hop) is sent
//!   through a [`rustman_http::GovernedSender`]. One global governor built
//!   from the `[bench]` caps ([`caps`]) enforces `max_rps` across every
//!   executor.
//! - **Executors**: `constant-vus` and `ramping-vus` (closed models) and
//!   `constant-arrival-rate` (the open model, with coordinated-omission
//!   correction). See [`profile`].
//! - **Data** ([`data`]): rows from `rustman_core::data_rows`, fed
//!   `sequential`, `per-vu` or `random`.
//! - **Metrics** ([`metrics`]): per-second HDR histograms per request file
//!   and overall, merged at the end, with error classes, checks, phase
//!   timings and bytes; bounded in memory.
//! - **Thresholds** ([`threshold`]): evaluated at the end, and every second
//!   after a warm-up for abort-on-fail (`!`) ones.
//! - **Generator lag** ([`lag`]): a warning when the generator itself is
//!   the bottleneck.
//! - **Reports** ([`report`]): the `kind: bench` report of a run.
//! - **Stress testing** ([`stress`]): `rustman stress`, modes that search
//!   for the target's limit over the same metrics, with stricter safety
//!   rules and caps of their own.
//! - **Live view** ([`live`]): snapshots of the run so far for the TTY
//!   dashboard, the non-TTY progress lines and the viz companion, read
//!   without slowing the VUs ([`Bench::live`]).
//!
//! Usage: build a [`BenchInput`], [`Bench::prepare`] it (caps, policy and
//! static checks; nothing is sent), then [`Bench::run`] it on a Tokio
//! runtime. Building on this, a stress executor only needs a new
//! [`Profile`] variant and its scheduler beside [`closed`] and [`open`].
//!
//! See `docs/bench.md`.

mod bench;
pub mod caps;
mod closed;
pub mod data;
mod error;
pub mod lag;
pub mod live;
pub mod metrics;
mod open;
pub mod profile;
pub mod report;
pub mod safety;
pub mod stress;
pub mod summary;
pub mod threshold;
mod vu;

pub use bench::{
    ABORT_DRAIN, ABORT_WARMUP, Bench, BenchInput, BenchOutcome, INTERRUPT_DRAIN, TICK,
};
pub use data::{DataFeed, DataMode, Exhaust, Feeder};
pub use error::BenchError;
pub use live::{Live, Snapshot, interrupt_notice};
pub use metrics::{BenchSummary, ErrorClass, LatencyKind, Metrics};
pub use profile::{
    ArrivalRate, ConstantVus, ExecutorKind, GRACEFUL_STOP, Limit, Profile, ProfileSpec, RampingVus,
    Stage, ThinkTime,
};
pub use report::{ReportContext, bench_report};
pub use rustman_report::bench::Aborted;
pub use safety::Safety;
pub use threshold::Threshold;
