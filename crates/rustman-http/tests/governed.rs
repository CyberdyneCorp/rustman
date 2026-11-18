//! `GovernedSender` against local servers (task 2.2): the policy checks every
//! redirect hop, a 429 halves the rate and honors `Retry-After`, and budgets
//! stop execution. Timing assertions are lower bounds measured on the
//! server, so a slow machine can't make them fail.

mod support;

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use rustman_core::Secret;
use rustman_core::model::{Auth, OAuth2, OAuth2Grant};
use rustman_core::safety::{
    Blocked, BudgetExhausted, Governor, Limits, Origin, Permission, PolicyOptions, SafetyPolicy,
    SafetySettings,
};
use rustman_http::{GovernedError, GovernedSender, RequestError, SendOptions};
use support::recorder::Recorder;
use support::request::get;

const HOUR: Duration = Duration::from_secs(3600);

fn limits(rate: u32, concurrency: u32, max_probes: u64, max_duration: Duration) -> Limits {
    Limits {
        rate: NonZeroU32::new(rate).unwrap(),
        concurrency: NonZeroU32::new(concurrency).unwrap(),
        max_probes,
        max_duration,
    }
}

fn sender_with(target: &str, allow_hosts: &[Origin], limits: Limits) -> GovernedSender {
    sender_as(target, allow_hosts, Permission::ReadOnly, limits)
}

fn sender_as(
    target: &str,
    allow_hosts: &[Origin],
    permission: Permission,
    limits: Limits,
) -> GovernedSender {
    let options = PolicyOptions {
        target,
        permission,
        allow_hosts,
        ..PolicyOptions::default()
    };
    let policy = SafetyPolicy::new(&SafetySettings::default(), options).unwrap();
    GovernedSender::new(policy, Governor::new(limits))
}

fn sender(target: &str, limits: Limits) -> GovernedSender {
    sender_with(target, &[], limits)
}

fn fast() -> Limits {
    limits(1000, 4, 100, HOUR)
}

#[tokio::test]
async fn an_off_host_redirect_is_blocked_and_not_followed() {
    let target = Recorder::start().await;
    let other = Recorder::start().await;
    let sender = sender(&target.url("/"), fast());
    let elsewhere = other.url("/ok");
    let url = target.url(&format!("/redirect?to={elsewhere}"));
    let err = sender
        .send(&get(&url), &SendOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "blocked: host not allowed");
    assert_eq!(
        err,
        GovernedError::Blocked {
            reason: Blocked::HostNotAllowed,
            url: elsewhere,
        }
    );
    assert_eq!(target.hits().len(), 1);
    assert!(other.hits().is_empty(), "the redirect was followed");
}

#[tokio::test]
async fn same_origin_and_allowed_redirects_are_followed() {
    let target = Recorder::start().await;
    let other = Recorder::start().await;
    let allowed = [Origin::parse(&other.url("")).unwrap()];
    let sender = sender_with(&target.url("/"), &allowed, fast());
    let hop = target.url(&format!("/redirect?to={}", other.url("/ok")));
    let url = target.url(&format!("/redirect?to={hop}"));
    let exchange = sender
        .send(&get(&url), &SendOptions::default())
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    assert_eq!(exchange.redirects.len(), 2);
    assert_eq!(target.hits().len(), 2);
    assert_eq!(other.hits().len(), 1);
    // One probe, however many hops.
    assert_eq!(sender.governor().admitted(), 1);
}

#[tokio::test]
async fn a_blocked_request_sends_nothing_and_spends_no_budget() {
    let target = Recorder::start().await;
    let other = Recorder::start().await;
    let sender = sender(&target.url("/"), fast());
    let err = sender
        .send(&get(&other.url("/ok")), &SendOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "blocked: host not allowed");
    let mut post = get(&target.url("/ok"));
    post.method = "POST".into();
    let err = sender
        .send(&post, &SendOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "blocked: mutations not allowed");
    assert!(target.hits().is_empty());
    assert!(other.hits().is_empty());
    assert_eq!(sender.governor().admitted(), 0);
}

