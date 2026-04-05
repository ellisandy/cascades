//! Visual integration tests for the five full-layout Liquid templates.
//!
//! Each test renders a template with representative fixture data via
//! [`TemplateEngine`] and asserts that the expected values appear in the output.
//! The rendered HTML is printed to stderr so it can be inspected with
//! `cargo test -- --nocapture`.

use cascades::template::{NowContext, RenderContext, TemplateEngine, TripDecisionContext};
use std::collections::HashMap;
use std::path::Path;

fn templates_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/templates"))
}

fn now() -> NowContext {
    NowContext::from_unix(1_775_390_400) // 2026-04-05 12:00:00 UTC
}

fn render(template_name: &str, ctx: &RenderContext) -> String {
    let engine = TemplateEngine::new(templates_dir()).expect("templates directory must exist");
    engine
        .render(template_name, ctx)
        .unwrap_or_else(|e| panic!("render '{template_name}' failed: {e}"))
}

// ─── river_full ──────────────────────────────────────────────────────────────

#[test]
fn river_full_renders_level_and_flow() {
    let ctx = RenderContext {
        data: serde_json::json!({
            "site_id": "12200500",
            "site_name": "Skagit River Near Mount Vernon",
            "water_level_ft": 11.87,
            "streamflow_cfs": 8750.0,
            "timestamp": 1_775_390_400_u64,
        }),
        settings: HashMap::from([(
            "site_name".to_owned(),
            serde_json::Value::String("Skagit at Concrete".to_owned()),
        )]),
        trip_decision: None,
        now: now(),
        error: None,
    };

    let html = render("river_full", &ctx);
    eprintln!("=== river_full ===\n{html}\n");

    assert!(html.contains("Skagit at Concrete"), "site name: {html}");
    assert!(html.contains("11.9 ft"), "water level: {html}");
    assert!(html.contains("8,750"), "streamflow with delimiter: {html}");
    assert!(html.contains("cfs"), "cfs unit: {html}");
    assert!(html.contains("Apr"), "date: {html}");
}

#[test]
fn river_full_shows_go_decision() {
    let ctx = RenderContext {
        data: serde_json::json!({
            "site_id": "12200500",
            "site_name": "Skagit River",
            "water_level_ft": 4.5,
            "streamflow_cfs": 1200.0,
            "timestamp": 1_775_390_400_u64,
        }),
        settings: HashMap::new(),
        trip_decision: Some(TripDecisionContext {
            go: true,
            destination: Some("Cascade Pass".to_owned()),
            results: vec![],
        }),
        now: now(),
        error: None,
    };

    let html = render("river_full", &ctx);
    eprintln!("=== river_full (GO) ===\n{html}\n");

    assert!(html.contains("GO"), "GO decision: {html}");
    assert!(!html.contains("NO GO"), "should not show NO GO: {html}");
    assert!(html.contains("Cascade Pass"), "destination: {html}");
}

#[test]
fn river_full_shows_stale_error() {
    let ctx = RenderContext {
        data: serde_json::json!({
            "site_id": "12200500",
            "site_name": "Skagit River",
            "water_level_ft": 8.0,
            "streamflow_cfs": 3000.0,
            "timestamp": 1_775_390_400_u64,
        }),
        settings: HashMap::new(),
        trip_decision: None,
        now: now(),
        error: Some("fetch timeout".to_owned()),
    };

    let html = render("river_full", &ctx);
    eprintln!("=== river_full (stale) ===\n{html}\n");

    assert!(html.contains("stale data"), "stale indicator: {html}");
}

// ─── weather_full ─────────────────────────────────────────────────────────────

