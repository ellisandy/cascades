//! Phase 7 — visible_when + DataIcon integration tests.
//!
//! Each test maps to a red-green step in the Phase 7 implementation
//! (see `docs/plugin-customization-design.md`). Unit tests for the
//! `visible_when::evaluate` operator semantics live alongside the module
//! itself; these are the wire / store / compositor integration checks.

use cascades::layout_store::{LayoutConfig, LayoutItem, LayoutStore};
use cascades::visible_when::VisibleWhen;
use serde_json::json;
use std::collections::HashMap;

/// `visible_when` must roundtrip through SQLite — proves the JSON-encoded
/// column is wired up in both the read and write paths.
#[test]
fn visible_when_roundtrips_through_layout_store() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = LayoutStore::open(&dir.path().join("test.db")).unwrap();

    let vw = VisibleWhen {
        path: "$.weather.precip_chance_pct".to_string(),
        op: ">".to_string(),
        value: json!(50),
    };
    let original = LayoutItem::StaticText {
        id: "txt".to_string(),
        z_index: 0,
        x: 0, y: 0, width: 100, height: 20,
        text_content: "rain warning".to_string(),
        font_size: 16,
        orientation: None,
        bold: None, italic: None, underline: None, font_family: None, color: None,
        parent_id: None,
        visible_when: Some(vw.clone()),
    };
    store
        .upsert_layout(&LayoutConfig {
            id: "L".into(),
            name: "L".into(),
            items: vec![original],
            updated_at: 0,
        })
        .unwrap();

    let loaded = store.get_layout("L").unwrap().unwrap();
    assert_eq!(loaded.items.len(), 1);
    let got = loaded.items[0].visible_when().expect("visible_when survived roundtrip");
    assert_eq!(got, &vw);
}

/// Items without a clause must still load with `visible_when: None`. This
/// guards against the additive-column migration breaking pre-Phase-7 rows.
#[test]
fn item_without_visible_when_loads_as_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = LayoutStore::open(&dir.path().join("test.db")).unwrap();

    let item = LayoutItem::StaticDivider {
        id: "d".to_string(),
        z_index: 0, x: 0, y: 0, width: 100, height: 4,
        orientation: None,
        parent_id: None,
        visible_when: None,
    };
    store
        .upsert_layout(&LayoutConfig {
            id: "L".into(), name: "L".into(),
            items: vec![item], updated_at: 0,
        })
        .unwrap();
    let loaded = store.get_layout("L").unwrap().unwrap();
    assert!(loaded.items[0].visible_when().is_none());
}

/// `LayoutItem::DataIcon` must roundtrip with a populated `icon_map`.
/// Mirrors Phase 6a's `layout_item_image_roundtrips_through_store`.
#[test]
fn data_icon_roundtrips_with_icon_map() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = LayoutStore::open(&dir.path().join("test.db")).unwrap();

    let mut icon_map = HashMap::new();
    icon_map.insert("sun".to_string(), "asset-aaa".to_string());
    icon_map.insert("rain".to_string(), "asset-bbb".to_string());
    let original = LayoutItem::DataIcon {
        id: "icon-1".to_string(),
        z_index: 1, x: 10, y: 20, width: 64, height: 64,
        field_mapping_id: "fm-weather-condition".to_string(),
        icon_map: icon_map.clone(),
        parent_id: None,
        visible_when: None,
    };
    store
        .upsert_layout(&LayoutConfig {
            id: "L".into(), name: "L".into(),
            items: vec![original], updated_at: 0,
        })
        .unwrap();

    let loaded = store.get_layout("L").unwrap().unwrap();
    match &loaded.items[0] {
        LayoutItem::DataIcon { id, field_mapping_id, icon_map: m, x, y, width, height, .. } => {
            assert_eq!(id, "icon-1");
            assert_eq!(field_mapping_id, "fm-weather-condition");
            assert_eq!((*x, *y, *width, *height), (10, 20, 64, 64));
            assert_eq!(m, &icon_map);
        }
        other => panic!("expected DataIcon, got {other:?}"),
    }
}

