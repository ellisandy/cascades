//! Phase 5 — style plumbing tests.
//!
//! Each test here maps to one red-green step in the Phase 5 implementation
//! (see `docs/plugin-customization-design.md`). Tests are added incrementally.

use axum::body::Body;
use axum::http::Request;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tower::ServiceExt;

/// Minimal Axum router + AppState for route-level tests. Mirrors the helper
/// in `server_acceptance_tests.rs`; duplicated here to keep the Phase 5 test
/// file self-contained.
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

/// Red 1: the curated-fonts manifest and its referenced woff2 files must exist
/// on disk. This is the single source of truth consumed by both the Rust
/// `/fonts/*` route and the sidecar's `@font-face` wrapper, so "files are on
/// disk and non-empty" is the cheapest guard against a half-committed font set.
#[test]
fn fonts_manifest_and_referenced_files_exist() {
    let manifest_path = Path::new("fonts/fonts.json");
    assert!(
        manifest_path.exists(),
        "fonts/fonts.json must exist — it's the manifest consumed by both the \
         server's /fonts/* route and the sidecar's @font-face builder"
    );

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest_path)
            .expect("fonts/fonts.json must be readable"),
    )
    .expect("fonts/fonts.json must parse as JSON");

    let families = manifest["families"]
        .as_array()
        .expect("manifest must have a top-level `families` array");
    assert_eq!(
        families.len(),
        5,
        "Phase 5 ships 5 curated families (Inter, IBM Plex Sans, DM Serif \
         Display, JetBrains Mono, Space Grotesk)"
    );

    let expected_names = [
        "Inter",
        "IBM Plex Sans",
        "DM Serif Display",
        "JetBrains Mono",
        "Space Grotesk",
    ];
    let actual_names: Vec<&str> = families
        .iter()
        .map(|f| f["name"].as_str().expect("family.name is required"))
        .collect();
    for name in expected_names {
        assert!(
            actual_names.contains(&name),
            "family {name:?} missing from manifest; have {actual_names:?}"
        );
    }

    for family in families {
        let name = family["name"].as_str().unwrap();
        let files = family["files"]
            .as_array()
            .unwrap_or_else(|| panic!("family {name:?} must have `files` array"));
        assert!(
            !files.is_empty(),
            "family {name:?} must declare at least one font file"
        );
        for file in files {
            let rel_path = file["path"]
                .as_str()
                .unwrap_or_else(|| panic!("family {name:?} file.path must be a string"));
            let full_path = Path::new("fonts").join(rel_path);
            assert!(
                full_path.exists(),
                "font file referenced in manifest is missing: {full_path:?}"
            );
            let size = std::fs::metadata(&full_path)
                .unwrap_or_else(|e| panic!("cannot stat {full_path:?}: {e}"))
                .len();
            assert!(
                size > 1000,
                "font file {full_path:?} is suspiciously small ({size} bytes) \
                 — likely a broken download"
            );
        }
    }
}

/// Red 2: the Axum router must expose `/fonts/*` — the sidecar's `@font-face`
/// wrapper and the admin UI will both fetch from here. This verifies the
/// route is wired and returns the woff2 with a browser-parseable content-type.
#[tokio::test]
async fn fonts_route_serves_woff2_with_correct_content_type() {
    let tmp = tempfile::TempDir::new().unwrap();
    let app = make_test_router(tmp.path());

    let req = Request::builder()
        .uri("/fonts/inter/400.woff2")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), 200, "/fonts/inter/400.woff2 must return 200");
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .expect("content-type header must be set")
        .to_str()
        .unwrap();
    assert_eq!(
        content_type, "font/woff2",
        "woff2 response must declare content-type font/woff2 so browsers parse it"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        body.len() > 1000,
        "body too small ({} bytes) — route likely served an empty/wrong file",
        body.len()
    );
}

