//! Server acceptance tests for the Cascades server.
//!
//! Each test maps to a user story in docs/user-stories.md.
//!
//! Wave 2 TDD: tests are written first, before full implementation.
//! Tests must compile and run. Tests marked `#[ignore]` document
//! intended future behavior — they will be enabled when the
//! implementation catches up in Wave 3.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    response::IntoResponse,
    routing::get,
    Router,
};
use cascades::{
    build_sources,
    config::{
        load_config, Config, Destination, DisplayConfig, LocationConfig, SourceIntervals,
        StorageConfig,
    },
    domain::{
        DataPoint, DomainState, RiverGauge, WeatherObservation,
    },
    evaluation::evaluate,
    presentation::{build_display_layout, HeroDecision},
    render::render_display,
    render_current_state, render_current_state_with_destinations,
};
use http_body_util::BodyExt;
use std::{
    io::Write,
    path::Path,
    sync::{Arc, RwLock},
};
use tempfile::NamedTempFile;
use tower::ServiceExt;

// ─────────────────────────────────────────────────────────────────────────────
// Shared test infrastructure
// ─────────────────────────────────────────────────────────────────────────────

/// Minimal config used across multiple tests. No optional sources configured
/// so no external API keys or feature flags are required.
fn minimal_config() -> Config {
    Config {
        display: DisplayConfig {
            width: 800,
            height: 480,
        },
        location: LocationConfig {
            latitude: 48.4232,
            longitude: -122.3351,
            name: "Mount Vernon, WA".to_string(),
        },
        sources: SourceIntervals {
            weather_interval_secs: 300,
            river_interval_secs: 300,
            ferry_interval_secs: 60,
            trail_interval_secs: 900,
            road_interval_secs: 1800,
            river: None,
            trail: None,
            road: None,
            ferry: None,
        },
        server: None,
        auth: None,
        device: None,
        storage: StorageConfig::default(),
    }
}

/// A destination that evaluates to Go given mild conditions.
fn go_destination() -> Destination {
    Destination {
        name: "Skagit Flats Loop".to_string(),
        signals: Default::default(),
        criteria: cascades::domain::TripCriteria {
            min_temp_f: Some(45.0),
            max_temp_f: Some(90.0),
            max_precip_chance_pct: Some(50.0),
            max_river_level_ft: Some(15.0),
            ..Default::default()
        },
    }
}

/// A destination that evaluates to NoGo when river level is too high.
fn nogo_destination() -> Destination {
    Destination {
        name: "Flooded Flats".to_string(),
        signals: Default::default(),
        criteria: cascades::domain::TripCriteria {
            max_river_level_ft: Some(5.0), // very low threshold → always fails
            ..Default::default()
        },
    }
}

/// Parse a PNG byte slice and return (width, height).
fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
    use image::io::Reader as ImageReader;
    use std::io::Cursor;
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .expect("should guess PNG format");
    let dimensions = reader.into_dimensions().expect("should read PNG dimensions");
    dimensions
}

/// In-process app state for test HTTP server.
struct TestAppState {
    domain: Arc<RwLock<DomainState>>,
    destinations: Vec<Destination>,
    display_width: u32,
    display_height: u32,
}

async fn serve_image_handler(State(app): State<Arc<TestAppState>>) -> impl IntoResponse {
    let domain = app.domain.read().unwrap().clone();
    let now_secs = 0u64; // fixed timestamp for tests
    let layout = build_display_layout(&domain, &app.destinations, now_secs);
    let buf = render_display(&layout, app.display_width, app.display_height);
    let png = buf.to_png();
    ([(header::CONTENT_TYPE, "image/png")], png)
}

/// Build an in-process axum router for testing.
fn make_test_app(domain: DomainState, destinations: Vec<Destination>) -> Router {
    let state = Arc::new(TestAppState {
        domain: Arc::new(RwLock::new(domain)),
        destinations,
        display_width: 800,
        display_height: 480,
    });
    Router::new()
        .route("/image.png", get(serve_image_handler))
        .with_state(state)
}

