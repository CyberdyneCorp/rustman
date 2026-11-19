//! Validation keywords that get stricter or looser. A stricter request
//! constraint breaks clients; a looser response constraint may surprise
//! them (`unknown`); the other two cases are harmless. A change that is
//! both (a `multipleOf` of 4 becoming 6 rejects 4 and allows 6) is
//! classified by its worse side in each direction, and one that cannot be
//! ordered exactly is `constraint-changed` (`unknown`).

use serde_json::Value;

use super::{Direction, Pair, Rule, shown};
use crate::catalog::RuleId;
use crate::difference::Sink;

pub struct ConstraintRule;

/// How a keyword's value orders strictness.
#[derive(Clone, Copy)]
enum Bound {
    /// A smaller value is stricter (`maxLength`, `maximum`, ...).
    Upper,
    /// A larger value is stricter (`minLength`, `minimum`, ...).
    Lower,
    /// Any new or changed value is stricter (`pattern`, `format`, ...).
    Exact,
    /// `true` is stricter (`uniqueItems`).
    Flag,
    /// A divisor (`multipleOf`): a value that divides the old one allows
    /// every old value, so it is looser; a multiple of it is stricter.
    Divisor,
}

/// How a changed constraint relates to the old one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Change {
    /// It rejects values the old one accepted, and accepts no new ones.
    Stricter,
    /// It accepts every old value, and more.
    Looser,
    /// It rejects some old values and accepts some new ones.
    Both,
    /// The two cannot be ordered exactly.
    Unclear,
}

const KEYWORDS: [(&str, Bound); 15] = [
    ("maxLength", Bound::Upper),
    ("maximum", Bound::Upper),
    ("exclusiveMaximum", Bound::Upper),
    ("maxItems", Bound::Upper),
    ("maxProperties", Bound::Upper),
    ("minLength", Bound::Lower),
    ("minimum", Bound::Lower),
    ("exclusiveMinimum", Bound::Lower),
    ("minItems", Bound::Lower),
    ("minProperties", Bound::Lower),
    ("pattern", Bound::Exact),
    ("format", Bound::Exact),
    ("multipleOf", Bound::Divisor),
    ("const", Bound::Exact),
    ("uniqueItems", Bound::Flag),
];

impl Rule for ConstraintRule {
    fn check(&self, pair: &Pair<'_, '_>, sink: &mut Sink<'_>) {
        for (keyword, bound) in KEYWORDS {
            let old = pair.old.get(keyword);
            let new = pair.new.get(keyword);
            if old == new {
                continue;
            }
            let rule = rule(pair.direction, stricter(bound, old, new));
            let what = format!("{keyword} {} -> {}", shown(old), shown(new));
            pair.report(sink, rule, what);
        }
    }
}

/// The rule for a constraint that changed this way in `direction`.
pub(super) fn rule(direction: Direction, change: Change) -> RuleId {
    match (direction, change) {
        (_, Change::Unclear) => RuleId::ConstraintChanged,
        (Direction::Request, Change::Stricter | Change::Both) => RuleId::RequestConstraintTightened,
        (Direction::Request, Change::Looser) => RuleId::RequestConstraintLoosened,
        (Direction::Response, Change::Stricter) => RuleId::ResponseConstraintTightened,
        (Direction::Response, Change::Looser | Change::Both) => RuleId::ResponseConstraintLoosened,
    }
}

/// How going from `old` to `new` (which differ) changes what is accepted.
fn stricter(bound: Bound, old: Option<&Value>, new: Option<&Value>) -> Change {
    let (Some(old), Some(new)) = (old, new) else {
        // Added is stricter, removed is looser, except a `false` flag.
        let added = new.is_some_and(|n| !matches!(bound, Bound::Flag) || n == &Value::Bool(true));
        return if added {
            Change::Stricter
        } else {
            Change::Looser
        };
    };
    let numbers = old.as_f64().zip(new.as_f64());
    let tighter = match (bound, numbers) {
        (Bound::Divisor, _) => return divisor(old, new),
        (Bound::Upper, Some((old, new))) => new < old,
        (Bound::Lower, Some((old, new))) => new > old,
        (Bound::Flag, _) => new == &Value::Bool(true),
        _ => true,
    };
    if tighter {
        Change::Stricter
    } else {
        Change::Looser
    }
}

/// `multipleOf` from `old` to `new`: looser when `new` divides `old`
/// (10 -> 5), stricter when `old` divides `new` (5 -> 10), else both. The
/// numbers are compared as exact decimals; values that are not positive
/// numbers, or too large or precise to compare exactly, are unclear.
fn divisor(old: &Value, new: &Value) -> Change {
    let (Some(old), Some(new)) = (Decimal::of(old), Decimal::of(new)) else {
        return Change::Unclear;
    };
    match (new.divides(old), old.divides(new)) {
        (Some(true), _) => Change::Looser,
        (_, Some(true)) => Change::Stricter,
        (Some(false), Some(false)) => Change::Both,
        _ => Change::Unclear,
    }
}

/// A positive decimal number, `mantissa * 10^exponent`, read exactly from
/// its JSON text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Decimal {
    mantissa: u128,
    exponent: i32,
}

