//! Template engine — Jinja rendering via minijinja with custom filters.
//!
//! Loads `.html.jinja` files from a `templates/` directory at the path supplied
//! to [`TemplateEngine::new`].  Assembles the render context
//! `{ data, settings, trip_decision, now, error }` defined in
//! `docs/research/target-architecture.md §5c` and renders the template to an
//! HTML string.  No sidecar integration — callers receive raw HTML.
//!
//! # On the file extension
//!
//! Templates use Jinja2 syntax (filter calls with parens — `default("x")` —
//! and `{# ... #}` comments) implemented by the [`minijinja`] crate. Earlier
//! revisions of this codebase named the files `.html.liquid` despite parsing
//! them with minijinja; that misnomer was cleaned up in the
//! "template-pipeline cleanup" PR. If you find a stale "Liquid" reference in
//! a doc or comment, treat it as a bug and submit a fix.

use minijinja::value::Rest;
use minijinja::{Environment, Error, Value};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum TemplateError {
    #[error("template directory not found: {0}")]
    DirectoryNotFound(PathBuf),

    #[error("failed to read template '{name}': {source}")]
    ReadError {
        name: String,
        source: std::io::Error,
    },

    #[error("template '{name}' not found")]
    TemplateNotFound { name: String },

    #[error("render error in '{name}': {source}")]
    RenderError {
        name: String,
        source: minijinja::Error,
    },
}

// ─── Context types ───────────────────────────────────────────────────────────

/// The `now` sub-object injected into every template render.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NowContext {
    /// Unix timestamp in seconds.
    pub unix: u64,
    /// ISO-8601 UTC string, e.g. "2026-04-05T12:00:00Z".
    pub iso: String,
    /// Human-readable local string, e.g. "Sun Apr 5 12:00".
    pub local: String,
}

impl NowContext {
    /// Build a `NowContext` from a Unix timestamp.
    pub fn from_unix(unix: u64) -> Self {
        let iso = unix_to_iso(unix);
        let local = unix_to_local_display(unix);
        NowContext { unix, iso, local }
    }
}

/// The `trip_decision` sub-object injected when the plugin has evaluation criteria.
///
/// `Deserialize` is implemented so the admin template-preview endpoint can
/// accept a user-supplied trip-decision shape verbatim — production code
/// only ever serializes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TripDecisionContext {
    pub go: bool,
    pub destination: Option<String>,
    pub results: Vec<CriterionResult>,
}

/// One evaluated criterion result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CriterionResult {
    pub key: String,
    pub pass: bool,
    pub reason: String,
}

/// Full render context passed to every Jinja template (target-architecture.md §5c).
#[derive(Debug, Clone, serde::Serialize)]
pub struct RenderContext {
    /// The plugin's own fetch result as an open JSON value.
    pub data: JsonValue,
    /// User-configured plugin settings (key → value map).
    pub settings: HashMap<String, JsonValue>,
    /// Go/no-go evaluation; `None` for plugins with no criteria.
    pub trip_decision: Option<TripDecisionContext>,
    /// Current time.
    pub now: NowContext,
    /// Non-null when the last fetch failed and stale data is being shown.
    pub error: Option<String>,
    /// Phase 9: theming-knob overrides for the plugin Group rendering this
    /// slot. Templates read sub-keys via `{{ style.<knob_key>.<sub> }}`
    /// (e.g. `{{ style.temp_style.color }}`) with the `default` filter
    /// supplying fallbacks. Empty when the slot has no Group binding or
    /// the user hasn't customised any knobs — templates render with their
    /// built-in defaults in that case.
    #[serde(default)]
    pub style: HashMap<String, JsonValue>,
}

// ─── Engine ──────────────────────────────────────────────────────────────────

/// Jinja template engine backed by minijinja.
///
/// Call [`TemplateEngine::new`] once at startup, then [`TemplateEngine::render`]
/// for every display cycle.  Templates are parsed once at construction time;
/// renders are cheap (context serialisation + tree walk only).
pub struct TemplateEngine {
    /// Pre-built environment with all templates loaded and filters registered.
    env: Environment<'static>,
    /// Set of loaded template names, for O(1) membership checks.
    names: HashSet<String>,
}

