//! Synthesized numbers: zero, moved inside the declared bounds and onto
//! `multipleOf`.
//!
//! After the 3.0 rewrite, `exclusiveMinimum`/`exclusiveMaximum` are always
//! numbers (JSON Schema 2020-12).

use serde_json::{Map, Number, Value};

/// The bounds of a numeric schema.
#[derive(Clone, Copy, Debug, Default)]
struct Bounds {
    min: Option<f64>,
    exclusive_min: Option<f64>,
    max: Option<f64>,
    exclusive_max: Option<f64>,
    multiple_of: Option<f64>,
}

impl Bounds {
    fn of(schema: &Map<String, Value>) -> Self {
        let get = |key: &str| schema.get(key).and_then(Value::as_f64);
        Self {
            min: get("minimum"),
            exclusive_min: get("exclusiveMinimum"),
            max: get("maximum"),
            exclusive_max: get("exclusiveMaximum"),
            multiple_of: get("multipleOf").filter(|m| *m > 0.0),
        }
    }

    /// The smallest allowed value, with `step` added past an exclusive one.
    fn low(&self, step: f64) -> Option<f64> {
        let exclusive = self.exclusive_min.map(|m| m + step);
        max_of(self.min, exclusive)
    }

    /// The largest allowed value, with `step` taken off an exclusive one.
    fn high(&self, step: f64) -> Option<f64> {
        let exclusive = self.exclusive_max.map(|m| m - step);
        min_of(self.max, exclusive)
    }
}

/// An integer for `schema`.
pub fn integer(schema: &Map<String, Value>) -> Value {
    let bounds = Bounds::of(schema);
    let low = bounds.low(1.0).map(f64::ceil);
    let high = bounds.high(1.0).map(f64::floor);
    let mut value = start(low, high);
    if let Some(step) = bounds.multiple_of {
        value = (value / step).ceil() * step;
    }
    let value = value.round();
    if value.abs() < 9.0e15 {
        // Exact: the magnitude is far below 2^63 and the value is whole.
        #[allow(clippy::cast_possible_truncation)]
        let whole = value as i64;
        Value::Number(Number::from(whole))
    } else {
        Number::from_f64(value).map_or(Value::Null, Value::Number)
    }
}

/// A number for `schema`. An exclusive bound is stepped past by half the
/// room left, or by one when the other side is open.
pub fn number(schema: &Map<String, Value>) -> Value {
    let bounds = Bounds::of(schema);
    let room = match (bounds.low(0.0), bounds.high(0.0)) {
        (Some(low), Some(high)) => ((high - low) / 2.0).abs(),
        _ => 1.0,
    };
    let mut value = start(bounds.low(room), bounds.high(room));
    if let Some(step) = bounds.multiple_of {
        value = (value / step).ceil() * step;
    }
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// `value` (from [`integer`] or [`number`]) moved up by up to 99 steps
/// (`multipleOf`, else 1), as `pick` says, without passing the upper bound.
/// A fractional `multipleOf` leaves the value as it is.
pub fn shift(schema: &Map<String, Value>, value: &Value, pick: u64) -> Value {
    let bounds = Bounds::of(schema);
    let step = bounds.multiple_of.unwrap_or(1.0);
    let Some(base) = value.as_f64() else {
        return value.clone();
    };
    // Fractional multiples would drift off the step in floating point.
    if step.fract() != 0.0 {
        return value.clone();
    }
    let room = bounds
        .high(step)
        .map_or(99.0, |high| ((high - base) / step).floor().clamp(0.0, 99.0));
    // Exact: `room` is a whole number in 0..=99.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let steps = pick % (room as u64 + 1);
    #[allow(clippy::cast_precision_loss)]
    let shifted = base + steps as f64 * step;
    match value.as_i64() {
        Some(whole) => {
            // Exact: at most 99 steps of a whole `multipleOf` from a whole value.
            #[allow(clippy::cast_possible_truncation)]
            let offset = (shifted - base).round() as i64;
            Value::Number(Number::from(whole.saturating_add(offset)))
        }
        None => Number::from_f64(shifted).map_or_else(|| value.clone(), Value::Number),
    }
}

/// Zero, raised to `low` or lowered to `high`.
fn start(low: Option<f64>, high: Option<f64>) -> f64 {
    match (low, high) {
        (Some(low), _) if low > 0.0 => low,
        (_, Some(high)) if high < 0.0 => high,
        (Some(low), _) => low.max(0.0).min(high.unwrap_or(f64::MAX)),
        _ => 0.0,
    }
}

fn max_of(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn min_of(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn int(schema: Value) -> Value {
        integer(schema.as_object().unwrap())
    }

    fn num(schema: Value) -> Value {
        number(schema.as_object().unwrap())
    }

    #[test]
    fn integers_respect_bounds_and_multiples() {
        assert_eq!(int(json!({})), json!(0));
        assert_eq!(int(json!({"minimum": 5})), json!(5));
        assert_eq!(int(json!({"exclusiveMinimum": 5})), json!(6));
        assert_eq!(int(json!({"maximum": -3})), json!(-3));
        assert_eq!(int(json!({"exclusiveMaximum": 0})), json!(-1));
        assert_eq!(int(json!({"minimum": 1, "multipleOf": 5})), json!(5));
        assert_eq!(int(json!({"minimum": 0.5})), json!(1));
    }

    #[test]
    fn shifts_stay_under_the_upper_bound_and_on_the_step() {
        let schema = json!({"minimum": 1, "maximum": 3});
        let object = schema.as_object().unwrap();
        for pick in 0..10 {
            let shifted = shift(object, &json!(1), pick).as_i64().unwrap();
            assert!((1..=3).contains(&shifted), "{shifted}");
        }
        let stepped = json!({"multipleOf": 5});
        let shifted = shift(stepped.as_object().unwrap(), &json!(0), 7);
        assert_eq!(shifted, json!(35));
        let exclusive = json!({"exclusiveMaximum": 1.5});
        let shifted = shift(exclusive.as_object().unwrap(), &json!(0.0), 3);
        assert_eq!(shifted, json!(0.0));
    }

    #[test]
    fn numbers_step_inside_exclusive_bounds() {
        assert_eq!(num(json!({})), json!(0.0));
        assert_eq!(
            num(json!({"exclusiveMinimum": 0, "maximum": 1})),
            json!(0.5)
        );
        assert_eq!(num(json!({"exclusiveMinimum": 2})), json!(3.0));
        assert_eq!(num(json!({"minimum": -10, "maximum": -2})), json!(-2.0));
    }
}
