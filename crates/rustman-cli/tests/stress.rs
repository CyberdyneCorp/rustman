//! `rustman stress` (add-stress-testing tasks 1.2 and 2.x): every refusal of
//! the stress gate and the caps exits 4 (safety) or 2 (usage) before the
//! stub server receives a single request, and a short spike run prints its
//! summary without leaking a secret.

mod common;

use common::{rustman, write};
use rustman_test_support::StressServer;

const CANARY: &str = "canary-9Zx4-stress-secret";

/// The stub and the runtime it is served from.
struct Stub {
    server: StressServer,
    _runtime: tokio::runtime::Runtime,
}

impl Stub {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(StressServer::start());
        Self {
            server,
            _runtime: runtime,
        }
    }

    fn origin(&self) -> String {
        self.server.origin()
    }

    fn count(&self) -> u64 {
        self.server.count()
    }
}

fn request(method: &str, url: &str) -> String {
    format!("version: 1\nkind: http\nname: r\nmethod: {method}\nurl: \"{url}\"\n")
}

/// A workspace with `stress` as the `[stress]` table, `dev` and `prod`
/// environments pointing at `origin`, and a GET request.
fn workspace(origin: &str, stress: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n[stress]\n{stress}"),
    );
    for env in ["dev", "prod"] {
        write(
            dir.path(),
            &format!("environments/{env}.yaml"),
            &format!("variables:\n  base_url: {origin}\n"),
        );
    }
    write(dir.path(), ".gitignore", ".rustman/\n");
    write(
        dir.path(),
        "api/get.rman.yaml",
        &request("GET", "{{base_url}}/"),
    );
    dir
}

/// `targets = [origin]`.
fn listed(origin: &str) -> String {
    format!("targets = [\"{origin}\"]\n")
}

