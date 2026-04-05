use crate::config::Destination;
use crate::domain::{CriterionResult, DomainState, TripDecision};
use std::time::{Duration, UNIX_EPOCH};

// ─── Staleness thresholds ────────────────────────────────────────────────────
// Data older than these limits (in seconds) causes a data-missing result for
// any criterion that depends on that source.

const WEATHER_STALE_SECS: u64 = 10_800; // 3 hours
const RIVER_STALE_SECS: u64 = 21_600; // 6 hours
const ROAD_STALE_SECS: u64 = 86_400; // 24 hours

// ─── Near-miss margins ───────────────────────────────────────────────────────
// A criterion within this margin of its threshold returns near_miss = true,
// flagging a CAUTION display state even though the criterion technically passes.

const TEMP_CAUTION_MARGIN_F: f32 = 5.0; // °F
const PRECIP_CAUTION_MARGIN_PCT: f32 = 10.0; // percentage points
const RIVER_LEVEL_CAUTION_RATIO: f32 = 0.10; // 10% of threshold
const RIVER_FLOW_CAUTION_RATIO: f32 = 0.10; // 10% of threshold

fn is_stale(data_ts: u64, now_secs: u64, threshold: u64) -> bool {
    now_secs.saturating_sub(data_ts) > threshold
}

// ─── Criterion trait ─────────────────────────────────────────────────────────

/// A single evaluable condition for a trip destination.
///
/// Implementations are registered by source at startup and evaluated in a
/// plain loop against the serialised [`DomainState`] cache.
pub trait Criterion: Send + Sync {
    /// Evaluate this criterion against `data` (the serialised [`DomainState`]).
    fn evaluate(&self, data: &serde_json::Value) -> CriterionResult;
}

// ─── Internal helper ─────────────────────────────────────────────────────────

fn missing_result(
    key: &str,
    label: &str,
    threshold: serde_json::Value,
    reason: impl Into<String>,
) -> CriterionResult {
    CriterionResult {
        key: key.to_string(),
        label: label.to_string(),
        value: serde_json::Value::Null,
        threshold,
        pass: false,
        reason: reason.into(),
        data_missing: true,
        near_miss: false,
    }
}

// ─── Weather criteria ─────────────────────────────────────────────────────────

struct MinTempCriterion {
    threshold: f32,
    now_secs: u64,
}

impl Criterion for MinTempCriterion {
    fn evaluate(&self, data: &serde_json::Value) -> CriterionResult {
        let w = &data["weather"];
        if w.is_null() {
            return missing_result(
                "temperature_min",
                "Min temperature",
                serde_json::json!(self.threshold),
                "weather (no data)",
            );
        }
        let ts = w["observation_time"].as_u64().unwrap_or(0);
        if is_stale(ts, self.now_secs, WEATHER_STALE_SECS) {
            let age_h = self.now_secs.saturating_sub(ts) / 3600;
            return missing_result(
                "temperature_min",
                "Min temperature",
                serde_json::json!(self.threshold),
                format!("weather data (stale >{}h)", age_h),
            );
        }
        let temp = w["temperature_f"].as_f64().unwrap_or(0.0) as f32;
        let pass = temp >= self.threshold;
        let near_miss = pass && temp < self.threshold + TEMP_CAUTION_MARGIN_F;
        let reason = if !pass {
            format!("Temperature {:.0}°F below minimum {:.0}°F", temp, self.threshold)
        } else if near_miss {
            format!("Temp {:.0}°F — {:.0}° above minimum", temp, temp - self.threshold)
        } else {
            format!("{:.0}°F ✓", temp)
        };
        CriterionResult {
            key: "temperature_min".to_string(),
            label: "Min temperature".to_string(),
            value: serde_json::json!(temp),
            threshold: serde_json::json!(self.threshold),
            pass,
            reason,
            data_missing: false,
            near_miss,
        }
    }
}

struct MaxTempCriterion {
    threshold: f32,
    now_secs: u64,
}

