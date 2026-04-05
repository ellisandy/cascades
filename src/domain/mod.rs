use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Current weather at a NOAA observation station.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherObservation {
    pub temperature_f: f32,
    pub wind_speed_mph: f32,
    pub wind_direction: String,
    pub sky_condition: String,
    /// Probability of precipitation, 0–100.
    #[serde(default)]
    pub precip_chance_pct: f32,
    /// Unix timestamp of the observation.
    pub observation_time: u64,
}

/// River gauge reading from a USGS NWIS site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiverGauge {
    pub site_id: String,
    pub site_name: String,
    pub water_level_ft: f32,
    pub streamflow_cfs: f32,
    /// Unix timestamp of the reading.
    pub timestamp: u64,
}

/// Ferry vessel status and upcoming departures for a WSDOT route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FerryStatus {
    pub route: String,
    pub vessel_name: String,
    /// Estimated departure times as Unix timestamps.
    pub estimated_departures: Vec<u64>,
}

/// Trail or campsite suitability from external sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrailCondition {
    pub destination_name: String,
    pub suitability_summary: String,
    /// Unix timestamp of the last update.
    pub last_updated: u64,
}

/// Road closure or restriction status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoadStatus {
    pub road_name: String,
    /// Human-readable status: "open", "closed", "restricted", etc.
    pub status: String,
    pub affected_segment: String,
    /// Unix timestamp of the last status update.
    #[serde(default)]
    pub timestamp: u64,
}

/// Which data signals are relevant for a given destination.
///
/// Controls both which source panels appear in the planning view and which
/// criteria checks are applied during evaluation. Setting a signal to `false`
/// suppresses it entirely for that destination — even if a criterion threshold
/// is configured.
///
/// Weather is always relevant; the remaining signals default to `true` for
/// backward compatibility. Explicitly set unused signals to `false` in
/// `destinations.toml` so the display omits them for that destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelevantSignals {
    /// Temperature, precipitation, wind. Always meaningful.
    #[serde(default = "signal_default_true")]
    pub weather: bool,
    /// River gauge — flooding and streamflow data.
    /// Relevant for lowland or valley destinations at flood risk.
    #[serde(default = "signal_default_true")]
    pub river: bool,
    /// Ferry schedule and vessel status.
    /// Relevant for island or ferry-dependent destinations.
    #[serde(default = "signal_default_true")]
    pub ferry: bool,
    /// Trail and campsite conditions.
    /// Relevant for hiking or camping destinations.
    #[serde(default = "signal_default_true")]
    pub trail: bool,
    /// Road access and closure status.
    /// Relevant for destinations with seasonal or closure-prone roads.
    #[serde(default = "signal_default_true")]
    pub road: bool,
}

fn signal_default_true() -> bool {
    true
}

impl Default for RelevantSignals {
    fn default() -> Self {
        RelevantSignals {
            weather: true,
            river: true,
            ferry: true,
            trail: true,
            road: true,
        }
    }
}

/// Per-destination go/no-go thresholds, loaded from destinations.toml.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TripCriteria {
    pub min_temp_f: Option<f32>,
    pub max_temp_f: Option<f32>,
    pub max_precip_chance_pct: Option<f32>,
    pub max_river_level_ft: Option<f32>,
    pub max_river_flow_cfs: Option<f32>,
    #[serde(default)]
    pub road_open_required: bool,
}

/// Result of evaluating a destination against current conditions.
///
/// States in priority order (see `docs/product/trip-recommendation-model.md`):
/// - `NoGo`: a hard criterion is exceeded — don't go.
/// - `Unknown`: no blocker confirmed but required data is absent or stale.
/// - `Caution`: all criteria met but one or more are near-miss or data aging.
/// - `Go`: all criteria met, all required data present and fresh.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "decision")]
pub enum TripDecision {
    Go,
    Caution { warnings: Vec<String> },
    NoGo { reasons: Vec<String> },
    Unknown { missing: Vec<String> },
}

/// A single evaluated criterion — used in the planning view.
///
/// Each `EvalFactor` represents one configured threshold that was checked against
/// live data. The planning view uses these to show a structured breakdown of why
/// the recommendation is GO or NO GO.
#[derive(Debug, Clone, Serialize)]
pub struct EvalFactor {
    /// Short label, e.g. "Temperature", "River level".
    pub name: String,
    /// Formatted actual value, e.g. "14.2 ft".
    pub actual: String,
    /// Formatted threshold, e.g. "≤ 12.0 ft".
    pub threshold: String,
    /// Human-readable verdict, e.g. "14.2 ft — 2.2 ft over limit".
    pub detail: String,
}

