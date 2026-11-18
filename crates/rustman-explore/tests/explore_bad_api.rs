//! Tasks 3.4 and 3.5: the detectors, lifecycle chains and BOLA against the
//! deliberately broken `bad-api` fixture server, through a governed sender.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::time::Duration;

use common::load_bad_api;
use rustman_core::Environment;
use rustman_core::model::{VarValue, Variables};
use rustman_core::safety::{
    Governor, Limits, Permission, PolicyOptions, SafetyPolicy, SafetySettings,
};
use rustman_core::vars::SecretLookup;
use rustman_explore::explore::detect::Finding;
use rustman_explore::explore::exec::{Context, Executor, Identity, Outcome, Status};
use rustman_explore::explore::graph::Graph;
use rustman_explore::explore::plan::{Family, PlanOptions, plan};
use rustman_explore::explore::rules::Severity;
use rustman_http::{GovernedSender, SendOptions};
use rustman_test_support::bad_api::{BadApi, Hit, TOKEN_A, TOKEN_B};

/// The bearer tokens, as the OS keychain would hold them.
struct Tokens;

impl SecretLookup for Tokens {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok(match (env, name) {
            ("alice", "bearerAuth_token") => Some(TOKEN_A.to_owned()),
            ("bob", "bearerAuth_token") => Some(TOKEN_B.to_owned()),
            _ => None,
        })
    }
}

fn environment(name: &str) -> Environment {
    Environment {
        name: name.into(),
        variables: Variables::default(),
        secrets: vec!["bearerAuth_token".into()],
        production: false,
    }
}

/// An environment holding `token` as a plain variable, not a secret.
fn plain_environment(name: &str, token: &str) -> Environment {
    let token = VarValue::String(token.into());
    Environment {
        name: name.into(),
        variables: Variables::new(vec![("bearerAuth_token".into(), token)]),
        secrets: Vec::new(),
        production: false,
    }
}

struct Exploration {
    families: BTreeSet<Family>,
    permission: Permission,
    identity_b: bool,
    /// The tokens are plain environment variables instead of secrets.
    plain_tokens: bool,
    /// `(method, path prefix)` the server never answers; each probe is
    /// then dropped after [`STALLED_PROBE`], as a cancellation would.
    stall: Option<(&'static str, &'static str)>,
}

/// How long a probe may wait on a stalled server.
const STALLED_PROBE: Duration = Duration::from_secs(3);

struct Explored {
    outcomes: Vec<Outcome>,
    /// What [`Executor::abandoned`] returned after the dropped probes.
    abandoned: Vec<(String, Vec<rustman_explore::explore::resources::Resource>)>,
    hits: Vec<Hit>,
    server: BadApi,
}

impl Exploration {
    fn new(families: &[Family], permission: Permission) -> Self {
        Self {
            families: families.iter().copied().collect(),
            permission,
            identity_b: false,
            plain_tokens: false,
            stall: None,
        }
    }

    async fn run(&self) -> Explored {
        let server = match self.stall {
            Some((method, prefix)) => BadApi::start_stalling(method, prefix).await,
            None => BadApi::start().await,
        };
        let model = load_bad_api().await.model;
        let graph = Graph::build(&model, &BTreeMap::new());
        let target = server.url("");
        let options = PolicyOptions {
            target: &target,
            permission: self.permission,
            ..PolicyOptions::default()
        };
        let policy = SafetyPolicy::new(&SafetySettings::default(), options).unwrap();
        let planned = plan(
            &model,
            &graph,
            &PlanOptions {
                families: self.families.clone(),
                operations: Vec::new(),
                seed: 7,
                fuzz_cases: 2,
                identity_b: self.identity_b,
                known: BTreeSet::new(),
                policy: &policy,
            },
        );
        let limits = Limits {
            rate: NonZeroU32::new(1000).unwrap(),
            concurrency: NonZeroU32::new(4).unwrap(),
            max_probes: 10_000,
            max_duration: Duration::from_secs(3600),
        };
        let sender = GovernedSender::new(policy, Governor::new(limits));
        let (alice, bob) = if self.plain_tokens {
            (
                plain_environment("alice", TOKEN_A),
                plain_environment("bob", TOKEN_B),
            )
        } else {
            (environment("alice"), environment("bob"))
        };
        let variables = Variables::default();
        let disabled = Vec::new();
        let context = Context {
            model: &model,
            graph: &graph,
            target: &target,
            variables: &variables,
            identity: Identity {
                environment: Some(&alice),
                secrets: &Tokens,
            },
            identity_b: self.identity_b.then_some(Identity {
                environment: Some(&bob),
                secrets: &Tokens,
            }),
            scans: &planned.scans,
            disabled_rules: &disabled,
            send: SendOptions::default(),
        };
        let executor = Executor::new(&sender, context);
        let mut outcomes = Vec::new();
        for probe in &planned.probes {
            match self.stall {
                Some(_) => {
                    if let Ok(o) = tokio::time::timeout(STALLED_PROBE, executor.run(probe)).await {
                        outcomes.push(o);
                    }
                }
                None => outcomes.push(executor.run(probe).await),
            }
        }
        let abandoned = executor.abandoned();
        let hits = server.hits();
        Explored {
            outcomes,
            abandoned,
            hits,
            server,
        }
    }
}

