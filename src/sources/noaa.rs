use crate::config::LocationConfig;
use crate::domain::WeatherObservation;
use crate::sources::{Source, SourceError};
use serde::Deserialize;
use std::time::Duration;

/// NOAA/NWS API base URL.
const NWS_API_BASE: &str = "https://api.weather.gov";

/// Fixture JSON returned when SKAGIT_FIXTURE_DATA=1.
const FIXTURE_RESPONSE: &str = include_str!("fixtures/noaa_observation.json");

/// Required User-Agent for the NWS API (they block default user agents).
const USER_AGENT: &str = "skagit-flats/0.1 (e-ink dashboard)";

/// Maximum number of retry attempts on transient errors.
const MAX_RETRIES: u32 = 4;

/// NOAA weather observation source backed by the NWS API.
///
/// On first fetch, resolves the nearest observation station via the /points
/// endpoint, then fetches the latest observation from that station. The station
/// ID is cached for subsequent fetches. Uses exponential backoff on errors with
/// a maximum interval of 5 minutes.
pub struct NoaaSource {
    latitude: f64,
    longitude: f64,
    /// Cached station ID resolved from the /points endpoint.
    station_id: std::cell::RefCell<Option<String>>,
    refresh: Duration,
    use_fixtures: bool,
}

impl NoaaSource {
    pub fn new(location: &LocationConfig, interval_secs: u64, use_fixtures: bool) -> Self {
        Self {
            latitude: location.latitude,
            longitude: location.longitude,
            station_id: std::cell::RefCell::new(None),
            refresh: Duration::from_secs(interval_secs),
            use_fixtures,
        }
    }
}

// SAFETY: NoaaSource uses RefCell for interior mutability of the station_id
// cache. This is safe because Source::fetch takes &self and each source runs on
// a single dedicated thread — there is no concurrent access.
unsafe impl Send for NoaaSource {}

/// Raw observation response from GET /stations/{id}/observations/latest.
#[derive(Debug, Deserialize)]
struct ObservationResponse {
    properties: ObservationProperties,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObservationProperties {
    /// Station URL, e.g. "https://api.weather.gov/stations/KBVS".
    station: String,
    /// ISO-8601 observation timestamp.
    timestamp: String,
    /// Human-readable sky condition, e.g. "Mostly Cloudy".
    text_description: String,
    /// Temperature in Celsius (value may be null for missing readings).
    temperature: MeasuredValue,
    /// Wind speed in km/h (value may be null).
    wind_speed: MeasuredValue,
    /// Wind direction in degrees (value may be null).
    wind_direction: MeasuredValue,
}

#[derive(Debug, Deserialize)]
struct MeasuredValue {
    value: Option<f64>,
}

/// Response from GET /points/{lat},{lon} — we only need the station URL.
#[derive(Debug, Deserialize)]
struct PointsResponse {
    properties: PointsProperties,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PointsProperties {
    observation_stations: String,
}

/// Response from the observation stations list endpoint.
#[derive(Debug, Deserialize)]
struct StationsResponse {
    features: Vec<StationFeature>,
}

#[derive(Debug, Deserialize)]
struct StationFeature {
    properties: StationProperties,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StationProperties {
    station_identifier: String,
}

impl Source for NoaaSource {
    fn id(&self) -> &str {
        "weather"
    }

