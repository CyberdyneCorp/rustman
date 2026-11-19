//! `rustman-viz`, the optional graphical companion of Rustman (openspec
//! change `add-raven-visualizer`). The producer (`rustman`) starts it and
//! writes `rustman-viz/1` messages to its stdin; it shows them as
//! dashboards. It is read-only: it sends no request, runs no request file
//! and writes no workspace file (spec "Read-only companion").
//!
//! This library is everything but the window system: the reader thread
//! and its queue, the view models, saved reports, the layouts and the app
//! that routes messages to views through the [`surface::Surface`] trait.
//! The `rustman-viz` binary (feature `raven`) adds the Raven adapter, the
//! only code with `unsafe`.

pub mod app;
pub mod inbox;
pub mod layout;
pub mod model;
pub mod reader;
pub mod report;
pub mod surface;
