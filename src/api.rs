//! HTTP API layer — request handlers and router construction.
//!
//! Implements the output-layer endpoints:
//! - `POST /api/webhook/:plugin_instance_id` — store new data, re-render affected displays
//! - `GET  /api/display`                      — bearer-authenticated; returns image URL + refresh rate
//! - `GET  /api/image/:display_id`            — latest rendered PNG, `Cache-Control: no-store`
//! - `GET  /image.png`                        — legacy alias for the default display
//! - `GET  /api/status`                       — JSON health snapshot with per-source state

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap, Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde_json::Value;

use crate::{
    compositor::{Compositor, DisplayConfiguration},
    instance_store::InstanceStore,
};

// ─── Shared state ─────────────────────────────────────────────────────────────

/// Shared application state, held in an `Arc` and injected into every handler.
pub struct AppState {
    pub compositor: Arc<Compositor>,
    pub instance_store: Arc<InstanceStore>,
    /// All named display configurations keyed by display ID (e.g. `"default"`).
    pub display_configs: HashMap<String, DisplayConfiguration>,
    /// In-memory PNG cache: display_id → latest rendered PNG bytes.
    /// Invalidated by `POST /api/webhook/:id` for affected displays.
    pub image_cache: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// Bearer token required for `GET /api/display`.
    pub api_key: String,
    /// Device refresh rate in seconds, returned by `GET /api/display`.
    pub refresh_rate_secs: u64,
    /// Time the server started; used to compute `uptime_secs` in `GET /api/status`.
    pub started_at: std::time::Instant,
    /// Base URL of the Bun render sidecar; surfaced in `GET /api/status`.
    pub sidecar_url: String,
}

// ─── Router ──────────────────────────────────────────────────────────────────