impl Criterion for MaxTempCriterion {
    fn evaluate(&self, data: &serde_json::Value) -> CriterionResult {
        let w = &data["weather"];
        if w.is_null() {
            return missing_result(
                "temperature_max",
                "Max temperature",
                serde_json::json!(self.threshold),
                "weather (no data)",
            );
        }
        let ts = w["observation_time"].as_u64().unwrap_or(0);
        if is_stale(ts, self.now_secs, WEATHER_STALE_SECS) {
            let age_h = self.now_secs.saturating_sub(ts) / 3600;
            return missing_result(
                "temperature_max",
                "Max temperature",
                serde_json::json!(self.threshold),
                format!("weather data (stale >{}h)", age_h),
            );
        }
        let temp = w["temperature_f"].as_f64().unwrap_or(0.0) as f32;
        let pass = temp <= self.threshold;
        let near_miss = pass && temp > self.threshold - TEMP_CAUTION_MARGIN_F;
        let reason = if !pass {
            format!("Temperature {:.0}°F above maximum {:.0}°F", temp, self.threshold)
        } else if near_miss {
            format!("Temp {:.0}°F — {:.0}° below maximum", temp, self.threshold - temp)
        } else {
            format!("{:.0}°F ✓", temp)
        };
        CriterionResult {
            key: "temperature_max".to_string(),
            label: "Max temperature".to_string(),
            value: serde_json::json!(temp),
            threshold: serde_json::json!(self.threshold),
            pass,
            reason,
            data_missing: false,
            near_miss,
        }
    }
}

struct MaxPrecipCriterion {
    threshold: f32,
    now_secs: u64,
}

impl Criterion for MaxPrecipCriterion {
    fn evaluate(&self, data: &serde_json::Value) -> CriterionResult {
        let w = &data["weather"];
        if w.is_null() {
            return missing_result(
                "precip_chance",
                "Precipitation chance",
                serde_json::json!(self.threshold),
                "weather (no data)",
            );
        }
        let ts = w["observation_time"].as_u64().unwrap_or(0);
        if is_stale(ts, self.now_secs, WEATHER_STALE_SECS) {
            let age_h = self.now_secs.saturating_sub(ts) / 3600;
            return missing_result(
                "precip_chance",
                "Precipitation chance",
                serde_json::json!(self.threshold),
                format!("weather data (stale >{}h)", age_h),
            );
        }
        let precip = w["precip_chance_pct"].as_f64().unwrap_or(0.0) as f32;
        let pass = precip <= self.threshold;
        let near_miss = pass && precip > self.threshold - PRECIP_CAUTION_MARGIN_PCT;
        let reason = if !pass {
            format!("Precip chance {:.0}% exceeds limit {:.0}%", precip, self.threshold)
        } else if near_miss {
            format!(
                "Precip chance {:.0}% — {:.0}pp below limit",
                precip,
                self.threshold - precip
            )
        } else {
            format!("{:.0}% ✓", precip)
        };
        CriterionResult {
            key: "precip_chance".to_string(),
            label: "Precipitation chance".to_string(),
            value: serde_json::json!(precip),
            threshold: serde_json::json!(self.threshold),
            pass,
            reason,
            data_missing: false,
            near_miss,
        }
    }
}

// ─── River criteria ───────────────────────────────────────────────────────────

struct MaxRiverLevelCriterion {
    threshold: f32,
    now_secs: u64,
}

