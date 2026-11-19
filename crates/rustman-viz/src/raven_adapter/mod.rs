//! The Raven adapter (design D4): the only module with `unsafe`. It
//! implements [`Surface`] with the Raven C API. Every call is made on the
//! thread that called [`init`] (the main thread); the callbacks Raven makes
//! run on that thread too, during `Rwait_frame`.
//!
//! Ownership: a window's widgets belong to the window, which is destroyed
//! in [`Drop`]. The data a callback reads (a table's rows, the event queue)
//! is boxed and kept in the window, so it outlives the widgets that point
//! to it; Raven never frees it.

#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::rc::Rc;

use raven::*;

use rustman_viz::layout::ViewKind;
use rustman_viz::surface::{GraphLink, GraphNode, Rows, Series, Slice, Surface, UiEvent, WindowId};

/// How Raven draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The platform's GPU backend: Metal on macOS, Vulkan elsewhere.
    Default,
    /// No window system and no GPU (tests and CI).
    Headless,
}

/// Start Raven. `Err` when the graphics cannot start.
pub fn init(backend: Backend) -> Result<(), String> {
    // SAFETY: RInitDesc is a plain C struct for which all-zero is the
    // documented default of every field.
    let mut desc: RInitDesc = unsafe { std::mem::zeroed() };
    let app_name = cstr("rustman-viz");
    desc.app_name = app_name.as_ptr();
    if backend == Backend::Headless {
        desc.backend = R_BACKEND_NULL;
        desc.headless = 1;
    }
    // SAFETY: `desc` and the name it points to live through the call.
    if unsafe { Rinit_ex(&desc) } == 0 {
        return Err("Raven could not start its graphics backend".into());
    }
    // SAFETY: Raven is initialised; called on the init thread.
    unsafe { Rtheme_follow_system(1) };
    Ok(())
}

/// Stop Raven. Every [`RavenSurface`] must be dropped first.
pub fn shutdown() {
    // SAFETY: called once, on the init thread, after the windows are gone.
    unsafe { Rshutdown() };
}

/// Wait up to `timeout` seconds for events and draw a frame. `false` once
/// the application was asked to quit.
pub fn wait_frame(timeout: f64) -> bool {
    // SAFETY: on the init thread, between init and shutdown.
    unsafe { Rwait_frame(timeout) != 0 }
}

/// Wake [`wait_frame`] from another thread (the reader).
pub fn wake() {
    // SAFETY: Rwake_up is the one call documented as safe from any thread.
    unsafe { Rwake_up() };
}

/// The events callbacks push, shared by every window.
type Events = Rc<RefCell<Vec<UiEvent>>>;

/// What a callback needs: where to report, which window and slot it is,
/// and for a graph, its node handles.
struct Context {
    events: Events,
    window: WindowId,
    slot: String,
    /// The window's GraphState (graphs only; null otherwise).
    graph: *const GraphState,
}

/// A table's rows and the text of the last cell Raven asked for (valid
/// until it asks again).
struct TableSource {
    rows: Rows,
    cell: CString,
}

/// The node handles of a graph, in model order, and what was drawn.
#[derive(Default)]
struct GraphState {
    handles: Vec<c_int>,
    drawn: Option<(Vec<GraphNode>, Vec<GraphLink>)>,
}

struct Window {
    id: Rid,
    kind: ViewKind,
    /// Widgets created in slots, by slot name.
    widgets: HashMap<String, Rid>,
    /// Kept alive while the window lives: Raven holds pointers to them.
    /// Boxed so that each keeps its address when the vector grows.
    #[allow(clippy::vec_box)]
    contexts: Vec<Box<Context>>,
    tables: HashMap<String, Box<TableSource>>,
    graph: Box<GraphState>,
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window id came from Rwindow_create_from_xml; a window
        // the user closed is no longer valid and is not destroyed twice.
        unsafe {
            if Rwindow_is_valid(self.id) != 0 {
                Rwindow_destroy(self.id);
            }
        }
    }
}

/// The windows of the companion.
pub struct RavenSurface {
    windows: Vec<Option<Window>>,
    events: Events,
}

impl Default for RavenSurface {
    fn default() -> Self {
        Self::new()
    }
}

/// A C string for Raven (a NUL inside the text ends it).
fn cstr(text: &str) -> CString {
    raven::cstr(text)
}