impl Decimal {
    /// `12`, `0.25`, `1e-7` or `2.5E+3`, with the mantissa's trailing
    /// zeros moved to the exponent; `None` for anything else, zero and
    /// negative numbers.
    fn of(value: &Value) -> Option<Decimal> {
        let Value::Number(number) = value else {
            return None;
        };
        let text = number.to_string();
        let (digits, exponent) = match text.split_once(['e', 'E']) {
            Some((digits, exponent)) => (digits.to_owned(), exponent.parse::<i32>().ok()?),
            None => (text, 0),
        };
        let (whole, fraction) = digits.split_once('.').unwrap_or((&digits, ""));
        let all = format!("{whole}{fraction}");
        if all.is_empty() || !all.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut mantissa: u128 = all.parse().ok()?;
        let mut exponent = exponent.checked_sub(i32::try_from(fraction.len()).ok()?)?;
        if mantissa == 0 {
            return None;
        }
        while mantissa.is_multiple_of(10) {
            mantissa /= 10;
            exponent = exponent.checked_add(1)?;
        }
        Some(Decimal { mantissa, exponent })
    }

    /// Whether `other / self` is a whole number; `None` when it cannot be
    /// computed exactly.
    fn divides(self, other: Decimal) -> Option<bool> {
        let shift = other.exponent.checked_sub(self.exponent)?;
        let power = |n: i32| 10u128.checked_pow(n.unsigned_abs());
        if shift >= 0 {
            let other = other.mantissa.checked_mul(power(shift)?)?;
            Some(other.is_multiple_of(self.mantissa))
        } else {
            let this = self.mantissa.checked_mul(power(shift)?)?;
            Some(other.mantissa.is_multiple_of(this))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn is_stricter(bound: Bound, old: Option<&Value>, new: Option<&Value>) -> bool {
        stricter(bound, old, new) == Change::Stricter
    }

    #[test]
    fn bounds_order_strictness() {
        let (a, b) = (json!(100), json!(50));
        assert!(is_stricter(Bound::Upper, Some(&a), Some(&b)));
        assert!(!is_stricter(Bound::Upper, Some(&b), Some(&a)));
        assert!(is_stricter(Bound::Lower, Some(&b), Some(&a)));
        assert!(is_stricter(Bound::Upper, None, Some(&b)));
        assert!(!is_stricter(Bound::Upper, Some(&b), None));
        assert!(is_stricter(
            Bound::Exact,
            Some(&json!("^a")),
            Some(&json!("^b"))
        ));
        assert!(!is_stricter(Bound::Flag, None, Some(&json!(false))));
        assert!(is_stricter(
            Bound::Flag,
            Some(&json!(false)),
            Some(&json!(true))
        ));
    }

    /// Regression: every `multipleOf` change counted as stricter, so 10 -> 5
    /// (every multiple of 10 is a multiple of 5) failed CI.
    #[test]
    fn multiple_of_is_ordered_by_divisibility() {
        let change = |old: Value, new: Value| stricter(Bound::Divisor, Some(&old), Some(&new));
        assert_eq!(change(json!(10), json!(5)), Change::Looser);
        assert_eq!(change(json!(5), json!(10)), Change::Stricter);
        assert_eq!(change(json!(4), json!(6)), Change::Both);
        assert_eq!(change(json!(0.1), json!(0.05)), Change::Looser);
        assert_eq!(change(json!(0.01), json!(0.1)), Change::Stricter);
        assert_eq!(change(json!(0.3), json!(0.2)), Change::Both);
        assert_eq!(change(json!(2), json!(0.5)), Change::Looser);
        assert_eq!(change(json!(1e-7), json!(1e-8)), Change::Looser);
        assert_eq!(change(json!(3), json!(1e300)), Change::Unclear);
        assert_eq!(change(json!(3), json!("3")), Change::Unclear);
        assert_eq!(change(json!(3), json!(0)), Change::Unclear);
        let none = stricter(Bound::Divisor, None, Some(&json!(2)));
        assert_eq!(none, Change::Stricter);
    }

    #[test]
    fn decimals_are_read_exactly() {
        let of = |v: Value| Decimal::of(&v).map(|d| (d.mantissa, d.exponent));
        assert_eq!(of(json!(12)), Some((12, 0)));
        assert_eq!(of(json!(100)), Some((1, 2)));
        assert_eq!(of(json!(0.25)), Some((25, -2)));
        assert_eq!(of(json!(2.5e3)), Some((25, 2)));
        assert_eq!(of(json!(-2)), None);
    }

    #[test]
    fn direction_picks_the_rule() {
        assert_eq!(
            rule(Direction::Request, Change::Stricter),
            RuleId::RequestConstraintTightened
        );
        assert_eq!(
            rule(Direction::Response, Change::Looser),
            RuleId::ResponseConstraintLoosened
        );
        assert_eq!(
            rule(Direction::Request, Change::Both),
            RuleId::RequestConstraintTightened
        );
        assert_eq!(
            rule(Direction::Response, Change::Both),
            RuleId::ResponseConstraintLoosened
        );
        assert_eq!(
            rule(Direction::Response, Change::Unclear),
            RuleId::ConstraintChanged
        );
    }
}
