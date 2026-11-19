//! The window of each view (design D5): an XML skeleton compiled into the
//! binary, whose named widgets the code fills. With
//! `RUSTMAN_VIZ_DEV_LAYOUTS=<dir>` the layouts are read from that folder
//! instead, so they can be edited while the companion runs (a development
//! aid, not a user feature).

use rustman_viz_protocol::View;

/// The development override of the embedded layouts.
pub const DEV_LAYOUTS_VAR: &str = "RUSTMAN_VIZ_DEV_LAYOUTS";

/// The views of the companion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewKind {
    Run,
    Exploration,
    Graph,
    Inspector,
}

impl ViewKind {
    pub const ALL: [Self; 4] = [Self::Run, Self::Exploration, Self::Graph, Self::Inspector];

    pub fn from_view(view: View) -> Option<Self> {
        match view {
            View::RunDashboard => Some(Self::Run),
            View::ExplorationDashboard => Some(Self::Exploration),
            View::ApiGraph => Some(Self::Graph),
            View::ResponseInspector => Some(Self::Inspector),
            View::Unknown => None,
        }
    }

    /// The layout's file name.
    pub fn file(self) -> &'static str {
        match self {
            Self::Run => "run_dashboard.xml",
            Self::Exploration => "exploration_dashboard.xml",
            Self::Graph => "api_graph.xml",
            Self::Inspector => "response_inspector.xml",
        }
    }

    fn embedded(self) -> &'static str {
        match self {
            Self::Run => include_str!("../layouts/run_dashboard.xml"),
            Self::Exploration => include_str!("../layouts/exploration_dashboard.xml"),
            Self::Graph => include_str!("../layouts/api_graph.xml"),
            Self::Inspector => include_str!("../layouts/response_inspector.xml"),
        }
    }

    /// The layout: from [`DEV_LAYOUTS_VAR`] when set and readable, else the
    /// embedded one.
    pub fn xml(self) -> String {
        let dir = std::env::var_os(DEV_LAYOUTS_VAR);
        self.xml_from(dir.as_deref().map(std::path::Path::new))
    }

    /// The layout from `dir` when it holds this view's file, else the
    /// embedded one.
    pub fn xml_from(self, dir: Option<&std::path::Path>) -> String {
        dir.and_then(|dir| std::fs::read_to_string(dir.join(self.file())).ok())
            .unwrap_or_else(|| self.embedded().to_owned())
    }

    /// Every slot the code looks up in this view's layout.
    pub fn slots(self) -> &'static [&'static str] {
        match self {
            Self::Run => &[
                "title",
                "subtitle",
                "state",
                "kpi_requests",
                "kpi_pass_rate",
                "kpi_p50",
                "kpi_p95",
                "kpi_errors",
                "spark_requests",
                "spark_pass_rate",
                "spark_p50",
                "spark_p95",
                "spark_errors",
                "outcomes",
                "latency",
                "histogram",
                "requests",
            ],
            Self::Exploration => &[
                "title",
                "subtitle",
                "state",
                "heatmap",
                "coverage_operations",
                "coverage_statuses",
                "findings_heading",
                "findings",
            ],
            Self::Graph => &[
                "title",
                "subtitle",
                "graph",
                "selection_heading",
                "selection",
            ],
            Self::Inspector => &["title", "heading", "summary", "tabs", "table", "json"],
        }
    }
}

/// The KPI tiles of the run dashboard: (value label, sparkline slot).
pub const RUN_TILES: [(&str, &str); 5] = [
    ("kpi_requests", "spark_requests"),
    ("kpi_pass_rate", "spark_pass_rate"),
    ("kpi_p50", "spark_p50"),
    ("kpi_p95", "spark_p95"),
    ("kpi_errors", "spark_errors"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Task 3.5: every widget name the code looks up exists in its layout.
    #[test]
    fn every_slot_is_named_in_its_layout() {
        for kind in ViewKind::ALL {
            let xml = kind.embedded();
            assert!(xml.trim_start().starts_with("<!--") || xml.contains("<Window"));
            for slot in kind.slots() {
                assert_eq!(
                    xml.matches(&format!("Name=\"{slot}\"")).count(),
                    1,
                    "{}: slot {slot} must be named exactly once",
                    kind.file()
                );
            }
        }
        for (label, spark) in RUN_TILES {
            assert!(ViewKind::Run.slots().contains(&label));
            assert!(ViewKind::Run.slots().contains(&spark));
        }
    }

    #[test]
    fn the_dev_override_reads_from_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("api_graph.xml"), "<Window Title=\"dev\"/>").unwrap();
        assert_eq!(
            ViewKind::Graph.xml_from(Some(dir.path())),
            "<Window Title=\"dev\"/>"
        );
        let run = ViewKind::Run.xml_from(Some(dir.path()));
        assert!(run.contains("kpi_requests"), "missing files fall back");
    }
}
