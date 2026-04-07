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
//! - `POST /api/admin/layout/{id}/item`                 — add item to layout
//! - `PUT  /api/admin/layout/{id}/item/{item_id}`       — update single item
//! - `DELETE /api/admin/layout/{id}/item/{item_id}`     — remove item

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
    instance_store::InstanceStore,
    layout_store::{LayoutConfig, LayoutItem, LayoutStore},
};

// ─── Shared state ─────────────────────────────────────────────────────────────

/// Shared application state, held in an `Arc` and injected into every handler.
pub struct AppState {
    pub compositor: Arc<Compositor>,
    pub instance_store: Arc<InstanceStore>,
    /// SQLite-backed store for display layout configurations.
    /// Replaces the startup-time `display_configs` HashMap; layouts are now
    /// mutable at runtime via the Admin UI.
    pub layout_store: Arc<LayoutStore>,
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
        .route("/api/admin/preview/{id}", post(admin_post_preview))
        .route("/api/admin/plugins", get(admin_list_plugins))
        .route("/api/admin/layout/{id}/item", post(admin_post_item))
        .route("/api/admin/layout/{id}/item/{item_id}", put(admin_put_item))
        .route("/api/admin/layout/{id}/item/{item_id}", delete(admin_delete_item))
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
    match compose_display(&app, &cfg).await {
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
            "static_divider" => Ok(LayoutItem::StaticDivider {
                id: self.id,
                z_index: self.z_index,
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
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
    match compose_display(&app, &cfg).await {
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

    let layout = app.layout_store.get_layout(display_id).ok().flatten()?;
    let config = DisplayConfiguration::from_layout_config(&layout);
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

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let instance_store = Arc::new(InstanceStore::open(&db_path).unwrap());
        let layout_store = Arc::new(LayoutStore::open(&db_path).unwrap());
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
            layout_store,
            image_cache: Arc::new(RwLock::new(HashMap::new())),
            api_key: "test-key".to_string(),
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

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let templates_dir = dir.path().join("templates");
        std::fs::create_dir_all(&templates_dir).unwrap();

        let instance_store = Arc::new(InstanceStore::open(&db_path).unwrap());
        let layout_store = Arc::new(LayoutStore::open(&db_path).unwrap());
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
            "http://localhost:3001".to_string(),
        ));

        let state = Arc::new(AppState {
            compositor,
            instance_store,
            layout_store,
            image_cache: Arc::new(RwLock::new(HashMap::new())),
            api_key: "test-key".to_string(),
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
}