unsafe extern "C" fn on_close(_window: Rid, user_data: *mut c_void) -> c_int {
    // SAFETY: user_data is the Context boxed in this window, alive until
    // the window is dropped (after Raven stops calling back).
    let context = unsafe { &*(user_data as *const Context) };
    context
        .events
        .borrow_mut()
        .push(UiEvent::Closed(context.window));
    1 // let the window close
}

unsafe extern "C" fn on_cell(
    _chart: Rid,
    row: c_int,
    _column: c_int,
    _value: f32,
    user_data: *mut c_void,
) {
    // SAFETY: as in `on_close`.
    let context = unsafe { &*(user_data as *const Context) };
    if let Ok(row) = usize::try_from(row) {
        let event = UiEvent::HeatmapRow {
            window: context.window,
            row,
        };
        context.events.borrow_mut().push(event);
    }
}

unsafe extern "C" fn on_sort(
    _table: Rid,
    column: c_int,
    descending: c_int,
    user_data: *mut c_void,
) {
    // SAFETY: as in `on_close`.
    let context = unsafe { &*(user_data as *const Context) };
    if let Ok(column) = usize::try_from(column) {
        context.events.borrow_mut().push(UiEvent::Sort {
            window: context.window,
            slot: context.slot.clone(),
            column,
            descending: descending != 0,
        });
    }
}

unsafe extern "C" fn on_select(_graph: Rid, item: c_int, user_data: *mut c_void) {
    // SAFETY: as in `on_close`; `context.graph` points to the window's
    // boxed GraphState, which lives as long as the context.
    let context = unsafe { &*(user_data as *const Context) };
    let handles = unsafe { &(*context.graph).handles };
    let node = (item != 0)
        .then(|| handles.iter().position(|h| *h == item))
        .flatten();
    context.events.borrow_mut().push(UiEvent::NodeSelected {
        window: context.window,
        node,
    });
}

unsafe extern "C" fn table_cell(
    _table: Rid,
    row: c_int,
    column: c_int,
    user_data: *mut c_void,
) -> *const c_char {
    // SAFETY: user_data is the window's boxed TableSource for this table,
    // replaced only after the table is given the new one; Raven calls this
    // on the UI thread, one cell at a time, and copies the text before the
    // next call.
    let source = unsafe { &mut *(user_data as *mut TableSource) };
    let (Ok(row), Ok(column)) = (usize::try_from(row), usize::try_from(column)) else {
        return std::ptr::null();
    };
    source.cell = cstr(&source.rows.cell(row, column));
    source.cell.as_ptr()
}

/// A column's width in logical units, from its header and the first rows
/// (about 8 units a character, between 70 and 520).
fn column_width(header: &str, rows: &Rows, column: usize) -> f32 {
    let widest = (0..rows.len().min(50))
        .map(|row| rows.cell(row, column).chars().count())
        .chain(std::iter::once(header.chars().count()))
        .max()
        .unwrap_or(0);
    (widest as f32 * 8.5 + 24.0).clamp(70.0, 520.0)
}

impl RavenSurface {
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            events: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn window(&mut self, window: WindowId) -> Option<&mut Window> {
        self.windows.get_mut(window).and_then(Option::as_mut)
    }

    /// The widget named `slot` in the layout.
    fn named(&mut self, window: WindowId, slot: &str) -> Option<Rid> {
        let w = self.window(window)?;
        let id = raven::widget_find(w.id, slot);
        (id != R_INVALID_ID).then_some(id)
    }

    /// The widget the code created in `slot`, creating it with `make` the
    /// first time.
    fn child(&mut self, window: WindowId, slot: &str, make: impl FnOnce() -> Rid) -> Option<Rid> {
        if let Some(id) = self
            .window(window)
            .and_then(|w| w.widgets.get(slot).copied())
        {
            return Some(id);
        }
        let parent = self.named(window, slot)?;
        let child = make();
        if child == R_INVALID_ID {
            return None;
        }
        // SAFETY: both ids are live widgets of this window, on the UI thread.
        unsafe {
            Rwidget_add_child(parent, child);
            Rslot_set_fill(child, 1.0);
        }
        self.window(window)?.widgets.insert(slot.to_owned(), child);
        Some(child)
    }