/// A criterion that could not be evaluated because the required data source has
/// not yet produced a reading.
#[derive(Debug, Clone, Serialize)]
pub struct UncheckedCriterion {
    /// Criterion label, e.g. "Min temperature".
    pub criterion: String,
    /// Which source is missing, e.g. "Weather".
    pub missing_source: String,
}

/// Full structured evaluation result for the planning view.
///
/// Unlike `TripDecision`, this carries the breakdown of all checked and
/// unchecked criteria so the planning view can render hero + detail separately.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationDetail {
    /// The binary recommendation.
    pub decision: TripDecision,
    /// Criteria that are currently blocking GO.
    pub blockers: Vec<EvalFactor>,
    /// Criteria that passed.
    pub passing: Vec<EvalFactor>,
    /// Criteria that could not be checked due to missing source data.
    pub unchecked: Vec<UncheckedCriterion>,
    /// Data freshness for each source, in seconds since last update.
    /// `None` means the source has never produced a reading.
    pub source_age_secs: SourceAge,
}

/// Per-source data age (seconds since last reading, or None if never received).
#[derive(Debug, Clone, Serialize, Default)]
pub struct SourceAge {
    pub weather_secs: Option<u64>,
    pub river_secs: Option<u64>,
    pub ferry_secs: Option<u64>,
    pub trail_secs: Option<u64>,
    pub road_secs: Option<u64>,
}

/// All possible outputs from a data source (backward-compatibility bridge).
///
/// Used by `DomainState::apply` to accept typed values and store them as JSON.
/// New code should use `DomainState::apply_raw` with `serde_json::Value` directly.
#[derive(Debug, Clone)]
pub enum DataPoint {
    Weather(WeatherObservation),
    River(RiverGauge),
    Ferry(FerryStatus),
    Trail(TrailCondition),
    Road(RoadStatus),
}

/// Stable plugin identifier used as the cache key in `DomainState`.
pub type PluginId = String;

/// A cached source value: the raw JSON data plus metadata.
#[derive(Debug, Clone)]
pub struct CachedValue {
    /// The JSON data returned by the last successful fetch.
    pub data: serde_json::Value,
    /// Unix timestamp (seconds) when this value was last successfully fetched.
    pub fetched_at: u64,
    /// Error message from the last fetch attempt, if it failed (stale data in use).
    pub error: Option<String>,
}

/// Snapshot of the most recent value from every source, keyed by plugin ID.
///
/// Well-known plugin IDs: `"weather"`, `"river"`, `"ferry"`, `"trail"`, `"road"`.
/// New sources use `apply_raw` with their own ID. Typed accessor methods
/// (`weather()`, `river()`, etc.) deserialize the JSON into domain structs.
#[derive(Debug, Clone, Default)]
pub struct DomainState {
    pub cache: HashMap<PluginId, CachedValue>,
}

impl DomainState {
    /// Apply an incoming DataPoint (backward-compatibility bridge).
    ///
    /// Serializes the typed value to JSON and stores it under the standard plugin ID.
    /// Existing callers (tests, acceptance tests) can continue using this method.
    pub fn apply(&mut self, point: DataPoint) {
        let (id, data) = match point {
            DataPoint::Weather(v) => (
                "weather".to_string(),
                serde_json::to_value(v).expect("WeatherObservation is always serializable"),
            ),
            DataPoint::River(v) => (
                "river".to_string(),
                serde_json::to_value(v).expect("RiverGauge is always serializable"),
            ),
            DataPoint::Ferry(v) => (
                "ferry".to_string(),
                serde_json::to_value(v).expect("FerryStatus is always serializable"),
            ),
            DataPoint::Trail(v) => (
                "trail".to_string(),
                serde_json::to_value(v).expect("TrailCondition is always serializable"),
            ),
            DataPoint::Road(v) => (
                "road".to_string(),
                serde_json::to_value(v).expect("RoadStatus is always serializable"),
            ),
        };
        let fetched_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.cache.insert(id, CachedValue { data, fetched_at, error: None });
    }

    /// Apply a raw JSON value from a source fetch, keyed by plugin ID.
    ///
    /// This is the primary path used by the scheduler after sources return
    /// `serde_json::Value` from their `fetch()` call.
    pub fn apply_raw(&mut self, id: &str, data: serde_json::Value) {
        let fetched_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.cache.insert(id.to_string(), CachedValue { data, fetched_at, error: None });
    }