    fn name(&self) -> &str {
        "noaa-weather"
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

        parse_observation(&response_text)
    }
}

/// Parse an observation JSON response into a serde_json::Value.
fn parse_observation(json: &str) -> Result<serde_json::Value, SourceError> {
    let obs: ObservationResponse =
        serde_json::from_str(json).map_err(|e| SourceError::Parse(e.to_string()))?;

    let props = &obs.properties;

    // Extract station name from the station URL (last path segment).
    let _station_name = props
        .station
        .rsplit('/')
        .next()
        .unwrap_or("Unknown Station");

    // Convert Celsius to Fahrenheit, defaulting to 0 if missing.
    let temp_c = props.temperature.value.unwrap_or(0.0);
    let temp_f = temp_c * 9.0 / 5.0 + 32.0;

    // Convert km/h to mph, defaulting to 0 if missing.
    let wind_kmh = props.wind_speed.value.unwrap_or(0.0);
    let wind_mph = wind_kmh * 0.621371;

    // Convert wind degrees to compass direction.
    let wind_dir = props
        .wind_direction
        .value
        .map(degrees_to_compass)
        .unwrap_or_else(|| "Calm".to_string());

    let sky = if props.text_description.is_empty() {
        "Unknown".to_string()
    } else {
        props.text_description.clone()
    };

    // Parse ISO-8601 timestamp to Unix epoch (best effort).
    let observation_time = parse_iso8601_to_epoch(&props.timestamp);

    let weather = WeatherObservation {
        temperature_f: temp_f as f32,
        wind_speed_mph: wind_mph as f32,
        wind_direction: wind_dir,
        sky_condition: sky,
        precip_chance_pct: 0.0,
        observation_time,
    };

    serde_json::to_value(weather).map_err(|e| SourceError::Parse(e.to_string()))
}

impl NoaaSource {
    /// Fetch live observation data from the NWS API with retry/backoff.
    fn fetch_live(&self) -> Result<String, SourceError> {
        let station_id = self.resolve_station()?;

        let url = format!(
            "{}/stations/{}/observations/latest",
            NWS_API_BASE, station_id
        );

        self.get_with_retry(&url)
    }

    /// Resolve the nearest observation station for the configured lat/lon.
    /// Caches the result for subsequent calls.
    fn resolve_station(&self) -> Result<String, SourceError> {
        if let Some(id) = self.station_id.borrow().as_ref() {
            return Ok(id.clone());
        }

        let points_url = format!(
            "{}/points/{:.4},{:.4}",
            NWS_API_BASE, self.latitude, self.longitude
        );

        let points_text = self.get_with_retry(&points_url)?;
        let points: PointsResponse =
            serde_json::from_str(&points_text).map_err(|e| SourceError::Parse(e.to_string()))?;

        // Fetch the stations list to get the nearest station identifier.
        let stations_text = self.get_with_retry(&points.properties.observation_stations)?;
        let stations: StationsResponse = serde_json::from_str(&stations_text)
            .map_err(|e| SourceError::Parse(e.to_string()))?;

        let station_id = stations
            .features
            .first()
            .map(|f| f.properties.station_identifier.clone())
            .ok_or_else(|| {
                SourceError::Parse("no observation stations found for location".to_string())
            })?;

        log::info!("resolved NOAA station: {}", station_id);
        *self.station_id.borrow_mut() = Some(station_id.clone());
        Ok(station_id)
    }

    /// HTTP GET with exponential backoff retry.
    fn get_with_retry(&self, url: &str) -> Result<String, SourceError> {
        let mut last_err = None;
        let mut delay = Duration::from_secs(2);
        let max_delay = Duration::from_secs(300); // 5 minutes

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                log::warn!(
                    "NOAA retry {}/{} after {:?} for {}",
                    attempt,
                    MAX_RETRIES,
                    delay,
                    url
                );
                std::thread::sleep(delay);
                delay = std::cmp::min(delay * 2, max_delay);
            }

