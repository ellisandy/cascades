use crate::config::Destination;
use crate::domain::{DomainState, EvalFactor, EvaluationDetail, SourceAge, TripDecision, UncheckedCriterion};

// ─── Staleness thresholds ────────────────────────────────────────────────────
// Data older than these limits (in seconds) causes an UNKNOWN result for any
// criterion that depends on that source. See trip-recommendation-model.md.

const WEATHER_STALE_SECS: u64 = 10_800; // 3 hours
const RIVER_STALE_SECS: u64 = 21_600; // 6 hours
const ROAD_STALE_SECS: u64 = 86_400; // 24 hours
// Reserved for when trail criteria are added to TripCriteria.
#[allow(dead_code)]
const TRAIL_STALE_SECS: u64 = 172_800; // 48 hours

// ─── Near-miss margins ───────────────────────────────────────────────────────
// A criterion within this margin of its threshold triggers CAUTION rather
// than GO, even though it technically passes. See trip-recommendation-model.md.

const TEMP_CAUTION_MARGIN_F: f32 = 5.0; // °F
const PRECIP_CAUTION_MARGIN_PCT: f32 = 10.0; // percentage points
const RIVER_LEVEL_CAUTION_RATIO: f32 = 0.10; // 10% of threshold
const RIVER_FLOW_CAUTION_RATIO: f32 = 0.10; // 10% of threshold

fn is_stale(data_ts: u64, now_secs: u64, threshold: u64) -> bool {
    now_secs.saturating_sub(data_ts) > threshold
}

