//! Helpers for the bench tests: a workspace pointed at the counting
//! server, and a bench over it. Nothing leaves the loopback interface.
#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use rustman_bench::{
    Bench, BenchError, BenchInput, BenchOutcome, ConstantVus, DataFeed, DataMode, Exhaust, Limit,
    Profile, Safety, ThinkTime, Threshold,
};
use rustman_core::data_rows::{DataRows, SecretColumns};
use rustman_core::model::Variables;
use rustman_core::run::{DataRun, plan};
use rustman_core::safety::Permission;
use rustman_core::{SecretBackend, SecretStore, Workspace};
use rustman_http::runner::RunContext;
use tokio_util::sync::CancellationToken;

pub fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A request file: `method url`, with extra YAML lines.
pub fn request(method: &str, url: &str, extra: &str) -> String {
    format!("version: 1\nkind: http\nname: r\nmethod: {method}\nurl: \"{url}\"\n{extra}")
}

/// A workspace with `settings` appended to `rustman.toml` and a `dev`
/// environment whose `base_url` is `origin`.
pub fn workspace(origin: &str, settings: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n{settings}"),
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {origin}\n"),
    );
    dir
}

pub fn constant(vus: u32, limit: Limit) -> Profile {
    Profile::ConstantVus(ConstantVus {
        vus,
        limit,
        think: ThinkTime::None,
    })
}

/// How a test bench differs from the defaults.
pub struct Setup<'a> {
    pub target: String,
    pub profile: Profile,
    pub permission: Permission,
    pub production: Option<&'a str>,
    pub env: &'a str,
    /// A data file of the workspace and its mode.
    pub data: Option<(&'a str, DataMode, Exhaust)>,
    pub thresholds: Vec<Threshold>,
    /// Replaces the 10-second warm-up of abort-on-fail thresholds.
    pub warmup: Option<Duration>,
    /// Interrupt the bench (a first Ctrl+C) this long after its start.
    pub interrupt_after: Option<Duration>,
}

impl<'a> Setup<'a> {
    pub fn new(target: &str, profile: Profile) -> Self {
        Self {
            target: target.to_owned(),
            profile,
            permission: Permission::ReadOnly,
            production: None,
            env: "dev",
            data: None,
            thresholds: Vec::new(),
            warmup: None,
            interrupt_after: None,
        }
    }
}

/// Load the workspace at `root`, prepare a bench of all its requests with
/// `setup` and run it, sampling the active VUs at each of `sample_at`
/// (from the start).
pub async fn bench(
    root: &Path,
    setup: Setup<'_>,
    sample_at: &[Duration],
) -> Result<(BenchOutcome, Vec<usize>), BenchError> {
    let ws = Workspace::load(root).unwrap();
    let env = ws.environments.iter().find(|e| e.name == setup.env);
    let secrets = SecretStore::open(root, SecretBackend::File);
    let plan = plan(&ws, &[], &[]);
    let ctx = RunContext {
        root,
        settings: &ws.settings,
        environment: env,
        secrets: &secrets,
        in_workspace: true,
    };
    let interrupt = CancellationToken::new();
    if let Some(after) = setup.interrupt_after {
        let interrupt = interrupt.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            interrupt.cancel();
        });
    }
    let data = setup.data.map(|(file, mode, exhaust)| {
        let rows = DataRows::read(&root.join(file)).unwrap();
        DataFeed {
            run: DataRun::new(rows, SecretColumns::new(Vec::new()), None).unwrap(),
            mode,
            exhaust,
        }
    });
    let input = BenchInput {
        ctx,
        plan: &plan,
        profile: setup.profile,
        safety: Safety {
            target: &setup.target,
            permission: setup.permission,
            production: setup.production,
        },
        overrides: Variables::default(),
        timeout: Some(Duration::from_secs(10)),
        insecure: false,
        data,
        thresholds: setup.thresholds,
        interrupt: interrupt.clone(),
        cancel: CancellationToken::new(),
    };
    let mut bench = Bench::prepare(input)?;
    if let Some(warmup) = setup.warmup {
        bench = bench.with_abort_warmup(warmup);
    }
    let metrics = bench.metrics();
    let start = Instant::now();
    let sampler = async {
        let mut samples = Vec::new();
        for at in sample_at {
            tokio::time::sleep_until((start + *at).into()).await;
            samples.push(metrics.active_vus());
        }
        samples
    };
    let (outcome, samples) = tokio::join!(bench.run(), sampler);
    Ok((outcome?, samples))
}
