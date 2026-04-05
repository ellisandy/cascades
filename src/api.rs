//! HTTP API layer — request handlers and router construction.
//!
//! Implements the output-layer endpoints:
//! - `POST /api/webhook/:plugin_instance_id` — store new data, re-render affected displays
//! - `GET  /api/display`                      — bearer-authenticated; returns image URL + refresh rate
//! - `GET  /api/image/:display_id`            — latest rendered PNG, `Cache-Control: no-store`
//! - `GET  /image.png`                        — legacy alias for the default display

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
}

// ─── Router ──────────────────────────────────────────────────────────────────

/// Build the axum `Router` with all routes wired up.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/image.png", get(serve_image_legacy))
        .route("/api/webhook/{plugin_instance_id}", post(post_webhook))
        .route("/api/display", get(get_display))
        .route("/api/image/{display_id}", get(get_image))
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