#[tokio::test]
async fn a_blocked_url_is_redacted() {
    let target = Recorder::start().await;
    let other = Recorder::start().await;
    let sender = sender(&target.url("/"), fast());
    let mut request = get(&other.url("/ok?token=CANARY123"));
    request.secrets = vec![Secret::new("token", "CANARY123".to_owned())];
    let err = sender
        .send(&request, &SendOptions::default())
        .await
        .unwrap_err();
    let GovernedError::Blocked { url, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(!format!("{err:?}").contains("CANARY123"), "{err:?}");
    assert!(url.ends_with("/ok?token=«redacted:token»"), "{url}");
}

#[tokio::test]
async fn a_429_halves_the_rate_and_honors_retry_after() {
    let server = Recorder::start().await;
    let sender = sender(&server.url("/"), limits(10, 1, 100, HOUR));
    let options = SendOptions::default();
    let limited = server.url("/limited?retry_after=1");
    let exchange = sender.send(&get(&limited), &options).await.unwrap();
    assert_eq!(exchange.status, 429);
    assert_eq!(sender.governor().rate().get(), 5);
    assert_eq!(sender.backoffs(), 1);
    for _ in 0..3 {
        let exchange = sender
            .send(&get(&server.url("/ok")), &options)
            .await
            .unwrap();
        assert_eq!(exchange.status, 200);
    }
    let hits = server.hits();
    assert_eq!(hits.len(), 4);
    let gaps: Vec<Duration> = hits.windows(2).map(|w| w[1].at - w[0].at).collect();
    // Nothing for the Retry-After second, then one request per 200 ms (5/s).
    assert!(gaps[0] >= Duration::from_millis(950), "{gaps:?}");
    for gap in &gaps[1..] {
        assert!(*gap >= Duration::from_millis(180), "{gaps:?}");
    }
}

#[tokio::test]
async fn a_429_without_retry_after_still_halves_the_rate() {
    let server = Recorder::start().await;
    let sender = sender(&server.url("/"), limits(8, 1, 100, HOUR));
    let limited = server.url("/limited");
    for expected in [4, 2, 1, 1] {
        let exchange = sender
            .send(&get(&limited), &SendOptions::default())
            .await
            .unwrap();
        assert_eq!(exchange.status, 429);
        assert_eq!(sender.governor().rate().get(), expected);
    }
}

#[tokio::test]
async fn the_probe_budget_stops_execution() {
    let server = Recorder::start().await;
    let sender = sender(&server.url("/"), limits(1000, 4, 3, HOUR));
    let mut outcomes = Vec::new();
    for _ in 0..5 {
        let result = sender
            .send(&get(&server.url("/ok")), &SendOptions::default())
            .await;
        outcomes.push(result.map(|e| e.status).map_err(|e| e.to_string()));
    }
    let not_run = Err("not run: budget".to_owned());
    assert_eq!(
        outcomes,
        [Ok(200), Ok(200), Ok(200), not_run.clone(), not_run]
    );
    assert_eq!(server.hits().len(), 3);
}

#[tokio::test]
async fn cleanup_is_sent_after_the_budgets_ran_out_but_still_checked() {
    let server = Recorder::start().await;
    let other = Recorder::start().await;
    let sender = sender_as(
        &server.url("/"),
        &[],
        Permission::Destructive,
        limits(1000, 1, 1, Duration::from_millis(200)),
    );
    let options = SendOptions::default();
    sender
        .send(&get(&server.url("/ok")), &options)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut delete = get(&server.url("/ok"));
    delete.method = "DELETE".into();
    let err = sender.send(&delete, &options).await.unwrap_err();
    assert_eq!(err.to_string(), "not run: budget");
    let exchange = sender.send_cleanup(&delete, &options).await.unwrap();
    assert_eq!(exchange.status, 200);
    let err = sender
        .send_cleanup(&get(&other.url("/ok")), &options)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "blocked: host not allowed");
    assert_eq!(server.hits().len(), 2);
    assert!(other.hits().is_empty());
    assert_eq!(sender.governor().admitted(), 1);
}

#[tokio::test]
async fn the_time_budget_stops_execution() {
    let server = Recorder::start().await;
    let sender = sender(
        &server.url("/"),
        limits(1000, 4, 100, Duration::from_millis(300)),
    );
    let options = SendOptions::default();
    sender
        .send(&get(&server.url("/ok")), &options)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let err = sender
        .send(&get(&server.url("/ok")), &options)
        .await
        .unwrap_err();
    assert_eq!(err, GovernedError::NotRun(BudgetExhausted::Duration));
    assert_eq!(server.hits().len(), 1);
}

#[tokio::test]
async fn a_retry_after_beyond_the_time_budget_stops_execution() {
    let server = Recorder::start().await;
    let sender = sender(
        &server.url("/"),
        limits(1000, 1, 100, Duration::from_secs(60)),
    );
    let options = SendOptions::default();
    let limited = server.url("/limited?retry_after=3600");
    sender.send(&get(&limited), &options).await.unwrap();
    let err = sender
        .send(&get(&server.url("/ok")), &options)
        .await
        .unwrap_err();
    assert_eq!(err, GovernedError::NotRun(BudgetExhausted::Duration));
    assert_eq!(server.hits().len(), 1);
}