/// Red 6: the compositor must call the sidecar with HTML wrapped in a full
/// document that embeds `@font-face`. Uses a capturing mock sidecar so we
/// can inspect exactly what bytes would have been screenshotted.
#[tokio::test]
async fn compositor_wraps_sidecar_html_with_font_face() {
    use axum::{
        Json, Router,
        extract::State,
        response::IntoResponse,
        routing::post,
    };
    use cascades::{
        compositor::{Compositor, DisplayConfiguration},
        fonts::FontsManifest,
        instance_store::InstanceStore,
        layout_store::{LayoutItem, LayoutStore},
        template::TemplateEngine,
    };
    use image::{GrayImage, ImageEncoder};
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    #[derive(serde::Deserialize)]
    struct CapturedBody {
        html: String,
        width: u32,
        height: u32,
    }

    async fn capturing_render(
        State(captured): State<Arc<Mutex<Vec<String>>>>,
        Json(body): Json<CapturedBody>,
    ) -> impl IntoResponse {
        captured.lock().unwrap().push(body.html);
        let pixels = vec![255u8; (body.width * body.height) as usize];
        let img = GrayImage::from_raw(body.width, body.height, pixels).unwrap();
        let mut png = Vec::new();
        ImageEncoder::write_image(
            image::codecs::png::PngEncoder::new(&mut png),
            img.as_raw(),
            body.width,
            body.height,
            image::ColorType::L8,
        )
        .unwrap();
        (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "image/png")],
            axum::body::Body::from(png),
        )
    }

    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/render", post(capturing_render))
        .with_state(Arc::clone(&captured));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let sidecar_url = format!("http://127.0.0.1:{}", addr.port());

    let tmp = tempfile::TempDir::new().unwrap();
    let db = tmp.path().join("test.db");
    let tpl = tmp.path().join("templates");
    std::fs::create_dir_all(&tpl).unwrap();
    let layout_store = Arc::new(LayoutStore::open(&db).unwrap());
    let instance_store = Arc::new(InstanceStore::open(&db).unwrap());
    let engine = Arc::new(TemplateEngine::new(&tpl).unwrap());
    let manifest =
        Arc::new(FontsManifest::load_from(Path::new("fonts/fonts.json")).unwrap());

    let compositor = Compositor::new(
        engine,
        Arc::clone(&instance_store),
        Arc::clone(&layout_store),
        sidecar_url,
        Arc::clone(&manifest),
        "http://localhost:9090".to_string(),
    );

    let config = DisplayConfiguration {
        name: "t".to_string(),
        items: vec![LayoutItem::StaticText {
            id: "txt-1".to_string(),
            z_index: 0,
            x: 0,
            y: 0,
            width: 100,
            height: 40,
            text_content: "hi".to_string(),
            font_size: 24,
            orientation: None,
            bold: None,
            italic: None,
            underline: None,
            font_family: Some("Inter".to_string()),
            color: None,
            parent_id: None,
            visible_when: None,
        }],
    };

    let _png = compositor.compose(&config, "einkPreview").await.unwrap();

    let bodies = captured.lock().unwrap();
    assert!(!bodies.is_empty(), "sidecar must have been called");
    let html = &bodies[0];
    assert!(
        html.contains("@font-face"),
        "wrapped HTML must include @font-face; got:\n{html}"
    );
    assert!(
        html.contains("font-family: \"Inter\""),
        "wrapped HTML must declare Inter; got:\n{html}"
    );
}