/// The DB's `visible_when_json` column rejects a malformed JSON gracefully:
/// the item loads with `visible_when: None` and a warning is logged. A
/// hand-edited DB shouldn't crash the read path. We simulate by writing a
/// row with a valid clause, corrupting the JSON via raw SQL, then re-reading.
#[test]
fn malformed_visible_when_json_loads_as_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let store = LayoutStore::open(&db_path).unwrap();
    store
        .upsert_layout(&LayoutConfig {
            id: "L".into(), name: "L".into(),
            items: vec![LayoutItem::StaticDivider {
                id: "d".into(), z_index: 0, x: 0, y: 0, width: 100, height: 4,
                orientation: None, parent_id: None,
                visible_when: Some(VisibleWhen {
                    path: "$.x".into(), op: "=".into(), value: json!(1),
                }),
            }],
            updated_at: 0,
        })
        .unwrap();

    // Corrupt the visible_when_json column directly.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE layout_items SET visible_when_json = '{not-valid-json' WHERE id = 'd'",
        [],
    )
    .unwrap();
    drop(conn);

    let loaded = store.get_layout("L").unwrap().unwrap();
    assert_eq!(loaded.items.len(), 1);
    assert!(
        loaded.items[0].visible_when().is_none(),
        "malformed clause must downgrade to None, not panic",
    );
}

/// `LayoutItem::Group` skips visible_when by design — hiding a group
/// separately from its children is a different (cascading) feature deferred
/// from v1. The accessor returns None even though Group has no field.
#[test]
fn group_visible_when_is_always_none() {
    let g = LayoutItem::Group {
        id: "g".to_string(),
        z_index: 0, x: 0, y: 0, width: 100, height: 100,
        plugin_instance_id: None, label: None, background: None, parent_id: None,
        default_elements_hash: None, defaults_stale: None, style_overrides: None,
    };
    assert!(g.visible_when().is_none());
}

/// All six non-Group variants must roundtrip `visible_when` through SQLite.
/// Each variant has its own serde-derived field list and its own write-path
/// INSERT; copy-paste mistakes between them won't surface in serde tests
/// because each derive is independent. Cover them all in one test so a
/// regression on any single variant fails loudly.
#[test]
fn visible_when_roundtrips_on_every_non_group_variant() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = LayoutStore::open(&dir.path().join("test.db")).unwrap();

    fn vw(suffix: &str) -> Option<VisibleWhen> {
        Some(VisibleWhen {
            path: format!("$.test.{suffix}"),
            op: "exists".into(),
            value: serde_json::Value::Null,
        })
    }

    let items = vec![
        LayoutItem::PluginSlot {
            id: "ps".into(), z_index: 0, x: 0, y: 0, width: 100, height: 100,
            plugin_instance_id: "x".into(), layout_variant: "full".into(),
            parent_id: None, visible_when: vw("plugin_slot"),
        },
        LayoutItem::StaticText {
            id: "st".into(), z_index: 1, x: 0, y: 0, width: 100, height: 20,
            text_content: "t".into(), font_size: 16, orientation: None,
            bold: None, italic: None, underline: None, font_family: None, color: None,
            parent_id: None, visible_when: vw("static_text"),
        },
        LayoutItem::StaticDateTime {
            id: "sdt".into(), z_index: 2, x: 0, y: 0, width: 100, height: 20,
            font_size: 16, format: None, orientation: None,
            bold: None, italic: None, underline: None, font_family: None, color: None,
            parent_id: None, visible_when: vw("static_datetime"),
        },
        LayoutItem::StaticDivider {
            id: "sd".into(), z_index: 3, x: 0, y: 0, width: 100, height: 4,
            orientation: None, parent_id: None,
            visible_when: vw("static_divider"),
        },
        LayoutItem::DataField {
            id: "df".into(), z_index: 4, x: 0, y: 0, width: 100, height: 20,
            field_mapping_id: "fm".into(), font_size: 16,
            format_string: "{{value}}".into(), label: None, orientation: None,
            bold: None, italic: None, underline: None, font_family: None, color: None,
            parent_id: None, visible_when: vw("data_field"),
        },
        LayoutItem::Image {
            id: "img".into(), z_index: 5, x: 0, y: 0, width: 100, height: 100,
            asset_id: "asset-x".into(),
            parent_id: None, visible_when: vw("image"),
        },
    ];
    store
        .upsert_layout(&LayoutConfig {
            id: "L".into(), name: "L".into(),
            items, updated_at: 0,
        })
        .unwrap();

    let loaded = store.get_layout("L").unwrap().unwrap();
    assert_eq!(loaded.items.len(), 6);
    // Every loaded item must carry the visible_when its predecessor put in.
    let suffixes = ["plugin_slot", "static_text", "static_datetime",
                    "static_divider", "data_field", "image"];
    for (it, suffix) in loaded.items.iter().zip(suffixes.iter()) {
        let got = it.visible_when().unwrap_or_else(|| panic!("variant {suffix:?} lost visible_when"));
        assert_eq!(got.path, format!("$.test.{suffix}"));
        assert_eq!(got.op, "exists");
    }
}
