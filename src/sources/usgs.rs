use crate::domain::RiverGauge;
use crate::sources::{Source, SourceError};
use serde::Deserialize;
use std::time::Duration;

/// USGS NWIS instantaneous values API endpoint.
const NWIS_IV_URL: &str = "https://waterservices.usgs.gov/nwis/iv/";

/// Fixture JSON returned when SKAGIT_FIXTURE_DATA=1.
const FIXTURE_RESPONSE: &str = include_str!("fixtures/usgs_gauge.json");

/// Maximum number of retry attempts on transient errors.
const MAX_RETRIES: u32 = 4;

/// USGS parameter code for gauge height (feet).
const PARAM_GAUGE_HEIGHT: &str = "00065";

/// USGS parameter code for streamflow (cubic feet per second).
const PARAM_STREAMFLOW: &str = "00060";

/// USGS river gauge source backed by the NWIS instantaneous values API.
///
/// Fetches gauge height and streamflow for a configured USGS site ID. The API
/// is free and requires no API key. Uses exponential backoff on errors.
pub struct UsgsSource {
    site_id: String,
    refresh: Duration,
    use_fixtures: bool,
}

impl UsgsSource {
    pub fn new(site_id: &str, interval_secs: u64, use_fixtures: bool) -> Self {
        Self {
            site_id: site_id.to_string(),
            refresh: Duration::from_secs(interval_secs),
            use_fixtures,
        }
    }
}

impl Source for UsgsSource {
    fn id(&self) -> &str {
        "river"
    }

    fn name(&self) -> &str {
        "usgs-river"
    }

    fn refresh_interval(&self) -> Duration {
        self.refresh
    }

    fn fetch(&self) -> Result<serde_json::Value, SourceError> {
        let response_text = if self.use_fixtures {
            FIXTURE_RESPONSE.to_string()
        } else {
            self.fetch_live()?
        };

        parse_gauge(&response_text)
    }
}

impl UsgsSource {
    /// Fetch live gauge data from the USGS NWIS API with retry/backoff.
    fn fetch_live(&self) -> Result<String, SourceError> {
        let url = format!(
            "{}?format=json&sites={}&parameterCd={},{}&siteStatus=all",
            NWIS_IV_URL, self.site_id, PARAM_GAUGE_HEIGHT, PARAM_STREAMFLOW
        );

        self.get_with_retry(&url)
    }

    /// HTTP GET with exponential backoff retry.
    fn get_with_retry(&self, url: &str) -> Result<String, SourceError> {
        let mut last_err = None;
        let mut delay = Duration::from_secs(2);
        let max_delay = Duration::from_secs(300);

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                log::warn!(
                    "USGS retry {}/{} after {:?} for {}",
                    attempt,
                    MAX_RETRIES,
                    delay,
                    url
                );
                std::thread::sleep(delay);
                delay = std::cmp::min(delay * 2, max_delay);
            }

            match ureq::get(url)
                .set("Accept", "application/json")
                .timeout(Duration::from_secs(15))
                .call()
            {
                Ok(response) => {
                    return response
                        .into_string()
                        .map_err(|e| SourceError::Parse(e.to_string()));
                }
                Err(e) => {
                    last_err = Some(SourceError::Network(e.to_string()));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| SourceError::Network("unknown error".to_string())))
    }
}

// --- NWIS JSON response types ---