#[tokio::test]
async fn concurrency_caps_requests_in_flight() {
    let server = Recorder::start().await;
    let sender = Arc::new(sender(&server.url("/"), limits(1000, 2, 100, HOUR)));
    let start = std::time::Instant::now();
    let tasks: Vec<_> = (0..4)
        .map(|_| {
            let sender = sender.clone();
            let url = server.url("/delay?ms=300");
            tokio::spawn(async move { sender.send(&get(&url), &SendOptions::default()).await })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap().status, 200);
    }
    // Two at a time: two rounds of 300 ms.
    assert!(
        start.elapsed() >= Duration::from_millis(590),
        "{:?}",
        start.elapsed()
    );
    let hits = server.hits();
    assert_eq!(hits.len(), 4);
    assert!(
        hits[2].at - hits[0].at >= Duration::from_millis(280),
        "{hits:?}"
    );
}

#[tokio::test]
async fn a_relative_redirect_stays_on_the_target_and_is_followed() {
    let server = Recorder::start().await;
    let sender = sender(&server.url("/"), fast());
    let url = server.url("/redirect?to=/ok");
    let exchange = sender
        .send(&get(&url), &SendOptions::default())
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    let paths: Vec<String> = server.hits().into_iter().map(|h| h.path).collect();
    assert_eq!(paths, ["/redirect", "/ok"]);
}

#[tokio::test]
async fn a_redirect_that_changes_the_scheme_is_blocked() {
    let server = Recorder::start().await;
    let sender = sender(&server.url("/"), fast());
    let same_host = server.url("/ok");
    let rows = [
        // Same host and port, other scheme: another origin.
        same_host.replacen("http://", "https://", 1),
        same_host.replacen("http://", "ftp://", 1),
    ];
    for to in rows {
        let url = server.url(&format!("/redirect?to={to}"));
        let err = sender
            .send(&get(&url), &SendOptions::default())
            .await
            .unwrap_err();
        assert_eq!(
            err,
            GovernedError::Blocked {
                reason: Blocked::HostNotAllowed,
                url: to.clone(),
            },
            "{to}"
        );
    }
    // Only the two redirecting requests; neither hop was requested.
    assert_eq!(server.hits().len(), 2);
}

#[tokio::test]
async fn redirect_hops_are_checked_with_the_method_they_are_sent_with() {
    let server = Recorder::start().await;
    let other = Recorder::start().await;
    let sender = sender_as(&server.url("/"), &[], Permission::Mutations, fast());
    let post = |path: &str| {
        let mut request = get(&server.url(path));
        request.method = "POST".into();
        request
    };
    // 303 turns POST into GET; 307 keeps POST, which the policy allows here.
    for (status, method) in [(303, "GET"), (307, "POST")] {
        let path = format!("/redirect?status={status}&to=/ok");
        let exchange = sender
            .send(&post(&path), &SendOptions::default())
            .await
            .unwrap();
        assert_eq!(exchange.status, 200);
        assert_eq!(server.hits().last().unwrap().method, method, "{status}");
    }
    // A 307 POST off the target is refused before it is sent.
    let path = format!("/redirect?status=307&to={}", other.url("/ok"));
    let err = sender
        .send(&post(&path), &SendOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "blocked: host not allowed");
    assert!(other.hits().is_empty());
}

#[tokio::test]
async fn an_unrepresentable_retry_after_stops_execution_without_panicking() {
    let server = Recorder::start().await;
    let sender = sender(&server.url("/"), limits(1000, 1, 100, Duration::MAX));
    let options = SendOptions::default();
    let limited = server.url("/limited?retry_after=18446744073709551615");
    let exchange = sender.send(&get(&limited), &options).await.unwrap();
    assert_eq!(exchange.status, 429);
    let err = sender
        .send(&get(&server.url("/ok")), &options)
        .await
        .unwrap_err();
    assert_eq!(err, GovernedError::NotRun(BudgetExhausted::Duration));
    assert_eq!(server.hits().len(), 1);
}

#[tokio::test]
async fn an_oauth2_request_fails_before_sending_or_spending_budget() {
    let target = Recorder::start().await;
    let sender = sender(&target.url("/"), fast());
    let mut request = get(&target.url("/ok"));
    request.auth = Some(Auth::OAuth2(OAuth2 {
        grant: OAuth2Grant::ClientCredentials,
        token_url: target.url("/token"),
        auth_url: None,
        client_id: "c".into(),
        client_secret: None,
        scopes: None,
    }));
    let err = sender
        .send(&request, &SendOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        GovernedError::Request(RequestError::Auth(
            "oauth2: no access token was acquired".into()
        ))
    );
    assert!(target.hits().is_empty());
    assert_eq!(sender.governor().admitted(), 0);
}
