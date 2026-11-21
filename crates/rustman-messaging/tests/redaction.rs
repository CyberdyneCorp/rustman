//! The SASL password of a Kafka request never reaches an output path
//! unredacted (add-messaging-client task 2.3): the exchange record kept
//! for history, the resolved request's `Debug` and error messages.

mod common;

use common::{KAFKA_PW, resolve};
use rustman_messaging::{MessagingError, MessagingExchange};

const CONSUME: &str = "\
version: 1
kind: kafka
name: Watch
operation: consume
brokers: 127.0.0.1:1
topic: orders
security:
  protocol: SASL_SSL
  mechanism: SCRAM-SHA-512
  username: app
  password: '{{kafka_pw}}'
";

#[test]
fn the_exchange_record_shows_the_password_as_its_secret_name() {
    let resolved = resolve(CONSUME, "");
    let exchange = MessagingExchange::start(&resolved);
    let recorded = exchange.to_json(&resolved.redactor()).to_string();
    assert!(!recorded.contains(KAFKA_PW), "{recorded}");
    assert!(recorded.contains("«redacted:kafka_pw»"), "{recorded}");
    assert!(
        recorded.contains("\"mechanism\":\"SCRAM-SHA-512\""),
        "{recorded}"
    );
    let shown = format!("{resolved:?}");
    assert!(!shown.contains(KAFKA_PW), "{shown}");
}

#[test]
fn error_messages_are_redacted() {
    let resolved = resolve(CONSUME, "");
    let err = MessagingError::broker("kafka", format!("refused {KAFKA_PW}"))
        .redacted(&resolved.redactor());
    assert_eq!(err.to_string(), "kafka: refused «redacted:kafka_pw»");
}

#[tokio::test]
async fn a_refused_connection_fails_without_the_password() {
    // A port that was just free, rather than port 1. Windows retries a
    // refused connect for about 2 s, so the error may be a refusal or the
    // retry deadline; both start with `kafka: cannot connect`.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    let text = CONSUME
        .replace("SASL_SSL", "SASL_PLAINTEXT")
        .replace("127.0.0.1:1", &addr);
    let resolved = resolve(&text, "");
    let mut options = rustman_messaging::RunOptions::headless(&resolved);
    options.connect_timeout = std::time::Duration::from_secs(1);
    let err = rustman_messaging::run(&resolved, options)
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with("kafka: cannot connect"), "{text}");
    assert!(!text.contains(KAFKA_PW), "{text}");
}

#[test]
fn a_literal_security_password_is_redacted_as_password() {
    let literal = "LITERAL-kafka-pw-5150";
    let resolved = resolve(&CONSUME.replace("'{{kafka_pw}}'", literal), "");
    let recorded = MessagingExchange::start(&resolved)
        .to_json(&resolved.redactor())
        .to_string();
    assert!(!recorded.contains(literal), "{recorded}");
    assert!(
        recorded.contains("\"password\":\"«redacted:password»\""),
        "{recorded}"
    );
}

#[test]
fn a_url_password_holding_a_slash_is_a_secret_and_never_echoed() {
    // Regression: the userinfo was cut at the first '/', so this password
    // was echoed by the invalid-broker error and never redacted.
    let broker = "amqp://guest:SLASH/PW@rabbit/";
    let text = format!(
        "version: 1\nkind: amqp\nname: a\noperation: consume\nbroker: '{broker}'\nexchange: x\n"
    );
    let resolved = resolve(&text, "");
    let redacted = resolved
        .redactor()
        .text(&format!("dial {broker}"))
        .into_owned();
    assert!(!redacted.contains("PW"), "{redacted}");
    let err = rustman_messaging::endpoint::amqp(broker)
        .unwrap_err()
        .to_string();
    assert!(!err.contains("SLASH"), "{err}");
    let kafka = "version: 1\nkind: kafka\nname: k\noperation: consume\nbrokers: 'app:K/PW@h:9092'\ntopic: t\n";
    let resolved = resolve(kafka, "");
    let redacted = resolved.redactor().text("app:K/PW@h:9092").into_owned();
    assert!(!redacted.contains("PW"), "{redacted}");
}
