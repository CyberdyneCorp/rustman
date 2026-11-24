//! Routes protocol messages to views and views to the window (design D2,
//! D6; spec "Live updates"): `open_view` and `load_report` open windows,
//! run, exploration and bench events update the latest live view of their
//! kind,
//! and the windows are redrawn at most 10 times a second. Clicks come back
//! as [`UiEvent`]s. Nothing here talks to the producer: the companion is
//! read-only.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustman_viz_protocol::{InspectResponse, Message, View};

use crate::layout::{BENCH_TILES, RUN_TILES, ViewKind};
use crate::model::Phase;
use crate::model::bench::BenchModel;
use crate::model::exploration::ExplorationModel;
use crate::model::graph::GraphModel;
use crate::model::inspector::{InspectorModel, JsonTable};
use crate::model::run::{CLASSES, Exchange, RunModel};
use crate::report::{self, Loaded};
use crate::surface::{GraphLink, GraphNode, Rows, Series, Slice, Surface, UiEvent, WindowId};

/// At most 10 redraws a second.
pub const MIN_REDRAW: Duration = Duration::from_millis(100);

/// The donut colours of [`CLASSES`].
const CLASS_RGB: [[f32; 3]; 6] = [
    [0.20, 0.70, 0.40],
    [0.30, 0.55, 0.90],
    [0.95, 0.60, 0.15],
    [0.88, 0.22, 0.24],
    [0.60, 0.40, 0.85],
    [0.55, 0.58, 0.62],
];

enum Content {
    Run(RunModel),
    Exploration(ExplorationModel),
    Graph(GraphModel),
    Inspector {
        model: InspectorModel,
        table: Option<Arc<Mutex<JsonTable>>>,
        /// Opened by `open_view`, still waiting for its response.
        empty: bool,
    },
    Bench(BenchModel),
}

struct ViewState {
    window: WindowId,
    content: Content,
    dirty: bool,
}

#[derive(Default)]
pub struct App {
    views: Vec<ViewState>,
    last_render: Option<Instant>,
    /// Errors to show when no view can (e.g. an unreadable report).
    pub errors: Vec<String>,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    fn open(
        &mut self,
        surface: &mut dyn Surface,
        kind: ViewKind,
        title: &str,
        content: Content,
    ) -> Option<usize> {
        match surface.open(kind, title) {
            Ok(window) => {
                self.views.push(ViewState {
                    window,
                    content,
                    dirty: true,
                });
                Some(self.views.len() - 1)
            }
            Err(e) => {
                self.errors
                    .push(format!("cannot open the {kind:?} window: {e}"));
                None
            }
        }
    }

    /// The latest open view whose content `pick` accepts.
    fn latest(&self, surface: &dyn Surface, pick: impl Fn(&Content) -> bool) -> Option<usize> {
        self.views
            .iter()
            .rposition(|v| surface.is_open(v.window) && pick(&v.content))
    }

    fn live_run(&mut self, surface: &mut dyn Surface) -> Option<&mut RunModel> {
        let index = match self.latest(
            surface,
            |c| matches!(c, Content::Run(m) if m.phase == Phase::Running),
        ) {
            Some(index) => index,
            None => self.open(
                surface,
                ViewKind::Run,
                "Run",
                Content::Run(RunModel::new("Run")),
            )?,
        };
        let view = &mut self.views[index];
        view.dirty = true;
        match &mut view.content {
            Content::Run(model) => Some(model),
            _ => None,
        }
    }

    fn live_exploration(&mut self, surface: &mut dyn Surface) -> Option<&mut ExplorationModel> {
        let pick = |c: &Content| matches!(c, Content::Exploration(m) if m.phase == Phase::Running);
        let index = match self.latest(surface, pick) {
            Some(index) => index,
            None => {
                let model = ExplorationModel::new("Exploration");
                self.open(
                    surface,
                    ViewKind::Exploration,
                    "Exploration",
                    Content::Exploration(model),
                )?
            }
        };
        let view = &mut self.views[index];
        view.dirty = true;
        match &mut view.content {
            Content::Exploration(model) => Some(model),
            _ => None,
        }
    }

    fn live_bench(&mut self, surface: &mut dyn Surface) -> Option<&mut BenchModel> {
        let pick = |c: &Content| matches!(c, Content::Bench(m) if m.phase == Phase::Running);
        let index = match self.latest(surface, pick) {
            Some(index) => index,
            None => self.open(
                surface,
                ViewKind::Bench,
                "Bench",
                Content::Bench(BenchModel::new("Bench")),
            )?,
        };
        let view = &mut self.views[index];
        view.dirty = true;
        match &mut view.content {
            Content::Bench(model) => Some(model),
            _ => None,
        }
    }

