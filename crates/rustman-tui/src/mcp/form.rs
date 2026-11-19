//! Argument forms of the MCP inspector (mcp-client "Schema-driven argument
//! form"): one row per property of a tool's `inputSchema` (or per prompt
//! argument, template variable or elicitation field). Strings and numbers
//! are typed, booleans toggled, `enum`s picked from exactly their values;
//! nested objects, arrays and variants are typed as JSON. The raw JSON
//! editor for the whole object is always one key away.

use rustman_mcp::client::{Field, FieldKind};
use serde_json::{Map, Value};

/// How one row is edited.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// Free text: a string (with its `format`, if any).
    Text {
        format: Option<String>,
    },
    Number,
    Integer,
    Boolean,
    /// A picker over exactly these values.
    Pick {
        values: Vec<Value>,
    },
    /// A nested object, array or variant, typed as JSON.
    Json,
}

/// One property of the form.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    pub required: bool,
    pub description: Option<String>,
    pub input: Input,
    /// The typed text (text, number, integer and JSON rows).
    pub text: String,
    /// The picked value (boolean and picker rows); `None`: not set.
    pub choice: Option<usize>,
}

/// What a form is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Tool(String),
    Prompt(String),
    /// A resource template: its variables expand the URI.
    Template(String),
    /// The open elicitation dialog.
    Elicitation,
}

/// A form being filled.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub target: Target,
    pub rows: Vec<Row>,
    pub selected: usize,
    /// The raw JSON editor replaces the rows.
    pub raw: Option<String>,
    /// Why the last submit was refused (invalid JSON, violations).
    pub errors: Vec<String>,
}

impl Row {
    fn new(field: &Field) -> Self {
        let input = match &field.kind {
            FieldKind::String { format } => Input::Text {
                format: format.clone(),
            },
            FieldKind::Number => Input::Number,
            FieldKind::Integer => Input::Integer,
            FieldKind::Boolean => Input::Boolean,
            FieldKind::Enum { values } => Input::Pick {
                values: values.clone(),
            },
            FieldKind::Object { .. }
            | FieldKind::Array { .. }
            | FieldKind::Variant { .. }
            | FieldKind::Raw { .. } => Input::Json,
        };
        let mut row = Self {
            name: field.name.clone(),
            required: field.required,
            description: field.description.clone(),
            input,
            text: String::new(),
            choice: None,
        };
        if let Some(default) = &field.default {
            row.set(default);
        }
        row
    }

    /// A plain string row (prompt arguments, template variables).
    pub fn text(name: &str, required: bool, description: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            required,
            description,
            input: Input::Text { format: None },
            text: String::new(),
            choice: None,
        }
    }

    fn set(&mut self, value: &Value) {
        match &self.input {
            Input::Boolean => self.choice = value.as_bool().map(usize::from),
            Input::Pick { values } => self.choice = values.iter().position(|v| v == value),
            Input::Text { .. } => self.text = value.as_str().unwrap_or_default().to_owned(),
            Input::Number | Input::Integer | Input::Json => self.text = value.to_string(),
        }
    }

    /// The type shown next to the name.
    pub fn kind_label(&self) -> String {
        match &self.input {
            Input::Text { format: Some(f) } => format!("string ({f})"),
            Input::Text { format: None } => "string".to_owned(),
            Input::Number => "number".to_owned(),
            Input::Integer => "integer".to_owned(),
            Input::Boolean => "boolean".to_owned(),
            Input::Pick { .. } => "one of".to_owned(),
            Input::Json => "JSON".to_owned(),
        }
    }

    /// The value as shown in the row.
    pub fn shown(&self) -> String {
        match &self.input {
            Input::Boolean => match self.choice {
                Some(1) => "true".to_owned(),
                Some(_) => "false".to_owned(),
                None => String::new(),
            },
            Input::Pick { values } => values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let text = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                    if Some(i) == self.choice {
                        format!("[{text}]")
                    } else {
                        text
                    }
                })
                .collect::<Vec<_>>()
                .join(" | "),
            _ => self.text.clone(),
        }
    }

    fn typed(&self) -> bool {
        matches!(
            self.input,
            Input::Text { .. } | Input::Number | Input::Integer | Input::Json
        )
    }

    /// The row's value; `Ok(None)` when it is left empty.
    fn value(&self) -> Result<Option<Value>, String> {
        let invalid = |what: &str| format!("/{}: expected {what}", self.name);
        match &self.input {
            Input::Boolean => Ok(self.choice.map(|c| Value::Bool(c == 1))),
            Input::Pick { values } => Ok(self.choice.and_then(|c| values.get(c).cloned())),
            _ if self.text.is_empty() => Ok(None),
            Input::Text { .. } => Ok(Some(Value::String(self.text.clone()))),
            Input::Integer => self
                .text
                .trim()
                .parse::<i64>()
                .map(|n| Some(Value::from(n)))
                .map_err(|_| invalid("integer")),
            Input::Number => self
                .text
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(|n| Some(Value::Number(n)))
                .ok_or_else(|| invalid("number")),
            Input::Json => serde_json::from_str(&self.text)
                .map(Some)
                .map_err(|_| invalid("JSON")),
        }
    }
}

impl Form {
    /// The form of a JSON schema (a tool's `inputSchema`, a requested
    /// schema): one row per property of the root object.
    pub fn of_schema(target: Target, field: &Field) -> Self {
        let rows = match &field.kind {
            FieldKind::Object { fields } => fields.iter().map(Row::new).collect(),
            _ => Vec::new(),
        };
        let raw = matches!(field.kind, FieldKind::Raw { .. }).then(|| "{}".to_owned());
        Self::with_rows(target, rows, raw)
    }