impl Explored {
    fn findings(&self) -> impl Iterator<Item = &Finding> {
        self.outcomes.iter().flat_map(|o| &o.findings)
    }

    fn find(&self, rule: &str, operation: &str) -> Vec<&Finding> {
        self.findings()
            .filter(|f| f.rule == rule && f.operation == operation)
            .collect()
    }

    fn first(&self, rule: &str, operation: &str) -> &Finding {
        let found = self.find(rule, operation);
        assert!(
            !found.is_empty(),
            "no {rule} on {operation}: {:#?}",
            self.summary()
        );
        found[0]
    }

    fn summary(&self) -> Vec<String> {
        self.findings()
            .map(|f| format!("{} {}", f.rule, f.operation))
            .collect()
    }
}

const DETECTOR_FAMILIES: [Family; 6] = [
    Family::Conformance,
    Family::NegativeInput,
    Family::Auth,
    Family::MethodNotAllowed,
    Family::UndocumentedStatus,
    Family::ErrorLeakage,
];

#[tokio::test]
async fn conformance_finds_the_missing_required_property() {
    let explored = Exploration::new(&DETECTOR_FAMILIES, Permission::Mutations)
        .run()
        .await;
    let finding = explored.first("response-schema-violation", "GET /users/{id}");
    assert_eq!(finding.severity, Severity::Medium);
    assert_eq!(finding.instance_pointer.as_deref(), Some(""));
    assert_eq!(
        finding.schema_pointer.as_deref(),
        Some("/components/schemas/User/required")
    );
    assert!(
        finding.message.contains("\"email\" is a required property"),
        "{}",
        finding.message
    );
    assert_eq!(finding.evidence.status, 200);
    assert!(
        explored
            .find("response-schema-violation", "GET /users")
            .is_empty()
    );
}

#[tokio::test]
async fn negative_input_finds_server_errors_and_accepted_input() {
    let explored = Exploration::new(&DETECTOR_FAMILIES, Permission::Mutations)
        .run()
        .await;
    let error = explored.first("server-error-on-invalid-input", "POST /users");
    assert_eq!(
        (error.severity, error.evidence.status),
        (Severity::High, 500)
    );
    let accepted = explored.first("invalid-input-accepted", "POST /users");
    assert_eq!(
        (accepted.severity, accepted.evidence.status),
        (Severity::Medium, 201)
    );
    assert!(
        accepted
            .evidence
            .request_body
            .as_deref()
            .unwrap()
            .contains("\"age\":\"not-a-number\"")
    );
    explored.first("server-error-on-invalid-input", "GET /search");
    assert!(
        explored
            .find("invalid-input-accepted", "POST /orders")
            .is_empty()
    );
    assert!(
        explored
            .find("server-error-on-invalid-input", "POST /orders")
            .is_empty()
    );
}

#[tokio::test]
async fn error_leakage_cites_the_matched_excerpt() {
    let explored = Exploration::new(&DETECTOR_FAMILIES, Permission::Mutations)
        .run()
        .await;
    let java = explored.first("error-leakage", "POST /users");
    assert_eq!(java.severity, Severity::Medium);
    assert!(
        java.message
            .contains("at com.example.UserService.create(UserService.java:42)"),
        "{}",
        java.message
    );
    let sql = explored.first("error-leakage", "GET /search");
    assert!(sql.message.contains("SQL"), "{}", sql.message);
}

#[tokio::test]
async fn auth_probes_find_the_unprotected_endpoint_only() {
    let explored = Exploration::new(&DETECTOR_FAMILIES, Permission::Mutations)
        .run()
        .await;
    let open = explored.first("auth-not-enforced", "GET /admin/users");
    assert_eq!(open.severity, Severity::Critical);
    assert_eq!(open.evidence.status, 200);
    let enforced = ["POST /orders", "GET /orders/{orderId}", "GET /search"];
    for operation in enforced {
        assert!(
            explored.find("auth-not-enforced", operation).is_empty(),
            "{operation}"
        );
        assert!(
            explored
                .find("unexpected-auth-status", operation)
                .is_empty(),
            "{operation}"
        );
    }
}