    /// Apply one message from the producer.
    pub fn handle(&mut self, surface: &mut dyn Surface, message: Message) {
        match message {
            Message::OpenView(open) => {
                let Some(kind) = ViewKind::from_view(open.view) else {
                    return;
                };
                let title = open.title.unwrap_or_else(|| format!("{kind:?}"));
                let content = match kind {
                    ViewKind::Run => Content::Run(RunModel::new(&title)),
                    ViewKind::Exploration => Content::Exploration(ExplorationModel::new(&title)),
                    ViewKind::Graph => {
                        Content::Graph(GraphModel::new(&title, &[], &[], Vec::new()))
                    }
                    ViewKind::Inspector => Content::Inspector {
                        model: InspectorModel {
                            title: title.clone(),
                            ..InspectorModel::default()
                        },
                        table: None,
                        empty: true,
                    },
                    ViewKind::Bench => Content::Bench(BenchModel::new(&title)),
                };
                self.open(surface, kind, &title, content);
            }
            Message::RunStarted(started) => {
                if let Some(run) = self.live_run(surface) {
                    run.started(&started);
                }
            }
            Message::ExchangeCompleted(exchange) => {
                if let Some(run) = self.live_run(surface) {
                    run.exchange(Exchange::from(exchange.as_ref()));
                }
            }
            Message::RunFinished(finished) => {
                if let Some(run) = self.live_run(surface) {
                    run.finished(&finished);
                }
            }
            Message::ExplorationStarted(started) => {
                if let Some(model) = self.live_exploration(surface) {
                    model.started(&started);
                }
            }
            Message::ProbeCompleted(probe) => {
                if let Some(model) = self.live_exploration(surface) {
                    model.probe(&probe);
                }
            }
            Message::Finding(finding) => {
                if let Some(model) = self.live_exploration(surface) {
                    model.finding(finding);
                }
            }
            Message::ExplorationFinished(finished) => {
                if let Some(model) = self.live_exploration(surface) {
                    model.finished(&finished);
                }
            }
            Message::BenchTick(tick) => {
                if let Some(model) = self.live_bench(surface) {
                    model.tick(&tick);
                }
            }
            Message::BenchFinished(finished) => {
                if let Some(model) = self.live_bench(surface) {
                    model.finished(&finished);
                }
            }
            Message::LoadReport(load) => self.load_report(surface, &load.path, load.view),
            Message::BlobRef(blob) => self.load_report(surface, &blob.path, None),
            Message::InspectResponse(response) => self.inspect(surface, &response),
            Message::Error(error) => self.errors.push(error.message),
            _ => {}
        }
    }