impl TemplateEngine {
    /// Load all `.html.jinja` files from `templates_dir`.
    ///
    /// Returns `Err(TemplateError::DirectoryNotFound)` if the directory does
    /// not exist.  Individual file-read errors are returned individually.
    pub fn new(templates_dir: &Path) -> Result<Self, TemplateError> {
        if !templates_dir.exists() {
            return Err(TemplateError::DirectoryNotFound(templates_dir.to_path_buf()));
        }

        let entries = std::fs::read_dir(templates_dir).map_err(|e| TemplateError::ReadError {
            name: templates_dir.display().to_string(),
            source: e,
        })?;

        let mut raw: Vec<(String, String)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("jinja") {
                // Strip the .jinja extension, then the inner .html suffix if present.
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.trim_end_matches(".html"))
                    .map(str::to_owned);

                if let Some(name) = stem {
                    let src =
                        std::fs::read_to_string(&path).map_err(|e| TemplateError::ReadError {
                            name: name.clone(),
                            source: e,
                        })?;
                    raw.push((name, src));
                }
            }
        }

        let mut env = build_env();
        let mut names = HashSet::new();
        for (name, src) in raw {
            env.add_template_owned(name.clone(), src)
                .map_err(|e| TemplateError::RenderError {
                    name: name.clone(),
                    source: e,
                })?;
            names.insert(name);
        }

        Ok(TemplateEngine { env, names })
    }

    /// Render `template_name` (the stem, without `.html.jinja`) with `ctx`.
    ///
    /// Returns the rendered HTML string.
    pub fn render(&self, template_name: &str, ctx: &RenderContext) -> Result<String, TemplateError> {
        if !self.names.contains(template_name) {
            return Err(TemplateError::TemplateNotFound {
                name: template_name.to_owned(),
            });
        }

        let tmpl = self.env.get_template(template_name).map_err(|e| TemplateError::RenderError {
            name: template_name.to_owned(),
            source: e,
        })?;

        // Convert the context via serde so nested JsonValue fields are traversable.
        let ctx_value = Value::from_serialize(ctx);

        tmpl.render(ctx_value)
            .map_err(|e| TemplateError::RenderError {
                name: template_name.to_owned(),
                source: e,
            })
    }

    /// Number of templates loaded.
    pub fn template_count(&self) -> usize {
        self.names.len()
    }

    /// Returns `true` if the named template was loaded.
    pub fn has_template(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Render a Jinja template provided as a raw source string against `ctx`.
    ///
    /// Used by the admin "preview a template edit" path: a fresh, isolated
    /// `Environment` is built per call (same custom filters and undefined
    /// behaviour as the loaded engine), so concurrent previews never see
    /// partial state and the on-disk template set is untouched.
    ///
    /// Returns `TemplateError::RenderError { name: "<inline>", source }` for
    /// both syntax errors (caught at `add_template_owned`) and runtime errors
    /// (caught at `render`). The caller can downcast `source` into
    /// `minijinja::Error` for `.line()` / `.range()` line/column diagnostics.
    pub fn render_source(
        template_source: &str,
        ctx: &RenderContext,
    ) -> Result<String, TemplateError> {
        const NAME: &str = "<inline>";
        let mut env = build_env();
        env.add_template_owned(NAME.to_owned(), template_source.to_owned())
            .map_err(|e| TemplateError::RenderError {
                name: NAME.to_owned(),
                source: e,
            })?;
        let tmpl = env
            .get_template(NAME)
            .map_err(|e| TemplateError::RenderError {
                name: NAME.to_owned(),
                source: e,
            })?;
        let ctx_value = Value::from_serialize(ctx);
        tmpl.render(ctx_value)
            .map_err(|e| TemplateError::RenderError {
                name: NAME.to_owned(),
                source: e,
            })
    }
}

// ─── minijinja environment with custom filters ────────────────────────────────