#[tokio::test]
async fn undocumented_methods_and_statuses_are_found() {
    let explored = Exploration::new(&DETECTOR_FAMILIES, Permission::Mutations)
        .run()
        .await;
    let put = explored.first("undocumented-method-allowed", "PUT /users");
    assert_eq!(
        (put.severity, put.evidence.method.as_str()),
        (Severity::Medium, "PUT")
    );
    assert!(
        explored
            .find("undocumented-method-allowed", "PUT /orders")
            .is_empty()
    );
    let status = explored.first("undocumented-status", "GET /status");
    assert_eq!(status.severity, Severity::Info);
    assert!(status.message.contains("status 202"), "{}", status.message);
}

#[tokio::test]
async fn security_headers_are_checked_on_every_response() {
    let families = [Family::Conformance, Family::SecurityHeaders];
    let explored = Exploration::new(&families, Permission::ReadOnly)
        .run()
        .await;
    let cors = explored.first("permissive-cors", "GET /status");
    assert_eq!(cors.severity, Severity::Low);
    explored.first("missing-content-type-options", "GET /users");
    assert!(
        explored.findings().all(|f| f.rule != "missing-hsts"),
        "plain HTTP has no HSTS rule"
    );
    // `GET /users/{id}` probes first list `GET /users` for a real id; what
    // that listing answers is filed under `GET /users`.
    assert!(matches_operation(
        "http://h:1/users/7?x=1",
        "GET /users/{id}"
    ));
    assert!(!matches_operation("http://h:1/users", "GET /users/{id}"));
    for f in explored.findings() {
        assert!(
            matches_operation(&f.evidence.url, &f.operation),
            "{} filed under {} for {}",
            f.rule,
            f.operation,
            f.evidence.url
        );
    }
}

/// Whether the path of `url` is one `operation` (`GET /users/{id}`) serves.
fn matches_operation(url: &str, operation: &str) -> bool {
    let template = operation.split_once(' ').map_or(operation, |(_, p)| p);
    let path = url.split("://").nth(1).unwrap_or(url);
    let path = path.find('/').map_or("/", |i| &path[i..]);
    let path = path.split('?').next().unwrap_or(path);
    let (want, got): (Vec<&str>, Vec<&str>) =
        (template.split('/').collect(), path.split('/').collect());
    want.len() == got.len()
        && want
            .iter()
            .zip(&got)
            .all(|(w, g)| w.starts_with('{') || w == g)
}

#[tokio::test]
async fn the_default_read_only_run_sends_no_mutation() {
    let explored = Exploration::new(
        &Family::defaults().into_iter().collect::<Vec<_>>(),
        Permission::ReadOnly,
    )
    .run()
    .await;
    assert!(!explored.hits.is_empty());
    for hit in &explored.hits {
        assert!(
            ["GET", "HEAD", "OPTIONS"].contains(&hit.method.as_str()),
            "{hit:?}"
        );
    }
    let blocked = explored
        .outcomes
        .iter()
        .filter(|o| o.status == Status::Blocked("mutations not allowed".into()))
        .count();
    assert!(blocked > 0);
    assert!(
        explored
            .outcomes
            .iter()
            .all(|o| o.status != Status::Blocked("host not allowed".into()))
    );
}

#[tokio::test]
async fn secrets_never_reach_findings() {
    let explored = Exploration::new(&DETECTOR_FAMILIES, Permission::Mutations)
        .run()
        .await;
    let sql = explored.first("error-leakage", "GET /search");
    assert!(
        sql.evidence.body.contains("request headers"),
        "{}",
        sql.evidence.body
    );
    assert!(
        sql.evidence.body.contains("redacted"),
        "{}",
        sql.evidence.body
    );
    // `GET /admin/users` answers without credentials, listing A's token:
    // the no-credentials probe never sent it, yet it must be redacted.
    let open = explored.first("auth-not-enforced", "GET /admin/users");
    assert!(
        open.evidence.body.contains("redacted"),
        "{}",
        open.evidence.body
    );
    let all = serde_json::to_string(&explored.outcomes).unwrap();
    assert!(!all.contains(TOKEN_A), "identity A's token leaked");
    let headers = &sql.evidence.request_headers;
    let bearer = (
        "Authorization".to_owned(),
        "Bearer «redacted:bearerAuth_token»".to_owned(),
    );
    assert!(headers.contains(&bearer), "{headers:?}");
    let explored = Exploration {
        identity_b: true,
        ..Exploration::new(&[Family::Bola], Permission::Mutations)
    }
    .run()
    .await;
    let all = serde_json::to_string(&explored.outcomes).unwrap();
    assert!(
        !all.contains(TOKEN_A) && !all.contains(TOKEN_B),
        "a token leaked"
    );
}

