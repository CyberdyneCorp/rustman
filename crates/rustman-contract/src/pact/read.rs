//! Reading Pact files (v3 and v4) for provider verification. HTTP
//! interactions are read; v4 message interactions are skipped with a
//! warning.

use serde_json::{Map, Value};

use super::model::Pact;
use super::model::{
    Interaction, PactRequest, PactResponse, PactVersion, ProviderState, RuleSet, Rules,
};

/// A Pact file read, with what was skipped.
#[derive(Clone, Debug)]
pub struct ReadPact {
    pub pact: Pact,
    pub warnings: Vec<String>,
}

/// Read a Pact file's JSON text. `name` names it in messages.
pub fn read(name: &str, text: &str) -> Result<ReadPact, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("{name}: not a JSON Pact file: {e}"))?;
    let version = version(&value).map_err(|e| format!("{name}: {e}"))?;
    let mut warnings = Vec::new();
    let mut interactions = Vec::new();
    let all = value["interactions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for (index, raw) in all.iter().enumerate() {
        let kind = raw["type"].as_str().unwrap_or("Synchronous/HTTP");
        if kind != "Synchronous/HTTP" {
            warnings.push(format!(
                "{name}: interaction {index} ({kind}) skipped: only HTTP interactions are verified"
            ));
            continue;
        }
        let interaction =
            interaction(raw, version).map_err(|e| format!("{name}: interaction {index}: {e}"))?;
        interactions.push(interaction);
    }
    let pact = Pact {
        consumer: name_of(&value["consumer"]),
        provider: name_of(&value["provider"]),
        version,
        interactions,
    };
    Ok(ReadPact { pact, warnings })
}

fn name_of(pacticipant: &Value) -> String {
    pacticipant["name"].as_str().unwrap_or_default().to_owned()
}

fn version(value: &Value) -> Result<PactVersion, String> {
    let metadata = &value["metadata"];
    let text = metadata["pactSpecification"]["version"]
        .as_str()
        .or_else(|| metadata["pact-specification"]["version"].as_str())
        .unwrap_or("3.0.0");
    PactVersion::parse(text)
        .ok_or_else(|| format!("unsupported Pact specification version {text} (expected 3 or 4)"))
}

fn interaction(raw: &Value, version: PactVersion) -> Result<Interaction, String> {
    let request = &raw["request"];
    let response = &raw["response"];
    let status = response["status"]
        .as_u64()
        .and_then(|s| u16::try_from(s).ok())
        .ok_or("the response has no status")?;
    Ok(Interaction {
        description: raw["description"].as_str().unwrap_or_default().to_owned(),
        provider_states: states(raw)?,
        request: PactRequest {
            method: request["method"].as_str().unwrap_or("GET").to_uppercase(),
            path: request["path"].as_str().unwrap_or("/").to_owned(),
            query: query(&request["query"]),
            headers: headers(&request["headers"]),
            body: body(&request["body"], version),
            rules: rules(&request["matchingRules"])?,
        },
        response: PactResponse {
            status,
            headers: headers(&response["headers"]),
            body: body(&response["body"], version),
            rules: rules(&response["matchingRules"])?,
        },
    })
}

fn states(raw: &Value) -> Result<Vec<ProviderState>, String> {
    if let Some(name) = raw["providerState"].as_str() {
        return Ok(vec![ProviderState {
            name: name.to_owned(),
            params: Map::new(),
        }]);
    }
    match raw.get("providerStates") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(states) => serde_json::from_value(states.clone())
            .map_err(|e| format!("invalid providerStates: {e}")),
    }
}