#[derive(Debug, Deserialize)]
struct NwisResponse {
    value: NwisValue,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NwisValue {
    time_series: Vec<TimeSeries>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimeSeries {
    source_info: SourceInfo,
    variable: Variable,
    values: Vec<ValueSet>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceInfo {
    site_name: String,
    site_code: Vec<SiteCode>,
}

#[derive(Debug, Deserialize)]
struct SiteCode {
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Variable {
    variable_code: Vec<VariableCode>,
}

#[derive(Debug, Deserialize)]
struct VariableCode {
    value: String,
}

#[derive(Debug, Deserialize)]
struct ValueSet {
    value: Vec<Reading>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reading {
    value: String,
    date_time: String,
}

/// Parse an NWIS instantaneous values response into a serde_json::Value.
fn parse_gauge(json: &str) -> Result<serde_json::Value, SourceError> {
    let resp: NwisResponse =
        serde_json::from_str(json).map_err(|e| SourceError::Parse(e.to_string()))?;

    if resp.value.time_series.is_empty() {
        return Err(SourceError::Parse(
            "no time series in USGS response".to_string(),
        ));
    }

    let mut site_id = String::new();
    let mut site_name = String::new();
    let mut water_level_ft: f32 = 0.0;
    let mut streamflow_cfs: f32 = 0.0;
    let mut timestamp: u64 = 0;

    for ts in &resp.value.time_series {
        // Extract site info from the first time series.
        if site_id.is_empty() {
            site_id = ts
                .source_info
                .site_code
                .first()
                .map(|sc| sc.value.clone())
                .unwrap_or_default();
            site_name = title_case(&ts.source_info.site_name);
        }

        let param_code = ts
            .variable
            .variable_code
            .first()
            .map(|vc| vc.value.as_str())
            .unwrap_or("");

        // Get the most recent reading (last in the array).
        let latest = ts
            .values
            .first()
            .and_then(|vs| vs.value.last());

        if let Some(reading) = latest {
            let val: f32 = reading.value.parse().unwrap_or(0.0);
            let ts_epoch = parse_nwis_datetime(&reading.date_time);

            match param_code {
                PARAM_GAUGE_HEIGHT => {
                    water_level_ft = val;
                    if ts_epoch > timestamp {
                        timestamp = ts_epoch;
                    }
                }
                PARAM_STREAMFLOW => {
                    streamflow_cfs = val;
                    if ts_epoch > timestamp {
                        timestamp = ts_epoch;
                    }
                }
                _ => {}
            }
        }
    }

    let gauge = RiverGauge {
        site_id,
        site_name,
        water_level_ft,
        streamflow_cfs,
        timestamp,
    };

    serde_json::to_value(gauge).map_err(|e| SourceError::Parse(e.to_string()))
}

/// Parse NWIS datetime format "2026-03-28T12:45:00.000-07:00" to Unix epoch.
/// Best-effort; returns 0 on failure.
fn parse_nwis_datetime(s: &str) -> u64 {
    let s = s.trim();

    // Split on 'T' to get date and time parts.
    let (date_part, time_rest) = match s.split_once('T') {
        Some(parts) => parts,
        None => return 0,
    };

    // Parse date: YYYY-MM-DD
    let date_parts: Vec<&str> = date_part.split('-').collect();
    if date_parts.len() != 3 {
        return 0;
    }
    let year: i64 = date_parts[0].parse().unwrap_or(0);
    let month: i64 = date_parts[1].parse().unwrap_or(0);
    let day: i64 = date_parts[2].parse().unwrap_or(0);

    // Strip fractional seconds and timezone for time extraction.
    // Format: "12:45:00.000-07:00" -> time="12:45:00", offset="-07:00"
    let (time_and_frac, tz_offset_secs) = extract_tz_offset(time_rest);
    let time_part = time_and_frac.split('.').next().unwrap_or(time_and_frac);

    let time_parts: Vec<&str> = time_part.split(':').collect();
    if time_parts.len() < 2 {
        return 0;
    }
    let hour: i64 = time_parts[0].parse().unwrap_or(0);
    let min: i64 = time_parts[1].parse().unwrap_or(0);
    let sec: i64 = time_parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    // Convert to Unix epoch.
    let mut days: i64 = 0;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }

    let month_days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += month_days[m as usize];
        if m == 2 && is_leap_year(year) {
            days += 1;
        }
    }
    days += day - 1;

    // Compute UTC epoch by subtracting the timezone offset.
    let epoch = days * 86400 + hour * 3600 + min * 60 + sec - tz_offset_secs;
    if epoch >= 0 {
        epoch as u64
    } else {
        0
    }
}

/// Extract timezone offset in seconds from a time string.
/// E.g. "12:45:00.000-07:00" -> ("12:45:00.000", -25200)
fn extract_tz_offset(s: &str) -> (&str, i64) {
    // Look for +HH:MM or -HH:MM at the end.
    if let Some(idx) = s.rfind('+')
        && idx > 0
    {
        let offset = parse_tz_hhmm(&s[idx + 1..]);
        return (&s[..idx], offset);
    }
    // For negative offset, find the last '-' that isn't at position 0.
    // The time portion won't have '-', but the fractional part might not either.
    // Search from the end for '-' after the time digits.
    if let Some(idx) = s.rfind('-')
        && idx > 0
    {
        let offset = -parse_tz_hhmm(&s[idx + 1..]);
        return (&s[..idx], offset);
    }
    (s, 0)
}

/// Parse "07:00" to 25200 seconds.
fn parse_tz_hhmm(s: &str) -> i64 {
    let parts: Vec<&str> = s.split(':').collect();
    let h: i64 = parts.first().and_then(|v| v.parse().ok()).unwrap_or(0);
    let m: i64 = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
    h * 3600 + m * 60
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Convert an all-caps site name like "SKAGIT RIVER NEAR MOUNT VERNON, WA"
/// to title case: "Skagit River Near Mount Vernon, WA".
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|word| {
            // Keep two-letter state abbreviations uppercase.
            if word.len() <= 2 && word.chars().all(|c| c.is_ascii_uppercase()) {
                word.to_string()
            } else {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => {
                        let mut result = first.to_uppercase().to_string();
                        result.extend(chars.map(|c| c.to_ascii_lowercase()));
                        result
                    }
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RiverGauge;

    #[test]
    fn parse_fixture_response() {
        let resp: NwisResponse =
            serde_json::from_str(FIXTURE_RESPONSE).expect("fixture JSON should parse");
        assert_eq!(resp.value.time_series.len(), 2);
        assert_eq!(
            resp.value.time_series[0].source_info.site_name,
            "SKAGIT RIVER NEAR MOUNT VERNON, WA"
        );
    }

    #[test]
    fn fixture_mode_returns_river_gauge() {
        let source = UsgsSource::new("12200500", 300, true);
        let result = source.fetch();
        assert!(result.is_ok(), "fixture fetch should succeed: {:?}", result);

        let gauge: RiverGauge = serde_json::from_value(result.unwrap()).unwrap();
        assert_eq!(gauge.site_id, "12200500");
        assert!(gauge.site_name.contains("Skagit River"));
        assert!((gauge.water_level_ft - 11.87).abs() < 0.01);
        assert!((gauge.streamflow_cfs - 8750.0).abs() < 1.0);
        assert!(gauge.timestamp > 0);
    }

    #[test]
    fn missing_parameter_returns_zero() {
        // Response with only gauge height, no streamflow.
        let json = r#"{
            "value": {
                "timeSeries": [
                    {
                        "sourceInfo": {
                            "siteName": "TEST SITE",
                            "siteCode": [{"value": "99999999"}]
                        },
                        "variable": {
                            "variableCode": [{"value": "00065"}]
                        },
                        "values": [{
                            "value": [
                                {"value": "5.5", "dateTime": "2026-03-28T10:00:00.000-07:00"}
                            ]
                        }]
                    }
                ]
            }
        }"#;

        let result = parse_gauge(json);
        assert!(result.is_ok());

        let gauge: RiverGauge = serde_json::from_value(result.unwrap()).unwrap();
        assert!((gauge.water_level_ft - 5.5).abs() < 0.01);
        assert!((gauge.streamflow_cfs - 0.0).abs() < 0.01);
    }

    #[test]
    fn invalid_json_returns_parse_error() {
        let result = parse_gauge("not json");
        assert!(result.is_err());
        match result.unwrap_err() {
            SourceError::Parse(_) => {}
            other => panic!("expected Parse error, got {:?}", other),
        }
    }

    #[test]
    fn empty_time_series_returns_error() {
        let json = r#"{"value": {"timeSeries": []}}"#;
        let result = parse_gauge(json);
        assert!(result.is_err());
        match result.unwrap_err() {
            SourceError::Parse(msg) => assert!(msg.contains("no time series")),
            other => panic!("expected Parse error, got {:?}", other),
        }
    }

    #[test]
    fn parse_nwis_datetime_with_negative_offset() {
        let ts = parse_nwis_datetime("2026-03-28T12:45:00.000-07:00");
        assert!(ts > 0);
        // 2026-03-28T12:45:00-07:00 = 2026-03-28T19:45:00Z
        // Should be around 1774828800 + 19*3600 + 45*60 = roughly 1774900500
        assert!(ts > 1774000000);
        assert!(ts < 1780000000);
    }

    #[test]
    fn parse_nwis_datetime_bad_input_returns_zero() {
        assert_eq!(parse_nwis_datetime("not a date"), 0);
        assert_eq!(parse_nwis_datetime(""), 0);
    }

    #[test]
    fn title_case_conversion() {
        assert_eq!(
            title_case("SKAGIT RIVER NEAR MOUNT VERNON, WA"),
            "Skagit River Near Mount Vernon, WA"
        );
        assert_eq!(title_case("SINGLE"), "Single");
        assert_eq!(title_case(""), "");
    }

    #[test]
    fn source_id_name_and_interval() {
        let source = UsgsSource::new("12200500", 300, false);
        assert_eq!(source.id(), "river");
        assert_eq!(source.name(), "usgs-river");
        assert_eq!(source.refresh_interval(), Duration::from_secs(300));
    }
}
