//! `visible_when` conditional-rendering primitive — Phase 7.
//!
//! Layout items may carry a `VisibleWhen { path, op, value }` clause that the
//! compositor evaluates against a per-render snapshot of plugin-instance data.
//! Items whose clause resolves to `false` are skipped and never paint.
//!
//! # Wire format
//!
//! Stored as JSON in the `visible_when_json` column on `layout_items`. The
//! shape is intentionally simple — a single comparison; no compound
//! expressions. This is the "firm stop at compound" decision in
//! `docs/plugin-customization-design.md`.
//!
//! ```json
//! { "path": "$.weather.precip_chance_pct", "op": ">", "value": 0 }
//! { "path": "$.river.go", "op": "=", "value": true }
//! { "path": "$.weather.alerts", "op": "exists", "value": null }
//! ```
//!
//! # Defensive evaluation (Phase 7 design decisions)
//!
//! - **Missing data → hide.** If `path` doesn't resolve, the clause evaluates
//!   to false and the item hides. This is the "fail closed" default — better
//!   to hide an item the user expected to see than to show one they expected
//!   gone.
//! - **Type mismatch → hide.** A `>` against a non-numeric value coerces to
//!   number where possible (string → parse) and otherwise returns false.
//! - **Malformed clause JSON in the DB → no clause.** The store's read path
//!   logs a warning and loads the item with `visible_when: None`, so a
//!   hand-edited corrupt row is treated as "the user never set a clause"
//!   rather than "the user set an unsatisfiable clause." This is *not* the
//!   same as the missing-data rule above: missing data on a known clause is
//!   the user's edge case (data hasn't arrived yet) and they want it hidden;
//!   a parse error is operational damage outside the admin UI's control and
//!   defaulting to "always show" matches what they'd see if they hadn't
//!   touched the row.
//! - **`exists`** ignores `value`; truthy iff the path resolves to *any* value
//!   (including `null` — the path being present is what counts).
//!
//! Group items deliberately do not carry a `visible_when` field. Hiding a
//! group separately from its children is a different feature with cascading
//! semantics; we keep the v1 scope strict.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::jsonpath::jsonpath_extract;

/// One side of a comparison clause. `value` is unused for the `exists`
/// operator but is still required on the wire so the admin UI's
/// (`path | op | value`) form keeps a uniform shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisibleWhen {
    pub path: String,
    pub op: String,
    #[serde(default)]
    pub value: Value,
}

impl VisibleWhen {
    /// Evaluate this clause against `snapshot` (typically the per-render
    /// unified instance map). Returns `true` if the item should render,
    /// `false` if it should be skipped.
    ///
    /// Errors (path missing, type mismatch, unknown op) all collapse to
    /// `false` per the "fail closed" contract.
    pub fn evaluate(&self, snapshot: &Value) -> bool {
        let actual = match jsonpath_extract(snapshot, &self.path) {
            Ok(v) => v,
            Err(_) => {
                // Path missing → hide. (`exists` would have returned false
                // here anyway, so this branch is correct for both modes.)
                return false;
            }
        };

        match self.op.as_str() {
            "exists" => true, // path resolved → exists is true
            "=" => values_equal(actual, &self.value),
            "!=" => !values_equal(actual, &self.value),
            ">" => compare_numbers(actual, &self.value).map(|o| o.is_gt()).unwrap_or(false),
            "<" => compare_numbers(actual, &self.value).map(|o| o.is_lt()).unwrap_or(false),
            ">=" => compare_numbers(actual, &self.value).map(|o| o.is_ge()).unwrap_or(false),
            "<=" => compare_numbers(actual, &self.value).map(|o| o.is_le()).unwrap_or(false),
            _ => false, // unknown op → hide
        }
    }
}

/// Equality with mild coercion: `1` matches `"1"`, `true` matches `true`.
/// Strings compared as strings; bools as bools; numbers as numbers; nulls
/// only equal nulls. Cross-type fall back to string equality after
/// `value_to_string`-style flattening.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => {
            // Compare as f64 — JSON numbers are loosely typed.
            x.as_f64() == y.as_f64()
        }
        (Value::Null, Value::Null) => true,
        // Cross-type: stringify both and compare. Lets the wire format stay
        // user-friendly ("0" vs 0 still match) without a strict-types
        // schema. Empirical decision matching the rest of this codebase's
        // forgiving JSON handling.
        _ => stringify(a) == stringify(b),
    }
}