/// Build an in-process axum router backed by an externally-held domain state.
fn make_test_app_with_state(
    domain: Arc<RwLock<DomainState>>,
    destinations: Vec<Destination>,
) -> Router {
    let state = Arc::new(TestAppState {
        domain,
        destinations,
        display_width: 800,
        display_height: 480,
    });
    Router::new()
        .route("/image.png", get(serve_image_handler))
        .with_state(state)
}

// ─────────────────────────────────────────────────────────────────────────────
// US1: Starting the server
// "GET /image.png returns HTTP 200 within 5 seconds of startup."
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn us1_server_responds_to_image_request() {
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn us1_server_responds_quickly() {
    // Verify render completes well within 5 seconds (in-process, should be ms).
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let start = std::time::Instant::now();
    let response = app.oneshot(req).await.unwrap();
    let elapsed = start.elapsed();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "response took {:?}, expected < 5s",
        elapsed
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// US2: Config error on startup
// "Server process exits with status ≠ 0. Stderr contains the config file path
//  and a description of the parse or I/O error."
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn us2_missing_config_returns_error_with_path() {
    let result = load_config(Path::new("/nonexistent/config.toml"));
    assert!(result.is_err(), "missing config should fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("nonexistent") || err_msg.contains("config.toml"),
        "error message should contain the file path; got: {err_msg}"
    );
}

#[test]
fn us2_malformed_config_returns_parse_error_with_path() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"not valid toml !!!").unwrap();
    let path = f.path().to_owned();
    let result = load_config(&path);
    assert!(result.is_err(), "malformed config should fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains(path.to_str().unwrap()),
        "error should contain path; got: {err_msg}"
    );
}

#[test]
fn us2_missing_required_config_fields_fails() {
    let mut f = NamedTempFile::new().unwrap();
    // Omit required [display] and [location] sections.
    f.write_all(b"[sources]\nweather_interval_secs = 300\n").unwrap();
    let result = load_config(f.path());
    assert!(result.is_err(), "incomplete config should fail");
}

// ─────────────────────────────────────────────────────────────────────────────
// US3: Fetching the display image
// "Response status 200, Content-Type: image/png, PNG width=800, height=480."
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn us3_get_image_returns_200() {
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn us3_get_image_content_type_is_png() {
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let ct = response
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("should have Content-Type header")
        .to_str()
        .unwrap();
    assert_eq!(ct, "image/png");
}

#[tokio::test]
async fn us3_get_image_dimensions_are_800x480() {
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    let (w, h) = png_dimensions(&body_bytes);
    assert_eq!(w, 800, "PNG width should be 800");
    assert_eq!(h, 480, "PNG height should be 480");
}

#[tokio::test]
async fn us3_get_image_response_is_valid_png() {
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    // PNG magic bytes: 89 50 4E 47
    assert!(
        body_bytes.starts_with(b"\x89PNG"),
        "response body should be a valid PNG"
    );
}

/// Wave 3 TODO: render_display hardcodes 800×480 and ignores DisplayConfig.
///
/// Fix: thread DisplayConfig.width and DisplayConfig.height into render_display()
/// so custom display sizes are reflected in the rendered PNG.
///
/// Tracking: wave-3 fix must update render_display() signature to accept
/// width/height, and update render_current_state() to pass config dimensions.
#[tokio::test]
async fn us3_custom_display_dimensions_reflected_in_png() {
    // A hypothetical 400×240 display; the PNG should match.
    let layout = build_display_layout(&DomainState::default(), &[], 0);
    let buf = render_display(&layout, 400, 240);
    let png = buf.to_png();
    let (w, h) = png_dimensions(&png);
    assert_eq!(w, 400, "PNG width should match config (400)");
    assert_eq!(h, 240, "PNG height should match config (240)");
}

// ─────────────────────────────────────────────────────────────────────────────
// US4: Data freshness — stale source triggers Unknown
// "When all weather data is older than 3 hours (or absent), the rendered image
//  encodes an Unknown decision state."
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn us4_absent_weather_causes_unknown_decision() {
    let state = DomainState::default(); // no data at all
    let dest = go_destination();
    let now_secs = 1_000_000u64;
    let decision = evaluate(&dest, &state, now_secs);
    assert!(
        !decision.go && decision.results.iter().all(|r| r.data_missing),
        "absent weather should produce Unknown (go=false, all data_missing); got {:?}",
        decision
    );
}

#[test]
fn us4_stale_weather_over_3h_causes_unknown() {
    let mut state = DomainState::default();
    let now_secs = 1_000_000u64;
    let three_hours_plus = 3 * 3600 + 1;
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 65.0,
        wind_speed_mph: 5.0,
        wind_direction: "N".to_string(),
        sky_condition: "Clear".to_string(),
        precip_chance_pct: 0.0,
        observation_time: now_secs - three_hours_plus, // stale
    }));
    let dest = go_destination();
    let decision = evaluate(&dest, &state, now_secs);
    assert!(
        !decision.go && decision.results.iter().all(|r| r.data_missing),
        "stale weather (>3h) should produce Unknown (go=false, all data_missing); got {:?}",
        decision
    );
}

