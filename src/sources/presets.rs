//! Source presets — pre-built configurations for common APIs.
//!
//! Each preset defines a URL template, default headers, response root path,
//! and default field mappings. Users supply parameters (e.g. site_id for USGS)
//! and get a fully configured generic data source.

use serde::{Deserialize, Serialize};

/// A parameter that the user must (or may) supply when creating a source from a preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetParam {
    /// Parameter name, used as `{name}` in the URL template.
    pub name: String,
    /// Human-readable description shown in the UI.
    pub description: String,
    /// Whether this parameter is required.
    pub required: bool,
    /// Default value if not provided by the user.
    pub default: Option<String>,
    /// Placeholder text shown in the input field.
    pub placeholder: Option<String>,
}

/// A default field mapping included with the preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetField {
    /// Field name / label.
    pub name: String,
    /// JSONPath expression to extract the value from the response.
    pub json_path: String,
    /// Format string for display (e.g. `"{value} ft"`).
    pub format_string: String,
}

/// A source preset — a template for creating generic HTTP data sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcePreset {
    /// Unique preset identifier (e.g. `"usgs_river_gauge"`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Brief description of what this preset does.
    pub description: String,
    /// Category for grouping in the UI.
    pub category: String,
    /// URL template with `{param}` placeholders.
    pub url_template: String,
    /// HTTP method.
    pub method: String,
    /// Default headers.
    pub headers: serde_json::Value,
    /// Response root path (JSONPath) to extract a sub-tree before caching.
    pub response_root_path: Option<String>,
    /// Default refresh interval in seconds.
    pub refresh_interval_secs: i64,
    /// Parameters that users fill in.
    pub params: Vec<PresetParam>,
    /// Default field mappings.
    pub default_fields: Vec<PresetField>,
}

/// Request payload for creating a source from a preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateFromPresetRequest {
    /// The preset ID to use.
    pub preset_id: String,
    /// User-supplied parameter values. Keys match `PresetParam::name`.
    pub params: std::collections::HashMap<String, String>,
    /// Optional custom name for the created source. Falls back to preset name.
    pub name: Option<String>,
}

/// Return all built-in presets.
pub fn all_presets() -> Vec<SourcePreset> {
    vec![usgs_river_gauge(), noaa_weather(), wsdot_ferries()]
}

/// Look up a preset by ID.
pub fn get_preset(id: &str) -> Option<SourcePreset> {
    all_presets().into_iter().find(|p| p.id == id)
}

/// Substitute `{param}` placeholders in a URL template with user-provided values.
pub fn substitute_params(
    template: &str,
    params: &std::collections::HashMap<String, String>,
) -> String {
    let mut result = template.to_string();
    for (key, value) in params {
        result = result.replace(&format!("{{{}}}", key), value);
    }
    result
}

/// Validate that all required parameters are present.
pub fn validate_params(
    preset: &SourcePreset,
    params: &std::collections::HashMap<String, String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut resolved = params.clone();
    for p in &preset.params {
        if !resolved.contains_key(&p.name) {
            if let Some(default) = &p.default {
                resolved.insert(p.name.clone(), default.clone());
            } else if p.required {
                return Err(format!("missing required parameter: {}", p.name));
            }
        }
    }
    Ok(resolved)
}

// ─── USGS River Gauge (NWIS API) ──────────────────────────────────────────