fn build_env() -> Environment<'static> {
    let mut env = Environment::new();
    // Phase 9: theming knobs are accessed via chained lookups like
    // `style.temp_style.color`. With the default Lenient mode, accessing
    // `.color` on an undefined `temp_style` raises an error before our
    // `default` filter has a chance to substitute. Chainable lets the
    // chain return undefined, which the filter then catches.
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Chainable);
    env.add_filter("number_with_delimiter", filter_number_with_delimiter);
    env.add_filter("round", filter_round);
    env.add_filter("default", filter_default);
    env.add_filter("pluralize", filter_pluralize);
    env.add_filter("days_ago", filter_days_ago);
    env.add_filter("time_of_day", filter_time_of_day);
    env
}

// ─── Custom filters ───────────────────────────────────────────────────────────

/// Format a number with comma thousands separators: 12345.6 → "12,345.6"
///
/// Used as: `{{ data.streamflow_cfs | number_with_delimiter }}`
fn filter_number_with_delimiter(value: Value) -> Result<Value, Error> {
    // Prefer integer path (exact, avoids float formatting edge cases).
    if let Some(i) = value.as_i64() {
        let negative = i < 0;
        let abs_str = format!("{}", i.unsigned_abs());
        let formatted = insert_commas(&abs_str);
        return Ok(Value::from(if negative {
            format!("-{formatted}")
        } else {
            formatted
        }));
    }

    // Float path: parse the string representation produced by minijinja's Display.
    let s = value.to_string();
    let n: f64 = match s.parse() {
        Ok(v) => v,
        Err(_) => return Ok(value), // non-numeric, pass through unchanged
    };

    // Split into integer and fractional parts. Use explicit integer truncation
    // for whole-number floats rather than relying on Display's ".0" behaviour.
    let (int_part, frac_part) = if n.fract() == 0.0 {
        (format!("{}", n as i64), None)
    } else {
        let s = format!("{n}");
        match s.find('.') {
            Some(pos) => (s[..pos].to_owned(), Some(s[pos..].to_owned())),
            None => (s, None),
        }
    };

    let negative = int_part.starts_with('-');
    let digits = int_part.trim_start_matches('-');
    let with_commas = insert_commas(digits);

    let result = match frac_part {
        Some(frac) => format!("{sign}{with_commas}{frac}", sign = if negative { "-" } else { "" }),
        None => format!("{sign}{with_commas}", sign = if negative { "-" } else { "" }),
    };

    Ok(Value::from(result))
}

fn insert_commas(digits: &str) -> String {
    let chars: Vec<char> = digits.chars().collect();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(*ch);
    }
    result
}

/// Round to N decimal places.
///
/// Used as: `{{ data.water_level_ft | round(1) }}` or `{{ value | round(0) }}`
///
/// Returns an integer value when `precision` is 0 (so chained `number_with_delimiter`
/// receives an integer rather than a float with `.0`).
fn filter_round(value: Value, precision: Option<i64>) -> Result<Value, Error> {
    let places = precision.unwrap_or(0).max(0) as u32;

    // For integer inputs, rounding is a no-op regardless of precision.
    if let Some(i) = value.as_i64() {
        if places == 0 {
            return Ok(Value::from(i));
        }
        return Ok(Value::from(format!("{:.prec$}", i as f64, prec = places as usize)));
    }

    // Float input: parse via string representation.
    let n: f64 = match value.to_string().parse() {
        Ok(v) => v,
        Err(_) => return Ok(value),
    };

    let factor = 10_f64.powi(places as i32);
    let rounded = (n * factor).round() / factor;

    if places == 0 {
        Ok(Value::from(rounded as i64))
    } else {
        Ok(Value::from(format!("{:.prec$}", rounded, prec = places as usize)))
    }
}

/// Return `value` if truthy, otherwise the first argument (fallback).
///
/// Used as: `{{ settings.site_name | default("River") }}`
///
/// Unlike minijinja's built-in, also treats empty strings and `null` as falsy —
/// matching Liquid / TRMNL behaviour (the filter shape is the same in both).
fn filter_default(value: Value, args: Rest<Value>) -> Result<Value, Error> {
    let fallback = args.first().cloned().unwrap_or_else(|| Value::from(""));

    let is_falsy = value.is_undefined()
        || value.is_none()
        || value.as_str() == Some("") // empty string
        || !value.is_true(); // covers bool false

    Ok(if is_falsy { fallback } else { value })
}

