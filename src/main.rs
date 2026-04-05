use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use cascades::{
    build_sources,
    compositor::{Compositor, DisplayConfiguration},
    config::{load_config, load_display_layouts},
    domain::DomainState,
    instance_store::{seed_from_config, InstanceStore},
    template::TemplateEngine,
};
use std::{
    path::Path,
    sync::{Arc, RwLock},
};
use tokio::net::TcpListener;

struct AppState {
    compositor: Arc<Compositor>,
    active_display: DisplayConfiguration,
}

async fn serve_image(State(app): State<Arc<AppState>>) -> Response {
    match app.compositor.compose(&app.active_display).await {
        Ok(png) => ([(header::CONTENT_TYPE, "image/png")], png).into_response(),
        Err(e) => {
            log::error!("compositor error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let config = load_config(Path::new("config.toml")).expect("failed to load config.toml");

    let display_layouts =
        load_display_layouts(Path::new("config/display.toml")).unwrap_or_else(|e| {
            log::warn!("failed to load config/display.toml: {e}");
            Default::default()
        });

    let fixture_mode = std::env::var("SKAGIT_FIXTURE_DATA").as_deref() == Ok("1");
    if fixture_mode {
        println!("Fixture mode enabled: sources return canned data (no live API calls)");
    }

    // Open instance store and seed the 5 well-known plugin instances.
    let store_path = Path::new(&config.storage.db_path);
    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let instance_store = Arc::new(
        InstanceStore::open(store_path).expect("failed to open instance store"),
    );
    seed_from_config(&instance_store, &config).expect("failed to seed instance store");

    // Template engine — load from templates/ directory.
    let template_engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("failed to load templates"),
    );

    // Pick the active display config (default to "default", fall back to first).
    let active_entry = display_layouts
        .displays
        .iter()
        .find(|d| d.name == "default")
        .or_else(|| display_layouts.displays.first());
    let active_display = active_entry
        .and_then(|e| DisplayConfiguration::from_config(e).ok())
        .unwrap_or_else(|| {
            use cascades::compositor::{LayoutSlot, LayoutVariant};
            DisplayConfiguration {
                name: "default".to_string(),
                slots: vec![LayoutSlot {
                    plugin_instance_id: "river".to_string(),
                    x: 0,
                    y: 0,
                    width: 800,
                    height: 480,
                    layout_variant: LayoutVariant::Full,
                }],
            }
        });

    let sidecar_url = std::env::var("SIDECAR_URL")
        .unwrap_or_else(|_| "http://localhost:3001".to_string());

    let compositor = Arc::new(Compositor::new(
        Arc::clone(&template_engine),
        Arc::clone(&instance_store),
        sidecar_url,
    ));

    let domain = Arc::new(RwLock::new(DomainState::default()));

    // Spawn one background task per data source. Each task fetches on its own
    // interval, updates DomainState, and mirrors the result to InstanceStore.
    for source in build_sources(&config, fixture_mode) {
        let domain = Arc::clone(&domain);
        let store = Arc::clone(&instance_store);
        let interval = source.refresh_interval();
        tokio::spawn(async move {
            let mut source = source;
            loop {
                let (s, result) = tokio::task::spawn_blocking(move || {
                    let r = source.fetch();
                    (source, r)
                })
                .await
                .expect("source task panicked");
                source = s;
                match result {
                    Ok(value) => {
                        domain.write().unwrap().apply_raw(source.id(), value.clone());
                        let now_secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        store.update_cached_data(source.id(), &value, now_secs).ok();
                    }
                    Err(e) => {
                        log::warn!("source '{}' fetch failed: {}", source.name(), e);
                        store.update_last_error(source.id(), &e.to_string()).ok();
                    }
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    let port = config.server.as_ref().map(|s| s.port).unwrap_or(8080);
    let app_state = Arc::new(AppState {
        compositor,
        active_display,
    });

    let app = Router::new()
        .route("/image.png", get(serve_image))
        .with_state(app_state);

    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await.expect("failed to bind");
    println!("Listening on http://{}", addr);
    axum::serve(listener, app).await.expect("server error");
}
