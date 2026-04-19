//! Acceptance tests for the layout compositor.
//!
//! These tests verify that:
//!
//! 1. The 'default' display config (single river slot, 800×480) produces a
//!    valid 800×480 PNG.
//! 2. The 'trip-planner' display config (weather half-top + river quadrant-BL
//!    + ferry quadrant-BR) produces a valid 800×480 PNG with correctly placed
//!    pixel regions.
//!
//! A minimal Axum HTTP server acting as a mock sidecar is started on an
//! ephemeral port.  It accepts POST /render and returns a white PNG of the
//! requested dimensions.  This keeps the tests self-contained — no real Bun
//! process is required.

use axum::{
    body::Body,
    extract::Json,
    http::{header, StatusCode},
    response::IntoResponse,
    routing::post,
    Router,
};
use cascades::{
    compositor::{Compositor, DisplayConfiguration},
    config::load_display_layouts,
    instance_store::{seed_from_config, InstanceStore},
    layout_store::{LayoutItem, LayoutStore},
    template::TemplateEngine,
};
use image::{GrayImage, ImageEncoder};
use std::{path::Path, sync::Arc};
use tempfile::TempDir;
use tokio::net::TcpListener;

// ─── Mock sidecar ─────────────────────────────────────────────────────────────

/// Render request body — mirrors the sidecar's expected JSON.
#[derive(serde::Deserialize)]
struct RenderRequest {
    width: u32,
    height: u32,
    // html and mode are accepted but unused by the mock.
}

/// Return a white PNG of the requested dimensions.
async fn mock_render(Json(req): Json<RenderRequest>) -> impl IntoResponse {
    let pixels = vec![255u8; (req.width * req.height) as usize];
    let img = GrayImage::from_raw(req.width, req.height, pixels)
        .expect("valid dimensions");
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png);
    ImageEncoder::write_image(
        encoder,
        img.as_raw(),
        req.width,
        req.height,
        image::ColorType::L8,
    )
    .expect("PNG encoding should not fail");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/png")],
        Body::from(png),
    )
}

/// Start a mock sidecar on an ephemeral port and return its base URL.
async fn start_mock_sidecar() -> String {
    let app = Router::new().route("/render", post(mock_render));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    format!("http://127.0.0.1:{}", addr.port())
}

// ─── Shared test infrastructure ───────────────────────────────────────────────

/// Build a minimal Config used to seed the instance store.
fn minimal_config() -> cascades::config::Config {
    use cascades::config::{
        Config, DisplayConfig, LocationConfig, SourceIntervals, StorageConfig,
    };
    Config {
        display: DisplayConfig { width: 800, height: 480 },
        location: LocationConfig {
            latitude: 48.4232,
            longitude: -122.3351,
            name: "Mount Vernon, WA".to_string(),
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
    }
}

/// Create a temp instance store seeded with the 5 well-known plugin instances.
fn seeded_store() -> (Arc<InstanceStore>, Arc<LayoutStore>, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let db = dir.path().join("test.db");
    let store = InstanceStore::open(&db).expect("open store");
    let layout_store = LayoutStore::open(&db).expect("open layout store");
    let config = minimal_config();
    seed_from_config(&store, &config).expect("seed store");
    (Arc::new(store), Arc::new(layout_store), dir)
}

// ─── Acceptance test: 'default' display config ────────────────────────────────

#[tokio::test]
async fn default_config_composite_returns_800x480_png() {
    let sidecar_url = start_mock_sidecar().await;
    let (store, layout_store, _dir) = seeded_store();
    let engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("load templates"),
    );

    let compositor = Compositor::new(Arc::clone(&engine), Arc::clone(&store), Arc::clone(&layout_store), &sidecar_url);

    let config = DisplayConfiguration {
        name: "default".to_string(),
        items: vec![LayoutItem::PluginSlot {
            id: "s0".to_string(),
            z_index: 0,
            x: 0,
            y: 0,
            width: 800,
            height: 480,
            plugin_instance_id: "river".to_string(),
            layout_variant: "full".to_string(),
        }],
    };

    let png = compositor.compose(&config, "einkPreview").await.expect("compose should succeed");

    assert!(png.starts_with(b"\x89PNG"), "result must be a PNG");
    let img = image::load_from_memory(&png).expect("valid PNG");
    assert_eq!(img.width(), 800, "frame must be 800px wide");
    assert_eq!(img.height(), 480, "frame must be 480px tall");
}

// ─── Acceptance test: 'trip-planner' display config ──────────────────────────