    pub fn with_rows(target: Target, rows: Vec<Row>, raw: Option<String>) -> Self {
        Self {
            target,
            rows,
            selected: 0,
            raw,
            errors: Vec::new(),
        }
    }

    pub fn row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }

    /// The values as a JSON object (empty rows left out), or why not.
    pub fn value(&self) -> Result<Value, Vec<String>> {
        if let Some(raw) = &self.raw {
            return match serde_json::from_str::<Value>(raw) {
                Ok(value @ Value::Object(_)) => Ok(value),
                Ok(_) => Err(vec!["/: expected a JSON object".to_owned()]),
                Err(e) => Err(vec![format!("/: invalid JSON: {e}")]),
            };
        }
        let mut object = Map::new();
        let mut errors = Vec::new();
        for row in &self.rows {
            match row.value() {
                Ok(Some(value)) => {
                    object.insert(row.name.clone(), value);
                }
                Ok(None) => {}
                Err(e) => errors.push(e),
            }
        }
        if errors.is_empty() {
            Ok(Value::Object(object))
        } else {
            Err(errors)
        }
    }

    /// The values as strings (prompt arguments, template variables).
    pub fn strings(&self) -> std::collections::BTreeMap<String, String> {
        self.rows
            .iter()
            .filter(|r| !r.text.is_empty())
            .map(|r| (r.name.clone(), r.text.clone()))
            .collect()
    }

    /// Switch between the rows and the raw JSON editor, carrying the values
    /// over when they are valid.
    pub fn toggle_raw(&mut self) {
        match self.raw.take() {
            Some(raw) => {
                if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) {
                    for row in &mut self.rows {
                        if let Some(value) = map.get(&row.name) {
                            row.set(value);
                        }
                    }
                }
            }
            None => {
                let value = self.value().unwrap_or_else(|_| Value::Object(Map::new()));
                self.raw = Some(value.to_string());
            }
        }
    }

    pub fn move_by(&mut self, by: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
    }

    /// Type `c` into the raw editor or the selected text row; a space
    /// toggles a boolean row.
    pub fn type_char(&mut self, c: char) {
        if let Some(raw) = &mut self.raw {
            raw.push(c);
            return;
        }
        let Some(row) = self.rows.get_mut(self.selected) else {
            return;
        };
        if row.typed() {
            row.text.push(c);
        } else if c == ' ' {
            row.cycle(1);
        }
    }

    pub fn backspace(&mut self) {
        if let Some(raw) = &mut self.raw {
            raw.pop();
        } else if let Some(row) = self.rows.get_mut(self.selected) {
            row.text.pop();
        }
    }

    /// Pick the previous or next value of a picker or boolean row.
    pub fn cycle(&mut self, by: isize) {
        if self.raw.is_none()
            && let Some(row) = self.rows.get_mut(self.selected)
        {
            row.cycle(by);
        }
    }
}

impl Row {
    fn cycle(&mut self, by: isize) {
        let count = match &self.input {
            Input::Boolean => 2,
            Input::Pick { values } => values.len(),
            _ => return,
        };
        if count == 0 {
            return;
        }
        let next = match self.choice {
            None if by < 0 => count - 1,
            None => 0,
            Some(c) => (c as isize + by).rem_euclid(count as isize) as usize,
        };
        self.choice = Some(next);
    }
}

#[cfg(test)]
mod tests {
    use rustman_mcp::client::schema::form;
    use serde_json::json;

    use super::*;

    fn tool_form() -> Form {
        let schema = json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string", "description": "Where"},
                "mode": {"enum": ["fast", "safe"]},
                "limit": {"type": "integer", "default": 10},
                "deep": {"type": "boolean"},
                "filter": {"type": "object", "properties": {"ext": {"type": "string"}}}
            }
        });
        Form::of_schema(Target::Tool("search".into()), &form(&schema))
    }

    #[test]
    fn an_enum_is_a_picker_with_exactly_its_values() {
        let form = tool_form();
        let mode = form.rows.iter().find(|r| r.name == "mode").unwrap();
        assert_eq!(
            mode.input,
            Input::Pick {
                values: vec![json!("fast"), json!("safe")]
            }
        );
        assert_eq!(mode.shown(), "fast | safe");
    }

    #[test]
    fn rows_become_a_json_object() {
        let mut form = tool_form();
        let at = |form: &Form, name: &str| form.rows.iter().position(|r| r.name == name).unwrap();
        form.selected = at(&form, "path");
        "src".chars().for_each(|c| form.type_char(c));
        form.selected = at(&form, "mode");
        form.cycle(1);
        form.cycle(1);
        form.selected = at(&form, "deep");
        form.type_char(' ');
        form.selected = at(&form, "filter");
        r#"{"ext":"rs"}"#.chars().for_each(|c| form.type_char(c));
        assert_eq!(
            form.value().unwrap(),
            json!({"path": "src", "mode": "safe", "limit": 10, "deep": false, "filter": {"ext": "rs"}})
        );
        assert!(form.rows[at(&form, "path")].required);
        form.selected = at(&form, "limit");
        form.backspace();
        form.backspace();
        form.type_char('x');
        assert_eq!(form.value().unwrap_err(), ["/limit: expected integer"]);
    }

    #[test]
    fn the_raw_editor_carries_the_values_both_ways() {
        let mut form = tool_form();
        form.toggle_raw();
        assert_eq!(form.raw.as_deref(), Some(r#"{"limit":10}"#));
        form.raw = Some(r#"{"path":"a","mode":"safe"}"#.into());
        form.toggle_raw();
        assert!(form.raw.is_none());
        let value = form.value().unwrap();
        assert_eq!(value["path"], "a");
        assert_eq!(value["mode"], "safe");
    }
}
