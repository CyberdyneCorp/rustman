//! `rustman kafka|mqtt|amqp produce|consume` (add-messaging-client task
//! 6.1) against the in-process MQTT broker: stdin produce, consume to JSON
//! lines, secrets redacted in the printed messages, and usage errors.
//!
//! The Kafka "consume to JSON lines" scenario runs only when the CI
//! `messaging-kafka` job sets `RUSTMAN_KAFKA_BROKERS`. Secrets use
//! `RUSTMAN_SECRET_STORE=file`, so the OS keychain is never touched.

use std::fs;
use std::path::Path;
use std::process::{Command as Process, Output, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin;
use predicates::prelude::PredicateBooleanExt;
use rustman_core::{Secret, SecretBackend, SecretStore};
use rustman_test_support::mqtt_broker::{Mode, MqttBroker};
use serde_json::Value;

const MQTT_PW: &str = "cli-mqtt-canary-7d41";

fn rustman(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("rustman").expect("rustman binary is built");
    cmd.current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env_remove("RUSTMAN_ENV");
    cmd
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A workspace whose `dev` environment holds the secret `mqtt_pw`.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[defaults]\ndefault_environment = \"dev\"\n",
    );
    write(dir.path(), "environments/dev.yaml", "secrets: [mqtt_pw]\n");
    write(dir.path(), ".gitignore", ".rustman/\n");
    SecretStore::open(dir.path(), SecretBackend::File)
        .set("dev", &Secret::new("mqtt_pw", MQTT_PW.to_owned()))
        .unwrap();
    dir
}

