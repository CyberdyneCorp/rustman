//! Broker-backed tests (Kafka, MQTT, AMQP) read their broker from an
//! environment variable and are skipped when it is unset, unless
//! [`REQUIRE_VAR`] asks for that protocol: then a missing variable fails
//! the test, so a misconfigured CI job cannot pass without testing
//! anything. Its value is `1` or `all`, or a comma-separated list of
//! protocols (`kafka`, `mqtt`, `amqp`).

/// The variable CI sets in its broker jobs.
pub const REQUIRE_VAR: &str = "RUSTMAN_REQUIRE_BROKERS";

/// The value of `name`, a setting of a `protocol` broker test; `None`
/// (skip the test) when it is unset or empty.
///
/// # Panics
///
/// When it is unset but [`REQUIRE_VAR`] requires `protocol` brokers.
pub fn env(protocol: &str, name: &str) -> Option<String> {
    let require = std::env::var(REQUIRE_VAR).ok();
    match setting(protocol, name, std::env::var(name).ok(), require.as_deref()) {
        Ok(value) => value,
        Err(message) => panic!("{message}"),
    }
}

/// `value` of `name` for a `protocol` test, given the [`REQUIRE_VAR`]
/// value `require`: `Err` when it is missing but required.
pub fn setting(
    protocol: &str,
    name: &str,
    value: Option<String>,
    require: Option<&str>,
) -> Result<Option<String>, String> {
    let value = value.filter(|v| !v.is_empty());
    match value {
        None if requires(require, protocol) => Err(format!(
            "{name} is not set, but {REQUIRE_VAR} requires the {protocol} broker tests to run"
        )),
        None => {
            eprintln!("skipped: {name} is not set");
            Ok(None)
        }
        some => Ok(some),
    }
}

fn requires(require: Option<&str>, protocol: &str) -> bool {
    let wanted =
        |p: &str| p == "1" || p.eq_ignore_ascii_case("all") || p.eq_ignore_ascii_case(protocol);
    require.is_some_and(|r| r.split(',').map(str::trim).any(wanted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_required_protocol_fails_instead_of_skipping() {
        // Regression: broker tests passed silently when CI lost the env var.
        let missing = setting("kafka", "RUSTMAN_KAFKA_BROKERS", None, Some("1"));
        assert_eq!(
            missing.unwrap_err(),
            "RUSTMAN_KAFKA_BROKERS is not set, but RUSTMAN_REQUIRE_BROKERS requires the kafka broker tests to run"
        );
        let empty = setting(
            "mqtt",
            "RUSTMAN_MQTT_BROKER",
            Some(String::new()),
            Some("kafka, mqtt"),
        );
        assert!(empty.is_err());
        assert!(setting("amqp", "X", None, Some("all")).is_err());
    }

    #[test]
    fn other_protocols_and_unset_requirements_skip() {
        assert_eq!(setting("amqp", "X", None, Some("kafka")), Ok(None));
        assert_eq!(setting("amqp", "X", None, None), Ok(None));
        assert_eq!(setting("amqp", "X", None, Some("")), Ok(None));
        let set = setting("kafka", "X", Some("127.0.0.1:9092".into()), Some("1"));
        assert_eq!(set, Ok(Some("127.0.0.1:9092".to_owned())));
    }
}