fn usgs_river_gauge() -> SourcePreset {
    SourcePreset {
        id: "usgs_river_gauge".to_string(),
        name: "USGS River Gauge".to_string(),
        description: "Real-time water level and streamflow data from the USGS National Water Information System (NWIS). Free, no API key required.".to_string(),
        category: "Water".to_string(),
        url_template: "https://waterservices.usgs.gov/nwis/iv/?format=json&sites={site_id}&parameterCd=00065,00060&siteStatus=all".to_string(),
        method: "GET".to_string(),
        headers: serde_json::json!({"Accept": "application/json"}),
        response_root_path: None,
        refresh_interval_secs: 300,
        params: vec![
            PresetParam {
                name: "site_id".to_string(),
                description: "USGS site ID (e.g. 12200500 for Skagit River near Mount Vernon, WA)".to_string(),
                required: true,
                default: None,
                placeholder: Some("12200500".to_string()),
            },
        ],
        default_fields: vec![
            PresetField {
                name: "Water Level".to_string(),
                json_path: "$.value.timeSeries[0].values[0].value[-1:].value".to_string(),
                format_string: "{value} ft".to_string(),
            },
            PresetField {
                name: "Streamflow".to_string(),
                json_path: "$.value.timeSeries[1].values[0].value[-1:].value".to_string(),
                format_string: "{value} cfs".to_string(),
            },
        ],
    }
}

// ─── NOAA Weather (NWS API) ───────────────────────────────────────────────

fn noaa_weather() -> SourcePreset {
    SourcePreset {
        id: "noaa_weather".to_string(),
        name: "NOAA Weather".to_string(),
        description: "Latest weather observation from the National Weather Service API. Free, no API key required. Requires a station ID (e.g. KBVS).".to_string(),
        category: "Weather".to_string(),
        url_template: "https://api.weather.gov/stations/{station_id}/observations/latest".to_string(),
        method: "GET".to_string(),
        headers: serde_json::json!({
            "User-Agent": "cascades-dashboard/1.0",
            "Accept": "application/geo+json"
        }),
        response_root_path: Some("$.properties".to_string()),
        refresh_interval_secs: 300,
        params: vec![
            PresetParam {
                name: "station_id".to_string(),
                description: "NWS station identifier (e.g. KBVS for Burlington/Mount Vernon, WA)".to_string(),
                required: true,
                default: None,
                placeholder: Some("KBVS".to_string()),
            },
        ],
        default_fields: vec![
            PresetField {
                name: "Temperature".to_string(),
                json_path: "$.temperature.value".to_string(),
                format_string: "{value} \u{00B0}C".to_string(),
            },
            PresetField {
                name: "Wind Speed".to_string(),
                json_path: "$.windSpeed.value".to_string(),
                format_string: "{value} km/h".to_string(),
            },
            PresetField {
                name: "Conditions".to_string(),
                json_path: "$.textDescription".to_string(),
                format_string: "{value}".to_string(),
            },
        ],
    }
}

// ─── WSDOT Ferries ────────────────────────────────────────────────────────