    fn load_report(&mut self, surface: &mut dyn Surface, path: &str, view: Option<View>) {
        let title = std::path::Path::new(path)
            .file_name()
            .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());
        match report::load(path) {
            Ok(Loaded::Run(mut model)) => {
                model.title = title.clone();
                self.open(surface, ViewKind::Run, &title, Content::Run(model));
            }
            Ok(Loaded::Exploration {
                mut dashboard,
                mut graph,
            }) => {
                if view == Some(View::ApiGraph) {
                    graph.title = title.clone();
                    self.open(surface, ViewKind::Graph, &title, Content::Graph(graph));
                } else {
                    dashboard.title = title.clone();
                    self.open(
                        surface,
                        ViewKind::Exploration,
                        &title,
                        Content::Exploration(dashboard),
                    );
                }
            }
            Ok(Loaded::Bench(mut model)) => {
                model.title = title.clone();
                self.open(surface, ViewKind::Bench, &title, Content::Bench(model));
            }
            Err(e) => self.errors.push(e),
        }
    }

    fn inspect(&mut self, surface: &mut dyn Surface, response: &InspectResponse) {
        let waiting = self.latest(surface, |c| {
            matches!(c, Content::Inspector { empty: true, .. })
        });
        let title = match waiting.map(|i| &self.views[i].content) {
            Some(Content::Inspector { model, .. }) => model.title.clone(),
            _ => response
                .request
                .clone()
                .unwrap_or_else(|| "Response".to_owned()),
        };
        let model = InspectorModel::new(&title, response);
        let table = model.table.clone().map(|t| Arc::new(Mutex::new(t)));
        let content = Content::Inspector {
            model,
            table,
            empty: false,
        };
        match waiting {
            Some(index) => {
                self.views[index].content = content;
                self.views[index].dirty = true;
            }
            None => {
                self.open(surface, ViewKind::Inspector, &title, content);
            }
        }
    }

    /// The producer is gone: live views keep their state, marked
    /// `Producer disconnected`.
    pub fn producer_gone(&mut self) {
        for view in &mut self.views {
            let phase = match &mut view.content {
                Content::Run(m) => &mut m.phase,
                Content::Exploration(m) => &mut m.phase,
                Content::Bench(m) => &mut m.phase,
                _ => continue,
            };
            if *phase == Phase::Running {
                *phase = Phase::Disconnected;
                view.dirty = true;
            }
        }
    }

    /// Apply what the user did.
    pub fn ui_event(&mut self, event: UiEvent) {
        let window_of = |e: &UiEvent| match e {
            UiEvent::HeatmapRow { window, .. }
            | UiEvent::NodeSelected { window, .. }
            | UiEvent::Sort { window, .. }
            | UiEvent::Closed(window) => *window,
        };
        let window = window_of(&event);
        if let UiEvent::Closed(_) = event {
            self.views.retain(|v| v.window != window);
            return;
        }
        let Some(view) = self.views.iter_mut().find(|v| v.window == window) else {
            return;
        };
        match (&mut view.content, event) {
            (Content::Exploration(model), UiEvent::HeatmapRow { row, .. }) => model.click_row(row),
            (Content::Graph(model), UiEvent::NodeSelected { node, .. }) => model.selected = node,
            (
                Content::Inspector {
                    table: Some(table), ..
                },
                UiEvent::Sort {
                    column, descending, ..
                },
            ) => {
                if let Ok(mut table) = table.lock() {
                    table.sort(column, descending);
                }
            }
            _ => return,
        }
        view.dirty = true;
        // A click answers at once, not at the next tick.
        self.last_render = None;
    }

    /// Whether a redraw is waiting.
    pub fn needs_render(&self) -> bool {
        self.views.iter().any(|v| v.dirty)
    }

    /// Whether any window is still open.
    pub fn has_windows(&self, surface: &dyn Surface) -> bool {
        self.views.iter().any(|v| surface.is_open(v.window))
    }

    /// Push the views that changed, unless the last redraw was less than
    /// [`MIN_REDRAW`] ago. Returns whether anything was drawn.
    pub fn render(&mut self, surface: &mut dyn Surface, now: Instant) -> bool {
        if !self.needs_render() {
            return false;
        }
        if self
            .last_render
            .is_some_and(|last| now.duration_since(last) < MIN_REDRAW)
        {
            return false;
        }
        self.last_render = Some(now);
        for view in self.views.iter_mut().filter(|v| v.dirty) {
            view.dirty = false;
            if !surface.is_open(view.window) {
                continue;
            }
            match &view.content {
                Content::Run(model) => draw_run(surface, view.window, model),
                Content::Exploration(model) => draw_exploration(surface, view.window, model),
                Content::Graph(model) => draw_graph(surface, view.window, model),
                Content::Inspector {
                    model,
                    table,
                    empty,
                } => {
                    if !*empty {
                        draw_inspector(surface, view.window, model, table.as_ref());
                    }
                }
                Content::Bench(model) => draw_bench(surface, view.window, model),
            }
        }
        true
    }
}

fn draw_run(s: &mut dyn Surface, w: WindowId, model: &RunModel) {
    s.label(w, "title", &model.title);
    let mut subtitle = Vec::new();
    if let Some(env) = &model.environment {
        subtitle.push(format!("environment {env}"));
    }
    if let Some(planned) = model.planned {
        subtitle.push(format!("{planned} requests planned"));
    }
    s.label(w, "subtitle", &subtitle.join(" · "));
    s.label(w, "state", model.state_text());
    let texts = model.tile_texts();
    let history = model.tile_history();
    for (i, (label, spark)) in RUN_TILES.iter().enumerate() {
        s.label(w, label, &texts[i]);
        s.sparkline(w, spark, &history[i]);
    }
    let counts = model.outcome_counts();
    let slices: Vec<Slice> = CLASSES
        .iter()
        .zip(counts)
        .zip(CLASS_RGB)
        .map(|((label, n), rgb)| Slice {
            label: (*label).to_owned(),
            value: n as f64,
            rgb,
        })
        .collect();
    s.donut(
        w,
        "outcomes",
        &slices,
        &counts.iter().sum::<u64>().to_string(),
    );
    s.plot(
        w,
        "latency",
        "request",
        "ms",
        &[Series {
            name: "latency".into(),
            points: model.latency_points(),
            step: false,
        }],
    );
    let histogram = model
        .histogram()
        .into_iter()
        .map(|(bound, n)| (bound, n as f64))
        .collect();
    s.plot(
        w,
        "histogram",
        "ms (upper bound)",
        "requests",
        &[Series {
            name: "requests".into(),
            points: histogram,
            step: true,
        }],
    );
    let (columns, rows) = model.request_table();
    s.table(w, "requests", &columns, Rows::Cells(rows), false);
}