impl Criterion for MaxRiverLevelCriterion {
    fn evaluate(&self, data: &serde_json::Value) -> CriterionResult {
        let r = &data["river"];
        if r.is_null() {
            return missing_result(
                "river_level_ft",
                "River level",
                serde_json::json!(self.threshold),
                "river gauge (no data)",
            );
        }
        let ts = r["timestamp"].as_u64().unwrap_or(0);
        if is_stale(ts, self.now_secs, RIVER_STALE_SECS) {
            let age_h = self.now_secs.saturating_sub(ts) / 3600;
            return missing_result(
                "river_level_ft",
                "River level",
                serde_json::json!(self.threshold),
                format!("river gauge (stale >{}h)", age_h),
            );
        }
        let level = r["water_level_ft"].as_f64().unwrap_or(0.0) as f32;
        let pass = level <= self.threshold;
        let near_miss =
            pass && level > self.threshold * (1.0 - RIVER_LEVEL_CAUTION_RATIO);
        let reason = if !pass {
            format!(
                "River level {:.1} ft — {:.1} ft over limit",
                level,
                level - self.threshold
            )
        } else if near_miss {
            format!("River level {:.1}ft — near limit {:.1}ft", level, self.threshold)
        } else {
            format!("{:.1} ft ✓", level)
        };
        CriterionResult {
            key: "river_level_ft".to_string(),
            label: "River level".to_string(),
            value: serde_json::json!(level),
            threshold: serde_json::json!(self.threshold),
            pass,
            reason,
            data_missing: false,
            near_miss,
        }
    }
}

struct MaxRiverFlowCriterion {
    threshold: f32,
    now_secs: u64,
}

impl Criterion for MaxRiverFlowCriterion {
    fn evaluate(&self, data: &serde_json::Value) -> CriterionResult {
        let r = &data["river"];
        if r.is_null() {
            return missing_result(
                "river_flow_cfs",
                "River flow",
                serde_json::json!(self.threshold),
                "river gauge (no data)",
            );
        }
        let ts = r["timestamp"].as_u64().unwrap_or(0);
        if is_stale(ts, self.now_secs, RIVER_STALE_SECS) {
            let age_h = self.now_secs.saturating_sub(ts) / 3600;
            return missing_result(
                "river_flow_cfs",
                "River flow",
                serde_json::json!(self.threshold),
                format!("river gauge (stale >{}h)", age_h),
            );
        }
        let flow = r["streamflow_cfs"].as_f64().unwrap_or(0.0) as f32;
        let pass = flow <= self.threshold;
        let near_miss =
            pass && flow > self.threshold * (1.0 - RIVER_FLOW_CAUTION_RATIO);
        let reason = if !pass {
            format!(
                "River flow {:.0} cfs — {:.0} cfs over limit",
                flow,
                flow - self.threshold
            )
        } else if near_miss {
            format!("River flow {:.0}cfs — near limit {:.0}cfs", flow, self.threshold)
        } else {
            format!("{:.0} cfs ✓", flow)
        };
        CriterionResult {
            key: "river_flow_cfs".to_string(),
            label: "River flow".to_string(),
            value: serde_json::json!(flow),
            threshold: serde_json::json!(self.threshold),
            pass,
            reason,
            data_missing: false,
            near_miss,
        }
    }
}

// ─── Road criterion ───────────────────────────────────────────────────────────

struct RoadOpenCriterion {
    now_secs: u64,
}

impl Criterion for RoadOpenCriterion {
    fn evaluate(&self, data: &serde_json::Value) -> CriterionResult {
        let rd = &data["road"];
        if rd.is_null() {
            return missing_result(
                "road_open",
                "Road access",
                serde_json::json!("open"),
                "road status (no data)",
            );
        }
        let ts = rd["timestamp"].as_u64().unwrap_or(0);
        if is_stale(ts, self.now_secs, ROAD_STALE_SECS) {
            let age_h = self.now_secs.saturating_sub(ts) / 3600;
            return missing_result(
                "road_open",
                "Road access",
                serde_json::json!("open"),
                format!("road status (stale >{}h)", age_h),
            );
        }
        let status = rd["status"].as_str().unwrap_or("unknown");
        let road_name = rd["road_name"].as_str().unwrap_or("Road");
        let segment = rd["affected_segment"].as_str().unwrap_or("");
        let is_open = status == "open" || status == "No active closures";
        let reason = if is_open {
            format!("{} ✓", road_name)
        } else {
            format!("{} is {} — {}", road_name, status, segment)
        };
        CriterionResult {
            key: "road_open".to_string(),
            label: "Road access".to_string(),
            value: serde_json::json!(status),
            threshold: serde_json::json!("open"),
            pass: is_open,
            reason,
            data_missing: false,
            near_miss: false,
        }
    }
}

