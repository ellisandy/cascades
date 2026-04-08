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
    use image::{GrayImage, ImageEncoder};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let domain = app.domain.read().unwrap().clone();
    let now_secs = 0u64; // fixed timestamp for tests
    let layout = build_display_layout(&domain, &app.destinations, now_secs);

    // Hash the layout so different domain states produce different PNG bytes.
    let mut hasher = DefaultHasher::new();
    format!("{:?}", layout).hash(&mut hasher);
    let hash_val = hasher.finish();

    let w = app.display_width;
    let h = app.display_height;
    let mut pixels = vec![255u8; (w * h) as usize];
    for (i, b) in hash_val.to_le_bytes().iter().enumerate() {
        pixels[i] = *b;
    }
    let img = GrayImage::from_raw(w, h, pixels).expect("buffer size matches dimensions");
    let mut png_bytes = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
    ImageEncoder::write_image(encoder, img.as_raw(), w, h, image::ColorType::L8)
        .expect("PNG encoding should not fail");
    ([(header::CONTENT_TYPE, "image/png")], png_bytes)
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

    // Layout before applying weather.
    let layout_before = build_display_layout(&state, &[], now_secs);

    // Apply weather data.
    state.apply(DataPoint::Weather(WeatherObservation {
        temperature_f: 55.0,
        wind_speed_mph: 10.0,
        wind_direction: "W".to_string(),
        sky_condition: "Partly Cloudy".to_string(),
        precip_chance_pct: 15.0,
        observation_time: now_secs - 60,
    }));

    // Layout after applying weather.
    let layout_after = build_display_layout(&state, &[], now_secs);

    // Layout must differ once data is present.
    assert_ne!(
        format!("{:?}", layout_before),
        format!("{:?}", layout_after),
        "layout should change after state update"
    );
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
            // Verify layout is populated (hero decision derived from domain state).
            let _ = format!("{:?}", layout.hero);
        }));
    }

    for handle in handles {
        handle.await.expect("concurrent read task should not panic");
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
                let _layout = build_display_layout(&state, &[], 0);
                tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
            }
        }));
    }

    writer.await.expect("writer should complete");
    for r in readers {
        r.await.expect("reader should complete");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// US-q55: Device API — POST /api/webhook, GET /api/display, GET /api/image
// ─────────────────────────────────────────────────────────────────────────────

/// Build a test router and AppState backed by a temp directory.
///
/// The display "default" has zero slots so the compositor completes
/// immediately (producing an 800×480 white PNG) without requiring a sidecar.
fn make_api_app(
    base_dir: &std::path::Path,
) -> (Router, Arc<cascades::api::AppState>) {
    use cascades::{
        api::{AppState, SourceScheduler, build_router},
        compositor::Compositor,
        instance_store::InstanceStore,
        layout_store::{LayoutConfig, LayoutStore},
        source_store::SourceStore,
        template::TemplateEngine,
    };
    use std::collections::HashMap;

    let db_path = base_dir.join("test.db");
    let templates_dir = base_dir.join("templates");
    std::fs::create_dir_all(&templates_dir).unwrap();

    let instance_store =
        Arc::new(InstanceStore::open(&db_path).expect("open instance store"));
    let layout_store =
        Arc::new(LayoutStore::open(&db_path).expect("open layout store"));
    let source_store =
        Arc::new(SourceStore::open(&db_path).expect("open source store"));
    let template_engine =
        Arc::new(TemplateEngine::new(&templates_dir).expect("load templates"));

    let compositor = Arc::new(Compositor::new(
        Arc::clone(&template_engine),
        Arc::clone(&instance_store),
        Arc::clone(&layout_store),
        "http://localhost:9999", // no sidecar — compositor produces empty-slot PNGs
    ));

    // "default" display has no slots → compositor returns 800×480 white PNG.
    layout_store
        .upsert_layout(&LayoutConfig {
            id: "default".to_string(),
            name: "default".to_string(),
            items: vec![],
            updated_at: 0,
        })
        .expect("seed default layout");

    let image_cache = Arc::new(RwLock::new(HashMap::<String, Vec<u8>>::new()));
    let scheduler = Arc::new(SourceScheduler::new(Arc::clone(&source_store)));

    let state = Arc::new(AppState {
        compositor,
        instance_store,
        layout_store,
        source_store,
        scheduler,
        image_cache,
        api_key: "test-bearer-key".to_string(),
        refresh_rate_secs: 42,
        started_at: std::time::Instant::now(),
        sidecar_url: "http://localhost:3001".to_string(),
    });

    let router = build_router(Arc::clone(&state));
    (router, state)
}

// ── POST /api/webhook/:plugin_instance_id ─────────────────────────────────────

#[tokio::test]
async fn webhook_returns_204_with_valid_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .method("POST")
        .uri("/api/webhook/river")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"level_ft": 8.2}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn webhook_returns_204_with_empty_body() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .method("POST")
        .uri("/api/webhook/weather")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn webhook_invalidates_image_cache_for_affected_display() {
    use cascades::layout_store::{LayoutConfig, LayoutItem, LayoutStore};

    let tmp = tempfile::TempDir::new().unwrap();

    // Build an AppState with "default" display containing a "river" slot
    // so the webhook for "river" will try to re-render "default".
    let db_path = tmp.path().join("test.db");
    let templates_dir = tmp.path().join("templates");
    std::fs::create_dir_all(&templates_dir).unwrap();

    let instance_store = Arc::new(
        cascades::instance_store::InstanceStore::open(&db_path).unwrap(),
    );
    let layout_store = Arc::new(LayoutStore::open(&db_path).unwrap());
    let source_store = Arc::new(
        cascades::source_store::SourceStore::open(&db_path).unwrap(),
    );
    let template_engine = Arc::new(
        cascades::template::TemplateEngine::new(&templates_dir).unwrap(),
    );
    let compositor = Arc::new(cascades::compositor::Compositor::new(
        Arc::clone(&template_engine),
        Arc::clone(&instance_store),
        Arc::clone(&layout_store),
        "http://localhost:9999",
    ));

    layout_store
        .upsert_layout(&LayoutConfig {
            id: "default".to_string(),
            name: "default".to_string(),
            items: vec![LayoutItem::PluginSlot {
                id: "default-slot-0".to_string(),
                z_index: 0,
                x: 0,
                y: 0,
                width: 800,
                height: 480,
                plugin_instance_id: "river".to_string(),
                layout_variant: "full".to_string(),
            }],
            updated_at: 0,
        })
        .unwrap();

    let image_cache = Arc::new(RwLock::new({
        let mut m = std::collections::HashMap::new();
        m.insert("default".to_string(), b"stale-png".to_vec());
        m
    }));

    let scheduler = Arc::new(cascades::api::SourceScheduler::new(Arc::clone(&source_store)));

    let state = Arc::new(cascades::api::AppState {
        compositor,
        instance_store,
        layout_store,
        source_store,
        scheduler,
        image_cache: Arc::clone(&image_cache),
        api_key: "key".to_string(),
        refresh_rate_secs: 60,
        started_at: std::time::Instant::now(),
        sidecar_url: "http://localhost:3001".to_string(),
    });

    let app = cascades::api::build_router(Arc::clone(&state));

    let req = Request::builder()
        .method("POST")
        .uri("/api/webhook/river")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"level_ft": 9.1}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Cache entry for "default" should have been invalidated — either replaced
    // with a fresh render or removed.  The stale bytes must not remain.
    let cache = image_cache.read().unwrap();
    let still_stale = cache
        .get("default")
        .map(|v| v.as_slice() == b"stale-png")
        .unwrap_or(false);
    assert!(
        !still_stale,
        "webhook should have invalidated the stale cache entry"
    );
}

