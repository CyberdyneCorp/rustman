//! Printing messages: consumed ones as they arrive, from the live feed
//! (redacted with the request's secrets before it reaches here), and
//! produced ones from the finished, redacted record.

use std::io::Write;

use rustman_core::run::RequestRecord;
use serde_json::Value;

/// How messages are printed: a short line each, or one JSON object per
/// message (`--json`).
#[derive(Clone, Copy, Debug)]
pub struct Printer {
    json: bool,
}

impl Printer {
    pub fn new(json: bool) -> Self {
        Self { json }
    }

    /// The line a consumed `message` prints as: with `--json` the message
    /// (partition, offset, timestamp, key, headers, value...), else where
    /// it came from and its value.
    pub fn consumed_line(self, message: &Value) -> String {
        if self.json {
            return message.to_string();
        }
        let value = payload(message.get("value"));
        match origin(message) {
            Some(origin) => format!("{origin} {value}"),
            None => value,
        }
    }

    /// The line a produced `message` prints as.
    pub fn produced_line(self, message: &Value) -> String {
        if self.json {
            return message.to_string();
        }
        let mut line = "produced".to_owned();
        if let Some(partition) = message.get("partition") {
            line.push_str(&format!(" partition {partition}"));
        }
        if let Some(offset) = message.get("offset") {
            line.push_str(&format!(" offset {offset}"));
        }
        if let Some(ack) = message.get("ack").and_then(Value::as_str) {
            line.push_str(&format!(" ({ack})"));
        }
        line
    }

    pub fn consumed(self, message: &Value) {
        print(&self.consumed_line(message));
    }

    /// Print what the produce of `record` sent.
    pub fn produced(self, record: &RequestRecord) {
        let exchange = record.messaging.as_ref().and_then(|m| m.exchange.as_ref());
        let produced = exchange
            .and_then(|e| e.get("produced"))
            .and_then(Value::as_array);
        for message in produced.into_iter().flatten() {
            print(&self.produced_line(message));
        }
    }
}

/// Where `message` came from: `p0@42` (Kafka), its topic (MQTT) or its
/// routing key (AMQP).
fn origin(message: &Value) -> Option<String> {
    if let (Some(partition), Some(offset)) = (message.get("partition"), message.get("offset")) {
        return Some(format!("p{partition}@{offset}"));
    }
    let topic = message.get("topic").or_else(|| message.get("routing_key"));
    topic.and_then(Value::as_str).map(str::to_owned)
}

/// A value as printed: text as is, anything else as JSON.
fn payload(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => "null".to_owned(),
    }
}

/// A closed stdout (`| head`) is not worth an error: the operation still
/// ends at its bounds.
fn print(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}").and_then(|()| out.flush());
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn consumed_messages_print_their_origin_and_value() {
        let plain = Printer::new(false);
        let kafka =
            json!({"partition": 0, "offset": 7, "key": "k", "headers": [], "value": {"id": 1}});
        assert_eq!(plain.consumed_line(&kafka), "p0@7 {\"id\":1}");
        let mqtt = json!({"topic": "sensors/a", "qos": 1, "headers": [], "value": "21.5"});
        assert_eq!(plain.consumed_line(&mqtt), "sensors/a 21.5");
        let json = Printer::new(true).consumed_line(&kafka);
        assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), kafka);
    }

    #[test]
    fn produced_messages_print_where_they_landed() {
        let plain = Printer::new(false);
        let kafka = json!({"partition": 0, "offset": 3, "value": "a"});
        assert_eq!(plain.produced_line(&kafka), "produced partition 0 offset 3");
        let mqtt = json!({"ack": "PUBACK", "value": "a"});
        assert_eq!(plain.produced_line(&mqtt), "produced (PUBACK)");
    }
}