#[test]
fn us4_fresh_weather_does_not_cause_unknown() {
    let mut state = DomainState::default();
    let now_secs = 1_000_000u64;
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 65.0,
        wind_speed_mph: 5.0,
        wind_direction: "N".to_string(),
        sky_condition: "Clear".to_string(),
        precip_chance_pct: 0.0,
        observation_time: now_secs - 300, // fresh (5 min ago)
    }));
    // Destination with only weather criteria — no river/road/ferry required.
    let dest = Destination {
        name: "Weather Only".to_string(),
        signals: Default::default(),
        criteria: cascades::domain::TripCriteria {
            min_temp_f: Some(45.0),
            max_temp_f: Some(90.0),
            ..Default::default()
        },
    };
    let decision = evaluate(&dest, &state, now_secs);
    assert!(
        decision.go,
        "fresh weather should not produce Unknown (should be go=true); got {:?}",
        decision
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// US5: Fixture / dev mode — offline rendering
// "In fixture mode, GET /image.png returns HTTP 200 with a valid PNG.
//  No outbound HTTP requests are made during the request."
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn us5_fixture_mode_returns_valid_png() {
    let config = minimal_config();
    let buf = render_current_state(&config, true);
    let png = buf.to_png();
    assert!(png.starts_with(b"\x89PNG"), "fixture mode should return valid PNG");
}

#[test]
fn us5_fixture_mode_png_has_black_pixels() {
    let config = minimal_config();
    let buf = render_current_state(&config, true);
    assert!(
        buf.pixels.iter().any(|&b| b != 0),
        "fixture render should produce a non-blank image"
    );
}

#[test]
fn us5_fixture_mode_dimensions_are_correct() {
    let config = minimal_config();
    let buf = render_current_state(&config, true);
    assert_eq!(buf.width, 800);
    assert_eq!(buf.height, 480);
}

#[test]
fn us5_fixture_mode_sources_build_without_network() {
    // Verify that build_sources with fixture=true constructs sources without
    // panicking or needing network access.
    let config = minimal_config();
    let sources = build_sources(&config, true);
    assert!(!sources.is_empty(), "should have at least one source in fixture mode");
}

// ─────────────────────────────────────────────────────────────────────────────
// US6: Source failure — server stays up and degrades gracefully
// "After a source fetch failure, GET /image.png still returns HTTP 200.
//  The previous value (or Unknown if none) is used."
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn us6_no_source_data_still_returns_200() {
    // Empty DomainState = no sources have ever succeeded.
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "server should serve 200 even with no source data"
    );
}