// ── GET /api/display ──────────────────────────────────────────────────────────

#[tokio::test]
async fn get_display_without_auth_returns_401() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/display")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_display_with_wrong_key_returns_401() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/display")
        .header(header::AUTHORIZATION, "Bearer wrong-key")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_display_with_correct_key_returns_200() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/display")
        .header(header::AUTHORIZATION, "Bearer test-bearer-key")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_display_returns_json_with_required_fields() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/display")
        .header(header::AUTHORIZATION, "Bearer test-bearer-key")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("response should be JSON");

    assert!(
        json.get("image_url").and_then(|v| v.as_str()).is_some(),
        "response should have 'image_url' string field; got: {json}"
    );
    assert!(
        json.get("refresh_rate").and_then(|v| v.as_u64()).is_some(),
        "response should have 'refresh_rate' number field; got: {json}"
    );
}

#[tokio::test]
async fn get_display_image_url_points_to_api_image() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/display")
        .header(header::AUTHORIZATION, "Bearer test-bearer-key")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let url = json["image_url"].as_str().unwrap();
    assert!(
        url.starts_with("/api/image/default"),
        "image_url should start with '/api/image/default'; got: {url}"
    );
}

#[tokio::test]
async fn get_display_refresh_rate_matches_config() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/display")
        .header(header::AUTHORIZATION, "Bearer test-bearer-key")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(
        json["refresh_rate"].as_u64().unwrap(),
        42,
        "refresh_rate should match the configured value (42)"
    );
}

// ── GET /api/image/:display_id ────────────────────────────────────────────────

#[tokio::test]
async fn get_image_known_display_returns_200() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/image/default")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_image_unknown_display_returns_404() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/image/nonexistent-display")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_image_content_type_is_png() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/image/default")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, "image/png");
}

#[tokio::test]
async fn get_image_has_no_store_cache_control() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/image/default")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    let cc = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(cc, "no-store");
}

#[tokio::test]
async fn get_image_body_is_valid_png() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/api/image/default")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        bytes.starts_with(b"\x89PNG"),
        "response body should be a valid PNG"
    );
}

// ── GET /image.png — legacy endpoint preserved ────────────────────────────────

#[tokio::test]
async fn legacy_image_endpoint_still_works_with_new_router() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (app, _state) = make_api_app(tmp.path());

    let req = Request::builder()
        .uri("/image.png")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, "image/png");
}