fn stringify(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}

fn compare_numbers(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    let af = to_f64(a)?;
    let bf = to_f64(b)?;
    af.partial_cmp(&bf)
}

fn to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        // Strings parse-coerce to numbers — covers "12.5" and "0" returning
        // from JSON APIs that quote numerics. Non-numeric strings → None.
        Value::String(s) => s.parse::<f64>().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap() -> Value {
        json!({
            "weather": { "precip_chance_pct": 30, "alerts": null, "temp_f": "72.5" },
            "river": { "go": true, "level_ft": 9.4 },
        })
    }

    #[test]
    fn exists_true_when_path_resolves() {
        let vw = VisibleWhen {
            path: "$.weather.alerts".into(),
            op: "exists".into(),
            value: Value::Null,
        };
        assert!(vw.evaluate(&snap()));
    }

    #[test]
    fn exists_false_when_missing() {
        let vw = VisibleWhen {
            path: "$.weather.does_not_exist".into(),
            op: "exists".into(),
            value: Value::Null,
        };
        assert!(!vw.evaluate(&snap()));
    }

    #[test]
    fn equality_with_bool_and_string() {
        let vw_bool = VisibleWhen {
            path: "$.river.go".into(),
            op: "=".into(),
            value: json!(true),
        };
        assert!(vw_bool.evaluate(&snap()));

        let vw_neq = VisibleWhen {
            path: "$.river.go".into(),
            op: "!=".into(),
            value: json!(false),
        };
        assert!(vw_neq.evaluate(&snap()));
    }

    #[test]
    fn numeric_comparisons() {
        let cases = [
            (">", 0, true),
            (">", 30, false),
            (">=", 30, true),
            ("<", 50, true),
            ("<=", 30, true),
            ("=", 30, true),
            ("=", 25, false),
        ];
        for (op, val, want) in cases {
            let vw = VisibleWhen {
                path: "$.weather.precip_chance_pct".into(),
                op: op.into(),
                value: json!(val),
            };
            assert_eq!(vw.evaluate(&snap()), want, "op={op} val={val}");
        }
    }

    #[test]
    fn string_number_coerces_for_gt() {
        // temp_f is "72.5" (string) — > 70 (number) should be true.
        let vw = VisibleWhen {
            path: "$.weather.temp_f".into(),
            op: ">".into(),
            value: json!(70),
        };
        assert!(vw.evaluate(&snap()));
    }

    #[test]
    fn missing_path_hides() {
        let vw = VisibleWhen {
            path: "$.does.not.exist".into(),
            op: ">".into(),
            value: json!(0),
        };
        assert!(!vw.evaluate(&snap()));
    }

    #[test]
    fn unknown_op_hides() {
        let vw = VisibleWhen {
            path: "$.river.go".into(),
            op: "regex".into(),
            value: json!(".*"),
        };
        assert!(!vw.evaluate(&snap()));
    }

    #[test]
    fn type_mismatch_on_gt_hides() {
        // Comparing a non-numeric string with > should hide.
        let vw = VisibleWhen {
            path: "$.weather.alerts".into(), // null in fixture
            op: ">".into(),
            value: json!(0),
        };
        assert!(!vw.evaluate(&snap()));
    }

    #[test]
    fn cross_type_equality_via_stringification() {
        // 30 == "30" — friendly forgiveness for stringified numerics.
        let vw = VisibleWhen {
            path: "$.weather.precip_chance_pct".into(),
            op: "=".into(),
            value: json!("30"),
        };
        assert!(vw.evaluate(&snap()));
    }

    #[test]
    fn serde_roundtrip() {
        let vw = VisibleWhen {
            path: "$.x".into(),
            op: ">=".into(),
            value: json!(5),
        };
        let s = serde_json::to_string(&vw).unwrap();
        let back: VisibleWhen = serde_json::from_str(&s).unwrap();
        assert_eq!(vw, back);
    }
}