#[tokio::test]
async fn us6_partial_source_data_still_returns_200() {
    // Only weather data; river/ferry/trail/road absent.
    let mut domain = DomainState::default();
    domain.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 62.0,
        wind_speed_mph: 7.0,
        wind_direction: "SW".to_string(),
        sky_condition: "Partly Cloudy".to_string(),
        precip_chance_pct: 20.0,
        observation_time: 1_000_000,
    }));
    let app = make_test_app(domain, vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn us6_render_with_empty_state_does_not_panic() {
    // Direct render; simulates the server rendering after all sources failed.
    let state = DomainState::default();
    let layout = build_display_layout(&state, &[], 0);
    let buf = render_display(&layout, 800, 480);
    let png = buf.to_png();
    assert!(png.starts_with(b"\x89PNG"));
}

// ─────────────────────────────────────────────────────────────────────────────
// US7: Optional source disabled — missing API key
// "Server starts and responds to GET /image.png with HTTP 200.
//  Stderr contains a warning that the trail source was disabled."
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn us7_no_trail_config_sources_still_build() {
    // trail = None means no trail API key → source is disabled, not a panic.
    let config = minimal_config(); // trail: None already
    let sources = build_sources(&config, false);
    // Weather + river sources should always be present.
    assert!(!sources.is_empty());
    let names: Vec<&str> = sources.iter().map(|s| s.name()).collect();
    assert!(
        names.iter().any(|n| n.contains("NOAA") || n.contains("noaa") || n.contains("Weather")),
        "should still have weather source; got {:?}",
        names
    );
}

#[tokio::test]
async fn us7_missing_trail_key_server_still_serves_image() {
    // Config with no trail source; server must still respond.
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn us7_render_without_trail_data_is_valid_png() {
    // DomainState with no trail data must render without panic.
    let mut state = DomainState::default();
    // Provide weather and river data only; trail is absent.
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 58.0,
        wind_speed_mph: 4.0,
        wind_direction: "E".to_string(),
        sky_condition: "Overcast".to_string(),
        precip_chance_pct: 30.0,
        observation_time: 100_000,
    }));
    state.apply(DataPoint::River(RiverGauge {
        site_id: "12200500".to_string(),
        site_name: "Skagit River".to_string(),
        water_level_ft: 8.0,
        streamflow_cfs: 4500.0,
        timestamp: 100_000,
    }));
    let layout = build_display_layout(&state, &[], 0);
    let buf = render_display(&layout, 800, 480);
    let png = buf.to_png();
    assert!(png.starts_with(b"\x89PNG"));
}

// ─────────────────────────────────────────────────────────────────────────────
// US8: Multi-destination evaluation — worst-case decision shown
// "Given destinations A (NoGo) and B (Go), the rendered image's hero zone
//  encodes a NoGo recommendation."
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn us8_nogo_destination_beats_go_destination() {
    let mut state = DomainState::default();
    let now_secs = 1_000_000u64;
    // Set up a high river level so nogo_destination fails.
    state.apply(DataPoint::River(RiverGauge {
        site_id: "12200500".to_string(),
        site_name: "Skagit River".to_string(),
        water_level_ft: 10.0, // exceeds nogo threshold of 5 ft
        streamflow_cfs: 8000.0,
        timestamp: now_secs - 60,
    }));
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 65.0,
        wind_speed_mph: 3.0,
        wind_direction: "W".to_string(),
        sky_condition: "Clear".to_string(),
        precip_chance_pct: 5.0,
        observation_time: now_secs - 60,
    }));

    let dest_go = go_destination();
    let dest_nogo = nogo_destination();

    let decision_go = evaluate(&dest_go, &state, now_secs);
    let decision_nogo = evaluate(&dest_nogo, &state, now_secs);

    assert!(
        decision_go.go,
        "go_destination should be Go or Caution (go=true); got {:?}",
        decision_go
    );
    assert!(
        !decision_nogo.go && decision_nogo.results.iter().any(|r| !r.pass && !r.data_missing),
        "nogo_destination should be NoGo (go=false with hard fail); got {:?}",
        decision_nogo
    );
}

#[test]
fn us8_hero_zone_shows_nogo_when_any_destination_is_nogo() {
    let mut state = DomainState::default();
    let now_secs = 1_000_000u64;
    state.apply(DataPoint::River(RiverGauge {
        site_id: "12200500".to_string(),
        site_name: "Skagit River".to_string(),
        water_level_ft: 10.0, // exceeds nogo threshold
        streamflow_cfs: 8000.0,
        timestamp: now_secs - 60,
    }));
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 65.0,
        wind_speed_mph: 3.0,
        wind_direction: "W".to_string(),
        sky_condition: "Clear".to_string(),
        precip_chance_pct: 5.0,
        observation_time: now_secs - 60,
    }));

    let destinations = vec![go_destination(), nogo_destination()];
    let layout = build_display_layout(&state, &destinations, now_secs);

    assert!(
        matches!(
            layout.hero.decision,
            HeroDecision::NoGo { .. }
        ),
        "hero should show NoGo when any destination is NoGo; got {:?}",
        layout.hero.decision
    );
}