/// Red 4: the compositor must emit `@font-face` CSS covering every curated
/// family before handing HTML to the sidecar, otherwise Chromium renders
/// with system fonts and user font selections are silently ignored.
///
/// Tests the standalone manifest → CSS conversion (the compositor's
/// integration is tested separately through a mock-sidecar capture).
#[test]
fn font_face_css_covers_all_five_families_and_uses_base_url() {
    let manifest = cascades::fonts::FontsManifest::load_from(Path::new("fonts/fonts.json"))
        .expect("manifest must load");

    let base_url = "http://localhost:9090";
    let css = manifest.to_font_face_css(base_url);

    // Every family must appear as an @font-face declaration.
    for family_name in [
        "Inter",
        "IBM Plex Sans",
        "DM Serif Display",
        "JetBrains Mono",
        "Space Grotesk",
    ] {
        // Accept either single-quote or double-quote style.
        let single = format!("font-family: '{family_name}'");
        let double = format!("font-family: \"{family_name}\"");
        assert!(
            css.contains(&single) || css.contains(&double),
            "@font-face must declare family {family_name:?}; got:\n{css}"
        );
    }

    // URLs must be absolute and rooted at the supplied base_url so Chromium
    // in the sidecar (potentially on a different host loopback) can fetch.
    assert!(
        css.contains(&format!("{base_url}/fonts/inter/400.woff2")),
        "@font-face src must use absolute URL under base_url; got:\n{css}"
    );

    // Total @font-face count should match the manifest (9 files: 5 families,
    // DM Serif Display has 1 weight, the others have 2).
    let face_count = css.matches("@font-face").count();
    assert_eq!(
        face_count, 9,
        "expected 9 @font-face blocks (one per manifest file); found {face_count}"
    );
}

/// Red 8: the compositor must propagate an item's `color` field into the
/// emitted CSS. Without this, the field round-trips through SQLite but
/// never makes it to Chromium.
#[tokio::test]
async fn compositor_emits_item_color_in_sidecar_html() {
    use axum::{
        Json, Router,
        extract::State,
        response::IntoResponse,
        routing::post,
    };
    use cascades::{
        compositor::{Compositor, DisplayConfiguration},
        fonts::FontsManifest,
        instance_store::InstanceStore,
        layout_store::{LayoutItem, LayoutStore},
        template::TemplateEngine,
    };
    use image::{GrayImage, ImageEncoder};
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    #[derive(serde::Deserialize)]
    struct Body {
        html: String,
        width: u32,
        height: u32,
    }

    async fn cap(
        State(c): State<Arc<Mutex<Vec<String>>>>,
        Json(b): Json<Body>,
    ) -> impl IntoResponse {
        c.lock().unwrap().push(b.html);
        let pixels = vec![255u8; (b.width * b.height) as usize];
        let img = GrayImage::from_raw(b.width, b.height, pixels).unwrap();
        let mut png = Vec::new();
        ImageEncoder::write_image(
            image::codecs::png::PngEncoder::new(&mut png),
            img.as_raw(),
            b.width,
            b.height,
            image::ColorType::L8,
        )
        .unwrap();
        (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "image/png")],
            axum::body::Body::from(png),
        )
    }

    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/render", post(cap))
        .with_state(Arc::clone(&captured));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    let sidecar_url = format!("http://127.0.0.1:{}", addr.port());

    let tmp = tempfile::TempDir::new().unwrap();
    let db = tmp.path().join("t.db");
    let tpl = tmp.path().join("templates");
    std::fs::create_dir_all(&tpl).unwrap();
    let ls = Arc::new(LayoutStore::open(&db).unwrap());
    let is = Arc::new(InstanceStore::open(&db).unwrap());
    let engine = Arc::new(TemplateEngine::new(&tpl).unwrap());
    let compositor = Compositor::new(
        engine,
        is,
        ls,
        sidecar_url,
        Arc::new(FontsManifest::empty()),
        "http://localhost:0".to_string(),
    );

    let config = DisplayConfiguration {
        name: "color-test".to_string(),
        items: vec![LayoutItem::StaticText {
            id: "red".to_string(),
            z_index: 0,
            x: 0,
            y: 0,
            width: 100,
            height: 40,
            text_content: "red".to_string(),
            font_size: 24,
            orientation: None,
            bold: None,
            italic: None,
            underline: None,
            font_family: None,
            color: Some("#ff0000".to_string()),
            parent_id: None,
            visible_when: None,
        }],
    };

    let _png = compositor.compose(&config, "einkPreview").await.unwrap();
    let bodies = captured.lock().unwrap();
    assert!(!bodies.is_empty());
    assert!(
        bodies[0].contains("color:#ff0000") || bodies[0].contains("color: #ff0000"),
        "HTML must contain the item's color; got:\n{}",
        bodies[0]
    );
}