// ─── Criteria registry ────────────────────────────────────────────────────────

/// Build the list of criteria to evaluate for `destination`.
///
/// Only criteria whose signal is enabled and whose threshold is configured are
/// included.  Order: weather (min_temp, max_temp, precip), river (level, flow),
/// road — matches the legacy per-signal evaluation order.
pub fn build_criteria(destination: &Destination, now_secs: u64) -> Vec<Box<dyn Criterion>> {
    let c = &destination.criteria;
    let s = &destination.signals;
    let mut criteria: Vec<Box<dyn Criterion>> = Vec::new();

    if s.weather {
        if let Some(min) = c.min_temp_f {
            criteria.push(Box::new(MinTempCriterion { threshold: min, now_secs }));
        }
        if let Some(max) = c.max_temp_f {
            criteria.push(Box::new(MaxTempCriterion { threshold: max, now_secs }));
        }
        if let Some(max_precip) = c.max_precip_chance_pct {
            criteria.push(Box::new(MaxPrecipCriterion { threshold: max_precip, now_secs }));
        }
    }

    if s.river {
        if let Some(max_level) = c.max_river_level_ft {
            criteria.push(Box::new(MaxRiverLevelCriterion { threshold: max_level, now_secs }));
        }
        if let Some(max_flow) = c.max_river_flow_cfs {
            criteria.push(Box::new(MaxRiverFlowCriterion { threshold: max_flow, now_secs }));
        }
    }

    if s.road && c.road_open_required {
        criteria.push(Box::new(RoadOpenCriterion { now_secs }));
    }

    criteria
}

// ─── Evaluator ────────────────────────────────────────────────────────────────

/// Evaluate a destination's go/no-go criteria against the current domain state.
///
/// Returns a [`TripDecision`] with `go = true` when all registered criteria
/// pass, `false` when any criterion fails (hard limit exceeded, or required
/// data absent/stale).  The individual `results` carry per-criterion detail.
///
/// `now_secs` is a Unix timestamp (seconds) used to measure data staleness.
/// Pass [`current_unix_secs`] in production; pass a fixed value in tests.
pub fn evaluate(destination: &Destination, state: &DomainState, now_secs: u64) -> TripDecision {
    let criteria = build_criteria(destination, now_secs);
    let cache_map: serde_json::Map<String, serde_json::Value> =
        state.cache.iter().map(|(k, cv)| (k.clone(), cv.data.clone())).collect();
    let cache = serde_json::Value::Object(cache_map);

    let results: Vec<CriterionResult> =
        criteria.iter().map(|c| c.evaluate(&cache)).collect();

    let go = results.iter().all(|r| r.pass);

    TripDecision {
        go,
        destination: destination.name.clone(),
        results,
        evaluated_at: UNIX_EPOCH + Duration::from_secs(now_secs),
    }
}