/// Evaluate a destination's go/no-go criteria against the current domain state.
///
/// Returns one of four states (priority order):
/// 1. `NoGo`    — a hard criterion is exceeded.
/// 2. `Unknown` — no blocker but required data is absent or stale.
/// 3. `Caution` — all criteria pass but a near-miss or aging data was found.
/// 4. `Go`      — all criteria met, all required data fresh.
///
/// `now_secs` is a Unix timestamp (seconds) used to measure data staleness.
/// Pass `current_unix_secs()` in production; pass a fixed value in tests.
pub fn evaluate(destination: &Destination, state: &DomainState, now_secs: u64) -> TripDecision {
    let criteria = &destination.criteria;
    let signals = &destination.signals;
    let mut reasons: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // ── Weather ──────────────────────────────────────────────────────────────
    let needs_weather = signals.weather
        && (criteria.min_temp_f.is_some()
            || criteria.max_temp_f.is_some()
            || criteria.max_precip_chance_pct.is_some());

    if needs_weather {
        match &state.weather {
            None => missing.push("No weather data".to_string()),
            Some(w) if is_stale(w.observation_time, now_secs, WEATHER_STALE_SECS) => {
                let age_h = now_secs.saturating_sub(w.observation_time) / 3600;
                missing.push(format!("Weather data stale (>{}h)", age_h));
            }
            Some(w) => {
                if let Some(min) = criteria.min_temp_f {
                    if w.temperature_f < min {
                        reasons.push(format!(
                            "Temperature {:.0}°F below minimum {:.0}°F",
                            w.temperature_f, min
                        ));
                    } else if w.temperature_f < min + TEMP_CAUTION_MARGIN_F {
                        warnings.push(format!(
                            "Temp {:.0}°F — {:.0}° above minimum",
                            w.temperature_f,
                            w.temperature_f - min
                        ));
                    }
                }
                if let Some(max) = criteria.max_temp_f {
                    if w.temperature_f > max {
                        reasons.push(format!(
                            "Temperature {:.0}°F above maximum {:.0}°F",
                            w.temperature_f, max
                        ));
                    } else if w.temperature_f > max - TEMP_CAUTION_MARGIN_F {
                        warnings.push(format!(
                            "Temp {:.0}°F — {:.0}° below maximum",
                            w.temperature_f,
                            max - w.temperature_f
                        ));
                    }
                }
                if let Some(max_precip) = criteria.max_precip_chance_pct {
                    if w.precip_chance_pct > max_precip {
                        reasons.push(format!(
                            "Precip chance {:.0}% exceeds limit {:.0}%",
                            w.precip_chance_pct, max_precip
                        ));
                    } else if w.precip_chance_pct > max_precip - PRECIP_CAUTION_MARGIN_PCT {
                        warnings.push(format!(
                            "Precip chance {:.0}% — {:.0}pp below limit",
                            w.precip_chance_pct,
                            max_precip - w.precip_chance_pct
                        ));
                    }
                }
            }
        }
    }

    // ── River ────────────────────────────────────────────────────────────────
    let needs_river = signals.river
        && (criteria.max_river_level_ft.is_some() || criteria.max_river_flow_cfs.is_some());

    if needs_river {
        match &state.river {
            None => missing.push("No river data".to_string()),
            Some(r) if is_stale(r.timestamp, now_secs, RIVER_STALE_SECS) => {
                let age_h = now_secs.saturating_sub(r.timestamp) / 3600;
                missing.push(format!("River data stale (>{}h)", age_h));
            }
            Some(r) => {
                if let Some(max_level) = criteria.max_river_level_ft {
                    if r.water_level_ft > max_level {
                        reasons.push(format!(
                            "River level {:.1}ft above limit {:.1}ft",
                            r.water_level_ft, max_level
                        ));
                    } else if r.water_level_ft > max_level * (1.0 - RIVER_LEVEL_CAUTION_RATIO) {
                        warnings.push(format!(
                            "River level {:.1}ft — near limit {:.1}ft",
                            r.water_level_ft, max_level
                        ));
                    }
                }
                if let Some(max_flow) = criteria.max_river_flow_cfs {
                    if r.streamflow_cfs > max_flow {
                        reasons.push(format!(
                            "River flow {:.0}cfs exceeds limit {:.0}cfs",
                            r.streamflow_cfs, max_flow
                        ));
                    } else if r.streamflow_cfs > max_flow * (1.0 - RIVER_FLOW_CAUTION_RATIO) {
                        warnings.push(format!(
                            "River flow {:.0}cfs — near limit {:.0}cfs",
                            r.streamflow_cfs, max_flow
                        ));
                    }
                }
            }
        }
    }

    // ── Road ─────────────────────────────────────────────────────────────────
    if signals.road && criteria.road_open_required {
        match &state.road {
            None => missing.push("No road data".to_string()),
            Some(rd) if is_stale(rd.timestamp, now_secs, ROAD_STALE_SECS) => {
                let age_h = now_secs.saturating_sub(rd.timestamp) / 3600;
                missing.push(format!("Road data stale (>{}h)", age_h));
            }
            Some(rd) => {
                if rd.status != "open" && rd.status != "No active closures" {
                    reasons.push(format!("{} is {}", rd.road_name, rd.status));
                }
            }
        }
    }

    // ── Apply priority rules ─────────────────────────────────────────────────
    // 1. NO GO beats everything.
    if !reasons.is_empty() {
        return TripDecision::NoGo { reasons };
    }
    // 2. UNKNOWN when required data is absent or stale (and no confirmed blocker).
    if !missing.is_empty() {
        return TripDecision::Unknown { missing };
    }
    // 3. CAUTION for near-miss conditions.
    if !warnings.is_empty() {
        return TripDecision::Caution { warnings };
    }
    // 4. GO.
    TripDecision::Go
}

