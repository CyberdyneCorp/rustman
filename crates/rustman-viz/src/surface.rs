//! The safe interface between the app and the window system (design D4):
//! views open windows from their layout and fill named slots. The Raven
//! adapter implements it with `unsafe` FFI calls; tests implement it with
//! a recorder.

use std::sync::{Arc, Mutex};

use crate::layout::ViewKind;
use crate::model::inspector::JsonTable;

/// An open window, as the app knows it.
pub type WindowId = usize;

/// A slice of a donut.
#[derive(Debug, Clone, PartialEq)]
pub struct Slice {
    pub label: String,
    pub value: f64,
    pub rgb: [f32; 3],
}

/// A series of a plot.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    pub points: Vec<(f64, f64)>,
    /// Drawn as steps (a histogram) instead of a line.
    pub step: bool,
}

/// A node of a graph.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNode {
    pub title: String,
    pub x: f32,
    pub y: f32,
    pub rgb: [f32; 3],
}

/// A link from node `from` to node `to`, labelled with a parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphLink {
    pub from: usize,
    pub to: usize,
    pub label: String,
}

/// The rows of a table: all in memory, or read on demand from a JSON
/// array (virtual rows, for large responses).
#[derive(Debug, Clone)]
pub enum Rows {
    Cells(Vec<Vec<String>>),
    Json(Arc<Mutex<JsonTable>>),
}

impl Rows {
    pub fn len(&self) -> usize {
        match self {
            Self::Cells(rows) => rows.len(),
            Self::Json(table) => table.lock().map_or(0, |t| t.len()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn cell(&self, row: usize, column: usize) -> String {
        match self {
            Self::Cells(rows) => rows
                .get(row)
                .and_then(|r| r.get(column))
                .cloned()
                .unwrap_or_default(),
            Self::Json(table) => table
                .lock()
                .map_or_else(|_| String::new(), |t| t.cell(row, column)),
        }
    }
}

/// What the user did in a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    /// A heatmap cell was clicked.
    HeatmapRow { window: WindowId, row: usize },
    /// A graph node was selected (`None`: the selection was cleared).
    NodeSelected {
        window: WindowId,
        node: Option<usize>,
    },
    /// A sortable table column header was clicked.
    Sort {
        window: WindowId,
        slot: String,
        column: usize,
        descending: bool,
    },
    /// The window was closed.
    Closed(WindowId),
}

/// The window system, as the views use it. Slot names come from the
/// view's layout ([`crate::layout`]).
pub trait Surface {
    /// Open a window of `kind`.
    fn open(&mut self, kind: ViewKind, title: &str) -> Result<WindowId, String>;
    /// Set a label's text.
    fn label(&mut self, window: WindowId, slot: &str, text: &str);
    /// A sparkline of `values` (oldest first).
    fn sparkline(&mut self, window: WindowId, slot: &str, values: &[f32]);
    /// A donut with `center` text in its hole.
    fn donut(&mut self, window: WindowId, slot: &str, slices: &[Slice], center: &str);
    /// A gauge at `percent` (0..100) with a caption.
    fn gauge(&mut self, window: WindowId, slot: &str, percent: f64, caption: &str);
    /// A heatmap: `cells[row][column]`.
    fn heatmap(
        &mut self,
        window: WindowId,
        slot: &str,
        rows: &[String],
        columns: &[String],
        cells: &[Vec<f32>],
    );
    /// A plot of `series` with axis labels.
    fn plot(
        &mut self,
        window: WindowId,
        slot: &str,
        x_label: &str,
        y_label: &str,
        series: &[Series],
    );
    /// A table; `sortable` columns report header clicks as [`UiEvent::Sort`].
    fn table(
        &mut self,
        window: WindowId,
        slot: &str,
        columns: &[String],
        rows: Rows,
        sortable: bool,
    );
    /// A read-only text view with syntax highlighting for `language`.
    fn text(&mut self, window: WindowId, slot: &str, text: &str, language: &str);
    /// A node graph.
    fn graph(&mut self, window: WindowId, slot: &str, nodes: &[GraphNode], links: &[GraphLink]);
    /// Whether the window is still open.
    fn is_open(&self, window: WindowId) -> bool;
    /// What the user did since the last call.
    fn events(&mut self) -> Vec<UiEvent>;
}