    /// Return the most recent weather observation, if available.
    pub fn weather(&self) -> Option<WeatherObservation> {
        self.cache
            .get("weather")
            .and_then(|cv| serde_json::from_value(cv.data.clone()).ok())
    }

    /// Return the most recent river gauge reading, if available.
    pub fn river(&self) -> Option<RiverGauge> {
        self.cache
            .get("river")
            .and_then(|cv| serde_json::from_value(cv.data.clone()).ok())
    }

    /// Return the most recent ferry status, if available.
    pub fn ferry(&self) -> Option<FerryStatus> {
        self.cache
            .get("ferry")
            .and_then(|cv| serde_json::from_value(cv.data.clone()).ok())
    }

    /// Return the most recent trail condition, if available.
    pub fn trail(&self) -> Option<TrailCondition> {
        self.cache
            .get("trail")
            .and_then(|cv| serde_json::from_value(cv.data.clone()).ok())
    }

    /// Return the most recent road status, if available.
    pub fn road(&self) -> Option<RoadStatus> {
        self.cache
            .get("road")
            .and_then(|cv| serde_json::from_value(cv.data.clone()).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_weather() -> WeatherObservation {
        WeatherObservation {
            temperature_f: 55.0,
            wind_speed_mph: 8.0,
            wind_direction: "NW".to_string(),
            sky_condition: "Clear".to_string(),
            precip_chance_pct: 10.0,
            observation_time: 1000,
        }
    }

    fn sample_river() -> RiverGauge {
        RiverGauge {
            site_id: "12200500".to_string(),
            site_name: "Skagit River".to_string(),
            water_level_ft: 8.5,
            streamflow_cfs: 5000.0,
            timestamp: 2000,
        }
    }

    fn sample_ferry() -> FerryStatus {
        FerryStatus {
            route: "Anacortes / San Juan Islands".to_string(),
            vessel_name: "MV Samish".to_string(),
            estimated_departures: vec![3000, 6000],
        }
    }

    fn sample_trail() -> TrailCondition {
        TrailCondition {
            destination_name: "Cascade Pass".to_string(),
            suitability_summary: "Snow above 5000ft".to_string(),
            last_updated: 4000,
        }
    }

    fn sample_road() -> RoadStatus {
        RoadStatus {
            road_name: "SR-20".to_string(),
            status: "closed".to_string(),
            affected_segment: "Newhalem to Rainy Pass".to_string(),
            timestamp: 5000,
        }
    }

    #[test]
    fn default_domain_state_is_all_none() {
        let state = DomainState::default();
        assert!(state.weather().is_none());
        assert!(state.river().is_none());
        assert!(state.ferry().is_none());
        assert!(state.trail().is_none());
        assert!(state.road().is_none());
    }

    #[test]
    fn apply_weather_sets_weather() {
        let mut state = DomainState::default();
        state.apply(DataPoint::Weather(sample_weather()));
        assert!(state.weather().is_some());
        assert_eq!(state.weather().unwrap().temperature_f, 55.0);
        assert!(state.river().is_none());
    }

    #[test]
    fn apply_river_sets_river() {
        let mut state = DomainState::default();
        state.apply(DataPoint::River(sample_river()));
        assert!(state.river().is_some());
        assert_eq!(state.river().unwrap().site_id, "12200500");
    }

    #[test]
    fn apply_ferry_sets_ferry() {
        let mut state = DomainState::default();
        state.apply(DataPoint::Ferry(sample_ferry()));
        assert!(state.ferry().is_some());
        assert_eq!(state.ferry().unwrap().vessel_name, "MV Samish");
    }

    #[test]
    fn apply_trail_sets_trail() {
        let mut state = DomainState::default();
        state.apply(DataPoint::Trail(sample_trail()));
        assert!(state.trail().is_some());
        assert_eq!(state.trail().unwrap().destination_name, "Cascade Pass");
    }

    #[test]
    fn apply_road_sets_road() {
        let mut state = DomainState::default();
        state.apply(DataPoint::Road(sample_road()));
        assert!(state.road().is_some());
        assert_eq!(state.road().unwrap().status, "closed");
    }

    #[test]
    fn apply_replaces_existing_value() {
        let mut state = DomainState::default();
        state.apply(DataPoint::Weather(sample_weather()));
        assert_eq!(state.weather().unwrap().temperature_f, 55.0);

        let mut updated = sample_weather();
        updated.temperature_f = 72.0;
        state.apply(DataPoint::Weather(updated));
        assert_eq!(state.weather().unwrap().temperature_f, 72.0);
    }

    #[test]
    fn apply_all_types_populates_full_state() {
        let mut state = DomainState::default();
        state.apply(DataPoint::Weather(sample_weather()));
        state.apply(DataPoint::River(sample_river()));
        state.apply(DataPoint::Ferry(sample_ferry()));
        state.apply(DataPoint::Trail(sample_trail()));
        state.apply(DataPoint::Road(sample_road()));

        assert!(state.weather().is_some());
        assert!(state.river().is_some());
        assert!(state.ferry().is_some());
        assert!(state.trail().is_some());
        assert!(state.road().is_some());
    }

    #[test]
    fn apply_raw_stores_and_retrieves_value() {
        let mut state = DomainState::default();
        let obs = sample_weather();
        let value = serde_json::to_value(&obs).unwrap();
        state.apply_raw("weather", value);
        let retrieved = state.weather().unwrap();
        assert_eq!(retrieved.temperature_f, obs.temperature_f);
        assert_eq!(retrieved.wind_direction, obs.wind_direction);
    }

    #[test]
    fn weather_observation_serialization_roundtrip() {
        let obs = sample_weather();
        let json = serde_json::to_string(&obs).unwrap();
        let parsed: WeatherObservation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.temperature_f, obs.temperature_f);
        assert_eq!(parsed.wind_direction, obs.wind_direction);
        assert_eq!(parsed.precip_chance_pct, obs.precip_chance_pct);
    }

    #[test]
    fn river_gauge_serialization_roundtrip() {
        let gauge = sample_river();
        let json = serde_json::to_string(&gauge).unwrap();
        let parsed: RiverGauge = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.site_id, gauge.site_id);
        assert_eq!(parsed.water_level_ft, gauge.water_level_ft);
    }