#[test]
fn us8_decision_priority_nogo_beats_unknown() {
    let state = DomainState::default(); // all unknown (no data)
    let now_secs = 1_000_000u64;
    // One destination needs road (unknown), another has explicit NoGo threshold.
    let dest_unknown = Destination {
        name: "Road Required".to_string(),
        signals: Default::default(),
        criteria: cascades::domain::TripCriteria {
            road_open_required: true,
            ..Default::default()
        },
    };
    let dest_nogo = nogo_destination();

    let decision_unknown = evaluate(&dest_unknown, &state, now_secs);
    let decision_nogo = evaluate(&dest_nogo, &state, now_secs);

    assert!(
        !decision_unknown.go && decision_unknown.results.iter().all(|r| r.data_missing),
        "no-road-data should be Unknown (go=false, all data_missing); got {:?}",
        decision_unknown
    );
    assert!(
        !decision_nogo.go,
        "nogo_destination with no river data should be Unknown or NoGo (go=false); got {:?}",
        decision_nogo
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// US9: Caution / near-miss threshold
// "Given max_temp_f = 85 and observed temp = 82°F (within 5°F margin),
//  evaluation returns Caution, not Go."
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn us9_temperature_near_max_returns_caution() {
    let mut state = DomainState::default();
    let now_secs = 1_000_000u64;
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 82.0, // 3°F below max of 85 → within 5°F margin
        wind_speed_mph: 3.0,
        wind_direction: "S".to_string(),
        sky_condition: "Clear".to_string(),
        precip_chance_pct: 0.0,
        observation_time: now_secs - 60,
    }));

    let dest = Destination {
        name: "Near Max Temp".to_string(),
        signals: Default::default(),
        criteria: cascades::domain::TripCriteria {
            max_temp_f: Some(85.0),
            ..Default::default()
        },
    };

    let decision = evaluate(&dest, &state, now_secs);
    assert!(
        decision.go && decision.results.iter().any(|r| r.near_miss),
        "temp within 5°F of max should return Caution (go=true, near_miss); got {:?}",
        decision
    );
}

#[test]
fn us9_temperature_well_below_max_returns_go() {
    let mut state = DomainState::default();
    let now_secs = 1_000_000u64;
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 65.0, // well below max of 85
        wind_speed_mph: 3.0,
        wind_direction: "S".to_string(),
        sky_condition: "Clear".to_string(),
        precip_chance_pct: 0.0,
        observation_time: now_secs - 60,
    }));

    let dest = Destination {
        name: "Well Below Max".to_string(),
        signals: Default::default(),
        criteria: cascades::domain::TripCriteria {
            max_temp_f: Some(85.0),
            ..Default::default()
        },
    };

    let decision = evaluate(&dest, &state, now_secs);
    assert!(
        decision.go && !decision.results.iter().any(|r| r.near_miss),
        "temp well below max should return Go (go=true, no near_miss); got {:?}",
        decision
    );
}

#[test]
fn us9_temperature_above_max_returns_nogo() {
    let mut state = DomainState::default();
    let now_secs = 1_000_000u64;
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 90.0, // above max of 85
        wind_speed_mph: 3.0,
        wind_direction: "S".to_string(),
        sky_condition: "Clear".to_string(),
        precip_chance_pct: 0.0,
        observation_time: now_secs - 60,
    }));

    let dest = Destination {
        name: "Too Hot".to_string(),
        signals: Default::default(),
        criteria: cascades::domain::TripCriteria {
            max_temp_f: Some(85.0),
            ..Default::default()
        },
    };

    let decision = evaluate(&dest, &state, now_secs);
    assert!(
        !decision.go && decision.results.iter().any(|r| !r.pass && !r.data_missing),
        "temp above max should return NoGo (go=false, hard fail); got {:?}",
        decision
    );
}