/// Return the current time as Unix seconds. Use in production call sites.
pub fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Destination;
    use crate::domain::{
        DataPoint, DomainState, RiverGauge, RoadStatus, TripCriteria, WeatherObservation,
    };

    // All test data uses timestamp = 0. We pass now_secs = 0 to evaluate
    // so data appears perfectly fresh. Tests that exercise staleness pass
    // a different now_secs explicitly.
    const NOW: u64 = 0;

    fn default_criteria() -> TripCriteria {
        TripCriteria {
            min_temp_f: None,
            max_temp_f: None,
            max_precip_chance_pct: None,
            max_river_level_ft: None,
            max_river_flow_cfs: None,
            road_open_required: false,
        }
    }

    fn make_dest_with(criteria: TripCriteria) -> Destination {
        Destination {
            name: "Test".to_string(),
            signals: Default::default(),
            criteria,
        }
    }

    fn make_dest_with_signals(criteria: TripCriteria, signals: crate::domain::RelevantSignals) -> Destination {
        Destination {
            name: "Test".to_string(),
            signals,
            criteria,
        }
    }

    fn make_dest(min_temp: Option<f32>, max_temp: Option<f32>) -> Destination {
        make_dest_with(TripCriteria {
            min_temp_f: min_temp,
            max_temp_f: max_temp,
            ..default_criteria()
        })
    }

    fn weather_obs(temp: f32) -> WeatherObservation {
        WeatherObservation {
            temperature_f: temp,
            wind_speed_mph: 5.0,
            wind_direction: "N".to_string(),
            sky_condition: "Clear".to_string(),
            precip_chance_pct: 0.0,
            observation_time: 0,
        }
    }

    fn weather_state(temp: f32) -> DomainState {
        let mut state = DomainState::default();
        state.apply(DataPoint::Weather(weather_obs(temp)));
        state
    }

    fn river_gauge(level: f32, flow: f32) -> RiverGauge {
        RiverGauge {
            site_id: "12200500".to_string(),
            site_name: "Skagit River Near Mount Vernon, WA".to_string(),
            water_level_ft: level,
            streamflow_cfs: flow,
            timestamp: 0,
        }
    }

    fn road_status(status: &str) -> RoadStatus {
        RoadStatus {
            road_name: "SR-20".to_string(),
            status: status.to_string(),
            affected_segment: "Newhalem to Rainy Pass".to_string(),
            timestamp: 0,
        }
    }

    // ── Temperature criteria ──────────────────────────────────────────────────

    #[test]
    fn go_when_all_criteria_met() {
        let dest = make_dest(Some(40.0), Some(90.0));
        let state = weather_state(65.0);
        assert!(evaluate(&dest, &state, NOW).go);
    }

    #[test]
    fn no_go_when_too_cold() {
        let dest = make_dest(Some(50.0), None);
        let state = weather_state(40.0);
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].reason.contains("below minimum"));
    }

    #[test]
    fn no_go_when_too_hot() {
        let dest = make_dest(None, Some(80.0));
        let state = weather_state(85.0);
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].reason.contains("above maximum"));
    }

    #[test]
    fn caution_at_exact_min_temp_boundary() {
        // Exactly at threshold → within near-miss margin → go=true, near_miss=true.
        let dest = make_dest(Some(50.0), None);
        let state = weather_state(50.0);
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        assert!(d.results.iter().any(|r| r.near_miss));
    }

    #[test]
    fn caution_at_exact_max_temp_boundary() {
        // Exactly at threshold → within near-miss margin → go=true, near_miss=true.
        let dest = make_dest(None, Some(80.0));
        let state = weather_state(80.0);
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        assert!(d.results.iter().any(|r| r.near_miss));
    }

    #[test]
    fn caution_when_temp_near_min() {
        // 52°F with min=50°F → within 5°F margin → near_miss
        let dest = make_dest(Some(50.0), None);
        let state = weather_state(52.0);
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        let cautions: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.near_miss).collect();
        assert_eq!(cautions.len(), 1);
        assert!(cautions[0].reason.contains("Temp"));
        assert!(cautions[0].reason.contains("minimum"));
    }

    #[test]
    fn caution_when_temp_near_max() {
        // 78°F with max=80°F → within 5°F margin → near_miss
        let dest = make_dest(None, Some(80.0));
        let state = weather_state(78.0);
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        let cautions: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.near_miss).collect();
        assert_eq!(cautions.len(), 1);
        assert!(cautions[0].reason.contains("Temp"));
        assert!(cautions[0].reason.contains("maximum"));
    }

    // ── Precipitation criteria ────────────────────────────────────────────────

    #[test]
    fn no_go_when_precip_too_high() {
        let dest = make_dest_with(TripCriteria {
            max_precip_chance_pct: Some(30.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        let mut obs = weather_obs(65.0);
        obs.precip_chance_pct = 80.0;
        state.apply(DataPoint::Weather(obs));
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].reason.contains("Precip chance"));
    }

    #[test]
    fn caution_at_exact_precip_boundary() {
        // Exactly at threshold → within near-miss margin → near_miss.
        let dest = make_dest_with(TripCriteria {
            max_precip_chance_pct: Some(50.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        let mut obs = weather_obs(65.0);
        obs.precip_chance_pct = 50.0;
        state.apply(DataPoint::Weather(obs));
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        assert!(d.results.iter().any(|r| r.near_miss));
    }

    #[test]
    fn caution_when_precip_near_limit() {
        // 45% with max=50% → within 10pp margin → near_miss
        let dest = make_dest_with(TripCriteria {
            max_precip_chance_pct: Some(50.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        let mut obs = weather_obs(65.0);
        obs.precip_chance_pct = 45.0;
        state.apply(DataPoint::Weather(obs));
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        let cautions: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.near_miss).collect();
        assert_eq!(cautions.len(), 1);
        assert!(cautions[0].reason.contains("Precip"));
    }

    // ── River level criteria ──────────────────────────────────────────────────

    #[test]
    fn no_go_when_river_too_high() {
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::River(river_gauge(14.5, 5000.0)));
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].reason.contains("River level"));
    }

    #[test]
    fn caution_at_exact_river_level_boundary() {
        // Exactly at threshold → within 10% near-miss margin → near_miss.
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::River(river_gauge(12.0, 5000.0)));
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        assert!(d.results.iter().any(|r| r.near_miss));
    }

    #[test]
    fn caution_when_river_near_level_limit() {
        // 11.0ft with max=12.0ft → within 10% (1.2ft margin) → near_miss
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::River(river_gauge(11.0, 1000.0)));
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        let cautions: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.near_miss).collect();
        assert!(cautions.iter().any(|r| r.reason.contains("River level")));
    }

    // ── River flow criteria ───────────────────────────────────────────────────

    #[test]
    fn no_go_when_river_flow_too_high() {
        let dest = make_dest_with(TripCriteria {
            max_river_flow_cfs: Some(10000.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::River(river_gauge(8.0, 15000.0)));
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].reason.contains("River flow"));
    }

    #[test]
    fn caution_at_exact_river_flow_boundary() {
        // Exactly at threshold → within 10% near-miss margin → near_miss.
        let dest = make_dest_with(TripCriteria {
            max_river_flow_cfs: Some(10000.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::River(river_gauge(8.0, 10000.0)));
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        assert!(d.results.iter().any(|r| r.near_miss));
    }

    #[test]
    fn caution_when_river_flow_near_limit() {
        // 9500cfs with max=10000cfs → within 10% (1000cfs margin) → near_miss
        let dest = make_dest_with(TripCriteria {
            max_river_flow_cfs: Some(10000.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::River(river_gauge(8.0, 9500.0)));
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        let cautions: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.near_miss).collect();
        assert!(cautions.iter().any(|r| r.reason.contains("River flow")));
    }

    // ── Road criteria ─────────────────────────────────────────────────────────

    #[test]
    fn no_go_when_road_closed() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::Road(road_status("closed")));
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].reason.contains("SR-20"));
        assert!(failures[0].reason.contains("closed"));
    }

    #[test]
    fn go_when_road_open() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::Road(road_status("open")));
        assert!(evaluate(&dest, &state, NOW).go);
    }

    #[test]
    fn go_when_road_not_required() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: false,
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::Road(road_status("closed")));
        assert!(evaluate(&dest, &state, NOW).go);
    }

    #[test]
    fn go_when_road_no_active_closures() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::Road(road_status("No active closures")));
        assert!(evaluate(&dest, &state, NOW).go);
    }

    // ── Missing data → go=false, data_missing=true ────────────────────────────

    #[test]
    fn unknown_when_no_weather_data_and_criteria_configured() {
        let dest = make_dest(Some(50.0), None);
        let state = DomainState::default();
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let missing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.data_missing).collect();
        assert!(missing.iter().any(|r| r.reason.contains("weather")));
    }

    #[test]
    fn unknown_when_no_river_data_and_criteria_configured() {
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let state = DomainState::default();
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let missing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.data_missing).collect();
        assert!(missing.iter().any(|r| r.reason.contains("river")));
    }

    #[test]
    fn unknown_when_no_road_data_and_road_required() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let state = DomainState::default();
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let missing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.data_missing).collect();
        assert!(missing.iter().any(|r| r.reason.contains("road")));
    }

    #[test]
    fn go_when_no_criteria_configured_and_no_data() {
        // No configured criteria → nothing to evaluate → go=true.
        let dest = make_dest_with(default_criteria());
        let state = DomainState::default();
        assert!(evaluate(&dest, &state, NOW).go);
    }

    #[test]
    fn go_when_no_criteria_configured_with_data() {
        let dest = make_dest_with(default_criteria());
        let mut state = DomainState::default();
        state.apply(DataPoint::Weather(weather_obs(100.0)));
        state.apply(DataPoint::River(river_gauge(50.0, 100000.0)));
        state.apply(DataPoint::Road(road_status("closed")));
        assert!(evaluate(&dest, &state, NOW).go);
    }

    // ── Stale data → go=false, data_missing=true ──────────────────────────────

    #[test]
    fn unknown_when_weather_stale() {
        let dest = make_dest(Some(50.0), None);
        // Data timestamp = 0, now = WEATHER_STALE_SECS + 1 → stale.
        let state = weather_state(65.0); // passes criteria, but stale
        let now = WEATHER_STALE_SECS + 1;
        let d = evaluate(&dest, &state, now);
        assert!(!d.go);
        let missing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.data_missing).collect();
        assert!(missing.iter().any(|r| r.reason.contains("stale")));
    }

    #[test]
    fn unknown_when_river_stale() {
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::River(river_gauge(8.0, 5000.0))); // passes, but stale
        let now = RIVER_STALE_SECS + 1;
        let d = evaluate(&dest, &state, now);
        assert!(!d.go);
        let missing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.data_missing).collect();
        assert!(missing.iter().any(|r| r.reason.contains("stale")));
    }

    #[test]
    fn unknown_when_road_stale() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::Road(road_status("open"))); // passes, but stale
        let now = ROAD_STALE_SECS + 1;
        let d = evaluate(&dest, &state, now);
        assert!(!d.go);
        let missing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.data_missing).collect();
        assert!(missing.iter().any(|r| r.reason.contains("stale")));
    }

    // ── Hard fail beats data-missing ─────────────────────────────────────────

    #[test]
    fn no_go_beats_unknown_when_road_closed_and_weather_missing() {
        let dest = make_dest_with(TripCriteria {
            min_temp_f: Some(50.0), // weather needed but absent
            road_open_required: true,
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::Road(road_status("closed"))); // confirmed blocker
        // weather absent (not applied)
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        // Hard failure present for road
        let hard_fails: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert!(hard_fails.iter().any(|r| r.reason.contains("SR-20")));
    }

    // ── Multiple blocking reasons ─────────────────────────────────────────────

    #[test]
    fn multiple_reasons_when_several_criteria_fail() {
        let dest = make_dest_with(TripCriteria {
            min_temp_f: Some(50.0),
            max_river_level_ft: Some(12.0),
            road_open_required: true,
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::Weather(weather_obs(30.0)));
        state.apply(DataPoint::River(river_gauge(15.0, 5000.0)));
        state.apply(DataPoint::Road(road_status("closed")));
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert_eq!(failures.len(), 3);
        assert!(failures[0].reason.contains("Temperature"));
        assert!(failures[1].reason.contains("River level"));
        assert!(failures[2].reason.contains("SR-20"));
    }

    #[test]
    fn all_criteria_fail_simultaneously() {
        let mut obs = weather_obs(30.0);
        obs.precip_chance_pct = 90.0;
        let dest = make_dest_with(TripCriteria {
            min_temp_f: Some(50.0),
            max_temp_f: Some(80.0),
            max_precip_chance_pct: Some(20.0),
            max_river_level_ft: Some(10.0),
            max_river_flow_cfs: Some(5000.0),
            road_open_required: true,
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::Weather(obs));
        state.apply(DataPoint::River(river_gauge(15.0, 20000.0)));
        state.apply(DataPoint::Road(road_status("restricted")));
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        // temp below min, precip too high, river level, river flow, road (max_temp passes at 30°F)
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert_eq!(failures.len(), 5);
    }

    // ── Signal relevance ─────────────────────────────────────────────────────

    #[test]
    fn weather_signal_disabled_skips_weather_criteria() {
        use crate::domain::RelevantSignals;
        let dest = make_dest_with_signals(
            TripCriteria {
                min_temp_f: Some(80.0), // would normally block at 40°F
                ..default_criteria()
            },
            RelevantSignals {
                weather: false,
                ..Default::default()
            },
        );
        let state = weather_state(40.0);
        // weather signal is off — temperature criteria are skipped → go
        assert!(evaluate(&dest, &state, NOW).go);
    }

    #[test]
    fn river_signal_disabled_skips_river_criteria() {
        use crate::domain::RelevantSignals;
        let dest = make_dest_with_signals(
            TripCriteria {
                max_river_level_ft: Some(5.0), // would block at 20ft
                ..default_criteria()
            },
            RelevantSignals {
                river: false,
                ..Default::default()
            },
        );
        let mut state = DomainState::default();
        state.apply(DataPoint::River(river_gauge(20.0, 50000.0)));
        // river signal is off — level criteria are skipped → go
        assert!(evaluate(&dest, &state, NOW).go);
    }

    #[test]
    fn road_signal_disabled_skips_road_criteria() {
        use crate::domain::RelevantSignals;
        let dest = make_dest_with_signals(
            TripCriteria {
                road_open_required: true,
                ..default_criteria()
            },
            RelevantSignals {
                road: false,
                ..Default::default()
            },
        );
        let mut state = DomainState::default();
        state.apply(DataPoint::Road(road_status("closed")));
        // road signal is off — road criteria are skipped → go
        assert!(evaluate(&dest, &state, NOW).go);
    }

    // ── Results detail ────────────────────────────────────────────────────────

    #[test]
    fn results_blocker_when_river_too_high() {
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        state.apply(DataPoint::River(river_gauge(14.5, 5000.0)));
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass && !r.data_missing).collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].label, "River level");
        assert!(failures[0].reason.contains("over limit"));
        let passing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.pass).collect();
        assert!(passing.is_empty());
        let missing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.data_missing).collect();
        assert!(missing.is_empty());
    }

    #[test]
    fn results_passing_when_all_criteria_met() {
        let dest = make_dest(Some(40.0), Some(90.0));
        let state = weather_state(65.0);
        let d = evaluate(&dest, &state, NOW);
        assert!(d.go);
        let passing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.pass).collect();
        assert_eq!(passing.len(), 2); // min temp + max temp
        let failures: Vec<&CriterionResult> =
            d.results.iter().filter(|r| !r.pass).collect();
        assert!(failures.is_empty());
    }

    #[test]
    fn results_data_missing_when_weather_absent() {
        let dest = make_dest(Some(50.0), None);
        let state = DomainState::default();
        let d = evaluate(&dest, &state, NOW);
        assert!(!d.go);
        let missing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.data_missing).collect();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].label, "Min temperature");
        assert!(missing[0].reason.contains("no data"));
    }

    #[test]
    fn results_data_missing_when_weather_stale() {
        let dest = make_dest(Some(50.0), None);
        let state = weather_state(65.0);
        let now = WEATHER_STALE_SECS + 1;
        let d = evaluate(&dest, &state, now);
        assert!(!d.go);
        let missing: Vec<&CriterionResult> =
            d.results.iter().filter(|r| r.data_missing).collect();
        assert_eq!(missing.len(), 1);
        assert!(missing[0].reason.contains("stale"));
    }

}