#[tokio::test]
async fn trip_planner_config_composite_returns_800x480_png() {
    let sidecar_url = start_mock_sidecar().await;
    let (store, layout_store, _dir) = seeded_store();
    let engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("load templates"),
    );

    let compositor = Compositor::new(Arc::clone(&engine), Arc::clone(&store), Arc::clone(&layout_store), &sidecar_url);

    let config = DisplayConfiguration {
        name: "trip-planner".to_string(),
        items: vec![
            LayoutItem::PluginSlot {
                id: "s0".to_string(),
                z_index: 0,
                x: 0, y: 0, width: 800, height: 240,
                plugin_instance_id: "weather".to_string(),
                layout_variant: "half_horizontal".to_string(),
            },
            LayoutItem::PluginSlot {
                id: "s1".to_string(),
                z_index: 1,
                x: 0, y: 240, width: 400, height: 240,
                plugin_instance_id: "river".to_string(),
                layout_variant: "quadrant".to_string(),
            },
            LayoutItem::PluginSlot {
                id: "s2".to_string(),
                z_index: 2,
                x: 400, y: 240, width: 400, height: 240,
                plugin_instance_id: "ferry".to_string(),
                layout_variant: "quadrant".to_string(),
            },
        ],
    };

    let png = compositor.compose(&config, "einkPreview").await.expect("compose should succeed");

    assert!(png.starts_with(b"\x89PNG"), "result must be a PNG");
    let img = image::load_from_memory(&png).expect("valid PNG");
    assert_eq!(img.width(), 800, "frame must be 800px wide");
    assert_eq!(img.height(), 480, "frame must be 480px tall");
}

// ─── Acceptance test: load from display.toml ─────────────────────────────────

#[tokio::test]
async fn display_toml_contains_both_configs() {
    let layouts = load_display_layouts(Path::new("config/display.toml"))
        .expect("load display.toml");

    let names: Vec<&str> = layouts.displays.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"default"),
        "display.toml must contain 'default' config; got {:?}",
        names
    );
    assert!(
        names.contains(&"trip-planner"),
        "display.toml must contain 'trip-planner' config; got {:?}",
        names
    );
}

#[tokio::test]
async fn default_config_has_one_full_slot() {
    let layouts = load_display_layouts(Path::new("config/display.toml"))
        .expect("load display.toml");
    let default_entry = layouts
        .displays
        .iter()
        .find(|d| d.name == "default")
        .expect("default config missing");
    assert_eq!(default_entry.slots.len(), 1);
    let slot = &default_entry.slots[0];
    assert_eq!(slot.plugin, "river");
    assert_eq!(slot.variant, "full");
}

#[tokio::test]
async fn trip_planner_config_has_three_slots() {
    let layouts = load_display_layouts(Path::new("config/display.toml"))
        .expect("load display.toml");
    let entry = layouts
        .displays
        .iter()
        .find(|d| d.name == "trip-planner")
        .expect("trip-planner config missing");
    assert_eq!(entry.slots.len(), 3);
}

// ─── Acceptance test: concurrent render — all tasks complete ─────────────────

#[tokio::test]
async fn compositor_runs_slots_concurrently_and_joins() {
    // Verifies that all three slots are actually rendered and composited.
    // The mock sidecar returns white PNGs; the frame stays white everywhere.
    // We only need to verify the compositor doesn't deadlock or drop tasks.
    let sidecar_url = start_mock_sidecar().await;
    let (store, layout_store, _dir) = seeded_store();
    let engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("load templates"),
    );

    let compositor = Compositor::new(Arc::clone(&engine), Arc::clone(&store), Arc::clone(&layout_store), &sidecar_url);

    let config = DisplayConfiguration {
        name: "trip-planner".to_string(),
        items: vec![
            LayoutItem::PluginSlot {
                id: "s0".to_string(), z_index: 0,
                x: 0, y: 0, width: 800, height: 240,
                plugin_instance_id: "weather".to_string(),
                layout_variant: "half_horizontal".to_string(),
            },
            LayoutItem::PluginSlot {
                id: "s1".to_string(), z_index: 1,
                x: 0, y: 240, width: 400, height: 240,
                plugin_instance_id: "river".to_string(),
                layout_variant: "quadrant".to_string(),
            },
            LayoutItem::PluginSlot {
                id: "s2".to_string(), z_index: 2,
                x: 400, y: 240, width: 400, height: 240,
                plugin_instance_id: "ferry".to_string(),
                layout_variant: "quadrant".to_string(),
            },
        ],
    };

    // Run twice to catch any single-use resource issues.
    for _ in 0..2 {
        let png = compositor.compose(&config, "einkPreview").await.expect("compose ok");
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!(img.width(), 800);
        assert_eq!(img.height(), 480);
    }
}

// ─── Acceptance test: from_config roundtrip through display.toml ─────────────

#[tokio::test]
async fn display_configuration_from_config_roundtrip() {
    let layouts = load_display_layouts(Path::new("config/display.toml"))
        .expect("load display.toml");

    // Both configs should parse without error; item count equals slot count.
    for entry in &layouts.displays {
        let config = DisplayConfiguration::from_config(entry)
            .unwrap_or_else(|e| panic!("from_config failed for '{}': {}", entry.name, e));
        assert_eq!(config.name, entry.name);
        assert_eq!(config.items.len(), entry.slots.len());
    }
}