/// Choose singular or plural label based on a count.
///
/// Two-arg form: `{{ count | pluralize("item", "items") }}` → full word returned.
/// One-arg form: `{{ count | pluralize("s") }}` → "" for 1, "s" otherwise.
fn filter_pluralize(value: Value, args: Rest<Value>) -> Result<Value, Error> {
    // Support both integer counts (most common) and float counts like 1.0.
    let count_f = value
        .as_i64()
        .map(|i| i as f64)
        .or_else(|| value.to_string().parse::<f64>().ok())
        .unwrap_or(0.0);
    let is_plural = count_f != 1.0;

    let result = match (args.first(), args.get(1)) {
        (Some(singular), Some(plural)) => {
            if is_plural { plural.clone() } else { singular.clone() }
        }
        (Some(suffix), None) => {
            if is_plural { suffix.clone() } else { Value::from("") }
        }
        _ => Value::from(""),
    };

    Ok(result)
}

/// Format a Unix timestamp as "N days ago" relative to now.
///
/// Used as: `{{ data.last_updated | days_ago }}`
fn filter_days_ago(value: Value) -> Result<Value, Error> {
    let ts = match value.as_i64() {
        Some(v) => v,
        None => return Ok(value),
    };

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let diff_secs = now_unix.saturating_sub(ts);
    let days = diff_secs / 86_400;

    Ok(match days {
        0 => Value::from("today"),
        1 => Value::from("1 day ago"),
        n => Value::from(format!("{n} days ago")),
    })
}

/// Format a Unix timestamp as "HH:MM" (UTC time of day).
///
/// Used as: `{{ data.departure_time | time_of_day }}`
fn filter_time_of_day(value: Value) -> Result<Value, Error> {
    let ts = match value.as_i64() {
        Some(v) => v as u64,
        None => return Ok(value),
    };
    let secs = ts % 86_400;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    Ok(Value::from(format!("{h:02}:{m:02}")))
}

// ─── Minimal time helpers (no external crates) ────────────────────────────────

