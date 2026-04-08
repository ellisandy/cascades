//! Source presets — pre-built configurations for common APIs.
//!
//! Each preset defines a URL template, default headers, response root path,
//! and default field mappings. Users provide parameters (e.g., site_id for USGS)
//! and the preset creates a fully configured generic data source.

use serde::{Deserialize, Serialize};

/// A source preset definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePreset {
    /// Unique preset identifier (e.g., "usgs_river").
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of what this preset does.
    pub description: String,
    /// URL template with `{{param}}` placeholders.
    pub url_template: String,
    /// HTTP method.
    pub method: String,
    /// Default headers as JSON array of `{key, value}` pairs.
    pub headers: Option<String>,
    /// Optional POST body template.
    pub body_template: Option<String>,
    /// JSONPath to extract from response before caching.
    pub response_root_path: Option<String>,
    /// Default refresh interval in seconds.
    pub refresh_interval_secs: i64,
    /// Parameters the user must provide.
    pub params: Vec<PresetParam>,
    /// Default field mappings created with the source.
    pub default_fields: Vec<PresetField>,
}

/// A parameter required by a preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetParam {
    /// Parameter name (matches `{{name}}` in url_template).
    pub name: String,
    /// Human-readable label.
    pub label: String,
    /// Hint/example text.
    pub placeholder: String,
    /// Whether the parameter is required.
    pub required: bool,
    /// Default value (if any).
    pub default: Option<String>,
}

/// A default field mapping created with a preset source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetField {
    /// Human-readable field name.
    pub name: String,
    /// JSONPath expression to extract the value.
    pub json_path: String,
    /// Suggested format string.
    pub format_string: String,
}

/// Apply parameter substitution to a URL template.
///
/// Replaces `{{param_name}}` with the corresponding value from the params map.
pub fn substitute_params(
    template: &str,
    params: &std::collections::HashMap<String, String>,
) -> String {
    let mut result = template.to_string();
    for (key, value) in params {
        result = result.replace(&format!("{{{{{key}}}}}"), value);
    }
    result
}

/// Return all built-in presets.
pub fn builtin_presets() -> Vec<SourcePreset> {
    vec![usgs_river_preset(), noaa_weather_preset(), wsdot_ferry_preset()]
}

