use cascades::{
    api::{AppState, build_router},
    build_sources,
    compositor::{Compositor, DisplayConfiguration},
    config::{load_config, load_display_layouts, load_or_create_secrets},
    domain::DomainState,
    instance_store::{seed_from_config, InstanceStore},
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

    // Template engine — load from templates/ directory.
    let template_engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("failed to load templates"),
    );

    // Build display configurations from TOML.
    let mut display_configs: HashMap<String, DisplayConfiguration> = HashMap::new();
    for entry in &display_layouts.displays {
        match DisplayConfiguration::from_config(entry) {
            Ok(cfg) => {
                display_configs.insert(cfg.name.clone(), cfg);
            }
            Err(e) => log::warn!("skipping display '{}': {}", entry.name, e),
        }
    }
    // Ensure "default" is always present.
    if !display_configs.contains_key("default") {
        use cascades::compositor::{LayoutSlot, LayoutVariant};
        display_configs.insert(
            "default".to_string(),
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
            },
        );
    }

    let sidecar_url = std::env::var("SIDECAR_URL")
        .unwrap_or_else(|_| "http://localhost:3001".to_string());

    let compositor = Arc::new(Compositor::new(
        Arc::clone(&template_engine),
        Arc::clone(&instance_store),
        sidecar_url,
    ));

    let refresh_rate_secs = config
        .server
        .as_ref()
        .map(|s| s.refresh_rate_secs)
        .unwrap_or(60);

    // Spawn one background task per data source.
    // Tasks update instance_store.cached_data so the compositor uses fresh data.
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

    let port = config.server.as_ref().map(|s| s.port).unwrap_or(8080);
    let app_state = Arc::new(AppState {
        compositor,
        instance_store,
        display_configs,
        image_cache: Arc::new(RwLock::new(HashMap::new())),
        api_key: secrets.api_key,
        refresh_rate_secs,
    });

    let app = build_router(app_state);

    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await.expect("failed to bind");
    println!("Listening on http://{}", addr);
    axum::serve(listener, app).await.expect("server error");
}