            match ureq::get(url)
                .set("User-Agent", USER_AGENT)
                .set("Accept", "application/geo+json")
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

/// Convert wind direction in degrees to a compass abbreviation.
fn degrees_to_compass(degrees: f64) -> String {
    let directions = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ];
    let idx = ((degrees + 11.25) / 22.5) as usize % 16;
    directions[idx].to_string()
}

/// Best-effort parse of an ISO-8601 timestamp to Unix epoch seconds.
/// Returns 0 if parsing fails (avoids pulling in a time library).
fn parse_iso8601_to_epoch(s: &str) -> u64 {
    // Expected format: "2026-03-28T12:53:00+00:00" or similar.
    // Minimal parser: extract date + time, assume UTC if offset is +00:00.
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

    // Parse time: HH:MM:SS (ignore timezone offset for simplicity).
    let time_part = time_rest
        .split('+')
        .next()
        .and_then(|s| s.split('-').next())
        .unwrap_or(time_rest);
    let time_parts: Vec<&str> = time_part.split(':').collect();
    if time_parts.len() < 2 {
        return 0;
    }
    let hour: i64 = time_parts[0].parse().unwrap_or(0);
    let min: i64 = time_parts[1].parse().unwrap_or(0);
    let sec: i64 = time_parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    // Convert to Unix epoch using a simplified calculation.
    // Days from year 1970 to year Y (ignoring leap seconds).
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

    let epoch = days * 86400 + hour * 3600 + min * 60 + sec;
    if epoch >= 0 {
        epoch as u64
    } else {
        0
    }
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::WeatherObservation;

    #[test]
    fn parse_fixture_response() {
        let obs: ObservationResponse =
            serde_json::from_str(FIXTURE_RESPONSE).expect("fixture JSON should parse");
        assert!(obs.properties.temperature.value.is_some());
        assert!(obs.properties.wind_speed.value.is_some());
        assert!(!obs.properties.text_description.is_empty());
    }

    #[test]
    fn fixture_mode_returns_weather_observation() {
        let location = crate::config::LocationConfig {
            latitude: 48.4232,
            longitude: -122.3351,
            name: "Mount Vernon, WA".to_string(),
        };
        let source = NoaaSource::new(&location, 300, true);

        let result = source.fetch();
        assert!(result.is_ok(), "fixture fetch should succeed: {:?}", result);

        let value = result.unwrap();
        let obs: WeatherObservation = serde_json::from_value(value).unwrap();
        // 11.1°C = ~51.98°F
        assert!((obs.temperature_f - 51.98).abs() < 1.0);
        // 14.8 km/h = ~9.2 mph
        assert!((obs.wind_speed_mph - 9.2).abs() < 1.0);
        assert_eq!(obs.wind_direction, "SSW");
        assert_eq!(obs.sky_condition, "Mostly Cloudy");
        assert!(obs.observation_time > 0);
    }

    #[test]
    fn missing_fields_uses_defaults() {
        let json = r#"{
            "properties": {
                "station": "https://api.weather.gov/stations/KBVS",
                "timestamp": "2026-03-28T12:53:00+00:00",
                "textDescription": "",
                "temperature": { "value": null, "unitCode": "wmoUnit:degC" },
                "windSpeed": { "value": null, "unitCode": "wmoUnit:km_h-1" },
                "windDirection": { "value": null, "unitCode": "wmoUnit:degree_(angle)" }
            }
        }"#;

        let result = parse_observation(json);
        assert!(result.is_ok());

        let obs: WeatherObservation = serde_json::from_value(result.unwrap()).unwrap();
        // 0°C = 32°F
        assert!((obs.temperature_f - 32.0).abs() < 0.1);
        assert!((obs.wind_speed_mph - 0.0).abs() < 0.1);
        assert_eq!(obs.wind_direction, "Calm");
        assert_eq!(obs.sky_condition, "Unknown");
    }

    #[test]
    fn invalid_json_returns_parse_error() {
        let result = parse_observation("not json");
        assert!(result.is_err());
        match result.unwrap_err() {
            SourceError::Parse(_) => {}
            other => panic!("expected Parse error, got {:?}", other),
        }
    }

    #[test]
    fn degrees_to_compass_conversions() {
        assert_eq!(degrees_to_compass(0.0), "N");
        assert_eq!(degrees_to_compass(90.0), "E");
        assert_eq!(degrees_to_compass(180.0), "S");
        assert_eq!(degrees_to_compass(270.0), "W");
        assert_eq!(degrees_to_compass(210.0), "SSW");
        assert_eq!(degrees_to_compass(45.0), "NE");
        assert_eq!(degrees_to_compass(315.0), "NW");
    }

    #[test]
    fn parse_iso8601_timestamp() {
        // 2026-03-28T12:53:00+00:00
        let ts = parse_iso8601_to_epoch("2026-03-28T12:53:00+00:00");
        assert!(ts > 0);
        // Should be roughly 2026-03-28 in Unix time (around 1774792380)
        assert!(ts > 1774000000);
        assert!(ts < 1780000000);
    }

    #[test]
    fn parse_iso8601_bad_input_returns_zero() {
        assert_eq!(parse_iso8601_to_epoch("not a date"), 0);
        assert_eq!(parse_iso8601_to_epoch(""), 0);
    }

    #[test]
    fn source_id_name_and_interval() {
        let location = crate::config::LocationConfig {
            latitude: 48.4232,
            longitude: -122.3351,
            name: "Mount Vernon, WA".to_string(),
        };
        let source = NoaaSource::new(&location, 300, false);
        assert_eq!(source.id(), "weather");
        assert_eq!(source.name(), "noaa-weather");
        assert_eq!(source.refresh_interval(), Duration::from_secs(300));
    }
}