#[test]
fn weather_full_renders_temperature_and_conditions() {
    let ctx = RenderContext {
        data: serde_json::json!({
            "temperature_f": 51.98,
            "wind_speed_mph": 9.2,
            "wind_direction": "SSW",
            "sky_condition": "Mostly Cloudy",
            "precip_chance_pct": 20.0,
            "observation_time": 1_775_390_400_u64,
        }),
        settings: HashMap::from([(
            "station_name".to_owned(),
            serde_json::Value::String("Burlington KBVS".to_owned()),
        )]),
        trip_decision: None,
        now: now(),
        error: None,
    };

    let html = render("weather_full", &ctx);
    eprintln!("=== weather_full ===\n{html}\n");

    assert!(html.contains("Burlington KBVS"), "station name: {html}");
    assert!(html.contains("52°F"), "temperature: {html}");
    assert!(html.contains("Mostly Cloudy"), "sky condition: {html}");
    assert!(html.contains("SSW"), "wind direction: {html}");
    assert!(html.contains("9 mph"), "wind speed: {html}");
    assert!(html.contains("20%"), "precip chance: {html}");
}

#[test]
fn weather_full_omits_precip_when_zero() {
    let ctx = RenderContext {
        data: serde_json::json!({
            "temperature_f": 65.0,
            "wind_speed_mph": 5.0,
            "wind_direction": "N",
            "sky_condition": "Clear",
            "precip_chance_pct": 0.0,
            "observation_time": 1_775_390_400_u64,
        }),
        settings: HashMap::new(),
        trip_decision: None,
        now: now(),
        error: None,
    };

    let html = render("weather_full", &ctx);
    eprintln!("=== weather_full (no precip) ===\n{html}\n");

    assert!(html.contains("Clear"), "sky condition: {html}");
    assert!(!html.contains("precip"), "precip should be hidden when zero: {html}");
}

// ─── ferry_full ───────────────────────────────────────────────────────────────

#[test]
fn ferry_full_renders_vessel_and_route() {
    // 10:30, 12:30, 14:30 UTC as Unix timestamps (seconds from midnight)
    let dep1: u64 = 1_775_390_400 - (1_775_390_400 % 86400) + 10 * 3600 + 30 * 60;
    let dep2: u64 = dep1 + 2 * 3600;
    let dep3: u64 = dep2 + 2 * 3600;

    let ctx = RenderContext {
        data: serde_json::json!({
            "route": "Anacortes → Friday Harbor",
            "vessel_name": "MV Samish",
            "estimated_departures": [dep1, dep2, dep3],
        }),
        settings: HashMap::new(),
        trip_decision: None,
        now: now(),
        error: None,
    };

    let html = render("ferry_full", &ctx);
    eprintln!("=== ferry_full ===\n{html}\n");

    assert!(html.contains("MV Samish"), "vessel name: {html}");
    assert!(html.contains("Anacortes"), "route: {html}");
    assert!(html.contains("10:30"), "first departure HH:MM: {html}");
    assert!(html.contains("12:30"), "second departure HH:MM: {html}");
    assert!(html.contains("14:30"), "third departure HH:MM: {html}");
}

#[test]
fn ferry_full_limits_to_three_departures() {
    let base: u64 = 1_775_390_400 - (1_775_390_400 % 86400) + 6 * 3600;
    let deps: Vec<u64> = (0..5).map(|i| base + i * 3600).collect();

    let ctx = RenderContext {
        data: serde_json::json!({
            "route": "Anacortes / San Juan Islands",
            "vessel_name": "MV Chetzemoka",
            "estimated_departures": deps,
        }),
        settings: HashMap::new(),
        trip_decision: None,
        now: now(),
        error: None,
    };

    let html = render("ferry_full", &ctx);
    eprintln!("=== ferry_full (5 deps, capped at 3) ===\n{html}\n");

    // The 4th and 5th departures should not appear
    let fourth_time = format!("{:02}:{:02}", (base + 3 * 3600) % 86400 / 3600, 0);
    let fifth_time = format!("{:02}:{:02}", (base + 4 * 3600) % 86400 / 3600, 0);
    assert!(!html.contains(&fourth_time), "4th departure should be hidden: {html}");
    assert!(!html.contains(&fifth_time), "5th departure should be hidden: {html}");
}

