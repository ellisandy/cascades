use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use cascades::{
    build_sources,
    config::{load_config, load_destinations, Destination},
    domain::DomainState,
    presentation::build_display_layout,
    render::render_display,
};
use std::{
    path::Path,
    sync::{Arc, RwLock},
};
use tokio::net::TcpListener;

struct AppState {
    domain: Arc<RwLock<DomainState>>,
    destinations: Vec<Destination>,
}

async fn serve_image(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let domain = app.domain.read().unwrap().clone();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let layout = build_display_layout(&domain, &app.destinations, now_secs);
    let buf = render_display(&layout);
    let png = buf.to_png();
    ([(header::CONTENT_TYPE, "image/png")], png)
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let config = load_config(Path::new("config.toml")).expect("failed to load config.toml");
    let destinations: Vec<Destination> = load_destinations(Path::new("destinations.toml"))
        .map(|d| d.destinations)
        .unwrap_or_default();

    let domain = Arc::new(RwLock::new(DomainState::default()));

    // Spawn one background task per data source. Each task fetches on its own
    // interval and applies the result to the shared DomainState.
    for source in build_sources(&config, false) {
        let domain = Arc::clone(&domain);
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
                    Ok(point) => domain.write().unwrap().apply(point),
                    Err(e) => log::warn!("source fetch failed: {}", e),
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    let port = config.server.as_ref().map(|s| s.port).unwrap_or(8080);
    let app_state = Arc::new(AppState { domain, destinations });

    let app = Router::new()
        .route("/image.png", get(serve_image))
        .with_state(app_state);

    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await.expect("failed to bind");
    println!("Listening on http://{}", addr);
    axum::serve(listener, app).await.expect("server error");
}