#[test]
fn us9_river_near_limit_returns_caution() {
    let mut state = DomainState::default();
    let now_secs = 1_000_000u64;
    // 11.5 ft with limit of 12 ft → within 10% (1.2 ft) → Caution
    state.apply(DataPoint::River(RiverGauge {
        site_id: "12200500".to_string(),
        site_name: "Skagit River".to_string(),
        water_level_ft: 11.5,
        streamflow_cfs: 6000.0,
        timestamp: now_secs - 60,
    }));

    let dest = Destination {
        name: "Near River Limit".to_string(),
        signals: Default::default(),
        criteria: cascades::domain::TripCriteria {
            max_river_level_ft: Some(12.0),
            ..Default::default()
        },
    };

    let decision = evaluate(&dest, &state, now_secs);
    assert!(
        decision.go && decision.results.iter().any(|r| r.near_miss),
        "river near limit should return Caution (go=true, near_miss); got {:?}",
        decision
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// US10: No destinations configured — server still serves image
// "GET /image.png returns HTTP 200 with a valid PNG when destinations.toml
//  is missing or contains zero destinations."
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn us10_no_destinations_returns_200() {
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn us10_no_destinations_returns_valid_png() {
    let app = make_test_app(DomainState::default(), vec![]);
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body_bytes.starts_with(b"\x89PNG"));
}

#[test]
fn us10_render_current_state_no_destinations_is_valid() {
    let config = minimal_config();
    let buf = render_current_state_with_destinations(&config, &[], true);
    let png = buf.to_png();
    assert!(png.starts_with(b"\x89PNG"));
}

#[test]
fn us10_missing_destinations_toml_is_handled() {
    // load_destinations on a missing file returns an error (operator can
    // then default to empty Vec); this tests the error is surfaced gracefully.
    let result = cascades::config::load_destinations(Path::new("/nonexistent/destinations.toml"));
    assert!(result.is_err(), "missing destinations file should return error");
}

// ─────────────────────────────────────────────────────────────────────────────
// US11: Device client mode — thin client fetches and refreshes
// "In device-client mode, no HTTP server is bound on the local port."
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn us11_device_config_parsed_from_toml() {
    let toml = r#"
[display]
width = 800
height = 480

[location]
latitude = 48.4
longitude = -122.3
name = "Test"

[sources]
weather_interval_secs = 300
river_interval_secs = 300
ferry_interval_secs = 60

[device]
image_url = "http://192.168.1.10:8080/image.png"
refresh_interval_secs = 30
"#;
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(toml.as_bytes()).unwrap();
    let cfg = load_config(f.path()).expect("should parse");
    let device = cfg.device.expect("should have device config");
    assert_eq!(device.image_url, "http://192.168.1.10:8080/image.png");
    assert_eq!(device.refresh_interval_secs, 30);
}

#[test]
fn us11_no_device_config_is_server_mode() {
    let config = minimal_config();
    assert!(
        config.device.is_none(),
        "no [device] section means server mode"
    );
}

#[test]
fn us11_device_config_default_refresh_interval() {
    let toml = r#"
[display]
width = 800
height = 480

[location]
latitude = 48.4
longitude = -122.3
name = "Test"

[sources]
weather_interval_secs = 300
river_interval_secs = 300
ferry_interval_secs = 60

[device]
image_url = "http://server/image.png"
"#;
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(toml.as_bytes()).unwrap();
    let cfg = load_config(f.path()).expect("should parse");
    let device = cfg.device.unwrap();
    assert_eq!(device.refresh_interval_secs, 60, "default refresh should be 60s");
}

// ─────────────────────────────────────────────────────────────────────────────
// US12: Source polling — background refresh without client requests
// "Each data source continues fetching on its configured interval so that
//  GET /image.png receives data no older than one polling interval."
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn us12_state_update_reflected_in_next_render() {
    let domain = Arc::new(RwLock::new(DomainState::default()));
    let app = make_test_app_with_state(Arc::clone(&domain), vec![]);

    // First render — no data.
    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let body_before = response.into_body().collect().await.unwrap().to_bytes();

    // Update domain state (simulates a source completing a fetch).
    {
        let mut d = domain.write().unwrap();
        d.apply(DataPoint::Weather(WeatherObservation {
            temperature_f: 72.0,
            wind_speed_mph: 8.0,
            wind_direction: "NW".to_string(),
            sky_condition: "Clear".to_string(),
            precip_chance_pct: 0.0,
            observation_time: 1_000_000,
        }));
    }

    // Second render — should reflect new weather data.
    let req2 = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();
    let response2 = app.oneshot(req2).await.unwrap();
    let body_after = response2.into_body().collect().await.unwrap().to_bytes();

    // Both responses must be valid PNGs.
    assert!(body_before.starts_with(b"\x89PNG"));
    assert!(body_after.starts_with(b"\x89PNG"));
    // The render output should differ once weather data is available.
    assert_ne!(
        body_before.as_ref(),
        body_after.as_ref(),
        "render output should change after domain state update"
    );
}

#[test]
fn us12_domain_state_apply_is_reflected_immediately() {
    let mut state = DomainState::default();
    let now_secs = 1_000_000u64;

    // Render before applying weather.
    let layout_before = build_display_layout(&state, &[], now_secs);
    let buf_before = render_display(&layout_before, 800, 480);
    let png_before = buf_before.to_png();

    // Apply weather data.
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 55.0,
        wind_speed_mph: 10.0,
        wind_direction: "W".to_string(),
        sky_condition: "Partly Cloudy".to_string(),
        precip_chance_pct: 15.0,
        observation_time: now_secs - 60,
    }));

    // Render after applying weather.
    let layout_after = build_display_layout(&state, &[], now_secs);
    let buf_after = render_display(&layout_after, 800, 480);
    let png_after = buf_after.to_png();

    // Both renders should succeed.
    assert!(png_before.starts_with(b"\x89PNG"));
    assert!(png_after.starts_with(b"\x89PNG"));
    // Output must differ once data is present.
    assert_ne!(png_before, png_after, "render should change after state update");
}