/// Full structured evaluation result for the planning view.
///
/// Where `evaluate` returns a single `TripDecision`, `evaluate_detail` returns
/// the complete breakdown: which criteria blocked, which passed, and which
/// could not be checked due to absent or stale data. The planning view uses
/// this to render a hero recommendation alongside supporting detail.
///
/// `now_secs` is a Unix timestamp (seconds) — pass `current_unix_secs()` in
/// production, or a fixed value in tests.
pub fn evaluate_detail(destination: &Destination, state: &DomainState, now_secs: u64) -> EvaluationDetail {
    let criteria = &destination.criteria;
    let signals = &destination.signals;
    let mut blockers: Vec<EvalFactor> = Vec::new();
    let mut passing: Vec<EvalFactor> = Vec::new();
    let mut unchecked: Vec<UncheckedCriterion> = Vec::new();

    // ── Weather ──────────────────────────────────────────────────────────────
    let needs_weather = signals.weather
        && (criteria.min_temp_f.is_some()
            || criteria.max_temp_f.is_some()
            || criteria.max_precip_chance_pct.is_some());

    if needs_weather {
        let weather_missing_source = match &state.weather {
            None => Some("Weather (no data)".to_string()),
            Some(w) if is_stale(w.observation_time, now_secs, WEATHER_STALE_SECS) => {
                let age_h = now_secs.saturating_sub(w.observation_time) / 3600;
                Some(format!("Weather (stale >{}h)", age_h))
            }
            _ => None,
        };

        if let Some(src) = weather_missing_source {
            if criteria.min_temp_f.is_some() {
                unchecked.push(UncheckedCriterion { criterion: "Min temperature".to_string(), missing_source: src.clone() });
            }
            if criteria.max_temp_f.is_some() {
                unchecked.push(UncheckedCriterion { criterion: "Max temperature".to_string(), missing_source: src.clone() });
            }
            if criteria.max_precip_chance_pct.is_some() {
                unchecked.push(UncheckedCriterion { criterion: "Precipitation chance".to_string(), missing_source: src });
            }
        } else if let Some(w) = &state.weather {
            if let Some(min) = criteria.min_temp_f {
                let factor = EvalFactor {
                    name: "Temperature".to_string(),
                    actual: format!("{:.0}°F", w.temperature_f),
                    threshold: format!("≥ {:.0}°F", min),
                    detail: if w.temperature_f < min {
                        format!("{:.0}°F — {:.0}° below minimum", w.temperature_f, min - w.temperature_f)
                    } else {
                        format!("{:.0}°F ✓", w.temperature_f)
                    },
                };
                if w.temperature_f < min { blockers.push(factor); } else { passing.push(factor); }
            }
            if let Some(max) = criteria.max_temp_f {
                let factor = EvalFactor {
                    name: "Max temperature".to_string(),
                    actual: format!("{:.0}°F", w.temperature_f),
                    threshold: format!("≤ {:.0}°F", max),
                    detail: if w.temperature_f > max {
                        format!("{:.0}°F — {:.0}° above maximum", w.temperature_f, w.temperature_f - max)
                    } else {
                        format!("{:.0}°F ✓", w.temperature_f)
                    },
                };
                if w.temperature_f > max { blockers.push(factor); } else { passing.push(factor); }
            }
            if let Some(max_precip) = criteria.max_precip_chance_pct {
                let factor = EvalFactor {
                    name: "Precipitation chance".to_string(),
                    actual: format!("{:.0}%", w.precip_chance_pct),
                    threshold: format!("≤ {:.0}%", max_precip),
                    detail: if w.precip_chance_pct > max_precip {
                        format!("{:.0}% — {:.0}pp over limit", w.precip_chance_pct, w.precip_chance_pct - max_precip)
                    } else {
                        format!("{:.0}% ✓", w.precip_chance_pct)
                    },
                };
                if w.precip_chance_pct > max_precip { blockers.push(factor); } else { passing.push(factor); }
            }
        }
    }

    // ── River ────────────────────────────────────────────────────────────────
    let needs_river = signals.river
        && (criteria.max_river_level_ft.is_some() || criteria.max_river_flow_cfs.is_some());

    if needs_river {
        let river_missing_source = match &state.river {
            None => Some("River gauge (no data)".to_string()),
            Some(r) if is_stale(r.timestamp, now_secs, RIVER_STALE_SECS) => {
                let age_h = now_secs.saturating_sub(r.timestamp) / 3600;
                Some(format!("River gauge (stale >{}h)", age_h))
            }
            _ => None,
        };

        if let Some(src) = river_missing_source {
            if criteria.max_river_level_ft.is_some() {
                unchecked.push(UncheckedCriterion { criterion: "River level".to_string(), missing_source: src.clone() });
            }
            if criteria.max_river_flow_cfs.is_some() {
                unchecked.push(UncheckedCriterion { criterion: "River flow".to_string(), missing_source: src });
            }
        } else if let Some(r) = &state.river {
            if let Some(max_level) = criteria.max_river_level_ft {
                let factor = EvalFactor {
                    name: "River level".to_string(),
                    actual: format!("{:.1} ft", r.water_level_ft),
                    threshold: format!("≤ {:.1} ft", max_level),
                    detail: if r.water_level_ft > max_level {
                        format!("{:.1} ft — {:.1} ft over limit", r.water_level_ft, r.water_level_ft - max_level)
                    } else {
                        format!("{:.1} ft ✓", r.water_level_ft)
                    },
                };
                if r.water_level_ft > max_level { blockers.push(factor); } else { passing.push(factor); }
            }
            if let Some(max_flow) = criteria.max_river_flow_cfs {
                let factor = EvalFactor {
                    name: "River flow".to_string(),
                    actual: format!("{:.0} cfs", r.streamflow_cfs),
                    threshold: format!("≤ {:.0} cfs", max_flow),
                    detail: if r.streamflow_cfs > max_flow {
                        format!("{:.0} cfs — {:.0} cfs over limit", r.streamflow_cfs, r.streamflow_cfs - max_flow)
                    } else {
                        format!("{:.0} cfs ✓", r.streamflow_cfs)
                    },
                };
                if r.streamflow_cfs > max_flow { blockers.push(factor); } else { passing.push(factor); }
            }
        }
    }

    // ── Road ─────────────────────────────────────────────────────────────────
    if signals.road && criteria.road_open_required {
        let road_missing_source = match &state.road {
            None => Some("Road status (no data)".to_string()),
            Some(rd) if is_stale(rd.timestamp, now_secs, ROAD_STALE_SECS) => {
                let age_h = now_secs.saturating_sub(rd.timestamp) / 3600;
                Some(format!("Road status (stale >{}h)", age_h))
            }
            _ => None,
        };

        if let Some(src) = road_missing_source {
            unchecked.push(UncheckedCriterion { criterion: "Road access".to_string(), missing_source: src });
        } else if let Some(rd) = &state.road {
            let is_open = rd.status == "open" || rd.status == "No active closures";
            let factor = EvalFactor {
                name: "Road access".to_string(),
                actual: rd.status.to_uppercase(),
                threshold: "OPEN".to_string(),
                detail: if is_open {
                    format!("{} ✓", rd.road_name)
                } else {
                    format!("{} is {} — {}", rd.road_name, rd.status, rd.affected_segment)
                },
            };
            if is_open { passing.push(factor); } else { blockers.push(factor); }
        }
    }

    // ── Source age ───────────────────────────────────────────────────────────
    let age_secs = |ts: u64| -> Option<u64> {
        if ts == 0 { None } else { Some(now_secs.saturating_sub(ts)) }
    };
    let source_age_secs = SourceAge {
        weather_secs: state.weather.as_ref().and_then(|w| age_secs(w.observation_time)),
        river_secs: state.river.as_ref().and_then(|r| age_secs(r.timestamp)),
        ferry_secs: state.ferry.as_ref().and_then(|f| {
            f.estimated_departures.first().and_then(|&t| age_secs(t))
        }),
        trail_secs: state.trail.as_ref().and_then(|t| age_secs(t.last_updated)),
        road_secs: state.road.as_ref().and_then(|rd| age_secs(rd.timestamp)),
    };

    let decision = evaluate(destination, state, now_secs);
    EvaluationDetail { decision, blockers, passing, unchecked, source_age_secs }
}

