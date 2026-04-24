//! Phase 6 — asset pipeline + LayoutItem::Image tests.
//!
//! Each test maps to a red-green step in the Phase 6 implementation
//! (see `docs/plugin-customization-design.md`).

use axum::body::Body;
use axum::http::Request;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tower::ServiceExt;

/// Minimal valid 1×1 PNG (67 bytes). Self-contained so tests don't touch
/// the filesystem.
fn one_pixel_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89,
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54,
        0x78, 0x9C, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01,
        0x0D, 0x0A, 0x2D, 0xB4,
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

fn make_test_router(base_dir: &Path) -> axum::Router {
    use cascades::{
        api::{AppState, build_router},
        compositor::Compositor,
        instance_store::InstanceStore,
        layout_store::{LayoutConfig, LayoutStore},
        template::TemplateEngine,
    };
    use std::collections::HashMap;

    let db_path = base_dir.join("test.db");
    let templates_dir = base_dir.join("templates");
    std::fs::create_dir_all(&templates_dir).unwrap();

    let instance_store = Arc::new(InstanceStore::open(&db_path).unwrap());
    let layout_store = Arc::new(LayoutStore::open(&db_path).unwrap());
    let template_engine = Arc::new(TemplateEngine::new(&templates_dir).unwrap());
    let compositor = Arc::new(Compositor::new(
        Arc::clone(&template_engine),
        Arc::clone(&instance_store),
        Arc::clone(&layout_store),
        "http://localhost:9999",
        Arc::new(cascades::fonts::FontsManifest::empty()),
        "http://localhost:0".to_string(),
    ));
    layout_store
        .upsert_layout(&LayoutConfig {
            id: "default".to_string(),
            name: "default".to_string(),
            items: vec![],
            updated_at: 0,
        })
        .unwrap();
    let source_store =
        Arc::new(cascades::source_store::SourceStore::open(&db_path).unwrap());
    let asset_store =
        Arc::new(cascades::asset_store::AssetStore::open(&db_path).unwrap());
    let scheduler = Arc::new(cascades::api::SourceScheduler::new(Arc::clone(&source_store)));

    let state = Arc::new(AppState {
        compositor,
        instance_store,
        layout_store,
        source_store,
        asset_store,
        scheduler,
        image_cache: Arc::new(RwLock::new(HashMap::<String, Vec<u8>>::new())),
        plugin_registry: cascades::plugin_registry::PluginRegistry::new(),
        api_key: "test-bearer-key".to_string(),
        refresh_rate_secs: 42,
        started_at: std::time::Instant::now(),
        sidecar_url: "http://localhost:3001".to_string(),
    });
    build_router(state)
}

/// Build a multipart/form-data body containing a single `file` field. The
/// admin upload route only cares about that field name; everything else is
/// boilerplate to satisfy the multer parser. Inlined so tests don't depend
/// on a multipart-builder crate.
fn multipart_body(filename: &str, content_type: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = "----CASCADESBOUNDARY";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n",
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let header = format!("multipart/form-data; boundary={boundary}");
    (header, body)
}

#[tokio::test]
async fn upload_route_accepts_png_and_returns_id() {
    let dir = tempfile::TempDir::new().unwrap();
    let app = make_test_router(dir.path());
    let png = one_pixel_png();
    let (ct, body) = multipart_body("logo.png", "image/png", &png);

    let req = Request::builder()
        .method("POST")
        .uri("/api/admin/assets")
        .header("x-api-key", "test-bearer-key")
        .header("content-type", ct)
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["id"].as_str().unwrap().starts_with("asset-"));
    assert_eq!(json["mime"], "image/png");
    assert_eq!(json["size"], png.len() as u64);
}

#[tokio::test]
async fn upload_route_requires_admin_auth() {
    let dir = tempfile::TempDir::new().unwrap();
    let app = make_test_router(dir.path());
    let (ct, body) = multipart_body("x.png", "image/png", &one_pixel_png());
    // No x-api-key header.
    let req = Request::builder()
        .method("POST")
        .uri("/api/admin/assets")
        .header("content-type", ct)
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn upload_route_rejects_non_image_bytes_with_415() {
    let dir = tempfile::TempDir::new().unwrap();
    let app = make_test_router(dir.path());
    // The upload claims image/png in its multipart Content-Type, but the
    // bytes are plainly not a PNG. Server-side MIME sniff must reject.
    let bytes = b"not actually a png at all";
    let (ct, body) = multipart_body("fake.png", "image/png", bytes);
    let req = Request::builder()
        .method("POST")
        .uri("/api/admin/assets")
        .header("x-api-key", "test-bearer-key")
        .header("content-type", ct)
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 415);
}

