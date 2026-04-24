//! Format string evaluation for DataField items.
//!
//! Replaces `{{value}}` with the extracted value, supporting pipe filters:
//!
//! - `round(N)`             — round a number to N decimal places
//! - `number_with_delimiter` — add thousand separators (commas)
//! - `uppercase`            — convert to uppercase
//! - `lowercase`            — convert to lowercase

/// Evaluate a format string, replacing `{{value}}` (with optional pipe filters)
/// with the processed raw value.
///
/// Examples:
/// - `"{{value}} ft"` with `"11.87"` -> `"11.87 ft"`
/// - `"{{value | round(1)}}"` with `"11.87"` -> `"11.9"`
/// - `"{{value | round(0) | number_with_delimiter}} cfs"` with `"8750"` -> `"8,750 cfs"`
pub fn apply_format(format_string: &str, raw_value: &str) -> String {
    let mut result = String::new();
    let mut remaining = format_string;

    while let Some(start) = remaining.find("{{") {
        result.push_str(&remaining[..start]);
        let after_open = &remaining[start + 2..];
        if let Some(end) = after_open.find("}}") {
            let expr = after_open[..end].trim();
            let replacement = evaluate_expr(expr, raw_value);
            result.push_str(&replacement);
            remaining = &after_open[end + 2..];
        } else {
            // No closing }}, treat as literal
            result.push_str("{{");
            remaining = after_open;
        }
    }
    result.push_str(remaining);
    result
}

/// Evaluate a single `value | filter1 | filter2` expression.
fn evaluate_expr(expr: &str, raw_value: &str) -> String {
    let parts: Vec<&str> = expr.split('|').collect();
    if parts.is_empty() {
        return raw_value.to_string();
    }

    // First part should be "value" (the placeholder name)
    let name = parts[0].trim();
    if name != "value" {
        // Unknown placeholder, return as-is
        return format!("{{{{{expr}}}}}");
    }

    let mut current = raw_value.to_string();
    for filter in &parts[1..] {
        current = apply_filter(filter.trim(), &current);
    }
    current
}

/// Apply a single filter to a string value.
fn apply_filter(filter: &str, value: &str) -> String {
    if let Some(rest) = filter.strip_prefix("round(") {
        if let Some(n_str) = rest.strip_suffix(')')
            && let Ok(n) = n_str.trim().parse::<u32>()
        {
            return apply_round(value, n);
        }
        return value.to_string();
    }

    match filter {
        "number_with_delimiter" => apply_number_with_delimiter(value),
        "uppercase" => value.to_uppercase(),
        "lowercase" => value.to_lowercase(),
        _ => value.to_string(),
    }
}

/// Round a numeric string to `n` decimal places.
fn apply_round(value: &str, decimals: u32) -> String {
    if let Ok(num) = value.parse::<f64>() {
        if decimals == 0 {
            format!("{}", num.round() as i64)
        } else {
            format!("{:.prec$}", num, prec = decimals as usize)
        }
    } else {
        value.to_string()
    }
}

/// Add comma thousand-separators to a numeric string.
///
/// Handles integers and decimals: `8750` -> `8,750`, `1234567.89` -> `1,234,567.89`.
fn apply_number_with_delimiter(value: &str) -> String {
    // Split on decimal point
    let (integer_part, decimal_part) = match value.find('.') {
        Some(pos) => (&value[..pos], Some(&value[pos..])),
        None => (value, None),
    };

    // Handle negative numbers
    let (sign, digits) = if let Some(stripped) = integer_part.strip_prefix('-') {
        ("-", stripped)
    } else {
        ("", integer_part)
    };

    // Verify all chars are digits
    if !digits.chars().all(|c| c.is_ascii_digit()) || digits.is_empty() {
        return value.to_string();
    }

    // Insert commas from right to left
    let mut result = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    let with_commas: String = result.chars().rev().collect();

    match decimal_part {
        Some(dec) => format!("{sign}{with_commas}{dec}"),
        None => format!("{sign}{with_commas}"),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_substitution() {
        assert_eq!(apply_format("{{value}} ft", "11.87"), "11.87 ft");
    }

    #[test]
    fn no_placeholder() {
        assert_eq!(apply_format("static text", "42"), "static text");
    }

    #[test]
    fn value_only() {
        assert_eq!(apply_format("{{value}}", "hello"), "hello");
    }

    #[test]
    fn round_filter() {
        assert_eq!(apply_format("{{value | round(1)}}", "11.87"), "11.9");
        assert_eq!(apply_format("{{value | round(0)}}", "11.87"), "12");
        assert_eq!(apply_format("{{value | round(2)}}", "3.1"), "3.10");
    }

    #[test]
    fn number_with_delimiter_filter() {
        assert_eq!(
            apply_format("{{value | number_with_delimiter}}", "8750"),
            "8,750"
        );
        assert_eq!(
            apply_format("{{value | number_with_delimiter}}", "1234567"),
            "1,234,567"
        );
        assert_eq!(
            apply_format("{{value | number_with_delimiter}}", "999"),
            "999"
        );
    }

    #[test]
    fn chained_filters() {
        assert_eq!(
            apply_format("{{value | round(0) | number_with_delimiter}} cfs", "8750.3"),
            "8,750 cfs"
        );
    }

    #[test]
    fn uppercase_filter() {
        assert_eq!(
            apply_format("{{value | uppercase}}", "skagit river"),
            "SKAGIT RIVER"
        );
    }

    #[test]
    fn lowercase_filter() {
        assert_eq!(
            apply_format("{{value | lowercase}}", "HELLO"),
            "hello"
        );
    }

    #[test]
    fn multiple_placeholders() {
        assert_eq!(
            apply_format("Level: {{value}} ft ({{value}})", "11.87"),
            "Level: 11.87 ft (11.87)"
        );
    }

    #[test]
    fn unclosed_braces_treated_as_literal() {
        assert_eq!(apply_format("{{value", "42"), "{{value");
    }

    #[test]
    fn number_with_delimiter_decimal() {
        assert_eq!(
            apply_format("{{value | number_with_delimiter}}", "1234567.89"),
            "1,234,567.89"
        );
    }

    #[test]
    fn number_with_delimiter_negative() {
        assert_eq!(
            apply_format("{{value | number_with_delimiter}}", "-1234567"),
            "-1,234,567"
        );
    }

    #[test]
    fn round_non_numeric() {
        assert_eq!(apply_format("{{value | round(1)}}", "abc"), "abc");
    }

    #[test]
    fn unknown_filter_passthrough() {
        assert_eq!(apply_format("{{value | bogus}}", "42"), "42");
    }
}
