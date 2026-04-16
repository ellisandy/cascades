//! HTTP API layer — request handlers and router construction.
//!
//! Implements the output-layer endpoints:
//! - `GET  /`                                  — HTML landing page: endpoint catalog, setup guide, live status
//! - `POST /api/webhook/:plugin_instance_id`   — store new data, re-render affected displays
//! - `GET  /api/display`                       — bearer-authenticated; returns image URL + refresh rate
//! - `GET  /api/image/:display_id`             — latest rendered PNG, `Cache-Control: no-store`
//! - `GET  /image.png`                         — legacy alias for the default display
//! - `GET  /api/status`                        — JSON health snapshot with per-source state
//!
//! Admin API endpoints (require `X-Api-Key` header):
//! - `GET  /admin`                                      — serve admin UI HTML placeholder
//! - `GET  /api/admin/layouts`                          — list layout summaries `[{id, name, updated_at}]`
//! - `GET  /api/admin/layout/{id}`                      — get full layout as JSON
//! - `PUT  /api/admin/layout/{id}`                      — replace full layout, returns updated layout
//! - `POST /api/admin/preview/{id}`                     — render layout to PNG
//! - `GET  /api/admin/plugins`                          — list plugin instances `[{id, name, supported_variants}]`
//! - `GET  /api/admin/active-layout`                     — get active layout ID
//! - `PUT  /api/admin/active-layout`                     — set which layout drives `/image.png`
//! - `POST /api/admin/layout/{id}/item`                 — add item to layout
//! - `PUT  /api/admin/layout/{id}/item/{item_id}`       — update single item
//! - `DELETE /api/admin/layout/{id}/item/{item_id}`     — remove item
//! - `GET  /api/admin/sources/{id}/fields`              — list field mappings for a source
//! - `POST /api/admin/sources/{id}/fields`              — create a field mapping
//! - `PUT  /api/admin/fields/{id}`                      — update a field mapping
//! - `DELETE /api/admin/fields/{id}`                    — delete a field mapping
//! - `GET  /api/admin/sources/{id}/data`                — cached data JSON for a source

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap, Response, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    compositor::{Compositor, DisplayConfiguration},
    crypto::{self, EncryptionKey},
    instance_store::InstanceStore,
    layout_store::{LayoutConfig, LayoutItem, LayoutStore},
    source_store::{DataSourceConfig, SourceStore},
    sources::{Source, generic::GenericHttpSource},
};

// ─── Shared state ─────────────────────────────────────────────────────────────

/// Shared application state, held in an `Arc` and injected into every handler.
pub struct AppState {
    pub compositor: Arc<Compositor>,
    pub instance_store: Arc<InstanceStore>,
    /// SQLite-backed store for display layout configurations.
    pub layout_store: Arc<LayoutStore>,
    /// SQLite-backed store for user-defined generic HTTP data sources.
    pub source_store: Arc<SourceStore>,
    /// Manages background fetch tasks for generic HTTP data sources.
    pub scheduler: Arc<SourceScheduler>,
    /// In-memory PNG cache: display_id → latest rendered PNG bytes.
    pub image_cache: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// Bearer token required for `GET /api/display`.
    pub api_key: String,
    /// Encryption key derived from api_key for encrypting secret header values.
    pub encryption_key: EncryptionKey,
    /// Device refresh rate in seconds, returned by `GET /api/display`.
    pub refresh_rate_secs: u64,
    /// Time the server started; used to compute `uptime_secs` in `GET /api/status`.
    pub started_at: std::time::Instant,
    /// Base URL of the Bun render sidecar; surfaced in `GET /api/status`.
    pub sidecar_url: String,
}

/// Manages background fetch tasks for generic HTTP data sources.
pub struct SourceScheduler {
    source_store: Arc<SourceStore>,
    tasks: std::sync::Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
}

impl SourceScheduler {
    pub fn new(source_store: Arc<SourceStore>) -> Self {
        Self {
            source_store,
            tasks: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Spawn a background fetch task for a generic source.
    pub fn spawn_source(&self, source: GenericHttpSource) {
        let source_id = source.id().to_string();
        let store = Arc::clone(&self.source_store);
        let interval = source.refresh_interval();

        let handle = tokio::spawn(async move {
            let mut source = source;
            loop {
                let (s, result) = tokio::task::spawn_blocking(move || {
                    let r = source.fetch();
                    (source, r)
                })
                .await
                .expect("generic source task panicked");
                source = s;

                match result {
                    Ok(value) => {
                        let now_secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        if let Err(e) = store.update_cached_data(source.id(), &value, now_secs) {
                            log::warn!("generic source '{}': failed to store data: {}", source.name(), e);
                        }
                    }
                    Err(e) => {
                        log::warn!("generic source '{}' fetch failed: {}", source.name(), e);
                        store.update_last_error(source.id(), &e.to_string()).ok();
                    }
                }
                tokio::time::sleep(interval).await;
            }
        });

        let mut tasks = self.tasks.lock().unwrap();
        if let Some(old) = tasks.remove(&source_id) {
            old.abort();
        }
        tasks.insert(source_id, handle);
    }

    /// Stop the background fetch task for a source.
    pub fn stop_source(&self, source_id: &str) {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(handle) = tasks.remove(source_id) {
            handle.abort();
        }
    }

    /// Execute a one-shot fetch. Returns the fetched data or error.
    pub async fn fetch_once(source: GenericHttpSource) -> Result<serde_json::Value, String> {
        let (_, result) = tokio::task::spawn_blocking(move || {
            let r = source.fetch();
            (source, r)
        })
        .await
        .map_err(|e| format!("task panicked: {e}"))?;
        result.map_err(|e| e.to_string())
    }
}

// ─── Router ──────────────────────────────────────────────────────────────────

/// Build the axum `Router` with all routes wired up.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(get_landing))
        .route("/image.png", get(serve_image_legacy))
        .route("/api/webhook/{plugin_instance_id}", post(post_webhook))
        .route("/api/display", get(get_display))
        .route("/api/image/{display_id}", get(get_image))
        .route("/api/status", get(get_status))
        // Admin routes — all require X-Api-Key header or session cookie
        .route("/admin", get(get_admin_ui))
        .route("/admin/login", post(post_admin_login))
        .route("/api/admin/layouts", get(admin_list_layouts))
        .route("/api/admin/layout", post(admin_post_layout))
        .route("/api/admin/layout/{id}", get(admin_get_layout))
        .route("/api/admin/layout/{id}", put(admin_put_layout))
        .route("/api/admin/layout/{id}", delete(admin_delete_layout))
        .route("/api/admin/active-layout", get(admin_get_active_layout))
        .route("/api/admin/active-layout", put(admin_set_active_layout))
        .route("/api/admin/preview/{id}", post(admin_post_preview))
        .route("/api/admin/plugins", get(admin_list_plugins))
        .route("/api/admin/layout/{id}/item", post(admin_post_item))
        .route("/api/admin/layout/{id}/item/{item_id}", put(admin_put_item))
        .route("/api/admin/layout/{id}/item/{item_id}", delete(admin_delete_item))
        // Generic data source CRUD
        .route("/api/admin/sources", get(admin_list_sources))
        .route("/api/admin/sources", post(admin_create_source))
        .route("/api/admin/sources/{id}", get(admin_get_source))
        .route("/api/admin/sources/{id}", put(admin_update_source))
        .route("/api/admin/sources/{id}", delete(admin_delete_source))
        .route("/api/admin/sources/{id}/fetch", post(admin_fetch_source))
        // Field mapping CRUD
        .route("/api/admin/sources/{id}/fields", get(admin_list_fields))
        .route("/api/admin/sources/{id}/fields", post(admin_create_field))
        .route("/api/admin/fields/{id}", put(admin_update_field))
        .route("/api/admin/fields/{id}", delete(admin_delete_field))
        .route("/api/admin/sources/{id}/data", get(admin_get_source_data))
        .route("/api/admin/rotate-encryption-key", post(admin_rotate_encryption_key))
        .with_state(state)
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `GET /` — developer landing page: endpoint catalog, setup guide, live status.
///
/// Returns a self-contained HTML page rendered from a Rust string template.
/// No authentication required. Fetches `/api/status` client-side via JavaScript
/// to populate the live source-state table on page load.
async fn get_landing() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        LANDING_HTML,
    )
}