#[tokio::test]
async fn credentials_from_plain_variables_never_reach_findings() {
    let families = [Family::NegativeInput, Family::ErrorLeakage, Family::Bola];
    let explored = Exploration {
        identity_b: true,
        plain_tokens: true,
        ..Exploration::new(&families, Permission::Mutations)
    }
    .run()
    .await;
    // Only the tokens' owners can read their orders: the tokens were sent.
    explored.first("broken-object-level-authorization", "GET /orders/{orderId}");
    let all = serde_json::to_string(&explored.outcomes).unwrap();
    assert!(
        !all.contains(TOKEN_A) && !all.contains(TOKEN_B),
        "a token leaked"
    );
    let finding = explored.first("server-error-on-invalid-input", "GET /search");
    let curl = &finding.reproduction.as_ref().unwrap().curl;
    assert!(
        curl.contains("-H 'Authorization: Bearer {{bearerAuth_token}}'"),
        "{curl}"
    );
    let headers = &finding.evidence.request_headers;
    let authorization = headers.iter().find(|(name, _)| name == "Authorization");
    assert_eq!(
        authorization.map(|(_, value)| value.as_str()),
        Some("Bearer «redacted:Authorization»"),
        "{headers:?}"
    );
}

#[tokio::test]
async fn lifecycle_finds_an_update_not_reflected_and_a_resource_surviving_delete() {
    let explored = Exploration::new(&[Family::Lifecycle], Permission::Destructive)
        .run()
        .await;
    let update = explored.first("update-not-reflected", "GET /users/{id}");
    assert_eq!(update.severity, Severity::Medium);
    assert!(
        update
            .message
            .contains("'name' is \"string\" after the update set \"updated\""),
        "{}",
        update.message
    );
    let survives = explored.first("resource-survives-delete", "GET /users/{id}");
    assert_eq!(
        (survives.severity, survives.evidence.status),
        (Severity::Medium, 200)
    );
    let chain: Vec<(String, String)> = explored
        .hits
        .iter()
        .map(|h| (h.method.clone(), h.target.clone()))
        .take(6)
        .collect();
    let created = explored.server.user_ids().into_iter().max().unwrap();
    let item = format!("/users/{created}");
    let expected = [
        ("POST", "/users"),
        ("GET", item.as_str()),
        ("PUT", item.as_str()),
        ("GET", item.as_str()),
        ("DELETE", item.as_str()),
        ("GET", item.as_str()),
    ]
    .map(|(m, p)| (m.to_owned(), p.to_owned()));
    assert_eq!(chain, expected);
}

#[tokio::test]
async fn lifecycle_without_destructive_stops_before_the_delete() {
    let explored = Exploration::new(&[Family::Lifecycle], Permission::Mutations)
        .run()
        .await;
    explored.first("update-not-reflected", "GET /users/{id}");
    assert!(explored.hits.iter().all(|h| h.method != "DELETE"));
    assert!(
        explored
            .find("resource-survives-delete", "GET /users/{id}")
            .is_empty()
    );
}

#[tokio::test]
async fn bola_finds_a_cross_tenant_read() {
    let explored = Exploration {
        identity_b: true,
        ..Exploration::new(&[Family::Bola], Permission::Mutations)
    }
    .run()
    .await;
    let bola = explored.first("broken-object-level-authorization", "GET /orders/{orderId}");
    assert_eq!(
        (bola.severity, bola.evidence.status),
        (Severity::Critical, 200)
    );
    assert!(
        bola.evidence.url.ends_with("/orders/o-1"),
        "{}",
        bola.evidence.url
    );
    let without = Exploration::new(&[Family::Bola], Permission::Mutations)
        .run()
        .await;
    assert!(without.hits.is_empty());
    assert!(
        without
            .outcomes
            .iter()
            .all(|o| o.status == Status::Skipped("no second identity (--identity-b)".into()))
    );
}

#[tokio::test]
async fn a_probe_dropped_in_flight_reports_what_it_created() {
    // Lifecycle chains create a user, then read it back; the read never
    // answers, so each probe is dropped after creating its user.
    let explored = Exploration {
        stall: Some(("GET", "/users/")),
        ..Exploration::new(&[Family::Lifecycle], Permission::Destructive)
    }
    .run()
    .await;
    assert!(!explored.abandoned.is_empty(), "{:?}", explored.outcomes);
    let created: Vec<String> = explored
        .abandoned
        .iter()
        .flat_map(|(_, resources)| resources.iter().map(|r| r.path.clone()))
        .collect();
    for id in explored.server.user_ids().into_iter().filter(|id| *id != 1) {
        assert!(
            created.contains(&format!("/users/{id}")),
            "{id} not reported: {created:?}"
        );
    }
}