fn unix_to_iso(unix: u64) -> String {
    let days_since_epoch = unix / 86_400;
    let time_of_day = unix % 86_400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    let (y, mo, d) = days_to_ymd(days_since_epoch);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn unix_to_local_display(unix: u64) -> String {
    let days_since_epoch = unix / 86_400;
    let time_of_day = unix % 86_400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let (_, mo, d) = days_to_ymd(days_since_epoch);
    let weekday = ((days_since_epoch + 4) % 7) as usize; // 1970-01-01 was Thursday (index 4)
    let dow = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][weekday];
    let month = [
        "", "Jan", "Feb", "Mar", "Apr", "May", "Jun",
        "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][mo as usize];
    format!("{dow} {month} {d} {h:02}:{m:02}")
}

/// Convert days since Unix epoch to (year, month, day).
///
/// Algorithm: <http://howardhinnant.github.io/date_algorithms.html> (civil_from_days).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn write_template(dir: &TempDir, name: &str, content: &str) {
        let path = dir.path().join(name);
        std::fs::write(path, content).unwrap();
    }

    fn make_ctx(data: JsonValue) -> RenderContext {
        RenderContext {
            data,
            settings: HashMap::from([(
                "site_name".to_owned(),
                JsonValue::String("Test River".to_owned()),
            )]),
            trip_decision: None,
            now: NowContext::from_unix(1_775_390_400), // 2026-04-05 12:00:00 UTC
            error: None,
            style: HashMap::new(),
        }
    }

    // ── Engine construction ──────────────────────────────────────────────────

    #[test]
    fn loads_liquid_templates_from_directory() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "river.html.jinja", "<p>hello</p>");
        write_template(&dir, "weather.html.jinja", "<p>world</p>");
        write_template(&dir, "ignored.txt", "not loaded");

        let engine = TemplateEngine::new(dir.path()).unwrap();
        assert_eq!(engine.template_count(), 2);
        assert!(engine.has_template("river"));
        assert!(engine.has_template("weather"));
        assert!(!engine.has_template("ignored"));
    }

    #[test]
    fn missing_directory_returns_error() {
        let result = TemplateEngine::new(Path::new("/nonexistent/path/templates"));
        assert!(matches!(result, Err(TemplateError::DirectoryNotFound(_))));
    }

    #[test]
    fn unknown_template_name_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(JsonValue::Null);
        let result = engine.render("noexist", &ctx);
        assert!(matches!(result, Err(TemplateError::TemplateNotFound { .. })));
    }

    // ── Context rendering ────────────────────────────────────────────────────

    #[test]
    fn renders_data_fields() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "river.html.jinja", r#"<span>{{ data.water_level_ft }} ft</span>"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({ "water_level_ft": 4.2 }));
        let html = engine.render("river", &ctx).unwrap();
        assert!(html.contains("4.2 ft"), "got: {html}");
    }

    #[test]
    fn renders_settings_fields() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "river.html.jinja", r#"<title>{{ settings.site_name }}</title>"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(JsonValue::Null);
        let html = engine.render("river", &ctx).unwrap();
        assert!(html.contains("Test River"), "got: {html}");
    }

    #[test]
    fn renders_now_fields() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ now.unix }} {{ now.iso }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(JsonValue::Null);
        let html = engine.render("t", &ctx).unwrap();
        assert!(html.contains("1775390400"), "got: {html}");
        assert!(html.contains("2026-04-05"), "got: {html}");
    }

    #[test]
    fn renders_error_field_when_present() {
        let dir = TempDir::new().unwrap();
        write_template(
            &dir,
            "t.html.jinja",
            r#"{% if error %}<span class="stale">{{ error }}</span>{% endif %}"#,
        );
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let mut ctx = make_ctx(JsonValue::Null);
        ctx.error = Some("API timeout".to_owned());
        let html = engine.render("t", &ctx).unwrap();
        assert!(html.contains("stale"), "got: {html}");
        assert!(html.contains("API timeout"), "got: {html}");
    }

    #[test]
    fn trip_decision_go_renders_correctly() {
        let dir = TempDir::new().unwrap();
        write_template(
            &dir,
            "t.html.jinja",
            r#"{% if trip_decision %}{% if trip_decision.go %}GO{% else %}NO GO{% endif %}{% endif %}"#,
        );
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let mut ctx = make_ctx(JsonValue::Null);
        ctx.trip_decision = Some(TripDecisionContext {
            go: true,
            destination: Some("Stevens Pass".to_owned()),
            results: vec![],
        });
        let html = engine.render("t", &ctx).unwrap();
        assert!(html.contains("GO"), "got: {html}");
        assert!(!html.contains("NO GO"), "got: {html}");
    }

    #[test]
    fn null_trip_decision_skips_block() {
        let dir = TempDir::new().unwrap();
        write_template(
            &dir,
            "t.html.jinja",
            r#"{% if trip_decision %}<b>eval</b>{% endif %}none"#,
        );
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(JsonValue::Null);
        let html = engine.render("t", &ctx).unwrap();
        assert!(!html.contains("<b>eval</b>"), "got: {html}");
        assert!(html.contains("none"), "got: {html}");
    }

    // ── Filter: number_with_delimiter ────────────────────────────────────────

    #[test]
    fn filter_number_with_delimiter_basic() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ data.n | number_with_delimiter }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({ "n": 12345 }));
        let html = engine.render("t", &ctx).unwrap();
        assert_eq!(html.trim(), "12,345");
    }

    #[test]
    fn filter_number_with_delimiter_large() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ data.flow | number_with_delimiter }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({ "flow": 1234567 }));
        let html = engine.render("t", &ctx).unwrap();
        assert_eq!(html.trim(), "1,234,567");
    }

    #[test]
    fn filter_number_with_delimiter_small() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ data.n | number_with_delimiter }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({ "n": 999 }));
        let html = engine.render("t", &ctx).unwrap();
        assert_eq!(html.trim(), "999");
    }

    // ── Filter: round ────────────────────────────────────────────────────────

    #[test]
    fn filter_round_one_decimal() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ data.v | round(1) }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({ "v": 4.567 }));
        let html = engine.render("t", &ctx).unwrap();
        assert_eq!(html.trim(), "4.6");
    }

    #[test]
    fn filter_round_zero_decimals() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ data.v | round(0) }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({ "v": 4.5 }));
        let html = engine.render("t", &ctx).unwrap();
        let trimmed = html.trim();
        // 4.5 rounds to either 4 or 5 depending on the rounding mode.
        assert!(trimmed == "4" || trimmed == "5", "got: {trimmed}");
    }

    // ── Filter: default ──────────────────────────────────────────────────────

    #[test]
    fn filter_default_passes_through_present_value() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ data.name | default("Unknown") }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({ "name": "Skagit" }));
        let html = engine.render("t", &ctx).unwrap();
        assert_eq!(html.trim(), "Skagit");
    }

    #[test]
    fn filter_default_returns_fallback_for_missing() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ data.missing | default("N/A") }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({}));
        let html = engine.render("t", &ctx).unwrap();
        assert_eq!(html.trim(), "N/A");
    }

    // ── Filter: pluralize ────────────────────────────────────────────────────

    #[test]
    fn filter_pluralize_one_is_singular() {
        let dir = TempDir::new().unwrap();
        write_template(
            &dir,
            "t.html.jinja",
            r#"{{ data.count }} {{ data.count | pluralize("day", "days") }}"#,
        );
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({ "count": 1 }));
        let html = engine.render("t", &ctx).unwrap();
        assert!(html.contains("1 day"), "got: {html}");
        assert!(!html.contains("1 days"), "got: {html}");
    }

    #[test]
    fn filter_pluralize_many_is_plural() {
        let dir = TempDir::new().unwrap();
        write_template(
            &dir,
            "t.html.jinja",
            r#"{{ data.count }} {{ data.count | pluralize("day", "days") }}"#,
        );
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let ctx = make_ctx(serde_json::json!({ "count": 3 }));
        let html = engine.render("t", &ctx).unwrap();
        assert!(html.contains("3 days"), "got: {html}");
    }

    // ── Filter: days_ago ─────────────────────────────────────────────────────

    #[test]
    fn filter_days_ago_same_day() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ data.ts | days_ago }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ctx = make_ctx(serde_json::json!({ "ts": now }));
        let html = engine.render("t", &ctx).unwrap();
        assert_eq!(html.trim(), "today");
    }

    #[test]
    fn filter_days_ago_two_days() {
        let dir = TempDir::new().unwrap();
        write_template(&dir, "t.html.jinja", r#"{{ data.ts | days_ago }}"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let two_days_ago = now - 2 * 86_400;
        let ctx = make_ctx(serde_json::json!({ "ts": two_days_ago }));
        let html = engine.render("t", &ctx).unwrap();
        assert_eq!(html.trim(), "2 days ago");
    }

    // ── Fixture template integration test ────────────────────────────────────

    #[test]
    fn fixture_template_renders_expected_values() {
        let dir = TempDir::new().unwrap();
        write_template(
            &dir,
            "river.html.jinja",
            r#"<!DOCTYPE html>
<html>
<body>
  <h1>{{ settings.site_name | default("River") }}</h1>
  <span class="level">{{ data.water_level_ft | round(1) }} ft</span>
  <span class="flow">{{ data.streamflow_cfs | round(0) | number_with_delimiter }} cfs</span>
  {% if trip_decision %}
    {% if trip_decision.go %}<div class="go">GO</div>{% else %}<div class="nogo">NO GO</div>{% endif %}
  {% endif %}
  {% if error %}<p class="error">{{ error }}</p>{% endif %}
  <footer>Updated: {{ now.iso }}</footer>
</body>
</html>"#,
        );

        let engine = TemplateEngine::new(dir.path()).unwrap();

        let ctx = RenderContext {
            data: serde_json::json!({
                "water_level_ft": 4.23,
                "streamflow_cfs": 5432.0,
            }),
            settings: HashMap::from([(
                "site_name".to_owned(),
                JsonValue::String("Skagit at Concrete".to_owned()),
            )]),
            trip_decision: Some(TripDecisionContext {
                go: true,
                destination: Some("Cascade Pass".to_owned()),
                results: vec![CriterionResult {
                    key: "water_level_ft".to_owned(),
                    pass: true,
                    reason: "4.23 ft ≤ 12.0 ft".to_owned(),
                }],
            }),
            now: NowContext::from_unix(1_775_390_400),
            error: None,
            style: HashMap::new(),
        };

        let html = engine.render("river", &ctx).unwrap();

        assert!(html.contains("Skagit at Concrete"), "site name missing: {html}");
        assert!(html.contains("4.2 ft"), "water level missing: {html}");
        assert!(html.contains("5,432"), "streamflow delimiter missing: {html}");
        assert!(html.contains("cfs"), "cfs unit missing: {html}");
        assert!(html.contains("GO"), "trip decision missing: {html}");
        assert!(!html.contains("NO GO"), "should not show NO GO: {html}");
        assert!(html.contains("2026-04-05"), "date missing: {html}");
        assert!(!html.contains(r#"class="error""#), "unexpected error element: {html}");
    }

    // ── render_source: one-off transient renders ─────────────────────────────

    #[test]
    fn render_source_happy_path() {
        let ctx = make_ctx(serde_json::json!({ "name": "Skagit" }));
        let html = TemplateEngine::render_source(
            r#"<h1>{{ data.name | default("River") }}</h1>"#,
            &ctx,
        )
        .unwrap();
        assert_eq!(html.trim(), "<h1>Skagit</h1>");
    }

    #[test]
    fn render_source_uses_custom_filters() {
        // Confirms the transient env wires the same filter set as production —
        // a regression here would mean previews silently disagree with /image.png.
        let ctx = make_ctx(serde_json::json!({ "n": 12345 }));
        let html = TemplateEngine::render_source(
            r#"{{ data.n | number_with_delimiter }}"#,
            &ctx,
        )
        .unwrap();
        assert_eq!(html.trim(), "12,345");
    }

    #[test]
    fn render_source_syntax_error_reports_inline_name() {
        let ctx = make_ctx(JsonValue::Null);
        // Unclosed `{%` triggers a parse error during add_template_owned.
        let err = TemplateEngine::render_source(r#"{% if true %"#, &ctx).unwrap_err();
        match err {
            TemplateError::RenderError { name, source } => {
                assert_eq!(name, "<inline>");
                // Source must carry line info we can surface to the editor.
                assert!(source.line().is_some(), "expected minijinja line info");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn render_source_runtime_error_is_caught() {
        let ctx = make_ctx(JsonValue::Null);
        // `strict` filter doesn't exist; runtime lookup fails.
        let err =
            TemplateEngine::render_source(r#"{{ data.x | nonexistent_filter }}"#, &ctx)
                .unwrap_err();
        assert!(matches!(err, TemplateError::RenderError { .. }));
    }

    #[test]
    fn render_source_does_not_leak_into_engine() {
        // Building a transient engine for an inline render must not affect a
        // production-style TemplateEngine that's been constructed alongside.
        let dir = TempDir::new().unwrap();
        write_template(&dir, "loaded.html.jinja", r#"<p>loaded</p>"#);
        let engine = TemplateEngine::new(dir.path()).unwrap();

        let _ = TemplateEngine::render_source(
            r#"<span>{{ data.x }}</span>"#,
            &make_ctx(JsonValue::Null),
        )
        .unwrap();

        // Loaded engine still has its template; "<inline>" is not registered.
        assert!(engine.has_template("loaded"));
        assert!(!engine.has_template("<inline>"));
        assert_eq!(engine.template_count(), 1);
    }

    // ── Unit tests for helpers ────────────────────────────────────────────────

    #[test]
    fn insert_commas_three_digits() {
        assert_eq!(insert_commas("123"), "123");
    }

    #[test]
    fn insert_commas_four_digits() {
        assert_eq!(insert_commas("1234"), "1,234");
    }

    #[test]
    fn insert_commas_seven_digits() {
        assert_eq!(insert_commas("1234567"), "1,234,567");
    }

    #[test]
    fn unix_to_iso_known_date() {
        // 2026-04-05 12:00:00 UTC
        assert_eq!(unix_to_iso(1_775_390_400), "2026-04-05T12:00:00Z");
    }

    #[test]
    fn now_context_iso_format() {
        let ctx = NowContext::from_unix(1_775_390_400);
        assert!(ctx.iso.starts_with("2026-04-05"), "got: {}", ctx.iso);
        assert!(ctx.local.contains("Apr"), "got: {}", ctx.local);
    }
}