    #[test]
    fn ferry_status_serialization_roundtrip() {
        let status = sample_ferry();
        let json = serde_json::to_string(&status).unwrap();
        let parsed: FerryStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.vessel_name, status.vessel_name);
        assert_eq!(parsed.estimated_departures.len(), 2);
    }

    #[test]
    fn trip_decision_go_serialization() {
        let decision = TripDecision::Go;
        let json = serde_json::to_string(&decision).unwrap();
        assert!(json.contains("\"decision\":\"Go\""));
    }

    #[test]
    fn trip_decision_caution_serialization() {
        let decision = TripDecision::Caution {
            warnings: vec!["Temp near minimum".to_string()],
        };
        let json = serde_json::to_string(&decision).unwrap();
        assert!(json.contains("\"decision\":\"Caution\""));
        assert!(json.contains("Temp near minimum"));
    }

    #[test]
    fn trip_decision_nogo_serialization() {
        let decision = TripDecision::NoGo {
            reasons: vec!["Too cold".to_string()],
        };
        let json = serde_json::to_string(&decision).unwrap();
        assert!(json.contains("\"decision\":\"NoGo\""));
        assert!(json.contains("Too cold"));
    }

    #[test]
    fn trip_decision_unknown_serialization() {
        let decision = TripDecision::Unknown {
            missing: vec!["No weather data".to_string()],
        };
        let json = serde_json::to_string(&decision).unwrap();
        assert!(json.contains("\"decision\":\"Unknown\""));
        assert!(json.contains("No weather data"));
    }

    #[test]
    fn trip_criteria_default() {
        let criteria = TripCriteria::default();
        assert!(criteria.min_temp_f.is_none());
        assert!(criteria.max_temp_f.is_none());
        assert!(criteria.max_precip_chance_pct.is_none());
        assert!(criteria.max_river_level_ft.is_none());
        assert!(criteria.max_river_flow_cfs.is_none());
        assert!(!criteria.road_open_required);
    }

    #[test]
    fn precip_chance_defaults_to_zero() {
        let json = r#"{
            "temperature_f": 50.0,
            "wind_speed_mph": 5.0,
            "wind_direction": "N",
            "sky_condition": "Clear",
            "observation_time": 0
        }"#;
        let obs: WeatherObservation = serde_json::from_str(json).unwrap();
        assert_eq!(obs.precip_chance_pct, 0.0);
    }
}