/// Return the current time as Unix seconds. Use in production call sites.
pub fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Destination;
    use crate::domain::{
        DomainState, RiverGauge, RoadStatus, TripCriteria, WeatherObservation,
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
        DomainState {
            weather: Some(weather_obs(temp)),
            ..Default::default()
        }
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
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Go));
    }

    #[test]
    fn no_go_when_too_cold() {
        let dest = make_dest(Some(50.0), None);
        let state = weather_state(40.0);
        match evaluate(&dest, &state, NOW) {
            TripDecision::NoGo { reasons } => {
                assert_eq!(reasons.len(), 1);
                assert!(reasons[0].contains("below minimum"));
            }
            _ => panic!("expected NoGo"),
        }
    }

    #[test]
    fn no_go_when_too_hot() {
        let dest = make_dest(None, Some(80.0));
        let state = weather_state(85.0);
        match evaluate(&dest, &state, NOW) {
            TripDecision::NoGo { reasons } => {
                assert_eq!(reasons.len(), 1);
                assert!(reasons[0].contains("above maximum"));
            }
            _ => panic!("expected NoGo"),
        }
    }

    #[test]
    fn caution_at_exact_min_temp_boundary() {
        // Exactly at threshold → within near-miss margin → CAUTION.
        let dest = make_dest(Some(50.0), None);
        let state = weather_state(50.0);
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Caution { .. }));
    }

    #[test]
    fn caution_at_exact_max_temp_boundary() {
        // Exactly at threshold → within near-miss margin → CAUTION.
        let dest = make_dest(None, Some(80.0));
        let state = weather_state(80.0);
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Caution { .. }));
    }

    #[test]
    fn caution_when_temp_near_min() {
        // 52°F with min=50°F → within 5°F margin → CAUTION
        let dest = make_dest(Some(50.0), None);
        let state = weather_state(52.0);
        match evaluate(&dest, &state, NOW) {
            TripDecision::Caution { warnings } => {
                assert_eq!(warnings.len(), 1);
                assert!(warnings[0].contains("Temp"));
                assert!(warnings[0].contains("minimum"));
            }
            _ => panic!("expected Caution"),
        }
    }

    #[test]
    fn caution_when_temp_near_max() {
        // 78°F with max=80°F → within 5°F margin → CAUTION
        let dest = make_dest(None, Some(80.0));
        let state = weather_state(78.0);
        match evaluate(&dest, &state, NOW) {
            TripDecision::Caution { warnings } => {
                assert_eq!(warnings.len(), 1);
                assert!(warnings[0].contains("Temp"));
                assert!(warnings[0].contains("maximum"));
            }
            _ => panic!("expected Caution"),
        }
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
        state.weather = Some(obs);
        match evaluate(&dest, &state, NOW) {
            TripDecision::NoGo { reasons } => {
                assert_eq!(reasons.len(), 1);
                assert!(reasons[0].contains("Precip chance"));
            }
            _ => panic!("expected NoGo"),
        }
    }

    #[test]
    fn caution_at_exact_precip_boundary() {
        // Exactly at threshold → within near-miss margin → CAUTION.
        let dest = make_dest_with(TripCriteria {
            max_precip_chance_pct: Some(50.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        let mut obs = weather_obs(65.0);
        obs.precip_chance_pct = 50.0;
        state.weather = Some(obs);
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Caution { .. }));
    }

    #[test]
    fn caution_when_precip_near_limit() {
        // 45% with max=50% → within 10pp margin → CAUTION
        let dest = make_dest_with(TripCriteria {
            max_precip_chance_pct: Some(50.0),
            ..default_criteria()
        });
        let mut state = DomainState::default();
        let mut obs = weather_obs(65.0);
        obs.precip_chance_pct = 45.0;
        state.weather = Some(obs);
        match evaluate(&dest, &state, NOW) {
            TripDecision::Caution { warnings } => {
                assert_eq!(warnings.len(), 1);
                assert!(warnings[0].contains("Precip"));
            }
            _ => panic!("expected Caution"),
        }
    }

    // ── River level criteria ──────────────────────────────────────────────────

    #[test]
    fn no_go_when_river_too_high() {
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let state = DomainState {
            river: Some(river_gauge(14.5, 5000.0)),
            ..Default::default()
        };
        match evaluate(&dest, &state, NOW) {
            TripDecision::NoGo { reasons } => {
                assert_eq!(reasons.len(), 1);
                assert!(reasons[0].contains("River level"));
            }
            _ => panic!("expected NoGo"),
        }
    }

    #[test]
    fn caution_at_exact_river_level_boundary() {
        // Exactly at threshold → within 10% near-miss margin → CAUTION.
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let state = DomainState {
            river: Some(river_gauge(12.0, 5000.0)),
            ..Default::default()
        };
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Caution { .. }));
    }

    #[test]
    fn caution_when_river_near_level_limit() {
        // 11.0ft with max=12.0ft → within 10% (1.2ft margin) → CAUTION
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let state = DomainState {
            river: Some(river_gauge(11.0, 1000.0)),
            ..Default::default()
        };
        match evaluate(&dest, &state, NOW) {
            TripDecision::Caution { warnings } => {
                assert!(warnings[0].contains("River level"));
            }
            _ => panic!("expected Caution"),
        }
    }

    // ── River flow criteria ───────────────────────────────────────────────────

    #[test]
    fn no_go_when_river_flow_too_high() {
        let dest = make_dest_with(TripCriteria {
            max_river_flow_cfs: Some(10000.0),
            ..default_criteria()
        });
        let state = DomainState {
            river: Some(river_gauge(8.0, 15000.0)),
            ..Default::default()
        };
        match evaluate(&dest, &state, NOW) {
            TripDecision::NoGo { reasons } => {
                assert_eq!(reasons.len(), 1);
                assert!(reasons[0].contains("River flow"));
            }
            _ => panic!("expected NoGo"),
        }
    }

    #[test]
    fn caution_at_exact_river_flow_boundary() {
        // Exactly at threshold → within 10% near-miss margin → CAUTION.
        let dest = make_dest_with(TripCriteria {
            max_river_flow_cfs: Some(10000.0),
            ..default_criteria()
        });
        let state = DomainState {
            river: Some(river_gauge(8.0, 10000.0)),
            ..Default::default()
        };
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Caution { .. }));
    }

    #[test]
    fn caution_when_river_flow_near_limit() {
        // 9500cfs with max=10000cfs → within 10% (1000cfs margin) → CAUTION
        let dest = make_dest_with(TripCriteria {
            max_river_flow_cfs: Some(10000.0),
            ..default_criteria()
        });
        let state = DomainState {
            river: Some(river_gauge(8.0, 9500.0)),
            ..Default::default()
        };
        match evaluate(&dest, &state, NOW) {
            TripDecision::Caution { warnings } => {
                assert!(warnings[0].contains("River flow"));
            }
            _ => panic!("expected Caution"),
        }
    }

    // ── Road criteria ─────────────────────────────────────────────────────────

    #[test]
    fn no_go_when_road_closed() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let state = DomainState {
            road: Some(road_status("closed")),
            ..Default::default()
        };
        match evaluate(&dest, &state, NOW) {
            TripDecision::NoGo { reasons } => {
                assert_eq!(reasons.len(), 1);
                assert!(reasons[0].contains("SR-20"));
                assert!(reasons[0].contains("closed"));
            }
            _ => panic!("expected NoGo"),
        }
    }

    #[test]
    fn go_when_road_open() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let state = DomainState {
            road: Some(road_status("open")),
            ..Default::default()
        };
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Go));
    }

    #[test]
    fn go_when_road_not_required() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: false,
            ..default_criteria()
        });
        let state = DomainState {
            road: Some(road_status("closed")),
            ..Default::default()
        };
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Go));
    }

    #[test]
    fn go_when_road_no_active_closures() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let state = DomainState {
            road: Some(road_status("No active closures")),
            ..Default::default()
        };
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Go));
    }

    // ── Missing data → UNKNOWN ────────────────────────────────────────────────

    #[test]
    fn unknown_when_no_weather_data_and_criteria_configured() {
        let dest = make_dest(Some(50.0), None);
        let state = DomainState::default();
        match evaluate(&dest, &state, NOW) {
            TripDecision::Unknown { missing } => {
                assert!(missing.iter().any(|m| m.contains("weather")));
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn unknown_when_no_river_data_and_criteria_configured() {
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let state = DomainState::default();
        match evaluate(&dest, &state, NOW) {
            TripDecision::Unknown { missing } => {
                assert!(missing.iter().any(|m| m.contains("river")));
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn unknown_when_no_road_data_and_road_required() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let state = DomainState::default();
        match evaluate(&dest, &state, NOW) {
            TripDecision::Unknown { missing } => {
                assert!(missing.iter().any(|m| m.contains("road")));
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn go_when_no_criteria_configured_and_no_data() {
        // No configured criteria → nothing to evaluate → GO regardless of data.
        let dest = make_dest_with(default_criteria());
        let state = DomainState::default();
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Go));
    }

    #[test]
    fn go_when_no_criteria_configured_with_data() {
        let dest = make_dest_with(default_criteria());
        let state = DomainState {
            weather: Some(weather_obs(100.0)),
            river: Some(river_gauge(50.0, 100000.0)),
            road: Some(road_status("closed")),
            ..Default::default()
        };
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Go));
    }

    // ── Stale data → UNKNOWN ──────────────────────────────────────────────────

    #[test]
    fn unknown_when_weather_stale() {
        let dest = make_dest(Some(50.0), None);
        // Data timestamp = 0, now = WEATHER_STALE_SECS + 1 → stale.
        let state = DomainState {
            weather: Some(weather_obs(65.0)), // passes criteria, but stale
            ..Default::default()
        };
        let now = WEATHER_STALE_SECS + 1;
        match evaluate(&dest, &state, now) {
            TripDecision::Unknown { missing } => {
                assert!(missing.iter().any(|m| m.contains("stale")));
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn unknown_when_river_stale() {
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let state = DomainState {
            river: Some(river_gauge(8.0, 5000.0)), // passes, but stale
            ..Default::default()
        };
        let now = RIVER_STALE_SECS + 1;
        match evaluate(&dest, &state, now) {
            TripDecision::Unknown { missing } => {
                assert!(missing.iter().any(|m| m.contains("stale")));
            }
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn unknown_when_road_stale() {
        let dest = make_dest_with(TripCriteria {
            road_open_required: true,
            ..default_criteria()
        });
        let state = DomainState {
            road: Some(road_status("open")), // passes, but stale
            ..Default::default()
        };
        let now = ROAD_STALE_SECS + 1;
        match evaluate(&dest, &state, now) {
            TripDecision::Unknown { missing } => {
                assert!(missing.iter().any(|m| m.contains("stale")));
            }
            _ => panic!("expected Unknown"),
        }
    }

    // ── Priority: NO GO beats UNKNOWN ─────────────────────────────────────────

    #[test]
    fn no_go_beats_unknown_when_road_closed_and_weather_missing() {
        let dest = make_dest_with(TripCriteria {
            min_temp_f: Some(50.0), // weather needed but absent
            road_open_required: true,
            ..default_criteria()
        });
        let state = DomainState {
            road: Some(road_status("closed")), // confirmed blocker
            weather: None,                     // missing
            ..Default::default()
        };
        match evaluate(&dest, &state, NOW) {
            TripDecision::NoGo { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("SR-20")));
            }
            _ => panic!("expected NoGo"),
        }
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
        let state = DomainState {
            weather: Some(weather_obs(30.0)),
            river: Some(river_gauge(15.0, 5000.0)),
            road: Some(road_status("closed")),
            ..Default::default()
        };
        match evaluate(&dest, &state, NOW) {
            TripDecision::NoGo { reasons } => {
                assert_eq!(reasons.len(), 3);
                assert!(reasons[0].contains("Temperature"));
                assert!(reasons[1].contains("River level"));
                assert!(reasons[2].contains("SR-20"));
            }
            _ => panic!("expected NoGo"),
        }
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
        let state = DomainState {
            weather: Some(obs),
            river: Some(river_gauge(15.0, 20000.0)),
            road: Some(road_status("restricted")),
            ..Default::default()
        };
        match evaluate(&dest, &state, NOW) {
            TripDecision::NoGo { reasons } => {
                // temp below min, precip too high, river level, river flow, road
                assert_eq!(reasons.len(), 5);
            }
            _ => panic!("expected NoGo"),
        }
    }

    // --- Signal relevance ---

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
        // weather signal is off — temperature criteria are skipped → Go
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Go));
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
        let state = DomainState {
            river: Some(river_gauge(20.0, 50000.0)),
            ..Default::default()
        };
        // river signal is off — level criteria are skipped → Go
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Go));
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
        let state = DomainState {
            road: Some(road_status("closed")),
            ..Default::default()
        };
        // road signal is off — road criteria are skipped → Go
        assert!(matches!(evaluate(&dest, &state, NOW), TripDecision::Go));
    }

    // ── evaluate_detail ───────────────────────────────────────────────────────

    #[test]
    fn detail_blocker_when_river_too_high() {
        let dest = make_dest_with(TripCriteria {
            max_river_level_ft: Some(12.0),
            ..default_criteria()
        });
        let state = DomainState {
            river: Some(river_gauge(14.5, 5000.0)),
            ..Default::default()
        };
        let detail = evaluate_detail(&dest, &state, NOW);
        assert_eq!(detail.blockers.len(), 1);
        assert_eq!(detail.blockers[0].name, "River level");
        assert!(detail.blockers[0].detail.contains("over limit"));
        assert!(detail.passing.is_empty());
        assert!(detail.unchecked.is_empty());
        assert!(matches!(detail.decision, TripDecision::NoGo { .. }));
    }

    #[test]
    fn detail_passing_when_all_criteria_met() {
        let dest = make_dest(Some(40.0), Some(90.0));
        let state = weather_state(65.0);
        let detail = evaluate_detail(&dest, &state, NOW);
        assert!(detail.blockers.is_empty());
        assert_eq!(detail.passing.len(), 2); // min temp + max temp
        assert!(detail.unchecked.is_empty());
        assert!(matches!(detail.decision, TripDecision::Go));
    }

    #[test]
    fn detail_unchecked_when_weather_absent() {
        let dest = make_dest(Some(50.0), None);
        let state = DomainState::default();
        let detail = evaluate_detail(&dest, &state, NOW);
        assert!(detail.blockers.is_empty());
        assert!(detail.passing.is_empty());
        assert_eq!(detail.unchecked.len(), 1);
        assert_eq!(detail.unchecked[0].criterion, "Min temperature");
        assert!(detail.unchecked[0].missing_source.contains("no data"));
        assert!(matches!(detail.decision, TripDecision::Unknown { .. }));
    }

    #[test]
    fn detail_unchecked_when_weather_stale() {
        let dest = make_dest(Some(50.0), None);
        let state = DomainState {
            weather: Some(weather_obs(65.0)),
            ..Default::default()
        };
        let now = WEATHER_STALE_SECS + 1;
        let detail = evaluate_detail(&dest, &state, now);
        assert!(detail.blockers.is_empty());
        assert!(detail.passing.is_empty());
        assert_eq!(detail.unchecked.len(), 1);
        assert!(detail.unchecked[0].missing_source.contains("stale"));
        assert!(matches!(detail.decision, TripDecision::Unknown { .. }));
    }

    #[test]
    fn detail_source_age_populated() {
        let dest = make_dest(Some(40.0), None);
        // timestamp = 100, now = 200 → age = 100
        let mut obs = weather_obs(65.0);
        obs.observation_time = 100;
        let state = DomainState { weather: Some(obs), ..Default::default() };
        let detail = evaluate_detail(&dest, &state, 200);
        assert_eq!(detail.source_age_secs.weather_secs, Some(100));
    }

    fn make_dest_with_signals(
        criteria: TripCriteria,
        signals: crate::domain::RelevantSignals,
    ) -> Destination {
        Destination {
            name: "Test".to_string(),
            signals,
            criteria,
        }
    }
}