/// Start `rustman <args>` in `dir` without waiting for it.
fn spawn(dir: &Path, args: &[&str]) -> std::process::Child {
    Process::new(cargo_bin("rustman"))
        .current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env_remove("RUSTMAN_ENV")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Wait until the broker logged a subscription.
async fn subscribed(broker: &MqttBroker) {
    let started = Instant::now();
    while !broker.log().iter().any(|l| l.starts_with("SUBSCRIBE")) {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "{:?}",
            broker.log()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait(child: std::process::Child) -> Output {
    tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap()
}

fn json_lines(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn stdin_lines_are_published_one_message_each() {
    // Scenario "Produce from stdin".
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    let dir = tempfile::tempdir().unwrap();
    let url = broker.url();
    let path = dir.path().to_owned();
    let assert = tokio::task::spawn_blocking(move || {
        rustman(&path)
            .args(["mqtt", "produce", "--broker", &url, "--topic", "t/1"])
            .write_stdin("first\n{\"n\":2}\r\nthird\n")
            .assert()
    })
    .await
    .unwrap();
    let output = assert.success().get_output().clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 3, "{stdout}");
    assert!(stdout.lines().all(|l| l == "produced (PUBACK)"), "{stdout}");
    let published: Vec<String> = broker
        .log()
        .into_iter()
        .filter(|l| l.starts_with("PUBLISH t/1"))
        .collect();
    assert_eq!(published.len(), 3, "{published:?}");
    assert!(published[1].ends_with("{\"n\":2}"), "{published:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn consume_prints_at_most_max_messages_json_lines() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    let dir = tempfile::tempdir().unwrap();
    let url = broker.url();
    let consumer = spawn(
        dir.path(),
        &[
            "mqtt",
            "consume",
            "--broker",
            &url,
            "--topic",
            "sensors/+/temp",
            "--max-messages",
            "2",
            "--duration",
            "20s",
            "--json",
        ],
    );
    subscribed(&broker).await;
    let path = dir.path().to_owned();
    tokio::task::spawn_blocking(move || {
        rustman(&path)
            .args([
                "mqtt",
                "produce",
                "--broker",
                &url,
                "--topic",
                "sensors/room1/temp",
            ])
            .args(["--value", "21.5", "--value", "{\"c\":22}", "--value", "23"])
            .assert()
            .success();
    })
    .await
    .unwrap();
    let output = wait(consumer).await;
    assert!(output.status.success(), "{output:?}");
    let lines = json_lines(&output);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["topic"], "sensors/room1/temp");
    assert_eq!(lines[0]["value"], 21.5);
    assert_eq!(lines[1]["value"]["c"], 22);
    assert!(lines[0]["headers"].is_array());
}

#[tokio::test(flavor = "multi_thread")]
async fn printed_messages_and_errors_never_show_a_secret() {
    let broker = MqttBroker::start(Mode::Tcp, None).await;
    broker.require_login("app", MQTT_PW);
    let ws = workspace();
    let auth = "auth: {username: app, password: '{{mqtt_pw}}'}";
    write(
        ws.path(),
        "events/listen.rman.yaml",
        &format!(
            "version: 1\nkind: mqtt\nname: Listen\noperation: consume\nbroker: '{}'\n{auth}\ntopic: 'leak/#'\nlimits: {{max_messages: 1, duration_ms: 20000}}\n",
            broker.url()
        ),
    );
    write(
        ws.path(),
        "events/leak.rman.yaml",
        &format!(
            "version: 1\nkind: mqtt\nname: Leak\noperation: produce\nbroker: '{}'\n{auth}\ntopic: leak/1\nmessages:\n- value: {{echo: '{{{{mqtt_pw}}}}'}}\n  headers: [{{name: x-token, value: '{{{{mqtt_pw}}}}'}}]\n",
            broker.url()
        ),
    );
    let consumer = spawn(ws.path(), &["mqtt", "consume", "events/listen", "--json"]);
    subscribed(&broker).await;
    let path = ws.path().to_owned();
    let produced = tokio::task::spawn_blocking(move || {
        rustman(&path)
            .args(["mqtt", "produce", "events/leak", "--json"])
            .assert()
            .success()
            .get_output()
            .clone()
    })
    .await
    .unwrap();
    let output = wait(consumer).await;
    assert!(output.status.success(), "{output:?}");
    for out in [&output, &produced] {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!text.contains(MQTT_PW), "a secret was printed: {text}");
    }
    let lines = json_lines(&output);
    assert_eq!(lines[0]["value"]["echo"], "«redacted:mqtt_pw»");
    assert_eq!(lines[0]["headers"][0]["value"], "«redacted:mqtt_pw»");
    let wrong = rustman(ws.path())
        .args(["mqtt", "produce", "events/leak", "--var", "mqtt_pw=nope"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&wrong.stderr);
    assert!(stderr.contains("error: "), "{stderr}");
    assert!(!stderr.contains(MQTT_PW), "{stderr}");
}

#[test]
fn a_request_file_takes_no_operation_flags_and_produce_needs_messages() {
    let ws = workspace();
    write(
        ws.path(),
        "events/p.rman.yaml",
        "version: 1\nkind: kafka\nname: P\noperation: produce\nbrokers: 127.0.0.1:1\ntopic: t\nmessages: [{value: a}]\n",
    );
    rustman(ws.path())
        .args(["kafka", "produce", "events/p", "--topic", "other"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "a request file already defines its operation",
        ));
    rustman(ws.path())
        .args(["mqtt", "consume", "events/p"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("is not a kind: mqtt request"));
    rustman(ws.path())
        .args(["kafka", "consume", "--topic", "t"])
        .assert()
        .code(2);
}

#[test]
fn input_errors_are_reported_before_connecting() {
    let ws = workspace();
    let refused = rustman(ws.path())
        .args([
            "amqp",
            "consume",
            "--broker",
            "amqp://127.0.0.1:1",
            "--queue",
            "q",
        ])
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8(refused).unwrap();
    assert!(
        refused.starts_with("error: amqp: consuming from an existing queue"),
        "{refused}"
    );
    assert!(!refused.contains("ad-hoc.rman.yaml"), "{refused}");
    rustman(ws.path())
        .args([
            "kafka",
            "consume",
            "--brokers",
            "127.0.0.1:1",
            "--topic",
            "t",
            "--offset",
            "bogus",
        ])
        .assert()
        .code(3)
        .stderr(predicates::str::contains(
            "kafka: offset timestamp 'bogus' is not an RFC 3339 time",
        ))
        .stderr(predicates::str::contains("cannot connect").not());
    write(
        ws.path(),
        "events/f.rman.yaml",
        "version: 1\nkind: kafka\nname: F\noperation: produce\nbrokers: 127.0.0.1:1\ntopic: t\nmessages: [{value: {file: nope.bin}}]\n",
    );
    rustman(ws.path())
        .args(["kafka", "produce", "events/f"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("messaging: cannot read nope.bin"))
        .stderr(predicates::str::contains("cannot connect").not());
}

#[test]
fn kafka_consume_prints_json_lines_with_partition_and_offset() {
    // Scenario "Consume to JSON lines".
    let Some(brokers) = rustman_test_support::brokers::env("kafka", "RUSTMAN_KAFKA_BROKERS") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let topic = format!("rustman-cli-{nanos}");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let client = rskafka_client(&brokers).await;
        client
            .controller_client()
            .unwrap()
            .create_topic(&topic, 1, 1, 10_000)
            .await
            .unwrap();
    });
    rustman(dir.path())
        .args([
            "kafka",
            "produce",
            "--brokers",
            &brokers,
            "--topic",
            &topic,
            "--key",
            "k",
        ])
        .write_stdin("1\n2\n3\n4\n5\n6\n7\n")
        .assert()
        .success();
    let output = rustman(dir.path())
        .args(["kafka", "consume", "--brokers", &brokers, "--topic", &topic])
        .args(["--offset", "earliest", "--max-messages", "5", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let lines = json_lines(&output);
    assert_eq!(lines.len(), 5, "{lines:?}");
    for line in &lines {
        for field in [
            "partition",
            "offset",
            "timestamp",
            "key",
            "headers",
            "value",
        ] {
            assert!(line.get(field).is_some(), "{field} missing: {line}");
        }
    }
}

async fn rskafka_client(brokers: &str) -> rskafka::client::Client {
    rskafka::client::ClientBuilder::new(vec![brokers.to_owned()])
        .build()
        .await
        .unwrap()
}
