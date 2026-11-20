//! Monitor trends in the viz companion (monitors "Trend charts in the viz
//! companion"), for `rustman monitor status --viz` and `v` in the Monitors
//! view: the companion is started and greeted the way every producer does
//! it ([`crate::companion`]), its `monitor_trend` view is opened, and one
//! `monitor_trend` message per monitor is sent before `close`.
//!
//! Without a companion, or with one whose `hello_ack` lacks the view, the
//! error says why and the caller shows the text status instead. Trends hold
//! counts, timings and outcomes only, so they carry nothing to redact.

use rustman_monitor::status::trend::Trend;
use rustman_viz_protocol::{HourOutcomes, LatencyPoint, Message, MonitorTrend, View};

use crate::bench_viz::start_view;
use crate::companion::Discovery;

/// Why trends are not shown when the companion lacks the view.
pub const NO_TREND_VIEW: &str = "the viz companion has no monitor_trend view";

/// The title of the trend view.
const TITLE: &str = "rustman monitor status";

/// Open the trend view and send `trends`. `Ok` names the companion; `Err`
/// says why it could not be used.
pub fn show(discovery: &Discovery, trends: &[Trend]) -> Result<String, String> {
    let stream = start_view(discovery, TITLE, View::MonitorTrend, NO_TREND_VIEW)
        // The reason is shown after our own note, not as a flag warning.
        .map_err(|e| e.strip_prefix("--viz: ").map_or(e.clone(), str::to_owned))?;
    let viewer = stream.viewer.clone();
    for trend in trends {
        stream.send(Message::MonitorTrend(Box::new(message(trend))));
    }
    stream.finish(None);
    Ok(viewer)
}

/// The protocol form of `trend`.
pub fn message(trend: &Trend) -> MonitorTrend {
    MonitorTrend {
        monitor: trend.name.clone(),
        environment: trend.environment.clone(),
        schedule: trend.schedule.clone(),
        uptime_24h: trend.uptime.day,
        uptime_7d: trend.uptime.week,
        uptime_30d: trend.uptime.month,
        latency: trend
            .latency
            .iter()
            .map(|point| LatencyPoint {
                hour: point.hour.to_string(),
                p95_ms: point.p95_ms,
            })
            .collect(),
        hour_of_day: (0u8..)
            .zip(&trend.hour_of_day)
            .map(|(hour, cell)| HourOutcomes {
                hour,
                passed: cell.passed,
                failed: cell.failed,
                errored: cell.errored,
                skipped: cell.skipped,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use rustman_monitor::Timestamp;
    use rustman_monitor::status::Uptime;
    use rustman_monitor::status::trend::{HourLatency, OutcomeCounts};

    use super::*;

    #[test]
    fn a_trend_maps_to_its_message() {
        let mut hour_of_day = [OutcomeCounts::default(); 24];
        hour_of_day[9].passed = 11;
        hour_of_day[9].failed = 1;
        let trend = Trend {
            name: "api-health".into(),
            environment: "prod".into(),
            schedule: "every 5m".into(),
            uptime: Uptime {
                day: Some(98.95),
                week: None,
                month: Some(99.9),
            },
            latency: vec![HourLatency {
                hour: "2026-11-17T09:00:00Z".parse::<Timestamp>().unwrap(),
                p95_ms: 123.0,
            }],
            hour_of_day,
        };
        let message = message(&trend);
        assert_eq!(message.monitor, "api-health");
        assert_eq!(message.uptime_24h, Some(98.95));
        assert_eq!(message.uptime_7d, None);
        assert_eq!(message.latency[0].hour, "2026-11-17T09:00:00Z");
        assert_eq!(message.hour_of_day.len(), 24);
        assert_eq!(message.hour_of_day[9].hour, 9);
        assert_eq!(message.hour_of_day[9].passed, 11);
        assert_eq!(message.hour_of_day[23].hour, 23);
    }

    #[test]
    fn without_a_companion_the_error_says_why() {
        let err = show(&Discovery::default(), &[]).unwrap_err();
        assert!(err.starts_with("Viz companion not found"), "{err}");
    }
}