/// Build the axum `Router` with all routes wired up.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/image.png", get(serve_image_legacy))
        .route("/api/webhook/{plugin_instance_id}", post(post_webhook))
        .route("/api/display", get(get_display))
        .route("/api/image/{display_id}", get(get_image))
        .route("/api/status", get(get_status))
        .with_state(state)
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `GET /image.png` — legacy endpoint, aliases to the default display.
async fn serve_image_legacy(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    match render_for_display(&app, "default").await {
        Some(png) => ([(header::CONTENT_TYPE, "image/png")], png).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `POST /api/webhook/:plugin_instance_id`
///
/// Stores the JSON body as `cached_data` for the named plugin instance and
/// re-renders every display config that contains that instance.
async fn post_webhook(
    Path(plugin_instance_id): Path<String>,
    State(app): State<Arc<AppState>>,
    body: Bytes,
) -> impl IntoResponse {
    let data: Value = serde_json::from_slice(&body)
        .unwrap_or_else(|_| Value::Object(Default::default()));

    let now = unix_now_secs();
    app.instance_store
        .update_cached_data(&plugin_instance_id, &data, now as i64)
        .ok();

    // Re-render every display that uses this plugin instance.
    let affected: Vec<(String, DisplayConfiguration)> = app
        .display_configs
        .iter()
        .filter(|(_, cfg)| {
            cfg.slots
                .iter()
                .any(|s| s.plugin_instance_id == plugin_instance_id)
        })
        .map(|(id, cfg)| (id.clone(), cfg.clone()))
        .collect();

    for (display_id, config) in affected {
        match compose_display(&app, &config).await {
            Some(png) => {
                app.image_cache.write().unwrap().insert(display_id, png);
            }
            None => {
                // Re-render failed — remove stale entry so the next GET re-renders.
                app.image_cache.write().unwrap().remove(&display_id);
            }
        }
    }

    StatusCode::NO_CONTENT
}

/// `GET /api/display` — returns image URL and refresh rate.
///
/// Requires `Authorization: Bearer <api_key>` header.
async fn get_display(headers: HeaderMap, State(app): State<Arc<AppState>>) -> impl IntoResponse {
    if !is_authorized(&headers, &app.api_key) {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("Unauthorized"))
            .unwrap();
    }

    let now = unix_now_secs();
    let body = serde_json::json!({
        "image_url": format!("/api/image/default?t={}", now),
        "refresh_rate": app.refresh_rate_secs,
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

/// `GET /api/image/:display_id` — returns the latest PNG for that display.
///
/// Served from cache when available; rendered on demand otherwise.
/// Always includes `Cache-Control: no-store`.
async fn get_image(
    Path(display_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    // Serve from cache if present.
    {
        let cache = app.image_cache.read().unwrap();
        if let Some(png) = cache.get(&display_id) {
            return image_response(png.clone());
        }
    }

    // Render on demand.
    let config = app.display_configs.get(&display_id).cloned();
    match config {
        Some(cfg) => match compose_display(&app, &cfg).await {
            Some(png) => {
                app.image_cache
                    .write()
                    .unwrap()
                    .insert(display_id, png.clone());
                image_response(png)
            }
            None => Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap(),
        },
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap(),
    }
}

/// `GET /api/status` — JSON health snapshot with per-source state.
///
/// No authentication required. Returns 200 with `Content-Type: application/json`.
///
/// Response shape:
/// ```json
/// {
///   "version": "0.1.0",
///   "uptime_secs": 42,
///   "sidecar_url": "http://localhost:3001",
///   "sources": [
///     {
///       "id": "weather",
///       "name": "Weather",
///       "enabled": true,
///       "last_fetched_at": 1700000000,
///       "last_error": null,
///       "data_age_secs": 30
///     }
///   ]
/// }
/// ```
async fn get_status(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let now = unix_now_secs();
    let uptime_secs = app.started_at.elapsed().as_secs();

    let instances = match app.instance_store.list_instances() {
        Ok(v) => v,
        Err(e) => {
            log::error!("get_status: list_instances failed: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let sources: Vec<serde_json::Value> = instances
        .iter()
        .map(|inst| {
            let name = capitalize_first(&inst.id);
            let last_fetched_at = inst.last_fetched_at.map(|ts| ts as u64);
            let data_age_secs = inst.last_fetched_at.and_then(|ts| {
                if ts > 0 && now as i64 >= ts {
                    Some((now as i64 - ts) as u64)
                } else {
                    None
                }
            });
            serde_json::json!({
                "id": inst.id,
                "name": name,
                "enabled": true,
                "last_fetched_at": last_fetched_at,
                "last_error": inst.last_error,
                "data_age_secs": data_age_secs,
            })
        })
        .collect();

    let body = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": uptime_secs,
        "sidecar_url": app.sidecar_url,
        "sources": sources,
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn image_response(png: Vec<u8>) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(png))
        .unwrap()
}

fn is_authorized(headers: &HeaderMap, api_key: &str) -> bool {
    let expected = format!("Bearer {}", api_key);
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == expected)
        .unwrap_or(false)
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Compose a display config via the compositor.  Returns `None` on error.
async fn compose_display(app: &AppState, config: &DisplayConfiguration) -> Option<Vec<u8>> {
    match app.compositor.compose(config).await {
        Ok(png) => Some(png),
        Err(e) => {
            log::error!("compositor error for '{}': {}", config.name, e);
            None
        }
    }
}

/// Render a named display config by ID, using cache if available.
async fn render_for_display(app: &AppState, display_id: &str) -> Option<Vec<u8>> {
    {
        let cache = app.image_cache.read().unwrap();
        if let Some(png) = cache.get(display_id) {
            return Some(png.clone());
        }
    }

    let config = app.display_configs.get(display_id).cloned()?;
    let png = compose_display(app, &config).await?;
    app.image_cache
        .write()
        .unwrap()
        .insert(display_id.to_string(), png.clone());
    Some(png)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        instance_store::{seed_from_config, InstanceStore},
        template::TemplateEngine,
    };
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Build a minimal AppState for testing the /api/status endpoint.
    ///
    /// Uses a temporary SQLite database seeded with the 5 well-known instances
    /// and an empty templates directory (status doesn't render any templates).
    fn make_test_state() -> Arc<AppState> {
        use crate::config::{Config, DisplayConfig, LocationConfig, SourceIntervals, StorageConfig};

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let instance_store = Arc::new(InstanceStore::open(&db_path).unwrap());
        let config = Config {
            display: DisplayConfig { width: 800, height: 480 },
            location: LocationConfig {
                latitude: 48.4,
                longitude: -122.3,
                name: "Test".to_string(),
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
        };
        seed_from_config(&instance_store, &config).unwrap();

        let template_engine = Arc::new(TemplateEngine::new(&templates_dir).unwrap());
        let compositor = Arc::new(Compositor::new(
            Arc::clone(&template_engine),
            Arc::clone(&instance_store),
            "http://localhost:3001".to_string(),
        ));

        Arc::new(AppState {
            compositor,
            instance_store,
            display_configs: HashMap::new(),
            image_cache: Arc::new(RwLock::new(HashMap::new())),
            api_key: "test-key".to_string(),
            refresh_rate_secs: 60,
            started_at: std::time::Instant::now(),
            sidecar_url: "http://localhost:3001".to_string(),
        })
    }

    #[tokio::test]
    async fn get_status_returns_200_json() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.contains("application/json"), "expected application/json, got {content_type}");
    }

    #[tokio::test]
    async fn get_status_body_has_required_top_level_fields() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert!(body["version"].is_string(), "version must be a string");
        assert!(body["uptime_secs"].is_number(), "uptime_secs must be a number");
        assert!(body["sidecar_url"].is_string(), "sidecar_url must be a string");
        assert!(body["sources"].is_array(), "sources must be an array");
    }

    #[tokio::test]
    async fn get_status_sources_include_weather_and_river() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let sources = body["sources"].as_array().unwrap();
        let ids: Vec<&str> = sources
            .iter()
            .filter_map(|s| s["id"].as_str())
            .collect();
        assert!(ids.contains(&"weather"), "sources must include 'weather', got: {ids:?}");
        assert!(ids.contains(&"river"), "sources must include 'river', got: {ids:?}");
    }

    #[tokio::test]
    async fn get_status_source_shape_is_correct() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/status")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let sources = body["sources"].as_array().unwrap();
        for source in sources {
            assert!(source["id"].is_string(), "source.id must be a string");
            assert!(source["name"].is_string(), "source.name must be a string");
            assert!(source["enabled"].is_boolean(), "source.enabled must be a boolean");
            // last_fetched_at and last_error may be null (freshly seeded, never fetched)
            // Verify the keys exist (Value::Null is fine; missing key returns Value::Null too,
            // but we check the array length >= 1 earlier so at least a source exists)
            assert!(source.get("last_fetched_at").is_some(), "last_fetched_at key must exist");
            assert!(source.get("last_error").is_some(), "last_error key must exist");
            assert!(source.get("data_age_secs").is_some(), "data_age_secs key must exist");
        }
    }
}