fn wsdot_ferries() -> SourcePreset {
    SourcePreset {
        id: "wsdot_ferries".to_string(),
        name: "WSDOT Ferries".to_string(),
        description: "Washington State ferry schedule for today. Requires a free WSDOT API access code from wsdot.wa.gov/traffic/api.".to_string(),
        category: "Transit".to_string(),
        url_template: "https://www.wsdot.wa.gov/Ferries/API/Schedule/rest/scheduletoday/{route_id}?apiaccesscode={access_code}".to_string(),
        method: "GET".to_string(),
        headers: serde_json::json!({}),
        response_root_path: None,
        refresh_interval_secs: 300,
        params: vec![
            PresetParam {
                name: "route_id".to_string(),
                description: "Ferry route ID (e.g. 9 for Anacortes/San Juan Islands)".to_string(),
                required: true,
                default: Some("9".to_string()),
                placeholder: Some("9".to_string()),
            },
            PresetParam {
                name: "access_code".to_string(),
                description: "WSDOT API access code (get one free at wsdot.wa.gov/traffic/api)".to_string(),
                required: true,
                default: None,
                placeholder: Some("your-access-code".to_string()),
            },
        ],
        default_fields: vec![
            PresetField {
                name: "Next Departure".to_string(),
                json_path: "$.TerminalCombos[0].Times[0].DepartingTime".to_string(),
                format_string: "{value}".to_string(),
            },
            PresetField {
                name: "Vessel".to_string(),
                json_path: "$.TerminalCombos[0].Times[0].VesselName".to_string(),
                format_string: "{value}".to_string(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn all_presets_returns_three() {
        let presets = all_presets();
        assert_eq!(presets.len(), 3);
        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"usgs_river_gauge"));
        assert!(ids.contains(&"noaa_weather"));
        assert!(ids.contains(&"wsdot_ferries"));
    }

    #[test]
    fn get_preset_by_id() {
        assert!(get_preset("usgs_river_gauge").is_some());
        assert!(get_preset("noaa_weather").is_some());
        assert!(get_preset("wsdot_ferries").is_some());
        assert!(get_preset("nonexistent").is_none());
    }

    #[test]
    fn substitute_params_replaces_placeholders() {
        let template = "https://example.com/{site_id}/data?key={api_key}";
        let mut params = HashMap::new();
        params.insert("site_id".to_string(), "12345".to_string());
        params.insert("api_key".to_string(), "abc123".to_string());
        let result = substitute_params(template, &params);
        assert_eq!(result, "https://example.com/12345/data?key=abc123");
    }

    #[test]
    fn substitute_params_leaves_unknown_placeholders() {
        let template = "https://example.com/{site_id}/{unknown}";
        let mut params = HashMap::new();
        params.insert("site_id".to_string(), "12345".to_string());
        let result = substitute_params(template, &params);
        assert_eq!(result, "https://example.com/12345/{unknown}");
    }

    #[test]
    fn validate_params_fills_defaults() {
        let preset = wsdot_ferries();
        let mut params = HashMap::new();
        params.insert("access_code".to_string(), "mycode".to_string());
        // route_id has a default of "9", so it should be filled in
        let resolved = validate_params(&preset, &params).unwrap();
        assert_eq!(resolved.get("route_id").unwrap(), "9");
        assert_eq!(resolved.get("access_code").unwrap(), "mycode");
    }

    #[test]
    fn validate_params_rejects_missing_required() {
        let preset = usgs_river_gauge();
        let params = HashMap::new();
        let result = validate_params(&preset, &params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("site_id"));
    }

    #[test]
    fn usgs_preset_url_substitution() {
        let preset = usgs_river_gauge();
        let mut params = HashMap::new();
        params.insert("site_id".to_string(), "12200500".to_string());
        let url = substitute_params(&preset.url_template, &params);
        assert!(url.contains("sites=12200500"));
        assert!(url.contains("parameterCd=00065,00060"));
    }

    #[test]
    fn noaa_preset_url_substitution() {
        let preset = noaa_weather();
        let mut params = HashMap::new();
        params.insert("station_id".to_string(), "KBVS".to_string());
        let url = substitute_params(&preset.url_template, &params);
        assert!(url.contains("stations/KBVS/observations/latest"));
    }

    #[test]
    fn wsdot_preset_url_substitution() {
        let preset = wsdot_ferries();
        let mut params = HashMap::new();
        params.insert("route_id".to_string(), "9".to_string());
        params.insert("access_code".to_string(), "test123".to_string());
        let url = substitute_params(&preset.url_template, &params);
        assert!(url.contains("scheduletoday/9"));
        assert!(url.contains("apiaccesscode=test123"));
    }

    #[test]
    fn presets_serialize_to_json() {
        let presets = all_presets();
        let json = serde_json::to_string(&presets).unwrap();
        assert!(json.contains("usgs_river_gauge"));
        assert!(json.contains("noaa_weather"));
        assert!(json.contains("wsdot_ferries"));
    }

    #[test]
    fn create_from_preset_request_deserializes() {
        let json = r#"{
            "preset_id": "usgs_river_gauge",
            "params": {"site_id": "12200500"},
            "name": "My River Gauge"
        }"#;
        let req: CreateFromPresetRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.preset_id, "usgs_river_gauge");
        assert_eq!(req.params.get("site_id").unwrap(), "12200500");
        assert_eq!(req.name.unwrap(), "My River Gauge");
    }
}