fn draw_exploration(s: &mut dyn Surface, w: WindowId, model: &ExplorationModel) {
    s.label(w, "title", &model.title);
    let mut subtitle = vec![format!("{} operations", model.operations_total)];
    if let Some(url) = &model.base_url {
        subtitle.insert(0, url.clone());
    }
    s.label(w, "subtitle", &subtitle.join(" · "));
    s.label(w, "state", model.phase.text());
    let (rows, columns, cells) = model.heatmap();
    s.heatmap(w, "heatmap", &rows, &columns, &cells);
    let ops = model.operation_coverage();
    let exercised = model
        .final_exercised
        .unwrap_or(model.exercised.len() as u64);
    s.gauge(
        w,
        "coverage_operations",
        ops,
        &format!("{exercised} of {} operations", model.operations_total),
    );
    match model.status_coverage() {
        Some(statuses) => s.gauge(
            w,
            "coverage_statuses",
            statuses,
            &format!(
                "{} of {} statuses",
                model.statuses_seen, model.statuses_documented
            ),
        ),
        None => s.gauge(
            w,
            "coverage_statuses",
            0.0,
            "known when the exploration ends",
        ),
    }
    s.label(w, "findings_heading", &model.findings_heading());
    let (columns, rows) = model.findings_cells();
    s.table(w, "findings", &columns, Rows::Cells(rows), false);
}

fn draw_graph(s: &mut dyn Surface, w: WindowId, model: &GraphModel) {
    s.label(w, "title", &model.title);
    s.label(
        w,
        "subtitle",
        "Operations linked by the values they pass to each other, coloured by their worst finding",
    );
    let nodes: Vec<GraphNode> = model
        .nodes
        .iter()
        .map(|n| GraphNode {
            title: n.operation.clone(),
            x: n.x,
            y: n.y,
            rgb: n.tint.rgb(),
        })
        .collect();
    let links: Vec<GraphLink> = model
        .links
        .iter()
        .map(|l| GraphLink {
            from: l.producer,
            to: l.consumer,
            label: l.parameter.clone(),
        })
        .collect();
    s.graph(w, "graph", &nodes, &links);
    s.label(w, "selection_heading", &model.selection_heading());
    let (columns, rows) = model.selection_cells();
    s.table(w, "selection", &columns, Rows::Cells(rows), false);
}

fn draw_inspector(
    s: &mut dyn Surface,
    w: WindowId,
    model: &InspectorModel,
    table: Option<&Arc<Mutex<JsonTable>>>,
) {
    s.label(w, "title", &model.title);
    s.label(w, "heading", &model.heading());
    s.label(w, "summary", &model.summary);
    match table {
        Some(table) => {
            let columns = table.lock().map(|t| t.columns.clone()).unwrap_or_default();
            s.table(w, "table", &columns, Rows::Json(Arc::clone(table)), true);
        }
        None => s.table(
            w,
            "table",
            &["(no array in this body)".to_owned()],
            Rows::Cells(Vec::new()),
            false,
        ),
    }
    s.text(w, "json", &model.text, "JSON");
}

fn draw_bench(s: &mut dyn Surface, w: WindowId, model: &BenchModel) {
    s.label(w, "title", &model.title);
    s.label(w, "subtitle", &model.subtitle());
    s.label(w, "state", &model.state_text());
    let texts = model.tile_texts();
    let history = model.tile_history();
    for (i, (label, spark)) in BENCH_TILES.iter().enumerate() {
        s.label(w, label, &texts[i]);
        s.sparkline(w, spark, &history[i]);
    }
    let series = |named: &[(&str, Vec<(f64, f64)>)]| -> Vec<Series> {
        named
            .iter()
            .map(|(name, points)| Series {
                name: (*name).to_owned(),
                points: points.clone(),
                step: false,
            })
            .collect()
    };
    s.plot(
        w,
        "throughput",
        "second",
        "per second",
        &series(&model.throughput()),
    );
    s.plot(w, "latency", "second", "ms", &series(&model.latency()));
    s.plot(w, "vus", "second", "VUs", &series(&[("VUs", model.vus())]));
    let (columns, rows) = model.summary_table();
    s.table(w, "summary", &columns, Rows::Cells(rows), false);
    s.label(w, "thresholds_heading", &model.thresholds_heading());
    let (columns, rows) = model.threshold_table();
    s.table(w, "thresholds", &columns, Rows::Cells(rows), false);
}

#[cfg(test)]
pub mod recorder {
    //! A [`Surface`] that records the last value of every slot.