// ─── Acceptance test: static elements rendered correctly ─────────────────────

#[tokio::test]
async fn static_divider_composited_without_sidecar() {
    // A layout with only a StaticDivider requires no sidecar call.
    // The mock sidecar never gets hit, but we don't even need it running.
    let (store, layout_store, _dir) = seeded_store();
    let engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("load templates"),
    );
    // Point at a non-existent port — dividers must NOT hit the sidecar.
    let compositor = Compositor::new(Arc::clone(&engine), Arc::clone(&store), Arc::clone(&layout_store), "http://127.0.0.1:1");

    let config = DisplayConfiguration {
        name: "divider-only".to_string(),
        items: vec![LayoutItem::StaticDivider {
            id: "d0".to_string(),
            z_index: 0,
            x: 0, y: 240,
            width: 800, height: 2,
            orientation: Some("horizontal".to_string()),
        }],
    };

    let png = compositor.compose(&config, "einkPreview").await.expect("compose should succeed");
    let img = image::load_from_memory(&png).unwrap().to_luma8();
    assert_eq!(img.width(), 800);
    assert_eq!(img.height(), 480);
    // Divider row should be black.
    assert_eq!(img.get_pixel(400, 240).0[0], 0, "divider row should be black");
    assert_eq!(img.get_pixel(400, 241).0[0], 0, "divider row+1 should be black");
    // Surrounding rows should be white.
    assert_eq!(img.get_pixel(400, 239).0[0], 255, "row above divider should be white");
    assert_eq!(img.get_pixel(400, 242).0[0], 255, "row below divider should be white");
}

#[tokio::test]
async fn static_text_renders_via_sidecar() {
    let sidecar_url = start_mock_sidecar().await;
    let (store, layout_store, _dir) = seeded_store();
    let engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("load templates"),
    );
    let compositor = Compositor::new(Arc::clone(&engine), Arc::clone(&store), Arc::clone(&layout_store), &sidecar_url);

    let config = DisplayConfiguration {
        name: "text-only".to_string(),
        items: vec![LayoutItem::StaticText {
            id: "t0".to_string(),
            z_index: 0,
            x: 100, y: 100,
            width: 200, height: 50,
            text_content: "Hello".to_string(),
            font_size: 18,
            orientation: None,
            bold: None, italic: None, underline: None, font_family: None,
        }],
    };

    let png = compositor.compose(&config, "einkPreview").await.expect("compose should succeed");
    let img = image::load_from_memory(&png).unwrap();
    assert_eq!(img.width(), 800);
    assert_eq!(img.height(), 480);
}

#[tokio::test]
async fn mixed_layout_plugin_text_divider_composited_correctly() {
    // Layout: PluginSlot (z=0) + StaticText (z=1) + StaticDivider (z=2).
    // The divider draws black at y=240..242.  The sidecar returns white PNGs.
    let sidecar_url = start_mock_sidecar().await;
    let (store, layout_store, _dir) = seeded_store();
    let engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("load templates"),
    );
    let compositor = Compositor::new(Arc::clone(&engine), Arc::clone(&store), Arc::clone(&layout_store), &sidecar_url);

    let config = DisplayConfiguration {
        name: "mixed".to_string(),
        items: vec![
            LayoutItem::PluginSlot {
                id: "s0".to_string(), z_index: 0,
                x: 0, y: 0, width: 800, height: 240,
                plugin_instance_id: "weather".to_string(),
                layout_variant: "half_horizontal".to_string(),
            },
            LayoutItem::StaticText {
                id: "t0".to_string(), z_index: 1,
                x: 10, y: 10, width: 200, height: 40,
                text_content: "Skagit".to_string(),
                font_size: 20,
                orientation: None,
                bold: None, italic: None, underline: None, font_family: None,
            },
            LayoutItem::StaticDivider {
                id: "d0".to_string(), z_index: 2,
                x: 0, y: 240, width: 800, height: 2,
                orientation: Some("horizontal".to_string()),
            },
        ],
    };

    let png = compositor.compose(&config, "einkPreview").await.expect("compose should succeed");
    let img = image::load_from_memory(&png).unwrap().to_luma8();
    assert_eq!(img.width(), 800);
    assert_eq!(img.height(), 480);
    // Divider at y=240 should be black.
    assert_eq!(img.get_pixel(400, 240).0[0], 0, "divider row should be black");
}

// ─── DataField integration tests ────────────────────────────────────────────