    fn context(&mut self, window: WindowId, slot: &str) -> *mut c_void {
        let graph = self
            .window(window)
            .map_or(std::ptr::null(), |w| &*w.graph as *const GraphState);
        let context = Box::new(Context {
            events: Rc::clone(&self.events),
            window,
            slot: slot.to_owned(),
            graph,
        });
        let pointer = &*context as *const Context as *mut c_void;
        if let Some(w) = self.window(window) {
            w.contexts.push(context);
        }
        pointer
    }

    fn chart(&mut self, window: WindowId, slot: &str, kind: RChartKind) -> Option<Rid> {
        // SAFETY: creating a widget on the UI thread.
        self.child(window, slot, || unsafe { Rchart_create(kind) })
    }

    /// Save a picture of `window` as PNG. Needs a GPU backend.
    pub fn capture(&mut self, window: WindowId, path: &str) -> Result<(), String> {
        let w = self.window(window).ok_or("no such window")?;
        let (mut width, mut height) = (0, 0);
        // SAFETY: a live window. Raven returns width × height × 4 bytes,
        // copied here, then freed with Rpixels_free as documented.
        let rgba = unsafe {
            let pixels = Rwindow_capture(w.id, &mut width, &mut height);
            if pixels.is_null() {
                return Err("this backend cannot capture windows".into());
            }
            let len =
                usize::try_from(width).unwrap_or(0) * usize::try_from(height).unwrap_or(0) * 4;
            let copy = std::slice::from_raw_parts(pixels, len).to_vec();
            Rpixels_free(pixels);
            copy
        };
        let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
        let (width, height) = (
            u32::try_from(width).unwrap_or(0),
            u32::try_from(height).unwrap_or(0),
        );
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&rgba))
            .map_err(|e| format!("{path}: {e}"))
    }

    /// Open windows, for `--capture`.
    pub fn open_windows(&self) -> Vec<(WindowId, ViewKind)> {
        self.windows
            .iter()
            .enumerate()
            .filter_map(|(i, w)| w.as_ref().map(|w| (i, w.kind)))
            .collect()
    }
}

impl Surface for RavenSurface {
    fn open(&mut self, kind: ViewKind, title: &str) -> Result<WindowId, String> {
        let id = raven::window_from_xml(&kind.xml());
        if id == R_INVALID_ID {
            return Err(format!("the {} layout did not load", kind.file()));
        }
        let missing: Vec<&str> = kind
            .slots()
            .iter()
            .copied()
            .filter(|slot| raven::widget_find(id, slot) == R_INVALID_ID)
            .collect();
        if !missing.is_empty() {
            // SAFETY: the window was just created.
            unsafe { Rwindow_destroy(id) };
            return Err(format!("{} lacks {}", kind.file(), missing.join(", ")));
        }
        let index = self.windows.len();
        self.windows.push(Some(Window {
            id,
            kind,
            widgets: HashMap::new(),
            contexts: Vec::new(),
            tables: HashMap::new(),
            graph: Box::default(),
        }));
        let caption = cstr(&format!("Rustman · {title}"));
        let context = self.context(index, "");
        // SAFETY: a live window; `context` is boxed in it.
        unsafe {
            Rwindow_set_title(id, caption.as_ptr());
            Rwindow_set_close_callback(id, Some(on_close), context);
        }
        Ok(index)
    }

    fn label(&mut self, window: WindowId, slot: &str, text: &str) {
        if let Some(label) = self.named(window, slot) {
            let text = cstr(text);
            // SAFETY: a live label; the text lives through the call.
            unsafe { Rlabel_set_text(label, text.as_ptr()) };
        }
    }

    fn sparkline(&mut self, window: WindowId, slot: &str, values: &[f32]) {
        if let Some(chart) = self.chart(window, slot, R_CHART_SPARKLINE) {
            let count = c_int::try_from(values.len()).unwrap_or(c_int::MAX);
            // SAFETY: Raven copies `count` values from the slice.
            unsafe { Rchart_set_values(chart, values.as_ptr(), count) };
        }
    }

    fn donut(&mut self, window: WindowId, slot: &str, slices: &[Slice], center: &str) {
        let Some(chart) = self.chart(window, slot, R_CHART_DONUT) else {
            return;
        };
        let center = cstr(center);
        let caption = cstr("requests");
        // SAFETY: a live chart; texts live through each call.
        unsafe {
            Rchart_clear(chart);
            for slice in slices {
                let label = cstr(&slice.label);
                let index = Rchart_add_slice(chart, label.as_ptr(), slice.value);
                let [r, g, b] = slice.rgb;
                Rchart_set_slice_color(chart, index, r, g, b, 1.0);
            }
            Rchart_set_center_text(chart, center.as_ptr(), caption.as_ptr());
        }
    }