// ─── trail_full ───────────────────────────────────────────────────────────────

#[test]
fn trail_full_renders_name_and_condition() {
    let ctx = RenderContext {
        data: serde_json::json!({
            "destination_name": "Cascade Pass Trail",
            "suitability_summary": "[Caution] Snow above 5000ft through late June",
            "last_updated": 1_775_390_400_u64,
        }),
        settings: HashMap::from([(
            "park_name".to_owned(),
            serde_json::Value::String("North Cascades NP".to_owned()),
        )]),
        trip_decision: None,
        now: now(),
        error: None,
    };

    let html = render("trail_full", &ctx);
    eprintln!("=== trail_full ===\n{html}\n");

    assert!(html.contains("North Cascades NP"), "park name: {html}");
    assert!(html.contains("Cascade Pass Trail"), "destination name: {html}");
    assert!(html.contains("Snow above 5000ft"), "condition text: {html}");
    assert!(html.contains("today"), "last updated: {html}");
}

#[test]
fn trail_full_no_active_alerts() {
    let ctx = RenderContext {
        data: serde_json::json!({
            "destination_name": "NOCA",
            "suitability_summary": "No active alerts",
            "last_updated": 1_775_390_400_u64,
        }),
        settings: HashMap::new(),
        trip_decision: None,
        now: now(),
        error: None,
    };

    let html = render("trail_full", &ctx);
    eprintln!("=== trail_full (no alerts) ===\n{html}\n");

    assert!(html.contains("No active alerts"), "no alerts text: {html}");
}

// ─── road_full ────────────────────────────────────────────────────────────────

#[test]
fn road_full_renders_closure() {
    let ctx = RenderContext {
        data: serde_json::json!({
            "road_name": "SR-20 North Cascades Hwy",
            "status": "Road closed for winter",
            "affected_segment": "MP 134 (Newhalem) to MP 158 (Rainy Pass)",
            "timestamp": 1_775_390_400_u64,
        }),
        settings: HashMap::new(),
        trip_decision: Some(TripDecisionContext {
            go: false,
            destination: Some("Rainy Pass".to_owned()),
            results: vec![],
        }),
        now: now(),
        error: None,
    };

    let html = render("road_full", &ctx);
    eprintln!("=== road_full (closure + NO GO) ===\n{html}\n");

    assert!(html.contains("SR-20 North Cascades Hwy"), "road name: {html}");
    assert!(html.contains("Road closed for winter"), "status: {html}");
    assert!(html.contains("Newhalem"), "segment start: {html}");
    assert!(html.contains("Rainy Pass"), "segment end: {html}");
    assert!(html.contains("NO GO"), "NO GO decision: {html}");
}

#[test]
fn road_full_renders_open_road() {
    let ctx = RenderContext {
        data: serde_json::json!({
            "road_name": "SR-20 North Cascades Hwy",
            "status": "No active closures",
            "affected_segment": "",
            "timestamp": 1_775_390_400_u64,
        }),
        settings: HashMap::new(),
        trip_decision: None,
        now: now(),
        error: None,
    };

    let html = render("road_full", &ctx);
    eprintln!("=== road_full (open) ===\n{html}\n");

    assert!(html.contains("No active closures"), "open status: {html}");
    assert!(!html.contains("NO GO"), "should not show NO GO when open: {html}");
}

// ─── Engine loads all five templates ─────────────────────────────────────────

#[test]
fn engine_loads_all_five_templates() {
    let engine = TemplateEngine::new(templates_dir()).expect("templates directory must exist");
    assert_eq!(engine.template_count(), 5, "expected 5 templates");
    assert!(engine.has_template("river_full"));
    assert!(engine.has_template("weather_full"));
    assert!(engine.has_template("ferry_full"));
    assert!(engine.has_template("trail_full"));
    assert!(engine.has_template("road_full"));
}