/// Self-contained HTML landing page embedded at compile time.
///
/// Dark-background developer status page — no external dependencies, no files.
const LANDING_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Cascades</title>
  <style>
    *{box-sizing:border-box;margin:0;padding:0}
    body{background:#0d1117;color:#c9d1d9;font-family:'Courier New',Courier,monospace;font-size:14px;line-height:1.6;padding:2rem;max-width:960px;margin:0 auto}
    h1{color:#58a6ff;font-size:1.4rem;margin-bottom:.5rem}
    h2{color:#79c0ff;font-size:1.05rem;margin:2rem 0 .6rem;border-bottom:1px solid #30363d;padding-bottom:.25rem}
    p{margin-bottom:1rem;color:#8b949e}
    a{color:#58a6ff;text-decoration:none}
    a:hover{text-decoration:underline}
    table{width:100%;border-collapse:collapse;margin-bottom:1rem;font-size:.9em}
    th{text-align:left;color:#58a6ff;border-bottom:1px solid #30363d;padding:.4rem .75rem}
    td{padding:.35rem .75rem;border-bottom:1px solid #21262d;vertical-align:top}
    tr:last-child td{border-bottom:none}
    code{background:#161b22;padding:.1em .35em;border-radius:3px;color:#e6edf3;font-size:.88em}
    pre{background:#161b22;padding:.9rem 1rem;border-radius:6px;overflow-x:auto;margin-bottom:1rem;color:#e6edf3;font-size:.88em}
    .auth-bearer{color:#f0883e}
    .auth-open{color:#3fb950}
    .ok{color:#3fb950}
    .err{color:#f85149}
    .muted{color:#484f58}
    ol{padding-left:1.4rem;color:#8b949e}
    ol li{margin-bottom:.5rem}
    img#preview{max-width:100%;border:1px solid #30363d;border-radius:4px;margin-top:.4rem;display:block}
    .section{margin-bottom:2.5rem}
  </style>
</head>
<body>

<div class="section">
  <h1>Cascades</h1>
  <p>
    Cascades is a self-hosted display server for e-ink and TFT wall panels.
    It aggregates real-time data from local sources — river gauges, ferry schedules,
    weather observations, trail conditions, and road alerts — and renders an 800&times;480
    pixel composite image that a wall-mounted device fetches on a configurable refresh cycle.
    Designed for Raspberry Pi and other Linux single-board computers.
  </p>
</div>

<div class="section">
  <h2>Endpoints</h2>
  <table>
    <thead><tr><th>Method</th><th>Path</th><th>Auth</th><th>Description</th></tr></thead>
    <tbody>
      <tr><td>GET</td><td><code>/</code></td><td><span class="auth-open">open</span></td><td>This page — catalog, setup guide, live status</td></tr>
      <tr><td>GET</td><td><code>/image.png</code></td><td><span class="auth-open">open</span></td><td>Latest rendered PNG for the default display</td></tr>
      <tr><td>GET</td><td><code>/api/display</code></td><td><span class="auth-bearer">Bearer</span></td><td>Returns <code>image_url</code> and <code>refresh_rate</code> (JSON) for device polling</td></tr>
      <tr><td>GET</td><td><code>/api/image/{id}</code></td><td><span class="auth-open">open</span></td><td>Latest rendered PNG for a named display ID</td></tr>
      <tr><td>POST</td><td><code>/api/webhook/{id}</code></td><td><span class="auth-open">open</span></td><td>Push new data for a plugin instance; triggers re-render of affected displays</td></tr>
      <tr><td>GET</td><td><code>/api/status</code></td><td><span class="auth-open">open</span></td><td>JSON health snapshot: version, uptime, per-source fetch state</td></tr>
    </tbody>
  </table>
</div>

<div class="section">
  <h2>Setup Guide</h2>
  <ol>
    <li>
      Install <a href="https://bun.sh" target="_blank">Bun</a> (JavaScript runtime for the render sidecar):
      <pre>curl -fsSL https://bun.sh/install | bash</pre>
    </li>
    <li>
      Start the render sidecar in a separate terminal:
      <pre>RENDER_PORT=3001 bun run src/sidecar/server.ts</pre>
    </li>
    <li>
      Start the Cascades server:
      <pre>cargo run --release</pre>
      <p style="margin-top:.4rem">Listens on <code>0.0.0.0:8080</code> by default. Set <code>[server] port</code> in <code>config.toml</code> to change.</p>
    </li>
    <li>Open <a href="/image.png"><code>/image.png</code></a> to verify the rendered output.</li>
  </ol>

  <h2>API Keys</h2>
  <p>Add credentials to <code>config.toml</code> so the data sources can reach their upstream APIs:</p>
  <pre>[sources]
wsdot_access_code = "your-wsdot-key"   # ferry schedules + highway alerts
nps_api_key       = "your-nps-key"     # trail conditions (National Park Service)</pre>
  <p>Restart the server after editing <code>config.toml</code>.</p>

  <h2>Adding a Plugin</h2>
  <ol>
    <li>Create a plugin instance config file (e.g. <code>config/plugins.d/my-plugin.toml</code>) with your plugin settings.</li>
    <li>Add a Liquid template at <code>templates/my-plugin.html.liquid</code> — the compositor renders it to an image slot.</li>
    <li>Restart the server — the plugin is auto-discovered and registered.</li>
  </ol>
</div>

<div class="section">
  <h2>Live Status</h2>
  <table id="status-table">
    <thead><tr><th>Source</th><th>Enabled</th><th>Last Fetch</th><th>Data Age</th><th>Last Error</th></tr></thead>
    <tbody id="status-body">
      <tr><td colspan="5" class="muted">Loading&#8230;</td></tr>
    </tbody>
  </table>
</div>

<div class="section">
  <h2>Preview</h2>
  <img id="preview" src="/image.png" alt="Current display output">
</div>

<script>
(function(){
  function esc(s){return s==null?'':String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;')}
  function fmtTs(ts){
    if(ts==null)return '<span class="muted">\u2014</span>';
    return new Date(ts*1000).toLocaleString();
  }
  function fmtAge(secs){
    if(secs==null)return '<span class="muted">\u2014</span>';
    if(secs<60)return secs+'s';
    if(secs<3600)return Math.floor(secs/60)+'m';
    return Math.floor(secs/3600)+'h '+Math.floor((secs%3600)/60)+'m';
  }
  function fmtErr(err){
    if(!err)return '<span class="muted">\u2014</span>';
    return '<span class="err">'+esc(err)+'</span>';
  }
  fetch('/api/status')
    .then(function(r){return r.json();})
    .then(function(data){
      var tbody=document.getElementById('status-body');
      var sources=data.sources||[];
      if(!sources.length){
        tbody.innerHTML='<tr><td colspan="5" class="muted">No sources configured.</td></tr>';
        return;
      }
      tbody.innerHTML=sources.map(function(s){
        return '<tr>'+
          '<td>'+esc(s.name)+'</td>'+
          '<td>'+(s.enabled?'<span class="ok">yes</span>':'<span class="err">no</span>')+'</td>'+
          '<td>'+fmtTs(s.last_fetched_at)+'</td>'+
          '<td>'+fmtAge(s.data_age_secs)+'</td>'+
          '<td>'+fmtErr(s.last_error)+'</td>'+
          '</tr>';
      }).join('');
    })
    .catch(function(e){
      document.getElementById('status-body').innerHTML=
        '<tr><td colspan="5" class="err">Failed to load status: '+esc(String(e))+'</td></tr>';
    });
})();
</script>

</body>
</html>"#;

/// `GET /image.png` — legacy endpoint, renders the active layout (or "default" fallback).
async fn serve_image_legacy(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let layout_id = app
        .layout_store
        .get_active_layout_id()
        .ok()
        .flatten()
        .unwrap_or_else(|| "default".to_string());
    match render_for_display(&app, &layout_id, "einkPreview").await {
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
    let all_layouts = match app.layout_store.list_layouts() {
        Ok(v) => v,
        Err(e) => {
            log::error!("post_webhook: list_layouts failed: {}", e);
            return StatusCode::NO_CONTENT;
        }
    };

    let affected: Vec<(String, DisplayConfiguration)> = all_layouts
        .iter()
        .filter(|layout| {
            layout.items.iter().any(|item| {
                matches!(item, LayoutItem::PluginSlot { plugin_instance_id: pid, .. } if *pid == plugin_instance_id)
            })
        })
        .map(|layout| (layout.id.clone(), DisplayConfiguration::from_layout_config(layout)))
        .collect();

    for (display_id, config) in affected {
        match compose_display(&app, &config, "einkPreview").await {
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
    let layout = match app.layout_store.get_layout(&display_id) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()
        }
        Err(e) => {
            log::error!("get_image: layout store error for '{}': {}", display_id, e);
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap();
        }
    };
    let cfg = DisplayConfiguration::from_layout_config(&layout);
    match compose_display(&app, &cfg, "einkPreview").await {
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

    let mut sources: Vec<serde_json::Value> = instances
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

    // Include generic HTTP sources
    if let Ok(generic_sources) = app.source_store.list() {
        for ds in &generic_sources {
            let last_fetched_at = ds.last_fetched_at.map(|ts| ts as u64);
            let data_age_secs = ds.last_fetched_at.and_then(|ts| {
                if ts > 0 && now as i64 >= ts {
                    Some((now as i64 - ts) as u64)
                } else {
                    None
                }
            });
            sources.push(serde_json::json!({
                "id": ds.id,
                "name": ds.name,
                "enabled": true,
                "last_fetched_at": last_fetched_at,
                "last_error": ds.last_error,
                "data_age_secs": data_age_secs,
            }));
        }
    }

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

// ─── Admin types ─────────────────────────────────────────────────────────────

/// Flat item payload used by POST/PUT item admin endpoints.
///
/// All geometry fields are required.  Type-specific fields (`plugin_instance_id`,
/// `layout_variant`, `text_content`, `font_size`, `orientation`) are optional;
/// missing ones get safe defaults on conversion to [`LayoutItem`].
#[derive(Debug, Deserialize)]
struct ItemPayload {
    id: String,
    item_type: String,
    z_index: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    plugin_instance_id: Option<String>,
    layout_variant: Option<String>,
    text_content: Option<String>,
    font_size: Option<i32>,
    orientation: Option<String>,
    field_mapping_id: Option<String>,
    format_string: Option<String>,
    label: Option<String>,
}

impl ItemPayload {
    fn into_layout_item(self) -> Result<LayoutItem, String> {
        match self.item_type.as_str() {
            "plugin_slot" => Ok(LayoutItem::PluginSlot {
                id: self.id,
                z_index: self.z_index,
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                plugin_instance_id: self.plugin_instance_id.unwrap_or_default(),
                layout_variant: self.layout_variant.unwrap_or_else(|| "full".to_string()),
            }),
            "static_text" => Ok(LayoutItem::StaticText {
                id: self.id,
                z_index: self.z_index,
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                text_content: self.text_content.unwrap_or_default(),
                font_size: self.font_size.unwrap_or(16),
                orientation: self.orientation,
            }),
            "static_datetime" => Ok(LayoutItem::StaticDateTime {
                id: self.id,
                z_index: self.z_index,
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                font_size: self.font_size.unwrap_or(16),
                format: self.text_content,
                orientation: self.orientation,
            }),
            "static_divider" => Ok(LayoutItem::StaticDivider {
                id: self.id,
                z_index: self.z_index,
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                orientation: self.orientation,
            }),
            "data_field" => Ok(LayoutItem::DataField {
                id: self.id,
                z_index: self.z_index,
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                field_mapping_id: self.field_mapping_id.unwrap_or_default(),
                font_size: self.font_size.unwrap_or(16),
                format_string: self
                    .format_string
                    .unwrap_or_else(|| "{{value}}".to_string()),
                label: self.label,
                orientation: self.orientation,
            }),
            other => Err(format!("unknown item_type '{other}'")),
        }
    }
}

/// Body for `PUT /api/admin/layout/{id}`.
#[derive(Debug, Deserialize)]
struct LayoutPayload {
    name: String,
    items: Vec<ItemPayload>,
}

/// Body for `PUT /api/admin/active-layout`.
#[derive(Debug, Deserialize)]
struct ActiveLayoutPayload {
    layout_id: String,
}

/// Summary entry returned by `GET /api/admin/layouts`.
#[derive(Debug, Serialize)]
struct LayoutSummary {
    id: String,
    name: String,
    updated_at: i64,
}

/// Plugin instance entry returned by `GET /api/admin/plugins`.
#[derive(Debug, Serialize)]
struct PluginInstanceSummary {
    id: String,
    name: String,
    supported_variants: Vec<&'static str>,
}

/// Body for `POST /api/admin/sources/:id/fields`.
#[derive(Debug, Deserialize)]
struct CreateFieldPayload {
    name: String,
    json_path: String,
}

/// Body for `PUT /api/admin/fields/:id`.
#[derive(Debug, Deserialize)]
struct UpdateFieldPayload {
    name: Option<String>,
    json_path: Option<String>,
}

// ─── Admin handlers ───────────────────────────────────────────────────────────

/// `GET /admin` — serve the admin UI if authenticated (session cookie), else the login page.
async fn get_admin_ui(
    headers: HeaderMap,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if is_admin_cookie_valid(&headers, &app.api_key) {
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            ADMIN_HTML,
        )
            .into_response()
    } else {
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            ADMIN_LOGIN_HTML,
        )
            .into_response()
    }
}

/// `POST /admin/login` — validate API key, set session cookie, redirect to admin.
async fn post_admin_login(
    State(app): State<Arc<AppState>>,
    body: Bytes,
) -> impl IntoResponse {
    // Parse form body: key=<value> (application/x-www-form-urlencoded)
    let body_str = String::from_utf8_lossy(&body);
    let mut submitted_key = String::new();
    for pair in body_str.split('&') {
        if let Some(val) = pair.strip_prefix("key=") {
            // Decode percent-encoding for the key value
            submitted_key = percent_decode(val);
        }
    }

    if submitted_key == app.api_key {
        Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(header::LOCATION, "/admin")
            .header(
                header::SET_COOKIE,
                format!(
                    "cascades_admin_key={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400",
                    submitted_key,
                ),
            )
            .body(Body::empty())
            .unwrap()
    } else {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(ADMIN_LOGIN_HTML.replace(
                "<!--LOGIN_ERROR-->",
                r#"<p class="error">Invalid API key.</p>"#,
            )))
            .unwrap()
    }
}

const ADMIN_HTML: &str = include_str!("../templates/admin.html");

const ADMIN_LOGIN_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Cascades Admin — Login</title>
  <style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      background: #0d1117; color: #c9d1d9;
      font-family: 'Courier New', Courier, monospace;
      display: flex; align-items: center; justify-content: center;
      height: 100vh;
    }
    .login-box {
      background: #161b22; border: 1px solid #30363d; border-radius: 8px;
      padding: 32px; width: 340px; text-align: center;
    }
    .login-box h1 { font-size: 18px; margin-bottom: 8px; color: #e6edf3; }
    .login-box p.sub { font-size: 12px; color: #8b949e; margin-bottom: 20px; }
    .login-box input[type="password"] {
      width: 100%; padding: 8px 10px; margin-bottom: 12px;
      background: #0d1117; border: 1px solid #30363d; border-radius: 4px;
      color: #c9d1d9; font-family: inherit; font-size: 13px;
    }
    .login-box input[type="password"]:focus {
      outline: none; border-color: #58a6ff;
    }
    .login-box button {
      width: 100%; padding: 8px; background: #238636; border: none;
      border-radius: 4px; color: #fff; font-family: inherit;
      font-size: 13px; cursor: pointer;
    }
    .login-box button:hover { background: #2ea043; }
    .error { color: #f85149; font-size: 12px; margin-bottom: 12px; }
  </style>
</head>
<body>
  <form class="login-box" method="POST" action="/admin/login">
    <h1>Cascades Admin</h1>
    <p class="sub">Enter your API key to continue.</p>
    <!--LOGIN_ERROR-->
    <input type="password" name="key" placeholder="API Key" autofocus required>
    <button type="submit">Sign in</button>
  </form>
</body>
</html>"#;

/// `GET /api/admin/layouts` — list all layout summaries.
async fn admin_list_layouts(
    headers: HeaderMap,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let layouts = match app.layout_store.list_layouts() {
        Ok(v) => v,
        Err(e) => {
            log::error!("admin_list_layouts: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let summaries: Vec<LayoutSummary> = layouts
        .into_iter()
        .map(|l| LayoutSummary { id: l.id, name: l.name, updated_at: l.updated_at })
        .collect();

    Json(summaries).into_response()
}

/// `GET /api/admin/layout/{id}` — get full layout as JSON.
async fn admin_get_layout(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app.layout_store.get_layout(&id) {
        Ok(Some(layout)) => Json(layout).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_get_layout '{}': {}", id, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `PUT /api/admin/layout/{id}` — replace entire layout; returns saved layout.
async fn admin_put_layout(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<LayoutPayload>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let items: Result<Vec<LayoutItem>, String> =
        payload.items.into_iter().map(|p| p.into_layout_item()).collect();

    let items = match items {
        Ok(v) => v,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"error": e})).unwrap(),
                ))
                .unwrap();
        }
    };

    let layout = LayoutConfig { id: id.clone(), name: payload.name, items, updated_at: 0 };

    if let Err(e) = app.layout_store.upsert_layout(&layout) {
        log::error!("admin_put_layout '{}': {}", id, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Invalidate image cache so next render picks up the new layout.
    app.image_cache.write().unwrap().remove(&id);

    match app.layout_store.get_layout(&id) {
        Ok(Some(saved)) => Json(saved).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `POST /api/admin/layout` — create a new layout.
async fn admin_post_layout(
    headers: HeaderMap,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<LayoutPayload>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Generate a new unique ID (simple timestamp-based)
    let id = format!("layout-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis());

    let items: Result<Vec<LayoutItem>, String> =
        payload.items.into_iter().map(|p| p.into_layout_item()).collect();

    let items = match items {
        Ok(v) => v,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"error": e})).unwrap(),
                ))
                .unwrap();
        }
    };

    let layout = LayoutConfig { id: id.clone(), name: payload.name, items, updated_at: 0 };

    if let Err(e) = app.layout_store.upsert_layout(&layout) {
        log::error!("admin_post_layout '{}': {}", id, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    match app.layout_store.get_layout(&id) {
        Ok(Some(saved)) => (StatusCode::CREATED, Json(saved)).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `DELETE /api/admin/layout/{id}` — delete a layout.
async fn admin_delete_layout(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if let Err(e) = app.layout_store.delete_layout(&id) {
        log::error!("admin_delete_layout '{}': {}", id, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Invalidate image cache
    app.image_cache.write().unwrap().remove(&id);

    StatusCode::NO_CONTENT.into_response()
}

/// `GET /api/admin/active-layout` — get the active layout ID.
async fn admin_get_active_layout(
    headers: HeaderMap,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let active_id = app
        .layout_store
        .get_active_layout_id()
        .ok()
        .flatten();

    Json(serde_json::json!({ "layout_id": active_id })).into_response()
}

/// `PUT /api/admin/active-layout` — set which layout drives `/image.png`.
async fn admin_set_active_layout(
    headers: HeaderMap,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<ActiveLayoutPayload>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Verify the layout exists
    match app.layout_store.get_layout(&payload.layout_id) {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_set_active_layout: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if let Err(e) = app.layout_store.set_active_layout_id(&payload.layout_id) {
        log::error!("admin_set_active_layout: {}", e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Invalidate image cache for the legacy endpoint
    app.image_cache.write().unwrap().clear();

    Json(serde_json::json!({ "layout_id": payload.layout_id })).into_response()
}

/// `POST /api/admin/preview/{id}` — render layout to PNG.
async fn admin_post_preview(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let layout = match app.layout_store.get_layout(&id) {
        Ok(Some(l)) => l,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_post_preview '{}': {}", id, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let cfg = DisplayConfiguration::from_layout_config(&layout);
    match compose_display(&app, &cfg, "einkPreview").await {
        Some(png) => ([(header::CONTENT_TYPE, "image/png")], png).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `GET /api/admin/plugins` — list plugin instances with supported variants.
async fn admin_list_plugins(
    headers: HeaderMap,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let instances = match app.instance_store.list_instances() {
        Ok(v) => v,
        Err(e) => {
            log::error!("admin_list_plugins: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let plugins: Vec<PluginInstanceSummary> = instances
        .into_iter()
        .map(|inst| PluginInstanceSummary {
            name: capitalize_first(&inst.plugin_id),
            id: inst.id,
            supported_variants: vec!["full", "half_horizontal", "half_vertical", "quadrant"],
        })
        .collect();

    Json(plugins).into_response()
}

/// `POST /api/admin/layout/{id}/item` — add an item to an existing layout.
async fn admin_post_item(
    headers: HeaderMap,
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<ItemPayload>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let mut layout = match app.layout_store.get_layout(&id) {
        Ok(Some(l)) => l,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_post_item '{}': {}", id, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let item = match payload.into_layout_item() {
        Ok(i) => i,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"error": e})).unwrap(),
                ))
                .unwrap();
        }
    };

    layout.items.push(item);

    if let Err(e) = app.layout_store.upsert_layout(&layout) {
        log::error!("admin_post_item upsert '{}': {}", id, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    app.image_cache.write().unwrap().remove(&id);

    match app.layout_store.get_layout(&id) {
        Ok(Some(saved)) => (StatusCode::CREATED, Json(saved)).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `PUT /api/admin/layout/{id}/item/{item_id}` — replace a single item.
async fn admin_put_item(
    headers: HeaderMap,
    Path((id, item_id)): Path<(String, String)>,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<ItemPayload>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let mut layout = match app.layout_store.get_layout(&id) {
        Ok(Some(l)) => l,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_put_item '{}': {}", id, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let pos = match layout.items.iter().position(|i| i.id() == item_id) {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let new_item = match payload.into_layout_item() {
        Ok(i) => i,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({"error": e})).unwrap(),
                ))
                .unwrap();
        }
    };

    layout.items[pos] = new_item;

    if let Err(e) = app.layout_store.upsert_layout(&layout) {
        log::error!("admin_put_item upsert '{}': {}", id, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    app.image_cache.write().unwrap().remove(&id);

    match app.layout_store.get_layout(&id) {
        Ok(Some(saved)) => Json(saved).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `DELETE /api/admin/layout/{id}/item/{item_id}` — remove a single item.
async fn admin_delete_item(
    headers: HeaderMap,
    Path((id, item_id)): Path<(String, String)>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let mut layout = match app.layout_store.get_layout(&id) {
        Ok(Some(l)) => l,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_delete_item '{}': {}", id, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let before = layout.items.len();
    layout.items.retain(|i| i.id() != item_id);
    if layout.items.len() == before {
        return StatusCode::NOT_FOUND.into_response();
    }

    if let Err(e) = app.layout_store.upsert_layout(&layout) {
        log::error!("admin_delete_item upsert '{}': {}", id, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    app.image_cache.write().unwrap().remove(&id);

    StatusCode::NO_CONTENT.into_response()
}

// ─── Encrypted headers helpers ──────────────────────────────────────────────

/// Encrypt secret_headers from a DataSourceConfig input.
/// Returns the JSON string to store in the encrypted_headers column.
fn encrypt_secret_headers(
    key: &EncryptionKey,
    config: &DataSourceConfig,
    existing_encrypted: Option<&serde_json::Value>,
) -> Result<String, String> {
    let Some(secret_headers) = &config.secret_headers else {
        // No secret headers in input — keep existing or default to empty
        if let Some(existing) = existing_encrypted {
            return serde_json::to_string(existing).map_err(|e| e.to_string());
        }
        return Ok("[]".to_string());
    };

    let mut result = Vec::new();
    for sh in secret_headers {
        match &sh.value {
            Some(plaintext) => {
                // New value — encrypt it
                let encrypted = crypto::encrypt(key, plaintext)
                    .map_err(|e| format!("encryption failed for header '{}': {}", sh.key, e))?;
                result.push(serde_json::json!({"key": sh.key, "value": encrypted}));
            }
            None => {
                // Null value — preserve existing encrypted value for this key
                if let Some(existing) = existing_encrypted {
                    if let Some(arr) = existing.as_array() {
                        if let Some(entry) = arr.iter().find(|e| e["key"] == sh.key) {
                            result.push(entry.clone());
                            continue;
                        }
                    }
                }
                // Key not found in existing — skip it
            }
        }
    }
    serde_json::to_string(&result).map_err(|e| e.to_string())
}

/// Mask encrypted header values for API responses (replace values with null).
fn mask_encrypted_headers(encrypted_headers: &serde_json::Value) -> serde_json::Value {
    if let Some(arr) = encrypted_headers.as_array() {
        let masked: Vec<serde_json::Value> = arr
            .iter()
            .filter_map(|entry| {
                entry["key"].as_str().map(|k| {
                    serde_json::json!({"key": k, "value": null})
                })
            })
            .collect();
        serde_json::Value::Array(masked)
    } else {
        serde_json::json!([])
    }
}

/// Build a masked DataSource for API responses.
fn mask_source_for_response(ds: &crate::source_store::DataSource) -> serde_json::Value {
    serde_json::json!({
        "id": ds.id,
        "name": ds.name,
        "url": ds.url,
        "method": ds.method,
        "headers": ds.headers,
        "encrypted_headers": mask_encrypted_headers(&ds.encrypted_headers),
        "body_template": ds.body_template,
        "response_root_path": ds.response_root_path,
        "refresh_interval_secs": ds.refresh_interval_secs,
        "cached_data": ds.cached_data,
        "last_fetched_at": ds.last_fetched_at,
        "last_error": ds.last_error,
        "created_at": ds.created_at,
        "updated_at": ds.updated_at,
    })
}

// ─── Generic data source handlers ───────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SourceSummary {
    id: String,
    name: String,
    source_kind: String,
    url: Option<String>,
    method: Option<String>,
    refresh_interval_secs: Option<i64>,
    last_fetched_at: Option<i64>,
    last_error: Option<String>,
}

/// `GET /api/admin/sources` — list all sources (built-in + generic).
async fn admin_list_sources(
    headers: HeaderMap,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let mut sources: Vec<SourceSummary> = Vec::new();

    if let Ok(instances) = app.instance_store.list_instances() {
        for inst in instances {
            sources.push(SourceSummary {
                id: inst.id.clone(),
                name: capitalize_first(&inst.id),
                source_kind: "builtin".to_string(),
                url: None,
                method: None,
                refresh_interval_secs: None,
                last_fetched_at: inst.last_fetched_at,
                last_error: inst.last_error,
            });
        }
    }

    if let Ok(generic_sources) = app.source_store.list() {
        for ds in generic_sources {
            sources.push(SourceSummary {
                id: ds.id.clone(),
                name: ds.name.clone(),
                source_kind: "generic".to_string(),
                url: Some(ds.url.clone()),
                method: Some(ds.method.clone()),
                refresh_interval_secs: Some(ds.refresh_interval_secs),
                last_fetched_at: ds.last_fetched_at,
                last_error: ds.last_error,
            });
        }
    }

    Json(sources).into_response()
}

/// `POST /api/admin/sources` — create a new generic data source.
async fn admin_create_source(
    headers: HeaderMap,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<DataSourceConfig>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let enc_json = match encrypt_secret_headers(&app.encryption_key, &payload, None) {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    };

    match app.source_store.create(&payload, &enc_json) {
        Ok(ds) => {
            let generic = GenericHttpSource::from_data_source(&ds, Some(&app.encryption_key));
            app.scheduler.spawn_source(generic);
            (StatusCode::CREATED, Json(mask_source_for_response(&ds))).into_response()
        }
        Err(e) => {
            log::error!("admin_create_source: {}", e);
            let status = if matches!(e, crate::source_store::SourceStoreError::Validation(_)) {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(serde_json::json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// `GET /api/admin/sources/:id` — get source details.
/// Encrypted header values are masked (returned as null).
async fn admin_get_source(
    headers: HeaderMap,
    Path(source_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app.source_store.get(&source_id) {
        Ok(Some(ds)) => return Json(mask_source_for_response(&ds)).into_response(),
        Ok(None) => {}
        Err(e) => {
            log::error!("admin_get_source '{}': {}", source_id, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    match app.instance_store.get_instance(&source_id) {
        Ok(Some(inst)) => {
            Json(serde_json::json!({
                "id": inst.id,
                "name": capitalize_first(&inst.id),
                "source_kind": "builtin",
                "cached_data": inst.cached_data,
                "last_fetched_at": inst.last_fetched_at,
                "last_error": inst.last_error,
            }))
            .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_get_source '{}': {}", source_id, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `PUT /api/admin/sources/:id` — update a generic source config.
async fn admin_update_source(
    headers: HeaderMap,
    Path(source_id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<DataSourceConfig>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Load existing source to preserve unchanged encrypted headers
    let existing = match app.source_store.get(&source_id) {
        Ok(Some(ds)) => Some(ds),
        Ok(None) => None,
        Err(e) => {
            log::error!("admin_update_source get '{}': {}", source_id, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let existing_enc = existing.as_ref().map(|ds| &ds.encrypted_headers);
    let enc_json = match encrypt_secret_headers(&app.encryption_key, &payload, existing_enc) {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    };

    match app.source_store.update(&source_id, &payload, &enc_json) {
        Ok(Some(ds)) => {
            app.scheduler.stop_source(&source_id);
            let generic = GenericHttpSource::from_data_source(&ds, Some(&app.encryption_key));
            app.scheduler.spawn_source(generic);
            Json(mask_source_for_response(&ds)).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_update_source '{}': {}", source_id, e);
            let status = if matches!(e, crate::source_store::SourceStoreError::Validation(_)) {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(serde_json::json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// `DELETE /api/admin/sources/:id` — delete a generic source + field mappings.
async fn admin_delete_source(
    headers: HeaderMap,
    Path(source_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app.source_store.delete(&source_id) {
        Ok(true) => {
            app.scheduler.stop_source(&source_id);
            if let Ok(fields) = app.layout_store.list_field_mappings(&source_id) {
                for field in fields {
                    app.layout_store.delete_field_mapping(&field.id).ok();
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_delete_source '{}': {}", source_id, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `POST /api/admin/sources/:id/fetch` — trigger an immediate one-shot fetch.
async fn admin_fetch_source(
    headers: HeaderMap,
    Path(source_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let ds = match app.source_store.get(&source_id) {
        Ok(Some(ds)) => ds,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_fetch_source '{}': {}", source_id, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let generic = GenericHttpSource::from_data_source(&ds, Some(&app.encryption_key));
    match SourceScheduler::fetch_once(generic).await {
        Ok(value) => {
            let now_secs = unix_now_secs() as i64;
            app.source_store
                .update_cached_data(&source_id, &value, now_secs)
                .ok();
            Json(serde_json::json!({
                "success": true,
                "data": value,
            }))
            .into_response()
        }
        Err(e) => {
            app.source_store
                .update_last_error(&source_id, &e)
                .ok();
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": e,
                })),
            )
                .into_response()
        }
    }
}

// ─── Field mapping handlers ─────────────────────────────────────────────────

/// `GET /api/admin/sources/:id/fields` — list field mappings for a source.
async fn admin_list_fields(
    headers: HeaderMap,
    Path(source_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app.layout_store.list_field_mappings(&source_id) {
        Ok(fields) => Json(fields).into_response(),
        Err(e) => {
            log::error!("admin_list_fields '{}': {}", source_id, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `POST /api/admin/sources/:id/fields` — create a field mapping.
async fn admin_create_field(
    headers: HeaderMap,
    Path(source_id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<CreateFieldPayload>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let id = format!(
        "fm-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );

    // Determine source_type: "builtin" if it matches a known plugin instance, else "generic".
    let source_type = match app.instance_store.get_instance(&source_id) {
        Ok(Some(_)) => "builtin",
        _ => "generic",
    };

    match app.layout_store.create_field_mapping(&id, &source_id, source_type, &payload.name, &payload.json_path) {
        Ok(fm) => (StatusCode::CREATED, Json(fm)).into_response(),
        Err(e) => {
            log::error!("admin_create_field '{}': {}", source_id, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `PUT /api/admin/fields/:id` — update a field mapping.
async fn admin_update_field(
    headers: HeaderMap,
    Path(field_id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<UpdateFieldPayload>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app.layout_store.update_field_mapping(
        &field_id,
        payload.name.as_deref(),
        payload.json_path.as_deref(),
    ) {
        Ok(Some(fm)) => Json(fm).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_update_field '{}': {}", field_id, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `DELETE /api/admin/fields/:id` — delete a field mapping.
async fn admin_delete_field(
    headers: HeaderMap,
    Path(field_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if let Err(e) = app.layout_store.delete_field_mapping(&field_id) {
        log::error!("admin_delete_field '{}': {}", field_id, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}

/// `GET /api/admin/sources/:id/data` — return cached_data JSON for a source.
///
/// Checks both built-in sources (instance store) and generic sources (source store).
async fn admin_get_source_data(
    headers: HeaderMap,
    Path(source_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Check built-in sources first
    match app.instance_store.get_instance(&source_id) {
        Ok(Some(inst)) => {
            let data = inst.cached_data.unwrap_or(Value::Null);
            return Json(data).into_response();
        }
        Ok(None) => {}
        Err(e) => {
            log::error!("admin_get_source_data '{}': {}", source_id, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    // Check generic sources
    match app.source_store.get(&source_id) {
        Ok(Some(ds)) => {
            let data = ds.cached_data.unwrap_or(Value::Null);
            Json(data).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            log::error!("admin_get_source_data '{}': {}", source_id, e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `POST /api/admin/rotate-encryption-key` — re-encrypt all encrypted headers
/// with a new key derived from the provided `new_api_key`.
///
/// Body: `{"new_api_key": "..."}`
async fn admin_rotate_encryption_key(
    headers: HeaderMap,
    State(app): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    if !is_admin_authorized(&headers, &app.api_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let new_api_key = match payload["new_api_key"].as_str() {
        Some(k) if !k.is_empty() => k,
        _ => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "new_api_key is required"})),
            )
                .into_response();
        }
    };

    let old_key = &app.encryption_key;
    let new_key = EncryptionKey::derive_from_api_key(new_api_key);

    let sources = match app.source_store.list() {
        Ok(s) => s,
        Err(e) => {
            log::error!("rotate_encryption_key list: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut rotated_count = 0usize;
    for ds in &sources {
        if let Some(arr) = ds.encrypted_headers.as_array() {
            if arr.is_empty() {
                continue;
            }
            let encrypted_values: Vec<String> = arr
                .iter()
                .filter_map(|e| e["value"].as_str().map(|s| s.to_string()))
                .collect();
            let keys: Vec<String> = arr
                .iter()
                .filter_map(|e| e["key"].as_str().map(|s| s.to_string()))
                .collect();

            match crypto::rotate_key(old_key, &new_key, &encrypted_values) {
                Ok(new_values) => {
                    let new_enc: Vec<serde_json::Value> = keys
                        .iter()
                        .zip(new_values.iter())
                        .map(|(k, v)| serde_json::json!({"key": k, "value": v}))
                        .collect();
                    let enc_json = serde_json::to_string(&new_enc).unwrap_or_default();
                    if let Err(e) = app.source_store.update_encrypted_headers(&ds.id, &enc_json) {
                        log::error!("rotate_encryption_key update '{}': {}", ds.id, e);
                    } else {
                        rotated_count += 1;
                    }
                }
                Err(e) => {
                    log::error!("rotate_encryption_key decrypt '{}': {}", ds.id, e);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "error": format!("failed to re-encrypt source '{}': {}", ds.id, e)
                        })),
                    )
                        .into_response();
                }
            }
        }
    }

    Json(serde_json::json!({
        "rotated_sources": rotated_count,
        "message": "Re-encryption complete. Update secrets.toml with the new API key and restart."
    }))
    .into_response()
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

fn is_admin_authorized(headers: &HeaderMap, api_key: &str) -> bool {
    // Check x-api-key header first (existing API clients)
    let header_ok = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == api_key)
        .unwrap_or(false);
    if header_ok {
        return true;
    }
    // Fall back to session cookie (browser admin UI)
    is_admin_cookie_valid(headers, api_key)
}

/// Check whether the `cascades_admin_key` cookie matches the API key.
fn is_admin_cookie_valid(headers: &HeaderMap, api_key: &str) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|c| {
                let c = c.trim();
                c.strip_prefix("cascades_admin_key=")
            })
        })
        .map(|v| v == api_key)
        .unwrap_or(false)
}

/// Minimal percent-decoding for form values (+ → space, %XX → byte).
fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let mut bytes = input.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'+' => out.push(b' '),
            b'%' => {
                let hi = bytes.next().unwrap_or(b'0');
                let lo = bytes.next().unwrap_or(b'0');
                let hex = [hi, lo];
                if let Ok(s) = std::str::from_utf8(&hex) {
                    if let Ok(val) = u8::from_str_radix(s, 16) {
                        out.push(val);
                        continue;
                    }
                }
                out.push(b'%');
                out.push(hi);
                out.push(lo);
            }
            _ => out.push(b),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
///
/// `render_mode` is forwarded to the sidecar: `"einkPreview"` for browser
/// endpoints, `"device"` for real e-ink hardware.
async fn compose_display(app: &AppState, config: &DisplayConfiguration, render_mode: &str) -> Option<Vec<u8>> {
    match app.compositor.compose(config, render_mode).await {
        Ok(png) => Some(png),
        Err(e) => {
            log::error!("compositor error for '{}': {}", config.name, e);
            None
        }
    }
}

/// Render a named display config by ID, using cache if available.
async fn render_for_display(app: &AppState, display_id: &str, render_mode: &str) -> Option<Vec<u8>> {
    {
        let cache = app.image_cache.read().unwrap();
        if let Some(png) = cache.get(display_id) {
            return Some(png.clone());
        }
    }

    let layout = app.layout_store.get_layout(display_id).ok().flatten()?;
    let config = DisplayConfiguration::from_layout_config(&layout);
    let png = compose_display(app, &config, render_mode).await?;
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
    use axum::{body::Body, http::Request, routing::get, Router};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Build a minimal AppState for testing the /api/status endpoint.
    ///
    /// Uses a temporary SQLite database seeded with the 5 well-known instances
    /// and an empty templates directory (status doesn't render any templates).
    fn make_test_state() -> Arc<AppState> {
        use crate::config::{Config, DisplayConfig, LocationConfig, SourceIntervals, StorageConfig};
        use crate::layout_store::LayoutStore;
        use crate::source_store::SourceStore;

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let instance_store = Arc::new(InstanceStore::open(&db_path).unwrap());
        let layout_store = Arc::new(LayoutStore::open(&db_path).unwrap());
        let source_store = Arc::new(SourceStore::open(&db_path).unwrap());
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
            Arc::clone(&layout_store),
            "http://localhost:3001".to_string(),
        ));

        let scheduler = Arc::new(SourceScheduler::new(Arc::clone(&source_store)));

        let api_key = "test-key".to_string();
        let encryption_key = EncryptionKey::derive_from_api_key(&api_key);
        Arc::new(AppState {
            compositor,
            instance_store,
            layout_store,
            source_store,
            scheduler,
            image_cache: Arc::new(RwLock::new(HashMap::new())),
            api_key,
            encryption_key,
            refresh_rate_secs: 60,
            started_at: std::time::Instant::now(),
            sidecar_url: "http://localhost:3001".to_string(),
        })
    }

    /// Minimal stateless router for testing the landing page.
    ///
    /// `get_landing` takes no State, so we don't need a full AppState here.
    fn landing_router() -> Router {
        Router::new().route("/", get(get_landing))
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
            assert!(source.get("last_fetched_at").is_some(), "last_fetched_at key must exist");
            assert!(source.get("last_error").is_some(), "last_error key must exist");
            assert!(source.get("data_age_secs").is_some(), "data_age_secs key must exist");
        }
    }

    #[tokio::test]
    async fn get_root_returns_200() {
        let app = landing_router();
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_root_content_type_is_html() {
        let app = landing_router();
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let response = app.oneshot(req).await.unwrap();
        let ct = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("text/html"), "expected text/html, got: {ct}");
    }

    #[tokio::test]
    async fn get_root_body_contains_expected_sections() {
        let app = landing_router();
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let response = app.oneshot(req).await.unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = std::str::from_utf8(&bytes).unwrap();

        assert!(body.contains("Endpoints"), "missing Endpoints section");
        assert!(body.contains("Setup Guide"), "missing Setup Guide section");
        assert!(body.contains("/api/status"), "missing /api/status in endpoint table");
        assert!(body.contains("/image.png"), "missing /image.png in endpoint table");
        assert!(body.contains("Live Status"), "missing Live Status section");
        assert!(body.contains("fetch('/api/status')"), "missing JS status fetch");
    }

    // ── Admin API tests ───────────────────────────────────────────────────────

    /// Returns `(AppState, TempDir)` — caller must keep `TempDir` alive for writes.
    fn make_writable_test_state() -> (Arc<AppState>, tempfile::TempDir) {
        use crate::config::{Config, DisplayConfig, LocationConfig, SourceIntervals, StorageConfig};
        use crate::layout_store::LayoutStore;
        use crate::source_store::SourceStore;

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let instance_store = Arc::new(InstanceStore::open(&db_path).unwrap());
        let layout_store = Arc::new(LayoutStore::open(&db_path).unwrap());
        let source_store = Arc::new(SourceStore::open(&db_path).unwrap());
        let config = Config {
            display: DisplayConfig { width: 800, height: 480 },
            location: LocationConfig { latitude: 48.4, longitude: -122.3, name: "Test".to_string() },
            sources: SourceIntervals {
                weather_interval_secs: 300,
                river_interval_secs: 300,
                ferry_interval_secs: 60,
                trail_interval_secs: 900,
                road_interval_secs: 1800,
                river: None, trail: None, road: None, ferry: None,
            },
            server: None, auth: None, device: None,
            storage: StorageConfig::default(),
        };
        seed_from_config(&instance_store, &config).unwrap();

        let template_engine = Arc::new(TemplateEngine::new(&templates_dir).unwrap());
        let compositor = Arc::new(Compositor::new(
            Arc::clone(&template_engine),
            Arc::clone(&instance_store),
            Arc::clone(&layout_store),
            "http://localhost:3001".to_string(),
        ));

        let scheduler = Arc::new(SourceScheduler::new(Arc::clone(&source_store)));

        let api_key = "test-key".to_string();
        let encryption_key = EncryptionKey::derive_from_api_key(&api_key);
        let state = Arc::new(AppState {
            compositor,
            instance_store,
            layout_store,
            source_store,
            scheduler,
            image_cache: Arc::new(RwLock::new(HashMap::new())),
            api_key,
            encryption_key,
            refresh_rate_secs: 60,
            started_at: std::time::Instant::now(),
            sidecar_url: "http://localhost:3001".to_string(),
        });
        (state, dir)
    }

    fn seed_default_layout(state: &Arc<AppState>) {
        state.layout_store.upsert_layout(&crate::layout_store::LayoutConfig {
            id: "default".to_string(),
            name: "Default".to_string(),
            items: vec![],
            updated_at: 0,
        }).unwrap();
    }

    #[tokio::test]
    async fn admin_get_ui_returns_200_html() {
        let app = build_router(make_test_state());
        let req = Request::builder().uri("/admin").body(Body::empty()).unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ct = response.headers().get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()).unwrap_or("");
        assert!(ct.contains("text/html"), "expected text/html, got: {ct}");
    }

    #[tokio::test]
    async fn admin_list_layouts_requires_auth() {
        let app = build_router(make_test_state());
        let req = Request::builder().uri("/api/admin/layouts").body(Body::empty()).unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_list_layouts_returns_empty_array_when_no_layouts() {
        let state = make_test_state();
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/layouts")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body.is_array(), "expected array");
        assert_eq!(body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn admin_list_layouts_returns_summaries() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/layouts")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = body.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "default");
        assert_eq!(arr[0]["name"], "Default");
        assert!(arr[0]["updated_at"].is_number());
    }

    #[tokio::test]
    async fn admin_get_layout_not_found() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/admin/layout/nonexistent")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_get_layout_returns_full_layout() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/layout/default")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["id"], "default");
        assert_eq!(body["name"], "Default");
        assert!(body["items"].is_array());
    }

    #[tokio::test]
    async fn admin_put_layout_replaces_layout() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let body = serde_json::json!({
            "name": "Updated",
            "items": [
                {
                    "id": "item-1",
                    "item_type": "plugin_slot",
                    "z_index": 0,
                    "x": 0, "y": 0, "width": 800, "height": 480,
                    "plugin_instance_id": "river",
                    "layout_variant": "full"
                }
            ]
        });
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/layout/default")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let resp: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp["name"], "Updated");
        assert_eq!(resp["items"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn admin_put_layout_rejects_unknown_item_type() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let body = serde_json::json!({
            "name": "Bad",
            "items": [
                {
                    "id": "x",
                    "item_type": "not_a_type",
                    "z_index": 0,
                    "x": 0, "y": 0, "width": 100, "height": 100
                }
            ]
        });
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/layout/default")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn admin_list_plugins_returns_instances() {
        let state = make_test_state();
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/plugins")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = body.as_array().unwrap();
        assert!(!arr.is_empty(), "should list seeded instances");
        // Each entry has id, name, supported_variants
        let first = &arr[0];
        assert!(first["id"].is_string());
        assert!(first["name"].is_string());
        assert!(first["supported_variants"].is_array());
        let variants = first["supported_variants"].as_array().unwrap();
        assert!(variants.iter().any(|v| v == "full"), "should include 'full' variant");
    }

    #[tokio::test]
    async fn admin_post_item_adds_to_layout() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let item = serde_json::json!({
            "id": "new-item",
            "item_type": "static_text",
            "z_index": 0,
            "x": 10, "y": 10, "width": 200, "height": 40,
            "text_content": "Hello",
            "font_size": 20
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/layout/default/item")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&item).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let resp: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resp["items"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn admin_post_item_404_for_missing_layout() {
        let app = build_router(make_test_state());
        let item = serde_json::json!({
            "id": "x", "item_type": "static_divider",
            "z_index": 0, "x": 0, "y": 0, "width": 800, "height": 2
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/layout/no-such-layout/item")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&item).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_delete_item_removes_item() {
        let (state, _dir) = make_writable_test_state();
        state.layout_store.upsert_layout(&crate::layout_store::LayoutConfig {
            id: "default".to_string(),
            name: "Default".to_string(),
            items: vec![crate::layout_store::LayoutItem::StaticDivider {
                id: "div-1".to_string(),
                z_index: 0, x: 0, y: 240, width: 800, height: 2,
                orientation: None,
            }],
            updated_at: 0,
        }).unwrap();
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/admin/layout/default/item/div-1")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        // Layout should now have 0 items
        let layout = state.layout_store.get_layout("default").unwrap().unwrap();
        assert_eq!(layout.items.len(), 0);
    }

    #[tokio::test]
    async fn admin_delete_item_404_for_missing_item() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/admin/layout/default/item/no-such-item")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_put_item_replaces_item() {
        let (state, _dir) = make_writable_test_state();
        state.layout_store.upsert_layout(&crate::layout_store::LayoutConfig {
            id: "default".to_string(),
            name: "Default".to_string(),
            items: vec![crate::layout_store::LayoutItem::StaticText {
                id: "txt-1".to_string(),
                z_index: 0, x: 0, y: 0, width: 200, height: 40,
                text_content: "Old".to_string(),
                font_size: 16,
                orientation: None,
            }],
            updated_at: 0,
        }).unwrap();
        let app = build_router(Arc::clone(&state));
        let updated = serde_json::json!({
            "id": "txt-1",
            "item_type": "static_text",
            "z_index": 0, "x": 0, "y": 0, "width": 200, "height": 40,
            "text_content": "New",
            "font_size": 24
        });
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/layout/default/item/txt-1")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&updated).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let layout = state.layout_store.get_layout("default").unwrap().unwrap();
        assert!(matches!(&layout.items[0],
            crate::layout_store::LayoutItem::StaticText { text_content, .. }
            if text_content == "New"
        ));
    }

    #[tokio::test]
    async fn put_layout_roundtrips_font_size() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));

        // PUT a layout with a static_text item with font_size=48
        let body = serde_json::json!({
            "name": "FontSizeTest",
            "items": [
                {
                    "id": "item-1",
                    "item_type": "static_text",
                    "z_index": 0,
                    "x": 0, "y": 0, "width": 800, "height": 480,
                    "text_content": "Test text",
                    "font_size": 48
                }
            ]
        });
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/layout/default")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let resp: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        // Verify the returned layout has font_size=48, not 16 (the unwrap_or default)
        let items = resp["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["font_size"], 48, "font_size should roundtrip as 48, not default to 16");
    }

    #[tokio::test]
    async fn admin_put_layout_returns_200_with_valid_key() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let body = serde_json::json!({
            "name": "Updated",
            "items": [
                {
                    "id": "item-1",
                    "item_type": "plugin_slot",
                    "z_index": 0,
                    "x": 0, "y": 0, "width": 800, "height": 480,
                    "plugin_instance_id": "river",
                    "layout_variant": "full"
                }
            ]
        });
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/layout/default")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "PUT with valid x-api-key should return 200");
    }

    #[tokio::test]
    async fn admin_put_layout_returns_401_without_key() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let body = serde_json::json!({
            "name": "Updated",
            "items": []
        });
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/layout/default")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "PUT without x-api-key should return 401");
    }

    #[tokio::test]
    async fn admin_post_preview_returns_png_content_type() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/preview/default")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "POST preview with valid key should return 200");

        let content_type = response.headers().get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(content_type, "image/png", "preview response should have content-type: image/png");
    }

    #[tokio::test]
    async fn admin_list_plugins_requires_auth() {
        let state = make_test_state();
        let app = build_router(Arc::clone(&state));

        // Without key
        let req = Request::builder()
            .uri("/api/admin/plugins")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "GET /api/admin/plugins without key should return 401");
    }

    #[tokio::test]
    async fn admin_list_plugins_returns_200_with_valid_key() {
        let state = make_test_state();
        let app = build_router(Arc::clone(&state));

        // With valid key
        let req = Request::builder()
            .uri("/api/admin/plugins")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "GET /api/admin/plugins with valid key should return 200");

        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = body.as_array().unwrap();
        assert!(!arr.is_empty(), "should list seeded instances");

        // Verify response structure matches what frontend expects
        let first = &arr[0];
        assert!(first["id"].is_string(), "each item should have id field");
        assert!(first["name"].is_string(), "each item should have name field");
        assert!(first["supported_variants"].is_array(), "each item should have supported_variants field");
    }

    // ── Admin auth gate tests (cs-0os) ───────────────────────────────────────

    #[tokio::test]
    async fn admin_page_without_auth_serves_login_page() {
        let app = build_router(make_test_state());
        let req = Request::builder().uri("/admin").body(Body::empty()).unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let ct = response.headers().get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()).unwrap_or("");
        assert!(ct.contains("text/html"), "expected text/html, got: {ct}");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = std::str::from_utf8(&bytes).unwrap();
        // Login page should contain a form for entering the API key
        assert!(
            body.contains("<form") || body.contains("<input"),
            "unauthenticated GET /admin should show a login form"
        );
        // Should NOT contain the full admin UI (apiFetch is only in the admin app)
        assert!(
            !body.contains("apiFetch"),
            "unauthenticated GET /admin should not expose the full admin UI"
        );
    }

    #[tokio::test]
    async fn admin_login_wrong_key_returns_401() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("api_key=wrong-key"))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "POST /admin/login with wrong key should return 401"
        );
    }

    #[tokio::test]
    async fn admin_login_correct_key_sets_cookie_and_redirects() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("api_key=test-key"))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert!(
            response.status() == StatusCode::SEE_OTHER || response.status() == StatusCode::FOUND,
            "POST /admin/login with correct key should redirect, got {}",
            response.status()
        );
        let set_cookie = response
            .headers()
            .get("set-cookie")
            .expect("login response should set a session cookie");
        let cookie_str = set_cookie.to_str().unwrap();
        assert!(!cookie_str.is_empty(), "set-cookie header should not be empty");
    }

    #[tokio::test]
    async fn admin_page_with_session_cookie_returns_full_ui() {
        // Step 1: log in to obtain the session cookie
        let state = make_test_state();
        let app = build_router(Arc::clone(&state));
        let login_req = Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("api_key=test-key"))
            .unwrap();
        let login_resp = app.oneshot(login_req).await.unwrap();
        let set_cookie = login_resp
            .headers()
            .get("set-cookie")
            .expect("login should set session cookie")
            .to_str()
            .unwrap();
        // Extract the cookie name=value portion (before any attributes like Path, HttpOnly)
        let cookie_nv = set_cookie.split(';').next().unwrap();

        // Step 2: GET /admin with the session cookie
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/admin")
            .header("cookie", cookie_nv)
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = std::str::from_utf8(&bytes).unwrap();
        // Authenticated view should contain the full admin UI
        assert!(
            body.contains("apiFetch") || body.contains("id=\"canvas\""),
            "authenticated GET /admin should return the full admin UI"
        );
    }

    #[tokio::test]
    async fn admin_api_accepts_api_key_header_after_auth_gate() {
        // API endpoints must still accept x-api-key header (backwards compat)
        let app = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/admin/layouts")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "API endpoints should still accept x-api-key header"
        );
    }

    #[tokio::test]
    async fn admin_logout_clears_session_cookie() {
        // Step 1: log in to get a session cookie
        let state = make_test_state();
        let app = build_router(Arc::clone(&state));
        let login_req = Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("api_key=test-key"))
            .unwrap();
        let login_resp = app.oneshot(login_req).await.unwrap();
        let set_cookie = login_resp
            .headers()
            .get("set-cookie")
            .expect("login should set session cookie")
            .to_str()
            .unwrap();
        let cookie_nv = set_cookie.split(';').next().unwrap();

        // Step 2: GET /admin/logout with the session cookie
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/admin/logout")
            .header("cookie", cookie_nv)
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();

        // Should clear the cookie by setting Max-Age=0 or an expired date
        let clear_cookie = response
            .headers()
            .get("set-cookie")
            .expect("logout should set a cookie header to clear the session");
        let clear_str = clear_cookie.to_str().unwrap();
        assert!(
            clear_str.contains("Max-Age=0")
                || clear_str.contains("max-age=0")
                || clear_str.contains("expires=Thu, 01 Jan 1970"),
            "logout should expire the session cookie, got: {clear_str}"
        );
    }

    // ── Active layout API tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn admin_get_active_layout_returns_null_when_unset() {
        let (state, _dir) = make_writable_test_state();
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/active-layout")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["layout_id"].is_null());
    }

    #[tokio::test]
    async fn admin_set_active_layout_succeeds() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/active-layout")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"layout_id":"default"}"#))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["layout_id"], "default");
    }

    #[tokio::test]
    async fn admin_set_active_layout_rejects_missing_layout() {
        let (state, _dir) = make_writable_test_state();
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/active-layout")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"layout_id":"nonexistent"}"#))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_set_active_layout_requires_auth() {
        let (state, _dir) = make_writable_test_state();
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/active-layout")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"layout_id":"default"}"#))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_get_active_layout_reflects_set() {
        let (state, _dir) = make_writable_test_state();
        seed_default_layout(&state);

        // Set active layout
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/active-layout")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"layout_id":"default"}"#))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Get active layout
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/active-layout")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["layout_id"], "default");
    }

    // ── Field mapping CRUD tests ────────────────────────────────────────────

    #[tokio::test]
    async fn admin_list_fields_requires_auth() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/admin/sources/weather/fields")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_list_fields_returns_empty_for_new_source() {
        let (state, _dir) = make_writable_test_state();
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/sources/weather/fields")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body.is_array());
        assert_eq!(body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn admin_create_field_returns_201() {
        let (state, _dir) = make_writable_test_state();
        let app = build_router(Arc::clone(&state));
        let payload = serde_json::json!({
            "name": "Water Level",
            "json_path": "$.water_level_ft"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/sources/river/fields")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body["id"].is_string());
        assert_eq!(body["name"], "Water Level");
        assert_eq!(body["json_path"], "$.water_level_ft");
        assert_eq!(body["data_source_id"], "river");
    }

    #[tokio::test]
    async fn admin_create_field_requires_auth() {
        let app = build_router(make_test_state());
        let payload = serde_json::json!({
            "name": "Temp",
            "json_path": "$.temp"
        });
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/sources/weather/fields")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_create_and_list_fields() {
        let (state, _dir) = make_writable_test_state();

        // Create two fields for river
        let app = build_router(Arc::clone(&state));
        let payload = serde_json::json!({ "name": "Level", "json_path": "$.level" });
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/sources/river/fields")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        app.oneshot(req).await.unwrap();

        // Need a small delay so the millis-based ID is different
        let app = build_router(Arc::clone(&state));
        let payload = serde_json::json!({ "name": "Flow", "json_path": "$.flow" });
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/sources/river/fields")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        app.oneshot(req).await.unwrap();

        // List — should have 2
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/sources/river/fields")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn admin_update_field_changes_name() {
        let (state, _dir) = make_writable_test_state();

        // Create a field
        state.layout_store.create_field_mapping(
            "fm-test-1", "river", "builtin", "Old Name", "$.level",
        ).unwrap();

        // Update it
        let app = build_router(Arc::clone(&state));
        let payload = serde_json::json!({ "name": "New Name" });
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/fields/fm-test-1")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["name"], "New Name");
        assert_eq!(body["json_path"], "$.level"); // unchanged
    }

    #[tokio::test]
    async fn admin_update_field_404_for_missing() {
        let (state, _dir) = make_writable_test_state();
        let app = build_router(Arc::clone(&state));
        let payload = serde_json::json!({ "name": "Nope" });
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/fields/nonexistent")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_update_field_requires_auth() {
        let app = build_router(make_test_state());
        let payload = serde_json::json!({ "name": "Nope" });
        let req = Request::builder()
            .method("PUT")
            .uri("/api/admin/fields/fm-1")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_delete_field_returns_204() {
        let (state, _dir) = make_writable_test_state();
        state.layout_store.create_field_mapping(
            "fm-del-1", "river", "builtin", "ToDelete", "$.x",
        ).unwrap();

        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/admin/fields/fm-del-1")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify deleted
        assert!(state.layout_store.get_field_mapping("fm-del-1").unwrap().is_none());
    }

    #[tokio::test]
    async fn admin_delete_field_requires_auth() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .method("DELETE")
            .uri("/api/admin/fields/fm-1")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_get_source_data_returns_cached_json() {
        let (state, _dir) = make_writable_test_state();

        // Set some cached data
        let data = serde_json::json!({ "water_level_ft": 8.5, "flow_cfs": 1200 });
        state.instance_store.update_cached_data("river", &data, 1_700_000_000).unwrap();

        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/sources/river/data")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["water_level_ft"], 8.5);
    }

    #[tokio::test]
    async fn admin_get_source_data_returns_null_when_no_data() {
        let (state, _dir) = make_writable_test_state();
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/sources/weather/data")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body.is_null());
    }

    #[tokio::test]
    async fn admin_get_source_data_404_for_missing_source() {
        let (state, _dir) = make_writable_test_state();
        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri("/api/admin/sources/nonexistent/data")
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_get_source_data_requires_auth() {
        let app = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/admin/sources/weather/data")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // ─── Encrypted headers tests ────────────────────────────────────────────

    #[tokio::test]
    async fn create_source_with_secret_headers_masks_in_response() {
        let (state, _dir) = make_state_with_full_config();
        seed_default_layout(&state);
        let app = build_router(Arc::clone(&state));

        let payload = serde_json::json!({
            "name": "Secret API",
            "url": "https://api.example.com/v1",
            "method": "GET",
            "headers": {"Content-Type": "application/json"},
            "secret_headers": [
                {"key": "Authorization", "value": "Bearer sk-secret-123"},
                {"key": "X-Api-Token", "value": "tok_abc"}
            ],
            "refresh_interval_secs": 60
        });

        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/sources")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Plaintext headers visible
        assert_eq!(json["headers"]["Content-Type"], "application/json");

        // Secret headers keys visible but values masked (null)
        let enc = json["encrypted_headers"].as_array().unwrap();
        assert_eq!(enc.len(), 2);
        assert_eq!(enc[0]["key"], "Authorization");
        assert!(enc[0]["value"].is_null(), "encrypted value should be masked");
        assert_eq!(enc[1]["key"], "X-Api-Token");
        assert!(enc[1]["value"].is_null(), "encrypted value should be masked");
    }

    #[tokio::test]
    async fn get_source_masks_encrypted_headers() {
        let (state, _dir) = make_state_with_full_config();
        seed_default_layout(&state);

        // Create a source with secret headers
        let config = crate::source_store::DataSourceConfig {
            name: "Test".to_string(),
            url: "https://example.com".to_string(),
            method: "GET".to_string(),
            headers: serde_json::json!({}),
            secret_headers: Some(vec![crate::source_store::SecretHeaderInput {
                key: "Authorization".to_string(),
                value: Some("Bearer secret".to_string()),
            }]),
            body_template: None,
            response_root_path: None,
            refresh_interval_secs: 60,
        };
        let enc_json = encrypt_secret_headers(&state.encryption_key, &config, None).unwrap();
        let ds = state.source_store.create(&config, &enc_json).unwrap();

        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .uri(format!("/api/admin/sources/{}", ds.id))
            .header("x-api-key", "test-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let enc = json["encrypted_headers"].as_array().unwrap();
        assert_eq!(enc.len(), 1);
        assert_eq!(enc[0]["key"], "Authorization");
        assert!(enc[0]["value"].is_null(), "value should be masked in GET response");
    }

    #[tokio::test]
    async fn secret_headers_decrypted_for_source_construction() {
        let key = EncryptionKey::derive_from_api_key("test-api-key-for-crypto");
        let encrypted = crate::crypto::encrypt(&key, "Bearer sk-secret").unwrap();

        let ds = crate::source_store::DataSource {
            id: "src-test".to_string(),
            name: "Test".to_string(),
            url: "https://httpbin.org/get".to_string(),
            method: "GET".to_string(),
            headers: serde_json::json!({"X-Public": "visible"}),
            encrypted_headers: serde_json::json!([
                {"key": "Authorization", "value": encrypted}
            ]),
            body_template: None,
            response_root_path: None,
            refresh_interval_secs: 60,
            cached_data: None,
            last_fetched_at: None,
            last_error: None,
            created_at: 0,
            updated_at: 0,
        };

        let generic = GenericHttpSource::from_data_source(&ds, Some(&key));
        // The source should have 2 headers: X-Public + decrypted Authorization
        // We can't directly inspect headers but we can verify it doesn't panic
        assert_eq!(generic.id(), "src-test");
        assert_eq!(generic.name(), "Test");
    }

    #[tokio::test]
    async fn update_source_preserves_unchanged_secret_headers() {
        let (state, _dir) = make_state_with_full_config();
        seed_default_layout(&state);

        // Create source with a secret header
        let create_payload = serde_json::json!({
            "name": "Preserve Test",
            "url": "https://example.com",
            "headers": {},
            "secret_headers": [{"key": "Secret-Key", "value": "original-value"}],
            "refresh_interval_secs": 60
        });

        let app = build_router(Arc::clone(&state));
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/sources")
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&create_payload).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let source_id = created["id"].as_str().unwrap().to_string();

        // Update with null value (should preserve the encrypted value)
        let update_payload = serde_json::json!({
            "name": "Preserve Test Updated",
            "url": "https://example.com",
            "headers": {},
            "secret_headers": [{"key": "Secret-Key", "value": null}],
            "refresh_interval_secs": 60
        });

        let app2 = build_router(Arc::clone(&state));
        let req2 = Request::builder()
            .method("PUT")
            .uri(format!("/api/admin/sources/{}", source_id))
            .header("x-api-key", "test-key")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&update_payload).unwrap()))
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);

        // Verify the encrypted header still exists in the DB
        let ds = state.source_store.get(&source_id).unwrap().unwrap();
        let enc_arr = ds.encrypted_headers.as_array().unwrap();
        assert_eq!(enc_arr.len(), 1);
        assert_eq!(enc_arr[0]["key"], "Secret-Key");
        // The stored value should be a non-null encrypted string
        assert!(enc_arr[0]["value"].is_string(), "encrypted value should be preserved");

        // And it should still decrypt correctly
        let encrypted_val = enc_arr[0]["value"].as_str().unwrap();
        let decrypted = crate::crypto::decrypt(&state.encryption_key, encrypted_val).unwrap();
        assert_eq!(decrypted, "original-value");
    }
}