    fn gauge(&mut self, window: WindowId, slot: &str, percent: f64, caption: &str) {
        let Some(chart) = self.chart(window, slot, R_CHART_GAUGE) else {
            return;
        };
        let format = cstr("%.0f%%");
        let caption = cstr(caption);
        // SAFETY: a live chart; texts live through each call.
        unsafe {
            Rchart_set_range(chart, 0.0, 100.0);
            Rchart_set_format(chart, format.as_ptr());
            Rchart_set_value(chart, percent);
            Rchart_set_title(chart, caption.as_ptr());
        }
    }

    fn heatmap(
        &mut self,
        window: WindowId,
        slot: &str,
        rows: &[String],
        columns: &[String],
        cells: &[Vec<f32>],
    ) {
        let first = !self
            .window(window)
            .is_some_and(|w| w.widgets.contains_key(slot));
        let Some(chart) = self.chart(window, slot, R_CHART_HEATMAP) else {
            return;
        };
        let flat: Vec<f32> = cells
            .iter()
            .flat_map(|row| (0..columns.len()).map(move |c| row.get(c).copied().unwrap_or(0.0)))
            .collect();
        let row_labels = cstr(
            &rows
                .iter()
                .map(|r| r.replace('|', "/"))
                .collect::<Vec<_>>()
                .join("|"),
        );
        let column_labels = cstr(&columns.join("|"));
        let (n_rows, n_columns) = (
            c_int::try_from(rows.len()).unwrap_or(0),
            c_int::try_from(columns.len()).unwrap_or(0),
        );
        let context = first.then(|| self.context(window, slot));
        // SAFETY: a live chart; Raven copies rows × columns values and the
        // labels; the click context is boxed in the window.
        unsafe {
            Rchart_set_grid(chart, n_rows, n_columns, flat.as_ptr());
            Rchart_set_row_labels(chart, row_labels.as_ptr());
            Rchart_set_column_labels(chart, column_labels.as_ptr());
            if let Some(context) = context {
                Rchart_set_cell_clicked_callback(chart, Some(on_cell), context);
            }
        }
    }

    fn plot(
        &mut self,
        window: WindowId,
        slot: &str,
        x_label: &str,
        y_label: &str,
        series: &[Series],
    ) {
        // SAFETY: creating a widget on the UI thread.
        let Some(plot) = self.child(window, slot, || unsafe { Rwidget_create(R_WIDGET_PLOT) })
        else {
            return;
        };
        let (x_label, y_label) = (cstr(x_label), cstr(y_label));
        // SAFETY: a live plot; Raven copies the points and the texts.
        unsafe {
            Rplot_clear(plot);
            Rplot_set_axis_label(plot, R_PLOT_X, x_label.as_ptr());
            Rplot_set_axis_label(plot, R_PLOT_Y, y_label.as_ptr());
            for s in series {
                let name = cstr(&s.name);
                let index = Rplot_add_series(plot, name.as_ptr());
                let xs: Vec<f64> = s.points.iter().map(|p| p.0).collect();
                let ys: Vec<f64> = s.points.iter().map(|p| p.1).collect();
                let count = c_int::try_from(xs.len()).unwrap_or(c_int::MAX);
                Rplot_set_series_data(plot, index, xs.as_ptr(), ys.as_ptr(), count);
                if s.step {
                    // A histogram: bucket bounds grow geometrically.
                    Rplot_set_series_step(plot, index, 1);
                    Rplot_set_series_fill(plot, index, 0.35);
                    Rplot_set_axis_scale(plot, R_PLOT_X, R_PLOT_SCALE_LOG);
                }
            }
            Rplot_fit(plot);
        }
    }

