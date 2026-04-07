//! Simple JSONPath extraction supporting a practical subset:
//!
//! - `$.key`        — top-level key
//! - `$.a.b`        — nested key
//! - `$.a[0]`       — array index
//! - `$.a[0].b`     — chained
//!
//! No wildcards (`$..`), filters (`[?()]`), or slices (`[0:3]`).

use serde_json::Value;

/// Errors that can occur during JSONPath extraction.
#[derive(Debug, thiserror::Error)]
pub enum JsonPathError {
    #[error("invalid jsonpath: {0}")]
    InvalidPath(String),
    #[error("path not found: {0}")]
    NotFound(String),
}

/// Extract a value from `json` using a simple JSONPath expression.
///
/// Returns the matched [`Value`]. For scalar leaves this is typically a
/// `Value::Number`, `Value::String`, or `Value::Bool`.
pub fn jsonpath_extract<'a>(json: &'a Value, path: &str) -> Result<&'a Value, JsonPathError> {
    let path = path.trim();
    if !path.starts_with('$') {
        return Err(JsonPathError::InvalidPath(format!(
            "path must start with '$': {path}"
        )));
    }

    let rest = &path[1..]; // skip '$'
    let segments = parse_segments(rest)?;

    let mut current = json;
    for seg in &segments {
        match seg {
            Segment::Key(key) => {
                current = current.get(key.as_str()).ok_or_else(|| {
                    JsonPathError::NotFound(format!("key '{key}' not found in {path}"))
                })?;
            }
            Segment::Index(idx) => {
                current = current.get(*idx).ok_or_else(|| {
                    JsonPathError::NotFound(format!("index [{idx}] not found in {path}"))
                })?;
            }
        }
    }

    Ok(current)
}

/// Convert a [`Value`] to its display string.
///
/// - Strings are returned without quotes.
/// - Numbers, bools, null are converted via `to_string()`.
/// - Objects/arrays are serialized as compact JSON.
pub fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

// ─── Internal ────────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Segment {
    Key(String),
    Index(usize),
}

/// Parse the portion after `$` into segments.
///
/// Grammar (informal):
///   segments = ("." key | "[" index "]")*
///   key      = identifier (no dots, no brackets)
///   index    = non-negative integer
fn parse_segments(input: &str) -> Result<Vec<Segment>, JsonPathError> {
    let mut segments = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '.' => {
                i += 1; // skip '.'
                let start = i;
                while i < chars.len() && chars[i] != '.' && chars[i] != '[' {
                    i += 1;
                }
                if i == start {
                    return Err(JsonPathError::InvalidPath(
                        "empty key after '.'".to_string(),
                    ));
                }
                let key: String = chars[start..i].iter().collect();
                segments.push(Segment::Key(key));
            }
            '[' => {
                i += 1; // skip '['
                let start = i;
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(JsonPathError::InvalidPath(
                        "unclosed bracket".to_string(),
                    ));
                }
                let idx_str: String = chars[start..i].iter().collect();
                let idx: usize = idx_str.parse().map_err(|_| {
                    JsonPathError::InvalidPath(format!("invalid array index: {idx_str}"))
                })?;
                segments.push(Segment::Index(idx));
                i += 1; // skip ']'
            }
            c => {
                return Err(JsonPathError::InvalidPath(format!(
                    "unexpected character '{c}'"
                )));
            }
        }
    }

    Ok(segments)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn top_level_key() {
        let data = json!({"water_level_ft": 11.87});
        let val = jsonpath_extract(&data, "$.water_level_ft").unwrap();
        assert_eq!(val, &json!(11.87));
    }

    #[test]
    fn nested_key() {
        let data = json!({"properties": {"temperature": {"value": 12.5}}});
        let val = jsonpath_extract(&data, "$.properties.temperature.value").unwrap();
        assert_eq!(val, &json!(12.5));
    }

    #[test]
    fn array_index() {
        let data = json!({"values": [10, 20, 30]});
        let val = jsonpath_extract(&data, "$.values[0]").unwrap();
        assert_eq!(val, &json!(10));
    }

    #[test]
    fn array_index_nested() {
        let data = json!({
            "timeSeries": [
                {"sourceInfo": {"siteName": "Skagit River"}}
            ]
        });
        let val = jsonpath_extract(&data, "$.timeSeries[0].sourceInfo.siteName").unwrap();
        assert_eq!(val, &json!("Skagit River"));
    }

    #[test]
    fn root_only() {
        let data = json!({"a": 1});
        let val = jsonpath_extract(&data, "$").unwrap();
        assert_eq!(val, &data);
    }

    #[test]
    fn string_value() {
        let data = json!({"name": "hello"});
        let val = jsonpath_extract(&data, "$.name").unwrap();
        assert_eq!(value_to_string(val), "hello");
    }

    #[test]
    fn number_value_to_string() {
        let data = json!({"n": 42});
        let val = jsonpath_extract(&data, "$.n").unwrap();
        assert_eq!(value_to_string(val), "42");
    }

    #[test]
    fn missing_key_returns_error() {
        let data = json!({"a": 1});
        assert!(jsonpath_extract(&data, "$.b").is_err());
    }

    #[test]
    fn missing_index_returns_error() {
        let data = json!({"a": [1]});
        assert!(jsonpath_extract(&data, "$.a[5]").is_err());
    }

    #[test]
    fn invalid_no_dollar() {
        let data = json!({"a": 1});
        assert!(jsonpath_extract(&data, ".a").is_err());
    }

    #[test]
    fn invalid_empty_key() {
        let data = json!({"a": 1});
        assert!(jsonpath_extract(&data, "$..a").is_err());
    }

    #[test]
    fn bool_and_null_values() {
        let data = json!({"flag": true, "empty": null});
        assert_eq!(
            value_to_string(jsonpath_extract(&data, "$.flag").unwrap()),
            "true"
        );
        assert_eq!(
            value_to_string(jsonpath_extract(&data, "$.empty").unwrap()),
            "null"
        );
    }
}