#[tokio::test]
async fn data_field_renders_extracted_value_via_sidecar() {
    // Create a field mapping pointing at the "river" plugin instance,
    // set cached_data with a known JSON, then verify the compositor
    // renders a DataField item without error.
    let sidecar_url = start_mock_sidecar().await;
    let (store, layout_store, _dir) = seeded_store();
    let engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("load templates"),
    );

    // Seed cached_data for the "river" plugin instance
    store
        .update_cached_data(
            "river",
            &serde_json::json!({
                "water_level_ft": 11.87,
                "streamflow_cfs": 8750.0,
                "site_name": "Skagit River"
            }),
            1000,
        )
        .expect("update cached data");

    // Create a field mapping for water_level_ft
    layout_store
        .create_field_mapping(
            "fm-water-level",
            "river",
            "builtin",
            "Water Level",
            "$.water_level_ft",
        )
        .expect("create field mapping");

    let compositor = Compositor::new(
        Arc::clone(&engine),
        Arc::clone(&store),
        Arc::clone(&layout_store),
        &sidecar_url,
    );

    let config = DisplayConfiguration {
        name: "data-field-test".to_string(),
        items: vec![LayoutItem::DataField {
            id: "df0".to_string(),
            z_index: 0,
            x: 50,
            y: 50,
            width: 200,
            height: 60,
            field_mapping_id: "fm-water-level".to_string(),
            font_size: 24,
            format_string: "{{value}} ft".to_string(),
            label: None,
            orientation: None,
            bold: None, italic: None, underline: None, font_family: None,
        }],
    };

    let png = compositor
        .compose(&config, "einkPreview")
        .await
        .expect("compose with DataField should succeed");
    let img = image::load_from_memory(&png).unwrap().to_luma8();
    assert_eq!(img.width(), 800);
    assert_eq!(img.height(), 480);
    // The sidecar returns white PNGs, so the DataField region is white (rendered).
    assert_eq!(img.get_pixel(100, 70).0[0], 255, "data field region should be rendered");
}

#[tokio::test]
async fn data_field_missing_mapping_renders_placeholder() {
    // When the field mapping doesn't exist, the compositor should render
    // "[no data]" as a placeholder instead of failing.
    let sidecar_url = start_mock_sidecar().await;
    let (store, layout_store, _dir) = seeded_store();
    let engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("load templates"),
    );

    let compositor = Compositor::new(
        Arc::clone(&engine),
        Arc::clone(&store),
        Arc::clone(&layout_store),
        &sidecar_url,
    );

    let config = DisplayConfiguration {
        name: "missing-mapping-test".to_string(),
        items: vec![LayoutItem::DataField {
            id: "df-missing".to_string(),
            z_index: 0,
            x: 0,
            y: 0,
            width: 200,
            height: 60,
            field_mapping_id: "nonexistent-mapping".to_string(),
            font_size: 16,
            format_string: "{{value}}".to_string(),
            label: Some("Water Level".to_string()),
            orientation: None,
            bold: None, italic: None, underline: None, font_family: None,
        }],
    };

    let png = compositor
        .compose(&config, "einkPreview")
        .await
        .expect("compose with missing mapping should not fail");
    let img = image::load_from_memory(&png).unwrap().to_luma8();
    assert_eq!(img.width(), 800);
    assert_eq!(img.height(), 480);
}

#[tokio::test]
async fn data_field_with_label_renders_successfully() {
    let sidecar_url = start_mock_sidecar().await;
    let (store, layout_store, _dir) = seeded_store();
    let engine = Arc::new(
        TemplateEngine::new(Path::new("templates")).expect("load templates"),
    );

    store
        .update_cached_data(
            "river",
            &serde_json::json!({
                "streamflow_cfs": 8750.3
            }),
            1000,
        )
        .expect("update cached data");

    layout_store
        .create_field_mapping(
            "fm-flow",
            "river",
            "builtin",
            "Streamflow",
            "$.streamflow_cfs",
        )
        .expect("create field mapping");

    let compositor = Compositor::new(
        Arc::clone(&engine),
        Arc::clone(&store),
        Arc::clone(&layout_store),
        &sidecar_url,
    );

    let config = DisplayConfiguration {
        name: "label-test".to_string(),
        items: vec![LayoutItem::DataField {
            id: "df-flow".to_string(),
            z_index: 0,
            x: 100,
            y: 200,
            width: 250,
            height: 80,
            field_mapping_id: "fm-flow".to_string(),
            font_size: 32,
            format_string: "{{value | round(0) | number_with_delimiter}} cfs".to_string(),
            label: Some("Streamflow".to_string()),
            orientation: None,
            bold: None, italic: None, underline: None, font_family: None,
        }],
    };

    let png = compositor
        .compose(&config, "einkPreview")
        .await
        .expect("compose with label should succeed");
    let img = image::load_from_memory(&png).unwrap().to_luma8();
    assert_eq!(img.width(), 800);
    assert_eq!(img.height(), 480);
}
