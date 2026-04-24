use cascades::{
    api::{AppState, SourceScheduler, build_router},
    asset_store::AssetStore,
    build_sources,
    config::{load_config, load_display_layouts, load_or_create_secrets},
    compositor::Compositor,
    domain::DomainState,
    instance_store::{seed_from_config, InstanceStore},
    layout_store::LayoutStore,
    plugin_registry::{self, PluginRegistry},
    source_store::SourceStore,
    sources::generic::GenericHttpSource,
    template::TemplateEngine,
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, RwLock},
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    env_logger::init();

    let config = load_config(Path::new("config.toml")).expect("failed to load config.toml");

    let display_layouts =
        load_display_layouts(Path::new("config/display.toml")).unwrap_or_else(|e| {
            log::warn!("failed to load config/display.toml: {e}");
            Default::default()
        });

    let secrets = load_or_create_secrets(Path::new("config/secrets.toml"));

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

    // Open layout store (same SQLite file) and seed from display.toml if empty.
    let layout_store = Arc::new(
        LayoutStore::open(store_path).expect("failed to open layout store"),
    );
    layout_store
        .seed_from_toml(&display_layouts)
        .expect("failed to seed layout store from display.toml");

    // Open source store for generic HTTP data sources.
    let source_store = Arc::new(
        SourceStore::open(store_path).expect("failed to open source store"),
    );

    // Open asset store for user-uploaded image assets (Phase 6).
    let asset_store = Arc::new(
        AssetStore::open(store_path).expect("failed to open asset store"),
    );

    // Plugin registry — load definitions from config/plugins.d/.
    let plugin_registry = plugin_registry::load_registry(Path::new("config"))
        .unwrap_or_else(|e| {
            log::warn!("failed to load plugin registry: {e}");
            PluginRegistry::new()
        });
    // Bootstrap field mappings for plugins that declare [[default_elements]].
    bootstrap_default_field_mappings(&plugin_registry, &layout_store);

    // Template engine — load from templates/ directory.
    let template_engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("failed to load templates"),
    );

    let sidecar_url = std::env::var("SIDECAR_URL")
        .unwrap_or_else(|_| "http://localhost:3001".to_string());

    // Curated-font manifest — served to both the sidecar's @font-face builder
    // and the admin UI font picker from ./fonts/ relative to the server's CWD.
    let fonts_manifest = Arc::new(
        cascades::fonts::FontsManifest::load_from(Path::new("fonts/fonts.json"))
            .expect("failed to load fonts/fonts.json — is the repo root the CWD?"),
    );

    // URL the sidecar's headless Chromium uses to fetch font files. The
    // sidecar runs on the same host, so localhost:<server-port> is the
    // canonical loopback.
    let server_port = config.server.as_ref().map(|s| s.port).unwrap_or(8080);
    let font_base_url = format!("http://localhost:{server_port}");

    let compositor = Arc::new(Compositor::new(
        Arc::clone(&template_engine),
        Arc::clone(&instance_store),
        Arc::clone(&layout_store),
        sidecar_url.clone(),
        Arc::clone(&fonts_manifest),
        font_base_url,
    ));

    let refresh_rate_secs = config
        .server
        .as_ref()
        .map(|s| s.refresh_rate_secs)
        .unwrap_or(60);

    // Spawn one background task per built-in data source.
    let domain = Arc::new(RwLock::new(DomainState::default()));
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

    // Create the source scheduler for dynamically managing generic HTTP sources.
    let scheduler = Arc::new(SourceScheduler::new(Arc::clone(&source_store)));

    // Spawn generic HTTP sources from database.
    if let Ok(sources) = source_store.list() {
        for ds in &sources {
            let generic = GenericHttpSource::from_data_source(ds);
            scheduler.spawn_source(generic);
        }
        if !sources.is_empty() {
            log::info!("spawned {} generic HTTP source(s)", sources.len());
        }
    }

    let port = config.server.as_ref().map(|s| s.port).unwrap_or(8080);
    let app_state = Arc::new(AppState {
        compositor,
        instance_store,
        layout_store,
        source_store,
        asset_store,
        scheduler,
        image_cache: Arc::new(RwLock::new(HashMap::new())),
        plugin_registry,
        api_key: secrets.api_key,
        refresh_rate_secs,
        started_at: std::time::Instant::now(),
        sidecar_url,
    });

    let app = build_router(app_state);

    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await.expect("failed to bind");
    println!("Listening on http://{}", addr);
    axum::serve(listener, app).await.expect("server error");
}

/// Upsert `data_source_fields` rows for every `data_field` entry declared by a
/// plugin's `[[default_elements]]`. Keys on `(data_source_id, json_path)` so
/// repeated boots are idempotent. `data_source_id` is the plugin id (which
/// matches the seeded plugin-instance id for built-ins).
fn bootstrap_default_field_mappings(registry: &PluginRegistry, layout_store: &LayoutStore) {
    for def in registry.all() {
        for el in &def.default_elements {
            if el.kind != "data_field" {
                continue;
            }
            let Some(path) = el.field_path.as_deref() else {
                continue;
            };
            let name = el.label.clone().unwrap_or_else(|| path.to_string());
            let sanitized: String = path
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let new_id = format!("fm-{}-{}", def.id, sanitized);
            if let Err(e) = layout_store.upsert_field_mapping_by_path(
                &new_id,
                &def.id,
                "builtin",
                &name,
                path,
            ) {
                log::warn!(
                    "plugin_registry bootstrap: upsert failed for '{}' {}: {}",
                    def.id,
                    path,
                    e
                );
            }
        }
    }
}