    use std::collections::HashMap;

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Drawn {
        Label(String),
        Sparkline(Vec<f32>),
        Donut(Vec<Slice>, String),
        Gauge(f64, String),
        Heatmap(Vec<String>, Vec<String>, Vec<Vec<f32>>),
        Plot(Vec<Series>),
        Table(Vec<String>, Vec<Vec<String>>, bool),
        Text(String),
        Graph(Vec<GraphNode>, Vec<GraphLink>),
    }

    #[derive(Default)]
    pub struct Recorder {
        pub windows: Vec<(ViewKind, String, bool)>,
        pub slots: HashMap<(WindowId, String), Drawn>,
        pub draws: usize,
        pub pending: Vec<UiEvent>,
    }

    impl Recorder {
        pub fn get(&self, window: WindowId, slot: &str) -> Option<&Drawn> {
            self.slots.get(&(window, slot.to_owned()))
        }

        pub fn label(&self, window: WindowId, slot: &str) -> String {
            match self.get(window, slot) {
                Some(Drawn::Label(text)) => text.clone(),
                other => panic!("{slot} is not a label: {other:?}"),
            }
        }

        fn set(&mut self, window: WindowId, slot: &str, drawn: Drawn) {
            assert!(
                self.windows[window].0.slots().contains(&slot),
                "{slot} is not a slot of {:?}",
                self.windows[window].0
            );
            self.draws += 1;
            self.slots.insert((window, slot.to_owned()), drawn);
        }
    }

    impl Surface for Recorder {
        fn open(&mut self, kind: ViewKind, title: &str) -> Result<WindowId, String> {
            self.windows.push((kind, title.to_owned(), true));
            Ok(self.windows.len() - 1)
        }
        fn label(&mut self, w: WindowId, slot: &str, text: &str) {
            self.set(w, slot, Drawn::Label(text.to_owned()));
        }
        fn sparkline(&mut self, w: WindowId, slot: &str, values: &[f32]) {
            self.set(w, slot, Drawn::Sparkline(values.to_vec()));
        }
        fn donut(&mut self, w: WindowId, slot: &str, slices: &[Slice], center: &str) {
            self.set(w, slot, Drawn::Donut(slices.to_vec(), center.to_owned()));
        }
        fn gauge(&mut self, w: WindowId, slot: &str, percent: f64, caption: &str) {
            self.set(w, slot, Drawn::Gauge(percent, caption.to_owned()));
        }
        fn heatmap(
            &mut self,
            w: WindowId,
            slot: &str,
            rows: &[String],
            columns: &[String],
            cells: &[Vec<f32>],
        ) {
            self.set(
                w,
                slot,
                Drawn::Heatmap(rows.to_vec(), columns.to_vec(), cells.to_vec()),
            );
        }
        fn plot(&mut self, w: WindowId, slot: &str, _x: &str, _y: &str, series: &[Series]) {
            self.set(w, slot, Drawn::Plot(series.to_vec()));
        }
        fn table(
            &mut self,
            w: WindowId,
            slot: &str,
            columns: &[String],
            rows: Rows,
            sortable: bool,
        ) {
            let cells = (0..rows.len())
                .map(|r| (0..columns.len()).map(|c| rows.cell(r, c)).collect())
                .collect();
            self.set(w, slot, Drawn::Table(columns.to_vec(), cells, sortable));
        }
        fn text(&mut self, w: WindowId, slot: &str, text: &str, _language: &str) {
            self.set(w, slot, Drawn::Text(text.to_owned()));
        }
        fn graph(&mut self, w: WindowId, slot: &str, nodes: &[GraphNode], links: &[GraphLink]) {
            self.set(w, slot, Drawn::Graph(nodes.to_vec(), links.to_vec()));
        }
        fn is_open(&self, w: WindowId) -> bool {
            self.windows.get(w).is_some_and(|(_, _, open)| *open)
        }
        fn events(&mut self) -> Vec<UiEvent> {
            std::mem::take(&mut self.pending)
        }
    }
}

#[cfg(test)]
mod tests {
    use rustman_viz_protocol::{
        Assertions, BenchFinished, BenchThreshold, BenchTick, Body, ExchangeCompleted,
        ExplorationFinished, ExplorationStarted, Finding, LoadReport, OpenView, Outcome,
        ProbeCompleted, ReportKind, RunFinished, RunStarted, RunState, RunSummary, Severity,
    };

    use super::recorder::{Drawn, Recorder};
    use super::*;

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    fn exchange(n: u64, status: u16) -> Message {
        Message::ExchangeCompleted(Box::new(ExchangeCompleted {
            request: format!("r{}", n % 3),
            method: "GET".into(),
            url: "https://api.test/x".into(),
            outcome: if status < 400 {
                Outcome::Passed
            } else {
                Outcome::Failed
            },
            status: Some(status),
            duration_ms: 10 + n % 40,
            assertions: Assertions {
                passed: 1,
                failed: 0,
            },
            error: None,
            iteration: None,
        }))
    }

