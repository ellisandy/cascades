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
    let scheduler = Arc::new(cascades::api::SourceScheduler::new(Arc::clone(&source_store)));

    let state = Arc::new(AppState {
        compositor,
        instance_store,
        layout_store,
        source_store,
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