    fn table(
        &mut self,
        window: WindowId,
        slot: &str,
        columns: &[String],
        rows: Rows,
        sortable: bool,
    ) {
        let first = !self
            .window(window)
            .is_some_and(|w| w.widgets.contains_key(slot));
        // SAFETY: creating a widget on the UI thread.
        let Some(table) = self.child(window, slot, || unsafe { Rwidget_create(R_WIDGET_TABLE) })
        else {
            return;
        };
        let count = c_int::try_from(rows.len()).unwrap_or(c_int::MAX);
        let mut source = Box::new(TableSource {
            rows,
            cell: CString::default(),
        });
        let pointer = &mut *source as *mut TableSource as *mut c_void;
        let sort_context = (first && sortable).then(|| self.context(window, slot));
        // SAFETY: a live table. The new source is handed over before the
        // old one is dropped (below), so Raven never reads a freed source.
        unsafe {
            Rtable_clear_columns(table);
            for (i, column) in columns.iter().enumerate() {
                let header = cstr(column);
                let width = column_width(column, &source.rows, i);
                let index = Rtable_add_column(table, header.as_ptr(), width);
                if sortable {
                    Rtable_set_column_sortable(table, index, 1);
                }
            }
            Rtable_set_striped(table, 1);
            Rtable_set_virtual_rows(table, count, Some(table_cell), pointer);
            if let Some(context) = sort_context {
                Rtable_set_sort_callback(table, Some(on_sort), context);
            }
        }
        if let Some(w) = self.window(window) {
            w.tables.insert(slot.to_owned(), source);
        }
    }

    fn text(&mut self, window: WindowId, slot: &str, text: &str, language: &str) {
        let text = cstr(text);
        let language = cstr(language);
        let empty = cstr("");
        // SAFETY: creating a widget on the UI thread.
        let Some(editor) = self.child(window, slot, || unsafe {
            Rtext_editor_create(empty.as_ptr())
        }) else {
            return;
        };
        // SAFETY: a live editor; Raven copies the texts.
        unsafe {
            Rtext_editor_set_language(editor, language.as_ptr());
            Rtext_editor_set_text(editor, text.as_ptr());
            Rtext_editor_set_read_only(editor, 1);
        }
    }

    fn graph(&mut self, window: WindowId, slot: &str, nodes: &[GraphNode], links: &[GraphLink]) {
        let first = !self
            .window(window)
            .is_some_and(|w| w.widgets.contains_key(slot));
        // SAFETY: creating a widget on the UI thread.
        let Some(graph) = self.child(window, slot, || unsafe { Rnode_graph_create() }) else {
            return;
        };
        let Some(w) = self.window(window) else {
            return;
        };
        let wanted = (nodes.to_vec(), links.to_vec());
        if w.graph.drawn.as_ref() == Some(&wanted) {
            return; // unchanged: keep the user's selection and view
        }
        let mut handles = Vec::with_capacity(nodes.len());
        let mut outputs = Vec::with_capacity(nodes.len());
        let mut inputs = Vec::with_capacity(nodes.len());
        let (out_label, in_label) = (cstr("provides"), cstr("uses"));
        // SAFETY: a live graph; Raven copies the titles and labels.
        unsafe {
            Rnode_graph_clear(graph);
            for node in nodes {
                let title = cstr(&node.title);
                let handle = Rnode_graph_add_node(graph, title.as_ptr(), node.x, node.y);
                inputs.push(Rnode_graph_add_input(
                    graph,
                    handle,
                    in_label.as_ptr(),
                    std::ptr::null(),
                ));
                outputs.push(Rnode_graph_add_output(
                    graph,
                    handle,
                    out_label.as_ptr(),
                    std::ptr::null(),
                ));
                let [r, g, b] = node.rgb;
                Rnode_graph_set_node_color(graph, handle, r, g, b);
                handles.push(handle);
            }
            for link in links {
                if let (Some(out), Some(input)) = (outputs.get(link.from), inputs.get(link.to)) {
                    Rnode_graph_connect(graph, *out, *input);
                }
            }
            Rnode_graph_frame_all(graph);
        }
        w.graph.handles = handles;
        w.graph.drawn = Some(wanted);
        if first {
            let context = self.context(window, slot);
            // SAFETY: a live graph; the context is boxed in the window.
            unsafe { Rnode_graph_set_selection_callback(graph, Some(on_select), context) };
        }
    }

    fn is_open(&self, window: WindowId) -> bool {
        self.windows
            .get(window)
            .and_then(Option::as_ref)
            // SAFETY: querying a window id on the UI thread.
            .is_some_and(|w| unsafe { Rwindow_is_valid(w.id) != 0 })
    }

    fn events(&mut self) -> Vec<UiEvent> {
        let events = std::mem::take(&mut *self.events.borrow_mut());
        for event in &events {
            if let UiEvent::Closed(window) = event
                && let Some(slot) = self.windows.get_mut(*window)
            {
                *slot = None;
            }
        }
        events
    }
}
