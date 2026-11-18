//! The response viewer drawn from fixture exchanges (tasks 6.1–6.7) and the
//! cancel scenario on a mocked clock (task 5.2).

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustman_core::assertions::{AssertionResult, Outcome};
use rustman_core::run::{RequestRecord, RequestStatus};
use tempfile::NamedTempFile;

use super::received::fixtures::{received, record};
use super::{BodyKind, Captured, RawBody, Received, ResponseTab, Source};
use crate::app::{Action, Effect, Overlay, Screen, Session, State, update};
use crate::config::{Overrides, TuiConfig, UiOptions};
use crate::focus::Focus;
use crate::keymap::chord::{Code, parse_chord};
use crate::snapshot::{buffer_text, render, style_of};
use crate::theme::{ColorMode, Token};

fn state(mode: ColorMode, width: u16, height: u16) -> State {
    let ui = UiOptions::resolve(&TuiConfig::default(), Overrides::default(), mode);
    let mut state = State::with_screen(Screen::Shell(Session::Scratch)).with_ui(ui);
    update(&mut state, Action::Resize { width, height });
    crate::workbench::new_tab(&mut state);
    state.shell.focus = Focus::Response;
    state
}

fn show(state: &mut State, received: Received) {
    state.tabs.active_mut().unwrap().show(received);
}

fn text(state: &State) -> String {
    buffer_text(&render(state, state.area.width, state.area.height))
}