    /// Render once the rate limit allows.
    fn flush(app: &mut App, surface: &mut Recorder) {
        app.last_render = None;
        app.render(surface, Instant::now());
    }

    #[test]
    fn a_streaming_run_updates_tiles_then_shows_completed() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(
            &mut s,
            Message::OpenView(OpenView {
                view: View::RunDashboard,
                title: Some("api/".into()),
            }),
        );
        app.handle(
            &mut s,
            Message::RunStarted(RunStarted {
                started_at: "t".into(),
                environment: Some("dev".into()),
                total: Some(1000),
            }),
        );
        for n in 0..400 {
            app.handle(&mut s, exchange(n, 200));
        }
        flush(&mut app, &mut s);
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.label(0, "kpi_requests"), "400 / 1000");
        assert_eq!(s.label(0, "state"), "Running");
        assert_eq!(
            s.label(0, "subtitle"),
            "environment dev · 1000 requests planned"
        );
        for n in 400..1000 {
            app.handle(&mut s, exchange(n, 200));
        }
        app.handle(
            &mut s,
            Message::RunFinished(RunFinished {
                state: RunState::Completed,
                summary: RunSummary::default(),
            }),
        );
        flush(&mut app, &mut s);
        assert_eq!(s.label(0, "kpi_requests"), "1000");
        assert_eq!(s.label(0, "state"), "Completed");
        let Some(Drawn::Donut(slices, center)) = s.get(0, "outcomes") else {
            panic!()
        };
        assert_eq!(slices[0].value, 1000.0);
        assert_eq!(center, "1000");
    }

    #[test]
    fn redraws_are_capped_at_ten_a_second() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        let start = Instant::now();
        app.handle(&mut s, exchange(0, 200));
        assert!(app.render(&mut s, start));
        app.handle(&mut s, exchange(1, 200));
        assert!(!app.render(&mut s, start + Duration::from_millis(50)));
        assert!(app.needs_render());
        assert!(app.render(&mut s, start + Duration::from_millis(100)));
        assert!(
            !app.render(&mut s, start + Duration::from_millis(500)),
            "nothing changed"
        );
    }

    #[test]
    fn events_without_open_view_open_a_window() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(&mut s, exchange(0, 500));
        assert_eq!(s.windows[0].0, ViewKind::Run);
    }

    #[test]
    fn the_producer_leaving_marks_live_views() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(&mut s, exchange(0, 200));
        app.producer_gone();
        flush(&mut app, &mut s);
        assert_eq!(s.label(0, "state"), "Producer disconnected");
        assert_eq!(s.label(0, "kpi_requests"), "1", "the last state stays");
    }

    #[test]
    fn a_finished_run_is_not_marked_disconnected() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(&mut s, exchange(0, 200));
        app.handle(
            &mut s,
            Message::RunFinished(RunFinished {
                state: RunState::Completed,
                summary: RunSummary::default(),
            }),
        );
        app.producer_gone();
        flush(&mut app, &mut s);
        assert_eq!(s.label(0, "state"), "Completed");
    }

    #[test]
    fn a_live_exploration_with_a_burst_of_findings() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(
            &mut s,
            Message::OpenView(OpenView {
                view: View::ExplorationDashboard,
                title: Some("api.yaml".into()),
            }),
        );
        app.handle(
            &mut s,
            Message::ExplorationStarted(ExplorationStarted {
                started_at: "t".into(),
                operations: 24,
                base_url: Some("https://api.test".into()),
            }),
        );
        for i in 0..18 {
            app.handle(
                &mut s,
                Message::ProbeCompleted(ProbeCompleted {
                    operation: format!("GET /r{i}"),
                    outcome: Outcome::Passed,
                    status: Some(200),
                    duration_ms: 3,
                }),
            );
        }
        for i in 0..500 {
            app.handle(
                &mut s,
                Message::Finding(Finding {
                    severity: if i % 2 == 0 {
                        Severity::High
                    } else {
                        Severity::Low
                    },
                    operation: if i % 2 == 0 {
                        "POST /users".into()
                    } else {
                        "GET /orders".into()
                    },
                    title: format!("f{i}"),
                    detail: None,
                }),
            );
        }
        app.handle(
            &mut s,
            Message::ExplorationFinished(ExplorationFinished {
                state: RunState::Completed,
                operations_total: 24,
                operations_exercised: 18,
                status_codes_documented: 40,
                status_codes_seen: 31,
            }),
        );
        flush(&mut app, &mut s);
        assert_eq!(
            s.get(0, "coverage_operations"),
            Some(&Drawn::Gauge(75.0, "18 of 24 operations".into()))
        );
        let Some(Drawn::Table(_, rows, _)) = s.get(0, "findings") else {
            panic!()
        };
        assert_eq!(rows.len(), 500);
        assert_eq!(rows[0][0], "high");
        // Click the heatmap row of POST /users: only its findings remain.
        let Some(Drawn::Heatmap(names, _, _)) = s.get(0, "heatmap") else {
            panic!()
        };
        let row = names.iter().position(|n| n == "POST /users").unwrap();
        app.ui_event(UiEvent::HeatmapRow { window: 0, row });
        app.render(&mut s, Instant::now());
        let Some(Drawn::Table(_, rows, _)) = s.get(0, "findings") else {
            panic!()
        };
        assert_eq!(rows.len(), 250);
        assert!(rows.iter().all(|r| r[1] == "POST /users"));
        assert!(
            s.label(0, "findings_heading")
                .starts_with("Findings for POST /users: 250 of 500")
        );
    }

    #[test]
    fn saved_reports_open_in_the_matching_view() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(
            &mut s,
            Message::LoadReport(LoadReport {
                path: fixture("run-report.json"),
                kind: ReportKind::Run,
                view: Some(View::RunDashboard),
            }),
        );
        app.handle(
            &mut s,
            Message::LoadReport(LoadReport {
                path: fixture("exploration-report.json"),
                kind: ReportKind::Exploration,
                view: Some(View::ExplorationDashboard),
            }),
        );
        app.handle(
            &mut s,
            Message::LoadReport(LoadReport {
                path: fixture("exploration-report.json"),
                kind: ReportKind::Exploration,
                view: Some(View::ApiGraph),
            }),
        );
        flush(&mut app, &mut s);
        let kinds: Vec<_> = s.windows.iter().map(|w| w.0).collect();
        assert_eq!(
            kinds,
            [ViewKind::Run, ViewKind::Exploration, ViewKind::Graph]
        );
        assert_eq!(s.label(0, "kpi_requests"), "200");
        let Some(Drawn::Donut(slices, _)) = s.get(0, "outcomes") else {
            panic!()
        };
        let values: Vec<f64> = slices.iter().map(|s| s.value).collect();
        assert_eq!(values, [180.0, 0.0, 15.0, 0.0, 5.0, 0.0]);
        assert_eq!(s.label(1, "state"), "Completed");
        let Some(Drawn::Graph(nodes, links)) = s.get(2, "graph") else {
            panic!()
        };
        let index = |op: &str| nodes.iter().position(|n| n.title == op).unwrap();
        assert!(
            links
                .iter()
                .any(|l| l.from == index("POST /users") && l.to == index("GET /users/{id}"))
        );
        app.ui_event(UiEvent::NodeSelected {
            window: 2,
            node: Some(index("POST /users")),
        });
        app.render(&mut s, Instant::now());
        assert_eq!(s.label(2, "selection_heading"), "POST /users: 1 findings");
    }

    #[test]
    fn an_unreadable_report_is_an_error_not_a_window() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(
            &mut s,
            Message::LoadReport(LoadReport {
                path: "/nope/report.json".into(),
                kind: ReportKind::Run,
                view: None,
            }),
        );
        assert!(s.windows.is_empty());
        assert!(app.errors[0].starts_with("cannot read /nope/report.json"));
    }

    #[test]
    fn the_inspector_fills_the_window_opened_for_it_and_sorts() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(
            &mut s,
            Message::OpenView(OpenView {
                view: View::ResponseInspector,
                title: Some("List users".into()),
            }),
        );
        app.handle(
            &mut s,
            Message::InspectResponse(Box::new(InspectResponse {
                request: Some("users/list".into()),
                status: Some(200),
                content_type: Some("application/json".into()),
                headers: Vec::new(),
                selector: None,
                body: Body::Inline {
                    text: r#"[{"id":2,"name":"b"},{"id":1,"email":"a@x"}]"#.into(),
                },
            })),
        );
        flush(&mut app, &mut s);
        assert_eq!(s.windows.len(), 1, "no second window");
        assert_eq!(s.label(0, "title"), "List users");
        let Some(Drawn::Table(columns, rows, sortable)) = s.get(0, "table") else {
            panic!()
        };
        assert_eq!(columns, &["id", "name", "email"]);
        assert_eq!(rows[0], ["2", "b", ""]);
        assert!(sortable);
        app.ui_event(UiEvent::Sort {
            window: 0,
            slot: "table".into(),
            column: 0,
            descending: false,
        });
        app.render(&mut s, Instant::now());
        let Some(Drawn::Table(_, rows, _)) = s.get(0, "table") else {
            panic!()
        };
        assert_eq!(rows[0], ["1", "", "a@x"]);
        let Some(Drawn::Text(text)) = s.get(0, "json") else {
            panic!()
        };
        assert!(text.contains("\"email\": \"a@x\""));
    }

    fn bench_tick(second: u32) -> Message {
        Message::BenchTick(BenchTick {
            second,
            elapsed_ms: u64::from(second) * 1000,
            remaining_ms: Some(u64::from(30 - second) * 1000),
            active_vus: 10,
            requests: 400,
            errors: u64::from(second % 2),
            rps: 400.0,
            p50_ms: 18.0,
            p95_ms: 60.0 + f64::from(second),
            p99_ms: 120.0,
        })
    }

    #[test]
    fn a_streaming_bench_updates_its_dashboard_then_shows_the_totals() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(
            &mut s,
            Message::OpenView(OpenView {
                view: View::Bench,
                title: Some("rustman bench https://staging.example.com".into()),
            }),
        );
        for second in 1..=3 {
            app.handle(&mut s, bench_tick(second));
        }
        flush(&mut app, &mut s);
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.windows[0].0, ViewKind::Bench);
        assert_eq!(s.label(0, "state"), "Running");
        assert_eq!(s.label(0, "kpi_requests"), "1200");
        assert_eq!(s.label(0, "kpi_p95"), "63 ms");
        assert_eq!(s.label(0, "kpi_vus"), "10");
        assert_eq!(s.label(0, "subtitle"), "3 s elapsed · 27 s left");
        let Some(Drawn::Plot(series)) = s.get(0, "latency") else {
            panic!()
        };
        assert_eq!(series.len(), 3);
        assert_eq!(series[1].points, [(1.0, 61.0), (2.0, 62.0), (3.0, 63.0)]);
        let Some(Drawn::Sparkline(rps)) = s.get(0, "spark_rps") else {
            panic!()
        };
        assert_eq!(rps.len(), 3);
        app.handle(
            &mut s,
            Message::BenchFinished(BenchFinished {
                state: RunState::Completed,
                duration_ms: 3000,
                iterations: 600,
                requests: 1200,
                errors: 2,
                rps: 400.0,
                p50_ms: 18.0,
                p95_ms: 62.0,
                p99_ms: 120.0,
                thresholds: vec![BenchThreshold {
                    expression: "p95<300ms".into(),
                    passed: true,
                    actual: "62ms".into(),
                }],
                aborted: None,
            }),
        );
        app.producer_gone();
        flush(&mut app, &mut s);
        assert_eq!(s.windows.len(), 1, "the same window");
        assert_eq!(s.label(0, "state"), "Completed");
        assert_eq!(s.label(0, "thresholds_heading"), "Thresholds: all 1 passed");
        let Some(Drawn::Table(_, rows, _)) = s.get(0, "thresholds") else {
            panic!()
        };
        assert_eq!(rows, &[["p95<300ms", "✓ passed", "62ms"]]);
    }

    #[test]
    fn a_bench_cut_short_is_marked_disconnected() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(&mut s, bench_tick(1));
        assert_eq!(s.windows[0].0, ViewKind::Bench, "ticks open a bench view");
        app.producer_gone();
        flush(&mut app, &mut s);
        assert_eq!(s.label(0, "state"), "Producer disconnected");
        assert_eq!(s.label(0, "kpi_requests"), "400");
    }

    #[test]
    fn a_saved_bench_report_opens_the_bench_view() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(
            &mut s,
            Message::LoadReport(LoadReport {
                path: fixture("bench-report.json"),
                kind: ReportKind::Bench,
                view: Some(View::Bench),
            }),
        );
        flush(&mut app, &mut s);
        assert_eq!(s.windows[0].0, ViewKind::Bench);
        assert_eq!(s.label(0, "title"), "bench-report.json");
        assert_eq!(s.label(0, "kpi_requests"), "120");
        assert_eq!(
            s.label(0, "thresholds_heading"),
            "Thresholds: 1 of 2 failed"
        );
        let Some(Drawn::Plot(series)) = s.get(0, "throughput") else {
            panic!()
        };
        assert_eq!(series[0].points.len(), 3);
    }

    #[test]
    fn closed_windows_are_forgotten() {
        let (mut app, mut s) = (App::new(), Recorder::default());
        app.handle(&mut s, exchange(0, 200));
        assert!(app.has_windows(&s));
        s.windows[0].2 = false;
        app.ui_event(UiEvent::Closed(0));
        assert!(!app.has_windows(&s));
        // The next event opens a new run window.
        app.handle(&mut s, exchange(1, 200));
        assert_eq!(s.windows.len(), 2);
    }
}