fn stress(dir: &std::path::Path, args: &[&str]) -> (Option<i32>, String, String) {
    let output = rustman(dir).arg("stress").args(args).output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// Run `args` (after the mode and path) against the stub with `settings`,
/// expecting `code` and a stderr containing `message`, and no request.
fn refused(settings: &str, env: &str, args: &[&str], code: i32, message: &str) {
    let stub = Stub::start();
    let origin = stub.origin();
    let ws = workspace(&origin, &settings.replace("ORIGIN", &origin));
    let mut all = vec!["--target", &origin, "--env", env];
    all.extend_from_slice(args);
    let (got, _, stderr) = stress(ws.path(), &all);
    assert_eq!(got, Some(code), "{stderr}");
    assert!(stderr.contains(message), "{stderr}");
    assert_eq!(stub.count(), 0, "the stub received requests");
}

const LISTED: &str = "targets = [\"ORIGIN\"]\n";
const PROD_ALLOWED: &str = "targets = [\"ORIGIN\"]\nallow_production = [\"127.0.0.1\"]\n";

#[test]
fn an_unlisted_target_exits_4() {
    let stub = Stub::start();
    let ws = workspace(
        &stub.origin(),
        "targets = [\"https://staging.example.com\"]\n",
    );
    let (code, _, stderr) = stress(
        ws.path(),
        &[
            "breakpoint",
            "api/",
            "--target",
            &stub.origin(),
            "--env",
            "dev",
        ],
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert_eq!(
        stderr.trim_end(),
        "error: stress: target not listed in rustman.toml [stress].targets"
    );
    assert_eq!(stub.count(), 0);
}

#[test]
fn production_without_production_flag_exits_4() {
    refused(
        PROD_ALLOWED,
        "prod",
        &["breakpoint", "api/"],
        4,
        "--production 127.0.0.1",
    );
}

#[test]
fn production_without_allow_production_exits_4() {
    refused(
        LISTED,
        "prod",
        &["breakpoint", "api/", "--production", "127.0.0.1"],
        4,
        "stress: production host 127.0.0.1 is not listed in rustman.toml [stress].allow_production",
    );
}

#[test]
fn fuzzing_production_exits_4_even_with_both_opt_ins() {
    let prod = ["breakpoint", "api/", "--production", "127.0.0.1"];
    let message = "stress: fuzzing and malformed input are never sent to production";
    for extra in [
        &["--fuzz-ratio", "0.1"][..],
        &["--malformed", "json-syntax"][..],
        &["--wire", "cl-te-conflict", "--allow-wire-faults"][..],
    ] {
        let mut args = prod.to_vec();
        args.extend_from_slice(extra);
        refused(PROD_ALLOWED, "prod", &args, 4, message);
    }
    refused(
        PROD_ALLOWED,
        "prod",
        &["fuzz", "api/", "--production", "127.0.0.1", "--rps", "5"],
        4,
        message,
    );
}

#[test]
fn a_compression_bomb_needs_the_families_key() {
    refused(
        LISTED,
        "dev",
        &["breakpoint", "api/", "--malformed", "compression-bomb"],
        4,
        "\"compression-bomb\" in rustman.toml [stress].families",
    );
}

#[test]
fn wire_faults_need_allow_wire_faults() {
    refused(
        LISTED,
        "dev",
        &["breakpoint", "api/", "--wire", "slow-body"],
        4,
        "needs --allow-wire-faults",
    );
}

#[test]
fn methods_follow_the_safety_policy() {
    let stub = Stub::start();
    let origin = stub.origin();
    let ws = workspace(&origin, &listed(&origin));
    write(
        ws.path(),
        "api/post.rman.yaml",
        &request("POST", "{{base_url}}/"),
    );
    let (code, _, stderr) = stress(
        ws.path(),
        &["breakpoint", "api/", "--target", &origin, "--env", "dev"],
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert!(stderr.contains("api/post.rman.yaml: blocked"), "{stderr}");
    assert_eq!(stub.count(), 0);
}

#[test]
fn a_soak_over_its_cap_exits_2_naming_the_key() {
    refused(
        LISTED,
        "dev",
        &["soak", "api/", "--rps", "5", "--duration", "4h"],
        2,
        "exceeds max_soak_duration = 2h (raise it in rustman.toml [stress])",
    );
}

#[test]
fn a_rate_over_the_cap_exits_2_and_config_raises_it_only() {
    refused(
        LISTED,
        "dev",
        &["breakpoint", "api/", "--start-rps", "600"],
        2,
        "stress: --start-rps 600 exceeds max_rps = 500 (raise it in rustman.toml [stress])",
    );
}

#[test]
fn usage_errors_exit_2() {
    refused(
        LISTED,
        "dev",
        &["breakpoint", "api/", "--fuzz-ratio", "1.5"],
        2,
        "outside 0 < r <= 1",
    );
    refused(
        LISTED,
        "dev",
        &["soak", "api/", "--duration", "1m"],
        2,
        "soak needs --rps",
    );
    refused(
        LISTED,
        "dev",
        &["spike", "api/", "--require-rps", "100"],
        2,
        "--require-rps applies to breakpoint runs",
    );
    refused(
        LISTED,
        "dev",
        &["breakpoint", "api/", "--step-duration", "10s"],
        2,
        "--step-duration must be at least 15s",
    );
    refused(
        "targets = [\"ORIGIN\"]\nmax_rsp = 9000\n",
        "dev",
        &["breakpoint", "api/"],
        2,
        "unknown field `max_rsp`",
    );
}

#[test]
fn an_unknown_mode_exits_2_listing_the_modes() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = stress(dir.path(), &["chaos", "api/"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("invalid value 'chaos'"), "{stderr}");
    assert!(
        stderr.contains("[possible values: breakpoint, spike, soak, fuzz]"),
        "{stderr}"
    );
}

#[test]
fn a_missing_target_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = stress(dir.path(), &["breakpoint"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert_eq!(stderr.trim_end(), "error: stress: --target is required");
}

/// A 3-second spike: the summary is printed and no output holds the
/// secret sent in a header.
#[test]
fn a_short_spike_prints_its_summary_without_secrets() {
    let stub = Stub::start();
    let origin = stub.origin();
    let ws = workspace(&origin, &listed(&origin));
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {origin}\nsecrets: [api_token]\n"),
    );
    rustman(ws.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin(CANARY)
        .assert()
        .success();
    write(
        ws.path(),
        "api/get.rman.yaml",
        &format!(
            "{}headers:\n  - name: Authorization\n    value: 'Bearer {{{{api_token}}}}'\n",
            request("GET", "{{base_url}}/status/500?t={{api_token}}")
        ),
    );
    let (code, stdout, stderr) = stress(
        ws.path(),
        &[
            "spike",
            "api/",
            "--target",
            &origin,
            "--env",
            "dev",
            "--baseline-rps",
            "4",
            "--baseline",
            "1s",
            "--spike-factor",
            "2",
            "--spike-duration",
            "1s",
            "-vv",
        ],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.starts_with("stress spike (SLOs: p95<1s, error_rate<5%)\n"),
        "{stdout}"
    );
    assert!(stdout.contains("during the spike"), "{stdout}");
    assert!(stub.count() > 0);
    assert!(!stdout.contains(CANARY), "{stdout}");
    assert!(!stderr.contains(CANARY), "{stderr}");
}

#[test]
fn wire_faults_through_a_proxy_outside_the_policy_exit_4() {
    refused(
        "targets = [\"ORIGIN\"]\n[http]\nproxy = \"http://127.0.0.1:9\"\n",
        "dev",
        &[
            "soak",
            "api/",
            "--rps",
            "1",
            "--duration",
            "1s",
            "--wire",
            "cl-te-conflict",
            "--allow-wire-faults",
        ],
        4,
        "proxy not allowed",
    );
}

#[test]
fn spec_without_fuzzing_is_a_usage_error() {
    refused(
        LISTED,
        "dev",
        &["breakpoint", "api/", "--spec", "openapi.yaml"],
        2,
        "--spec",
    );
}

/// A short fuzz run with a malformed family: the seed is printed, the
/// medium finding is listed and does not fail the run at `--fail-on high`.
#[test]
fn a_fuzz_run_prints_its_seed_and_findings() {
    let stub = Stub::start();
    let origin = stub.origin();
    let ws = workspace(&origin, &listed(&origin));
    write(
        ws.path(),
        "orders/create.rman.yaml",
        &format!(
            "{}body:\n  type: json\n  content: {{role: user}}\n",
            request("POST", "{{base_url}}/fuzz/accept")
        ),
    );
    let args = [
        "fuzz",
        "orders/",
        "--target",
        &origin,
        "--env",
        "dev",
        "--rps",
        "4",
        "--duration",
        "1s",
        "--baseline",
        "1s",
        "--seed",
        "7",
        "--fuzz-ratio",
        "1",
        "--malformed",
        "duplicate-keys",
        "--allow-mutations",
    ];
    let (code, stdout, stderr) = stress(ws.path(), &args);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stderr.contains("stress: seed 7"), "{stderr}");
    assert!(
        stdout.contains("fuzz: 100% of requests replaced (seed 7)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("medium accepted-malformed-input: duplicate-keys input on orders/create.rman.yaml answered 201"),
        "{stdout}"
    );
}

/// Wire faults against a server that accepts conflicting framing: a high
/// finding, so the run exits 1.
#[test]
fn accepted_framing_fails_the_run() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(rustman_test_support::WireServer::start(
        rustman_test_support::WireConfig {
            lax_framing: true,
            ..Default::default()
        },
    ));
    let origin = server.origin();
    let ws = workspace(&origin, &listed(&origin));
    let args = [
        "soak",
        "api/",
        "--target",
        &origin,
        "--env",
        "dev",
        "--rps",
        "1",
        "--duration",
        "1s",
        "--wire",
        "cl-te-conflict",
        "--allow-wire-faults",
    ];
    let (code, stdout, stderr) = stress(ws.path(), &args);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        stdout.contains("wire cl-te-conflict: 200, 200, 200"),
        "{stdout}"
    );
    assert!(
        stdout.contains("high ambiguous-framing-accepted: cl-te-conflict: the server answered 200"),
        "{stdout}"
    );
}