/// The response pane's content lines (inside its borders), trimmed.
fn pane(state: &State) -> String {
    let screen = text(state);
    let first: Vec<char> = screen.lines().next().unwrap_or_default().chars().collect();
    let column = first.iter().rposition(|c| *c == '┌').unwrap_or(0);
    screen
        .lines()
        .skip(1)
        .map(|line| {
            let chars: Vec<char> = line.chars().collect();
            let inner: String = chars.get(column + 1..).unwrap_or_default().iter().collect();
            inner.trim_end().trim_end_matches('│').trim().to_owned()
        })
        .take_while(|line| !line.starts_with('─'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
}

fn press(state: &mut State, keys: &str) -> Vec<Effect> {
    let mut effects = Vec::new();
    for key in keys.split(' ') {
        let chord = parse_chord(key).unwrap();
        effects.extend(crate::commands::shell_key(state, chord));
    }
    effects
}

fn assertion(label: &str, passed: bool, expected: &str, actual: &str) -> AssertionResult {
    AssertionResult {
        kind: "status",
        outcome: if passed { Outcome::Pass } else { Outcome::Fail },
        label: label.into(),
        expected: Some(expected.into()),
        actual: Some(actual.into()),
        message: String::new(),
    }
}

fn summary(state: &State) -> String {
    pane(state).lines().next().unwrap_or_default().to_owned()
}

#[test]
fn summary_lines_by_status_class_and_transport_failure() {
    // Scenarios "Successful exchange" and "DNS failure".
    let mut state = state(ColorMode::TrueColor, 160, 30);
    let mut lines = Vec::new();
    let mut created = received(201, "application/json", &[b' '; 1229]);
    created.record.assertions = vec![assertion("status == 201", true, "201", "201"); 3];
    show(&mut state, created);
    lines.push(summary(&state));
    let buffer = render(&state, 160, 30);
    let ok = style_of(&buffer, "201 Created").unwrap();
    assert_eq!(ok.fg, state.ui.theme.fg(Token::Ok).fg);
    for (status, token) in [(404, Token::Warn), (500, Token::Error)] {
        show(&mut state, received(status, "text/plain", b"nope"));
        lines.push(summary(&state));
        let buffer = render(&state, 160, 30);
        let style = style_of(&buffer, &status.to_string()).unwrap();
        assert_eq!(style.fg, state.ui.theme.fg(token).fg);
    }
    let failed = RequestRecord::new("a".into(), "a".into(), "a.rman.yaml".into())
        .errored("dns: cannot resolve nope.invalid: failed to lookup address information".into());
    show(&mut state, Received::without_body(failed));
    lines.push(summary(&state));
    assert_eq!(
        lines[0], "✔ 201 Created · 84 ms · 1.2 KiB · 3/3 passed",
        "{lines:?}"
    );
    assert!(
        lines[3].starts_with("✖ DNS error · cannot resolve nope.invalid"),
        "{lines:?}"
    );
    insta::assert_snapshot!("summary_lines", lines.join("\n"));
}

#[test]
fn no_color_uses_symbols_and_insecure_is_marked() {
    // Scenario "NO_COLOR" and the insecure marker of "Insecure TLS
    // indicator".
    let mut state = state(ColorMode::NoColor, 160, 30);
    let mut r = received(500, "text/plain", b"boom");
    r.record.exchange.as_mut().unwrap().insecure = true;
    show(&mut state, r);
    let line = summary(&state);
    assert!(line.starts_with("✖ 500 Internal Server Error"), "{line}");
    assert!(line.ends_with("TLS: INSECURE"), "{line}");
    let buffer = render(&state, 160, 30);
    let fg = style_of(&buffer, "500").unwrap().fg;
    assert!(
        matches!(fg, None | Some(ratatui::style::Color::Reset)),
        "{fg:?}"
    );
}

#[test]
fn headers_keep_duplicates_and_assertions_show_expected_and_actual() {
    // Scenarios "Duplicate response headers" and "Failed assertion details".
    let mut state = state(ColorMode::TrueColor, 160, 30);
    let mut r = received(404, "application/json", b"{}");
    r.record = record(
        404,
        &[
            ("Content-Type", "application/json"),
            ("Set-Cookie", "a=1"),
            ("Set-Cookie", "b=2"),
        ],
        2,
    );
    r.record.status = RequestStatus::Failed;
    r.record.assertions = vec![assertion("status == 200", false, "200", "404")];
    r.captured = vec![
        Captured {
            var: "id".into(),
            value: Some("42".into()),
            secret: false,
            message: None,
        },
        Captured {
            var: "token".into(),
            value: None,
            secret: true,
            message: None,
        },
    ];
    show(&mut state, r);
    press(&mut state, "]");
    let headers = pane(&state);
    assert_eq!(headers.matches("Set-Cookie:").count(), 2, "{headers}");
    assert!(headers.contains("id = 42") && headers.contains("token = ••••••••"));
    press(&mut state, "] ]");
    let assertions = pane(&state);
    assert!(
        assertions.contains("✖ failed status == 200 · expected 200 · actual 404"),
        "{assertions}"
    );
    insta::assert_snapshot!("headers_and_assertions", format!("{headers}\n{assertions}"));
}

#[test]
fn xml_is_highlighted_and_png_is_a_hex_dump_with_save() {
    // Scenarios "XML response" and "Image response".
    let mut state = state(ColorMode::TrueColor, 160, 30);
    show(
        &mut state,
        received(
            200,
            "application/xml",
            b"<customer id=\"42\">Ana</customer>\n",
        ),
    );
    let buffer = render(&state, 160, 30);
    let tag = style_of(&buffer, "customer").unwrap();
    assert_eq!(tag.fg, state.ui.theme.fg(Token::Accent).fg);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.resize(300 * 1024, 0);
    show(&mut state, received(200, "image/png", &png));
    let screen = pane(&state);
    assert!(screen.contains("Binary body · 300 KiB"), "{screen}");
    assert!(screen.contains("Save body…"), "{screen}");
    assert!(
        screen.contains("00000000  89 50 4e 47 0d 0a 1a 0a"),
        "{screen}"
    );
    press(&mut state, "s");
    let Some(Overlay::Input(input)) = &state.overlay else {
        panic!("expected the save prompt");
    };
    assert_eq!(input.text.text(), "response.png");
    let effects = press(&mut state, "Enter");
    assert!(
        matches!(effects.as_slice(), [Effect::SaveBody { path, .. }] if path.ends_with("response.png")),
        "{effects:?}"
    );
}

#[test]
fn an_80_mib_json_body_is_paged_with_a_notice() {
    // Scenario "80 MiB JSON response": a sparse spool file stands in for
    // the body; only its first page is read.
    let mut spool = NamedTempFile::new().unwrap();
    spool.write_all(b"{\"items\": [\n  1,\n  2,\n").unwrap();
    let size = 80 * 1024 * 1024;
    spool.as_file().set_len(size).unwrap();
    let source = Source::Spool(Arc::new(spool));
    let r = Received::with_body(
        record(200, &[("Content-Type", "application/json")], size),
        RawBody {
            source: source.clone(),
            size,
            binary: false,
            content_type: Some("application/json".into()),
        },
    );
    assert!(matches!(r.body, BodyKind::Paged));
    let mut state = state(ColorMode::TrueColor, 160, 30);
    show(&mut state, r);
    let screen = pane(&state);
    assert!(
        screen.contains("The tree view is disabled above 50 MiB"),
        "{screen}"
    );
    assert!(screen.contains("Page 1 of 1280 · loading…"), "{screen}");
    let tab = state.tabs.active().unwrap().id;
    let lines = super::received::read_page(&source, 0).unwrap();
    update(
        &mut state,
        Action::PageRead {
            tab,
            page: 0,
            result: Ok(lines),
        },
    );
    let screen = pane(&state);
    assert!(screen.contains("{\"items\": ["), "{screen}");
    let effects = press(&mut state, "End PageDown");
    assert!(
        matches!(effects.as_slice(), [Effect::ReadPage { page: 1, .. }]),
        "{effects:?}"
    );
}

#[test]
fn the_waterfall_for_fresh_and_reused_connections() {
    // Scenario "Reused connection".
    let mut state = state(ColorMode::TrueColor, 160, 30);
    show(&mut state, received(200, "text/plain", b"ok"));
    press(&mut state, "] ]");
    let fresh = pane(&state);
    let mut reused = received(200, "text/plain", b"ok");
    let timings = &mut reused.record.exchange.as_mut().unwrap().timings;
    (timings.dns_ms, timings.connect_ms, timings.tls_ms) = (None, None, None);
    show(&mut state, reused);
    state
        .tabs
        .active_mut()
        .unwrap()
        .viewer
        .set_tab(ResponseTab::Timing);
    let reused = pane(&state);
    for phase in ["DNS", "Connect", "TLS"] {
        let row = reused.lines().find(|l| l.contains(phase)).unwrap();
        assert!(row.contains('—'), "{row}");
    }
    let waiting = reused.lines().find(|l| l.contains("Waiting")).unwrap();
    assert!(waiting.contains("70 ms"), "{waiting}");
    assert!(reused.contains("not enough history"));
    insta::assert_snapshot!("waterfall_fresh", fresh);
    insta::assert_snapshot!("waterfall_reused", reused);
}

#[test]
fn the_sparkline_shows_thirty_points_with_stats() {
    // Scenario "Sparkline with history".
    let mut state = state(ColorMode::TrueColor, 160, 30);
    let mut r = received(200, "text/plain", b"ok");
    r.history = (1..=35).map(f64::from).collect();
    show(&mut state, r);
    state
        .tabs
        .active_mut()
        .unwrap()
        .viewer
        .set_tab(ResponseTab::Timing);
    let screen = pane(&state);
    assert!(screen.contains("min 6.0 ms · median 20 ms\n"), "{screen}");
    assert!(
        screen.contains("p95 34 ms · max 35 ms · 30 points"),
        "{screen}"
    );
    let spark = screen.lines().find(|l| l.contains('▁')).unwrap();
    assert_eq!(
        spark.chars().filter(|c| "▁▂▃▄▅▆▇█".contains(*c)).count(),
        30
    );
    assert!(spark.starts_with('▁') && spark.ends_with('█'), "{spark}");
    let buffer = render(&state, 160, 30);
    let newest = style_of(&buffer, "█").unwrap();
    assert_eq!(newest.fg, state.ui.theme.fg(Token::Accent).fg);
}

#[test]
fn json_keys_fold_filter_search_and_copy() {
    let mut state = state(ColorMode::TrueColor, 160, 30);
    let items: Vec<String> = (0..1000).map(|i| format!("{{\"id\":{i}}}")).collect();
    let body = format!("{{\"user\":{{\"id\":42}},\"items\":[{}]}}", items.join(","));
    show(
        &mut state,
        received(200, "application/json", body.as_bytes()),
    );
    press(&mut state, "Down Down Down Enter");
    assert!(pane(&state).contains("▸ items: [1000 items]"));
    press(&mut state, "Up");
    let effects = press(&mut state, "y");
    assert_eq!(effects, [Effect::Copy("42".into())]);
    update(
        &mut state,
        Action::Copied {
            text: "42".into(),
            tool: None,
        },
    );
    assert_eq!(state.clipboard.as_deref(), Some("42"), "OSC 52 fallback");
    let toast = state.notifications.visible().next().unwrap();
    assert!(toast.message.starts_with("Copied 42"), "{}", toast.message);
    let later = state.now + Duration::from_secs(11);
    update(&mut state, Action::Tick(later));
    let effects = press(&mut state, "f $ . u s e r Enter");
    assert!(
        matches!(effects.as_slice(), [Effect::Filter { expr, .. }] if expr == "$.user"),
        "{effects:?}"
    );
    press(&mut state, "f $ [ Enter");
    let screen = pane(&state);
    assert!(screen.contains("JSONPath $[_"), "{screen}");
    assert!(screen.contains('✖'), "the error is inline: {screen}");
    press(&mut state, "Esc / 9 9 9");
    let screen = pane(&state);
    assert!(screen.contains("/999 · 1 of 1"), "{screen}");
    assert!(screen.contains("id: 999"), "{screen}");
    press(&mut state, "Esc z M");
    assert_eq!(
        state
            .tabs
            .active()
            .unwrap()
            .viewer
            .shown_tree()
            .unwrap()
            .rows(),
        3
    );
    press(&mut state, "z R r");
    assert!(pane(&state).contains("Raw · r shows the tree"));
}

#[test]
fn a_filter_shows_its_match_count_and_survives_an_invalid_expression() {
    // Scenarios "Filter with JSONPath" and "Invalid filter", on screen.
    let mut state = state(ColorMode::TrueColor, 160, 30);
    let body = r#"{"items":[{"id":1},{"id":2},{"id":3}],"name":"Ana"}"#;
    show(
        &mut state,
        received(200, "application/json", body.as_bytes()),
    );
    let effects = press(&mut state, "f $ . i t e m s [ * ] . i d Enter");
    let [effect @ Effect::Filter { .. }] = effects.as_slice() else {
        panic!("expected a filter effect, got {effects:?}");
    };
    update(&mut state, crate::effects::execute(effect.clone()));
    let screen = pane(&state);
    assert!(screen.contains("$.items[*].id 3 matches"), "{screen}");
    assert!(!screen.contains("Ana"), "only the matches show: {screen}");

    // `f` reopens the box with the current expression: clear it first.
    let clear = "Backspace ".repeat("$.items[*].id".len());
    let keys = format!("f {clear}$ [ Enter");
    assert!(press(&mut state, &keys).is_empty());
    let screen = pane(&state);
    assert!(screen.contains('✖'), "the error is inline: {screen}");
    assert!(
        screen.contains("3 matches"),
        "the previous filtered view stays: {screen}"
    );
}

#[test]
fn cancelling_after_three_seconds_says_so() {
    // Scenario "Cancel a hanging request", on a mocked clock.
    let mut state = state(ColorMode::TrueColor, 160, 30);
    let effects = press(&mut state, "Ctrl+R");
    let [Effect::Send(job)] = effects.as_slice() else {
        panic!("expected a send");
    };
    let token = job.cancel.clone();
    let t0 = state.now;
    update(&mut state, Action::Tick(t0 + Duration::from_millis(1500)));
    assert!(
        summary(&state).contains("Sending… 1.5 s"),
        "{}",
        summary(&state)
    );
    let screen = text(&state);
    assert!(screen.contains("⠴ untitled"), "the tab spins too: {screen}");
    assert_eq!(state.status.in_flight, 1);
    update(&mut state, Action::Tick(t0 + Duration::from_secs(3)));
    press(&mut state, "Esc");
    assert!(token.is_cancelled(), "the connection is dropped");
    assert_eq!(summary(&state), "Cancelled after 3.0 s");
    assert_eq!(state.status.in_flight, 0);
    // The engine's late answer for the cancelled send is ignored.
    let tab = state.tabs.active().unwrap().id;
    update(
        &mut state,
        Action::SendFinished {
            tab,
            seq: job.seq,
            received: Box::new(received(200, "text/plain", b"late")),
        },
    );
    assert_eq!(summary(&state), "Cancelled after 3.0 s");
    // Ctrl+C with nothing in flight keeps running and says how to quit.
    press(&mut state, "Ctrl+C");
    assert!(!state.quit);
    let toast = state.notifications.visible().next().unwrap();
    assert_eq!(toast.message, "Press Ctrl+Q to quit");
}

#[test]
fn scrolling_a_50_mib_body_stays_under_16_ms_per_frame() {
    // Task 6.3's benchmark: a JSON array of a million strings padded to
    // exactly 50 MiB, the largest body shown as a tree, classified (parsed
    // and pretty-printed) the way a sent exchange is, off the measured
    // frames. Only the rows on screen are built per frame.
    let rows = 1_000_000;
    let row = "x".repeat(49);
    let mut items = vec![row; rows];
    let size = rows * (49 + 3) + 1;
    let limit = usize::try_from(super::received::TREE_LIMIT).unwrap();
    items[rows - 1].push_str(&"x".repeat(limit - size));
    let body = serde_json::to_vec(&items).unwrap();
    drop(items);
    assert_eq!(body.len(), limit);
    let r = received(200, "application/json", &body);
    drop(body);
    assert!(matches!(r.body, BodyKind::Json { .. }));
    let mut state = state(ColorMode::TrueColor, 160, 45);
    show(&mut state, r);
    // Every frame is checked. Each is rendered three times and the fastest
    // kept, so a runner pausing the test thread is not taken for a slow
    // frame; the frames take about 4 ms in a debug build.
    let frame = |state: &State| {
        (0..3)
            .map(|_| {
                let started = Instant::now();
                let buffer = render(state, 160, 45);
                (started.elapsed(), buffer)
            })
            .min_by_key(|(took, _)| *took)
            .unwrap()
    };
    let mut frames = Vec::new();
    for _ in 0..60 {
        press(&mut state, "PageDown");
        frames.push(frame(&state).0);
    }
    press(&mut state, "End");
    let (took, buffer) = frame(&state);
    frames.push(took);
    let screen = buffer_text(&buffer);
    assert!(screen.contains("[999999]"), "{screen}");
    // 16 ms per frame as a median; a ceiling well above it still catches
    // a frame that walks the whole body.
    crate::testing::assert_timing(
        "frame",
        &frames,
        Duration::from_millis(16),
        Duration::from_millis(100),
    );
}

#[test]
fn tabs_cycle_with_brackets() {
    let mut state = state(ColorMode::TrueColor, 160, 30);
    show(&mut state, received(200, "text/plain", b"ok"));
    press(&mut state, "[");
    assert_eq!(
        state.tabs.active().unwrap().viewer.tab,
        ResponseTab::Assertions
    );
    assert!(pane(&state).contains("No assertions."));
    let chord = parse_chord("]").unwrap();
    assert_eq!(chord.code, Code::Char(']'));
}

#[test]
fn searching_a_large_tree_runs_off_the_loop_and_drops_stale_results() {
    // Regression: the incremental search scanned the whole tree inside
    // `update`, about 250 ms per keystroke on a 27 MB body.
    let items: Vec<_> = (0..200_000)
        .map(|i| serde_json::json!({ "name": format!("item-{i}") }))
        .collect();
    let body = serde_json::to_vec(&items).unwrap();
    drop(items);
    let r = received(200, "application/json", &body);
    let BodyKind::Json { tree, .. } = &r.body else {
        panic!("expected a tree");
    };
    assert!(tree.len() > super::SYNC_SEARCH_NODES);
    let mut state = state(ColorMode::TrueColor, 120, 30);
    show(&mut state, r);
    assert!(press(&mut state, "/").is_empty());
    let mut jobs = Vec::new();
    let mut took = Vec::new();
    for key in ["i", "t", "e", "m", "-", "1", "9", "9", "9", "9", "9"] {
        let started = Instant::now();
        let effects = press(&mut state, key);
        took.push(started.elapsed());
        let [Effect::Search { job, .. }] = effects.as_slice() else {
            panic!("expected one search job, got {effects:?}");
        };
        jobs.push(job.clone());
    }
    crate::testing::assert_timing(
        "search keystroke",
        &took,
        Duration::from_millis(10),
        Duration::from_millis(100),
    );
    assert!(
        pane(&state).contains("/item-199999 · searching"),
        "{}",
        pane(&state)
    );
    let (last, earlier) = jobs.split_last().unwrap();
    assert!(earlier.iter().all(|j| j.cancel.is_cancelled()));
    assert!(!last.cancel.is_cancelled());
    assert_eq!(earlier[0].run(), None, "a superseded job stops early");
    // Stale completions (even ones that finished) are ignored; the last
    // one is shown and the tree jumps to its match.
    let tab = state.tabs.active().unwrap().id;
    let finish = |state: &mut State, job: &super::SearchJob, matches| {
        update(
            state,
            Action::Searched {
                tab,
                generation: job.generation,
                matches,
            },
        );
    };
    finish(&mut state, &earlier[0], Some(vec![0, 1, 2]));
    let search = state.tabs.active().unwrap().viewer.search.clone().unwrap();
    assert!(search.pending && search.matches.is_empty(), "{search:?}");
    let effect = Effect::Search {
        tab,
        job: last.clone(),
    };
    let action = crate::effects::execute(effect);
    update(&mut state, action);
    let viewer = &state.tabs.active().unwrap().viewer;
    let search = viewer.search.as_ref().unwrap();
    assert!(!search.pending);
    assert_eq!(search.matches.len(), 1);
    let shown = viewer.shown_tree().unwrap();
    assert_eq!(shown.tree.path(shown.selected().unwrap()), "$[199999].name");
    assert!(
        pane(&state).contains("/item-199999 · 1 of 1"),
        "{}",
        pane(&state)
    );
}

#[test]
fn the_sparkline_stats_fit_the_narrow_response_pane() {
    // Regression: at 120 columns (the README screenshot) the one-line
    // stats were cut after p95, hiding p95 and max.
    let mut state = state(ColorMode::TrueColor, 120, 30);
    let mut r = received(200, "text/plain", b"ok");
    r.history = vec![1234.5, 98.7, 1500.0, 2000.25, 999.9, 12.3];
    show(&mut state, r);
    state
        .tabs
        .active_mut()
        .unwrap()
        .viewer
        .set_tab(ResponseTab::Timing);
    let screen = pane(&state);
    let low = screen.lines().find(|l| l.starts_with("min ")).unwrap();
    let high = screen.lines().find(|l| l.starts_with("p95 ")).unwrap();
    assert!(low.contains("median "), "{screen}");
    assert!(high.ends_with("6 points"), "{screen}");
    assert!(high.contains(" max "), "{screen}");
}