/// Red 7: style-bearing items (StaticText, StaticDateTime, DataField) must
/// carry an optional `color` field that round-trips through the layout_store
/// — this is the data backing for the admin-UI color picker.
#[test]
fn layout_item_color_roundtrips_through_store() {
    use cascades::layout_store::{LayoutConfig, LayoutItem, LayoutStore};

    let tmp = tempfile::TempDir::new().unwrap();
    let store = LayoutStore::open(&tmp.path().join("test.db")).unwrap();

    let items = vec![
        LayoutItem::StaticText {
            id: "t1".to_string(),
            z_index: 0,
            x: 0,
            y: 0,
            width: 100,
            height: 40,
            text_content: "hi".to_string(),
            font_size: 24,
            orientation: None,
            bold: None,
            italic: None,
            underline: None,
            font_family: None,
            color: Some("#ff0000".to_string()),
            parent_id: None,
            visible_when: None,
        },
        LayoutItem::DataField {
            id: "d1".to_string(),
            z_index: 1,
            x: 10,
            y: 50,
            width: 200,
            height: 60,
            field_mapping_id: "fm-x".to_string(),
            font_size: 32,
            format_string: "{{ value }}".to_string(),
            label: None,
            orientation: None,
            bold: None,
            italic: None,
            underline: None,
            font_family: None,
            color: Some("#0033aa".to_string()),
            parent_id: None,
            visible_when: None,
        },
    ];

    store
        .upsert_layout(&LayoutConfig {
            id: "cx".to_string(),
            name: "cx".to_string(),
            items: items.clone(),
            updated_at: 0,
        })
        .unwrap();

    let loaded = store.get_layout("cx").unwrap().unwrap();
    assert_eq!(loaded.items.len(), 2);

    // Assert color survived the round-trip on both items.
    match &loaded.items[0] {
        LayoutItem::StaticText { color, .. } => {
            assert_eq!(color.as_deref(), Some("#ff0000"));
        }
        other => panic!("expected StaticText, got id={}", other.id()),
    }
    match &loaded.items[1] {
        LayoutItem::DataField { color, .. } => {
            assert_eq!(color.as_deref(), Some("#0033aa"));
        }
        other => panic!("expected DataField, got id={}", other.id()),
    }
}

/// Red 5: wrapping produces a full HTML document that embeds the @font-face
/// CSS ahead of the inner payload, so Puppeteer's `networkidle0` wait resolves
/// only after the curated fonts are fetched.
#[test]
fn wrap_html_embeds_font_face_and_preserves_inner() {
    let manifest = cascades::fonts::FontsManifest::load_from(Path::new("fonts/fonts.json"))
        .expect("manifest must load");
    let inner = "<div class=\"marker\">hello</div>";
    let wrapped = manifest.wrap_html(inner, "http://localhost:9090", &[]);

    assert!(wrapped.starts_with("<!DOCTYPE html>"), "must be a full document");
    assert!(wrapped.contains("@font-face"), "must include @font-face CSS");
    assert!(
        wrapped.contains("font-family: \"Inter\""),
        "@font-face must name Inter; got:\n{wrapped}"
    );
    assert!(
        wrapped.contains(inner),
        "inner HTML must appear verbatim after wrapping"
    );
    // The @font-face block must precede the inner content, so Chromium has
    // the faces registered before it parses the styled elements.
    let face_idx = wrapped.find("@font-face").unwrap();
    let inner_idx = wrapped.find(inner).unwrap();
    assert!(
        face_idx < inner_idx,
        "@font-face must appear before inner HTML"
    );
}

/// Red 3: the manifest itself is the discovery endpoint — admin UI and
/// sidecar both fetch it as JSON. Verifies it's reachable and parseable
/// through the same /fonts/* route.
#[tokio::test]
async fn fonts_route_serves_manifest_as_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    let app = make_test_router(tmp.path());

    let req = Request::builder()
        .uri("/fonts/fonts.json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), 200);
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(content_type, "application/json");
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["families"].is_array(), "manifest must parse as JSON");
}