#[tokio::test]
async fn upload_route_dedupes_identical_bytes() {
    let dir = tempfile::TempDir::new().unwrap();
    let app = make_test_router(dir.path());
    let png = one_pixel_png();

    async fn upload_once(app: &axum::Router, png: &[u8], filename: &str) -> String {
        let (ct, body) = multipart_body(filename, "image/png", png);
        let req = Request::builder()
            .method("POST")
            .uri("/api/admin/assets")
            .header("x-api-key", "test-bearer-key")
            .header("content-type", ct)
            .body(Body::from(body))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        json["id"].as_str().unwrap().to_string()
    }

    let id_a = upload_once(&app, &png, "first.png").await;
    let id_b = upload_once(&app, &png, "second.png").await;
    assert_eq!(id_a, id_b, "identical bytes must dedupe to one id");
}

#[tokio::test]
async fn serve_route_returns_bytes_with_correct_content_type() {
    let dir = tempfile::TempDir::new().unwrap();
    let app = make_test_router(dir.path());
    let png = one_pixel_png();
    let (ct, body) = multipart_body("u.png", "image/png", &png);
    let req = Request::builder()
        .method("POST")
        .uri("/api/admin/assets")
        .header("x-api-key", "test-bearer-key")
        .header("content-type", ct)
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Now fetch — no auth header; serving is public by design.
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/assets/{id}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap().to_str().unwrap(),
        "image/png",
    );
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body.as_ref(), png.as_slice());
}

#[tokio::test]
async fn serve_route_returns_404_for_unknown_id() {
    let dir = tempfile::TempDir::new().unwrap();
    let app = make_test_router(dir.path());
    let req = Request::builder()
        .method("GET")
        .uri("/api/assets/asset-doesnotexist")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 404);
}

/// `LayoutItem::Image` must roundtrip through the SQLite layout store —
/// proves the new `asset_id` column was added correctly and that the read
/// path picks up the variant. Mirrors Phase 5a's `layout_item_color_…` test.
#[test]
fn layout_item_image_roundtrips_through_store() {
    use cascades::layout_store::{LayoutConfig, LayoutItem, LayoutStore};

    let dir = tempfile::TempDir::new().unwrap();
    let store = LayoutStore::open(&dir.path().join("test.db")).unwrap();
    let original = LayoutItem::Image {
        id: "img-42".to_string(),
        z_index: 3,
        x: 50,
        y: 60,
        width: 120,
        height: 80,
        asset_id: "asset-abc123".to_string(),
        parent_id: Some("group-1".to_string()),
        visible_when: None,
    };
    store
        .upsert_layout(&LayoutConfig {
            id: "L1".to_string(),
            name: "L1".to_string(),
            items: vec![original.clone()],
            updated_at: 0,
        })
        .unwrap();

    let loaded = store.get_layout("L1").unwrap().unwrap();
    assert_eq!(loaded.items.len(), 1);
    match &loaded.items[0] {
        LayoutItem::Image { id, asset_id, x, y, width, height, parent_id, z_index, .. } => {
            assert_eq!(id, "img-42");
            assert_eq!(asset_id, "asset-abc123");
            assert_eq!((*x, *y, *width, *height), (50, 60, 120, 80));
            assert_eq!(*z_index, 3);
            assert_eq!(parent_id.as_deref(), Some("group-1"));
        }
        other => panic!("expected Image, got {other:?}"),
    }
}

#[tokio::test]
async fn list_route_returns_uploaded_assets() {
    let dir = tempfile::TempDir::new().unwrap();
    let app = make_test_router(dir.path());
    let (ct, body) = multipart_body("a.png", "image/png", &one_pixel_png());
    let req = Request::builder()
        .method("POST")
        .uri("/api/admin/assets")
        .header("x-api-key", "test-bearer-key")
        .header("content-type", ct)
        .body(Body::from(body))
        .unwrap();
    app.clone().oneshot(req).await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri("/api/admin/assets")
        .header("x-api-key", "test-bearer-key")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["filename"], "a.png");
    assert_eq!(arr[0]["mime"], "image/png");
}