/// USGS River Gauge preset — NWIS instantaneous values API.
///
/// No API key required. Fetches gauge height and streamflow for a site.
fn usgs_river_preset() -> SourcePreset {
    SourcePreset {
        id: "usgs_river".to_string(),
        name: "USGS River Gauge".to_string(),
        description: "Water level and streamflow from USGS National Water Information System. No API key required.".to_string(),
        url_template: "https://waterservices.usgs.gov/nwis/iv/?format=json&sites={{site_id}}&parameterCd=00065,00060&siteStatus=all".to_string(),
        method: "GET".to_string(),
        headers: Some(r#"[{"key": "Accept", "value": "application/json"}]"#.to_string()),
        body_template: None,
        response_root_path: None,
        refresh_interval_secs: 300,
        params: vec![
            PresetParam {
                name: "site_id".to_string(),
                label: "USGS Site ID".to_string(),
                placeholder: "e.g., 12200500 (Skagit River near Mount Vernon, WA)".to_string(),
                required: true,
                default: None,
            },
        ],
        default_fields: vec![
            PresetField {
                name: "Water Level".to_string(),
                json_path: "$.value.timeSeries[0].values[0].value[0].value".to_string(),
                format_string: "{{value}} ft".to_string(),
            },
            PresetField {
                name: "Streamflow".to_string(),
                json_path: "$.value.timeSeries[1].values[0].value[0].value".to_string(),
                format_string: "{{value | round(0) | number_with_delimiter}} cfs".to_string(),
            },
        ],
    }
}

/// NOAA Weather preset — NWS observation stations API.
///
/// No API key required. Fetches current conditions from a weather station.
fn noaa_weather_preset() -> SourcePreset {
    SourcePreset {
        id: "noaa_weather".to_string(),
        name: "NOAA Weather Station".to_string(),
        description: "Current weather conditions from NOAA/NWS observation stations. No API key required.".to_string(),
        url_template: "https://api.weather.gov/stations/{{station_id}}/observations/latest".to_string(),
        method: "GET".to_string(),
        headers: Some(
            r#"[{"key": "User-Agent", "value": "cascades-dashboard/0.1"}, {"key": "Accept", "value": "application/geo+json"}]"#.to_string(),
        ),
        body_template: None,
        response_root_path: Some("$.properties".to_string()),
        refresh_interval_secs: 300,
        params: vec![
            PresetParam {
                name: "station_id".to_string(),
                label: "Station ID".to_string(),
                placeholder: "e.g., KBVS (Skagit Regional Airport)".to_string(),
                required: true,
                default: None,
            },
        ],
        default_fields: vec![
            PresetField {
                name: "Temperature".to_string(),
                json_path: "$.temperature.value".to_string(),
                format_string: "{{value | round(1)}}°C".to_string(),
            },
            PresetField {
                name: "Wind Speed".to_string(),
                json_path: "$.windSpeed.value".to_string(),
                format_string: "{{value | round(0)}} km/h".to_string(),
            },
            PresetField {
                name: "Conditions".to_string(),
                json_path: "$.textDescription".to_string(),
                format_string: "{{value}}".to_string(),
            },
        ],
    }
}

/// WSDOT Ferry Schedule preset — Washington State Ferries API.
///
/// Requires a WSDOT API access code (free registration).
fn wsdot_ferry_preset() -> SourcePreset {
    SourcePreset {
        id: "wsdot_ferry".to_string(),
        name: "WSDOT Ferry Schedule".to_string(),
        description: "Ferry schedules from Washington State DOT. Requires a free WSDOT API access code.".to_string(),
        url_template: "https://www.wsdot.wa.gov/Ferries/API/Schedule/rest/scheduletoday/{{route_id}}?apiaccesscode={{access_code}}".to_string(),
        method: "GET".to_string(),
        headers: Some(r#"[{"key": "Accept", "value": "application/json"}]"#.to_string()),
        body_template: None,
        response_root_path: None,
        refresh_interval_secs: 120,
        params: vec![
            PresetParam {
                name: "route_id".to_string(),
                label: "Route ID".to_string(),
                placeholder: "e.g., 9 (Anacortes / San Juan Islands)".to_string(),
                required: true,
                default: Some("9".to_string()),
            },
            PresetParam {
                name: "access_code".to_string(),
                label: "WSDOT API Access Code".to_string(),
                placeholder: "Your WSDOT API access code".to_string(),
                required: true,
                default: None,
            },
        ],
        default_fields: vec![
            PresetField {
                name: "Next Departure".to_string(),
                json_path: "$.TerminalCombos[0].Times[0].DepartingTime".to_string(),
                format_string: "{{value}}".to_string(),
            },
            PresetField {
                name: "Vessel".to_string(),
                json_path: "$.TerminalCombos[0].Times[0].VesselName".to_string(),
                format_string: "{{value}}".to_string(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn builtin_presets_returns_three() {
        let presets = builtin_presets();
        assert_eq!(presets.len(), 3);
        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"usgs_river"));
        assert!(ids.contains(&"noaa_weather"));
        assert!(ids.contains(&"wsdot_ferry"));
    }

    #[test]
    fn substitute_single_param() {
        let mut params = HashMap::new();
        params.insert("site_id".to_string(), "12200500".to_string());
        let url = substitute_params(
            "https://waterservices.usgs.gov/nwis/iv/?sites={{site_id}}",
            &params,
        );
        assert_eq!(
            url,
            "https://waterservices.usgs.gov/nwis/iv/?sites=12200500"
        );
    }

    #[test]
    fn substitute_multiple_params() {
        let mut params = HashMap::new();
        params.insert("route_id".to_string(), "9".to_string());
        params.insert("access_code".to_string(), "abc123".to_string());
        let url = substitute_params(
            "https://example.com/api/{{route_id}}?code={{access_code}}",
            &params,
        );
        assert_eq!(url, "https://example.com/api/9?code=abc123");
    }

    #[test]
    fn substitute_missing_param_unchanged() {
        let params = HashMap::new();
        let url = substitute_params("https://example.com/{{missing}}", &params);
        assert_eq!(url, "https://example.com/{{missing}}");
    }

    #[test]
    fn usgs_preset_has_required_site_id() {
        let preset = usgs_river_preset();
        assert_eq!(preset.params.len(), 1);
        assert_eq!(preset.params[0].name, "site_id");
        assert!(preset.params[0].required);
        assert_eq!(preset.default_fields.len(), 2);
    }

    #[test]
    fn noaa_preset_has_station_id() {
        let preset = noaa_weather_preset();
        assert_eq!(preset.params.len(), 1);
        assert_eq!(preset.params[0].name, "station_id");
        assert_eq!(preset.default_fields.len(), 3);
    }

    #[test]
    fn wsdot_preset_has_two_params() {
        let preset = wsdot_ferry_preset();
        assert_eq!(preset.params.len(), 2);
        let names: Vec<&str> = preset.params.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"route_id"));
        assert!(names.contains(&"access_code"));
        assert_eq!(preset.default_fields.len(), 2);
    }

    #[test]
    fn preset_url_substitution_usgs() {
        let preset = usgs_river_preset();
        let mut params = HashMap::new();
        params.insert("site_id".to_string(), "12150800".to_string());
        let url = substitute_params(&preset.url_template, &params);
        assert!(url.contains("sites=12150800"));
        assert!(!url.contains("{{"));
    }

    #[test]
    fn preset_url_substitution_noaa() {
        let preset = noaa_weather_preset();
        let mut params = HashMap::new();
        params.insert("station_id".to_string(), "KSEA".to_string());
        let url = substitute_params(&preset.url_template, &params);
        assert!(url.contains("/stations/KSEA/"));
        assert!(!url.contains("{{"));
    }
}