/// `{"a": ["1", "2"]}`, `{"a": "1"}` or (v2) `"a=1&a=2"`.
fn query(raw: &Value) -> Vec<(String, Vec<String>)> {
    match raw {
        Value::Object(map) => map
            .iter()
            .map(|(name, values)| (name.clone(), strings(values)))
            .collect(),
        Value::String(text) => {
            let mut out: Vec<(String, Vec<String>)> = Vec::new();
            for (name, value) in form_urlencoded::parse(text.as_bytes()).into_owned() {
                match out.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, values)) => values.push(value),
                    None => out.push((name, vec![value])),
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().map(text_of).collect(),
        other => vec![text_of(other)],
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Header values are strings, or (v4) lists joined with `, `.
fn headers(raw: &Value) -> Vec<(String, String)> {
    raw.as_object()
        .map(|map| {
            map.iter()
                .map(|(name, value)| (name.clone(), strings(value).join(", ")))
                .collect()
        })
        .unwrap_or_default()
}

/// The body: as is in v3, the `content` of the v4 wrapper.
fn body(raw: &Value, version: PactVersion) -> Option<Value> {
    match (raw, version) {
        (Value::Null, _) => None,
        (Value::Object(map), PactVersion::V4) if map.contains_key("content") => {
            let content = map["content"].clone();
            Some(json_text(content, map.get("contentType")))
        }
        (other, _) => Some(other.clone()),
    }
}

/// A JSON body written as text is read as JSON.
fn json_text(content: Value, content_type: Option<&Value>) -> Value {
    let json = content_type
        .and_then(Value::as_str)
        .is_some_and(|t| t.contains("json"));
    match &content {
        Value::String(text) if json => serde_json::from_str(text).unwrap_or(content),
        _ => content,
    }
}

fn rules(raw: &Value) -> Result<Rules, String> {
    let Some(map) = raw.as_object() else {
        return Ok(Rules::default());
    };
    let mut out = Rules::default();
    for (category, value) in map {
        match category.as_str() {
            "body" => out.body = keyed(value)?,
            "header" | "headers" => out.header = keyed(value)?,
            "query" => out.query = keyed(value)?,
            "path" => out.path = Some(rule_set(value)?),
            "status" => out.status = flat_or_root(value)?,
            _ => {}
        }
    }
    Ok(out)
}

fn keyed(value: &Value) -> Result<Vec<(String, RuleSet)>, String> {
    let Some(map) = value.as_object() else {
        return Ok(Vec::new());
    };
    map.iter()
        .map(|(key, set)| rule_set(set).map(|set| (key.clone(), set)))
        .collect()
}

/// A rule set written flat, or under the root path `$`.
fn flat_or_root(value: &Value) -> Result<Option<RuleSet>, String> {
    if value.get("matchers").is_some() {
        return rule_set(value).map(Some);
    }
    match value.get("$").or_else(|| value.get("")) {
        Some(set) => rule_set(set).map(Some),
        None => Ok(None),
    }
}

fn rule_set(value: &Value) -> Result<RuleSet, String> {
    serde_json::from_value(value.clone()).map_err(|e| format!("invalid matching rules: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_files_unwrap_bodies_and_skip_messages() {
        let text = serde_json::json!({
            "consumer": {"name": "web"},
            "provider": {"name": "users"},
            "interactions": [
                {"type": "Synchronous/HTTP", "description": "get user",
                 "providerStates": [{"name": "user 42 exists", "params": {"id": 42}}],
                 "request": {"method": "get", "path": "/users/42", "query": {"a": ["1", "2"]},
                             "headers": {"Accept": ["application/json"]}},
                 "response": {"status": 200,
                              "body": {"content": "{\"id\":42}", "contentType": "application/json", "encoded": false},
                              "matchingRules": {"body": {"$.id": {"combine": "AND", "matchers": [{"match": "integer"}]}},
                                                "status": {"$": {"matchers": [{"match": "statusCode", "status": "success"}]}}}}},
                {"type": "Asynchronous/Messages", "description": "event"},
            ],
            "metadata": {"pactSpecification": {"version": "4.0"}},
        })
        .to_string();
        let read = read("p.json", &text).unwrap();
        assert_eq!(read.warnings.len(), 1, "{:?}", read.warnings);
        let interaction = &read.pact.interactions[0];
        assert_eq!(interaction.request.method, "GET");
        assert_eq!(interaction.request.query[0].1, vec!["1", "2"]);
        assert_eq!(interaction.provider_states[0].params["id"], 42);
        assert_eq!(
            interaction.response.body,
            Some(serde_json::json!({"id": 42}))
        );
        assert_eq!(interaction.response.rules.body[0].0, "$.id");
        assert!(interaction.response.rules.status.is_some());
    }

    #[test]
    fn v3_files_and_unsupported_versions() {
        let text = serde_json::json!({
            "interactions": [{"description": "d", "providerState": "s",
                              "request": {"method": "GET", "path": "/", "query": "a=1&a=2"},
                              "response": {"status": 204}}],
            "metadata": {"pactSpecification": {"version": "3.0.0"}},
        })
        .to_string();
        let read = read("p.json", &text).unwrap();
        assert_eq!(read.pact.version, PactVersion::V3);
        let interaction = &read.pact.interactions[0];
        assert_eq!(interaction.provider_states[0].name, "s");
        assert_eq!(
            interaction.request.query,
            vec![("a".into(), vec!["1".into(), "2".into()])]
        );
        let old =
            r#"{"interactions": [], "metadata": {"pactSpecification": {"version": "2.0.0"}}}"#;
        assert_eq!(
            super::read("p.json", old).unwrap_err(),
            "p.json: unsupported Pact specification version 2.0.0 (expected 3 or 4)"
        );
    }
}