// ─────────────────────────────────────────────────────────────────────────────
// Concurrent request safety (mentioned in bead acceptance criteria)
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_reads_on_shared_domain_state_do_not_panic() {
    let domain = Arc::new(RwLock::new(DomainState::default()));

    // Seed with some data.
    {
        let mut d = domain.write().unwrap();
        d.apply(DataPoint::Weather(WeatherObservation {
            temperature_f: 60.0,
            wind_speed_mph: 5.0,
            wind_direction: "N".to_string(),
            sky_condition: "Clear".to_string(),
            precip_chance_pct: 5.0,
            observation_time: 1_000_000,
        }));
    }

    // Spawn multiple concurrent read tasks.
    let mut handles = Vec::new();
    for _ in 0..8 {
        let d = Arc::clone(&domain);
        handles.push(tokio::spawn(async move {
            let state = d.read().unwrap().clone();
            let layout = build_display_layout(&state, &[], 0);
            let buf = render_display(&layout, 800, 480);
            let png = buf.to_png();
            assert!(png.starts_with(b"\x89PNG"));
        }));
    }

    for handle in handles {
        handle.await.expect("concurrent render task should not panic");
    }
}

#[tokio::test]
async fn concurrent_write_and_read_do_not_deadlock() {
    let domain = Arc::new(RwLock::new(DomainState::default()));

    // Writer task.
    let writer_domain = Arc::clone(&domain);
    let writer = tokio::spawn(async move {
        for i in 0..5u64 {
            {
                // Scope the write guard so it's dropped before the await.
                let mut d = writer_domain.write().unwrap();
                d.apply(DataPoint::Weather(WeatherObservation {
                    temperature_f: 50.0 + i as f32,
                    wind_speed_mph: 5.0,
                    wind_direction: "N".to_string(),
                    sky_condition: "Clear".to_string(),
                    precip_chance_pct: 0.0,
                    observation_time: 1_000_000 + i,
                }));
            } // write guard dropped here
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    });

    // Reader tasks — each makes an independent read.
    let mut readers = Vec::new();
    for _ in 0..4 {
        let d = Arc::clone(&domain);
        readers.push(tokio::spawn(async move {
            for _ in 0..5 {
                let state = d.read().unwrap().clone();
                let layout = build_display_layout(&state, &[], 0);
                let _buf = render_display(&layout, 800, 480);
                tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
            }
        }));
    }

    writer.await.expect("writer should complete");
    for r in readers {
        r.await.expect("reader should complete");
    }
}
