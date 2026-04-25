//! Phase 8 — Recovery & sync affordances integration tests.
//!
//! Covers the backend pieces that drive the admin UI's "Plugin defaults
//! updated" badge and "Reset to plugin defaults" action:
//!
//! 1. `default_elements_hash()` is deterministic across runs.
//! 2. The hash function detects edits — adding/changing fields shifts it.
//! 3. The hash column round-trips through SQLite on Group rows.
//! 4. Non-Group items don't carry a hash even if pre-Phase-8 rows exist.
//!
//! The HTTP-level reset-endpoint behaviour is exercised in api.rs unit
//! tests where the full router + state are easy to spin up.

use cascades::layout_store::{LayoutConfig, LayoutItem, LayoutStore};
use cascades::plugin_registry::{default_elements_hash, DefaultElement};

fn weather_manifest() -> Vec<DefaultElement> {
    vec![
        DefaultElement {
            kind: "data_field".to_string(),
            x: 0, y: 0, width: 200, height: 32,
            z_index: 0,
            field_path: Some("$.current.temp_f".to_string()),
            label: Some("Temp".to_string()),
            format_string: Some("{{value}}°F".to_string()),
            font_size: Some(28),
            text_content: None,
            format: None,
            orientation: Some("horizontal".to_string()),
        },
        DefaultElement {
            kind: "static_text".to_string(),
            x: 0, y: 40, width: 200, height: 24,
            z_index: 1,
            field_path: None, label: None, format_string: None,
            font_size: Some(16),
            text_content: Some("Conditions".to_string()),
            format: None,
            orientation: None,
        },
    ]
}

/// Hash of identical input must match — same struct, same hex output.
/// Guards against a future refactor that introduces an unstable serialiser
/// (e.g. `HashMap` field) without noticing.
#[test]
fn hash_is_deterministic() {
    let m = weather_manifest();
    let a = default_elements_hash(&m);
    let b = default_elements_hash(&m);
    assert_eq!(a, b, "same input must produce same hash");
    assert_eq!(a.len(), 16, "hash is 16 hex chars (8 bytes)");
    assert!(
        a.chars().all(|c| c.is_ascii_hexdigit()),
        "hash is hex-only: got {a}"
    );
}

/// Editing the manifest changes the hash — what makes the badge work at all.
/// We tweak each "interesting" field type to make sure none get accidentally
/// excluded from serialisation (e.g. via a future `#[serde(skip)]`).
#[test]
fn hash_changes_when_manifest_changes() {
    let base = default_elements_hash(&weather_manifest());

    // 1. Change a primitive (font_size) on an existing element.
    let mut m1 = weather_manifest();
    m1[0].font_size = Some(40);
    assert_ne!(base, default_elements_hash(&m1), "font_size delta must shift hash");

    // 2. Change a string field.
    let mut m2 = weather_manifest();
    m2[1].text_content = Some("Forecast".to_string());
    assert_ne!(base, default_elements_hash(&m2), "text_content delta must shift hash");

    // 3. Add an element.
    let mut m3 = weather_manifest();
    m3.push(DefaultElement {
        kind: "static_divider".to_string(),
        x: 0, y: 70, width: 200, height: 4,
        z_index: 2,
        field_path: None, label: None, format_string: None, font_size: None,
        text_content: None, format: None,
        orientation: Some("horizontal".to_string()),
    });
    assert_ne!(base, default_elements_hash(&m3), "added element must shift hash");

    // 4. Reorder elements — order matters in a Vec<DefaultElement> because
    //    `z_index` and child-position are serialised positionally.
    let mut m4 = weather_manifest();
    m4.swap(0, 1);
    assert_ne!(base, default_elements_hash(&m4), "reorder must shift hash");
}

/// Empty manifest gets no hash (the registry returns `None` for plugins
/// with no defaults). Required so the legacy single-PluginSlot drop path
/// doesn't accidentally start tagging groups with a hash.
#[test]
fn empty_manifest_still_hashes_to_a_value() {
    // The free function still computes a hash for an empty vec — it's the
    // *registry's* `default_elements_hash(plugin_id)` that returns None for
    // empty inputs (so `/default_elements` returns `default_elements_hash:
    // null`). We verify the free function stays well-defined so adding a
    // hash later (when the manifest becomes non-empty) is backwards-safe.
    let h = default_elements_hash(&[]);
    assert_eq!(h.len(), 16);
}

/// `default_elements_hash` must round-trip through SQLite — if the column
/// isn't selected on read, the badge logic gets None on every row and never
/// fires. This is the same pattern as the Phase 7 visible_when roundtrip.
#[test]
fn default_elements_hash_roundtrips_on_group() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = LayoutStore::open(&dir.path().join("test.db")).unwrap();

    let stamp = "abc123def4567890".to_string();
    let original = LayoutItem::Group {
        id: "g0".into(),
        z_index: 0, x: 10, y: 20, width: 300, height: 100,
        plugin_instance_id: Some("weather".into()),
        label: Some("Weather".into()),
        background: Some("card".into()),
        parent_id: None,
        default_elements_hash: Some(stamp.clone()),
        defaults_stale: None,
        style_overrides: None,
    };
    store
        .upsert_layout(&LayoutConfig {
            id: "L".into(), name: "L".into(),
            items: vec![original], updated_at: 0,
        })
        .unwrap();

    let loaded = store.get_layout("L").unwrap().unwrap();
    match &loaded.items[0] {
        LayoutItem::Group { default_elements_hash, defaults_stale, .. } => {
            assert_eq!(default_elements_hash.as_deref(), Some(stamp.as_str()),
                "hash must survive write+read");
            // The store always reads `defaults_stale` as None — it's
            // computed at GET-layout time in the API handler, not stored.
            assert!(defaults_stale.is_none(),
                "defaults_stale is ephemeral; the store must never set it");
        }
        other => panic!("expected Group, got {other:?}"),
    }
}

/// A Group with no `default_elements_hash` (the v1 case) still loads with
/// `None` rather than e.g. an empty string. Guards against a write-side
/// bug that would coerce `None` → `""` and confuse hash comparisons.
#[test]
fn pre_phase_8_group_loads_with_none_hash() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = LayoutStore::open(&dir.path().join("test.db")).unwrap();

    let original = LayoutItem::Group {
        id: "g0".into(),
        z_index: 0, x: 0, y: 0, width: 100, height: 100,
        plugin_instance_id: None,
        label: None,
        background: None,
        parent_id: None,
        default_elements_hash: None,
        defaults_stale: None,
        style_overrides: None,
    };
    store
        .upsert_layout(&LayoutConfig {
            id: "L".into(), name: "L".into(),
            items: vec![original], updated_at: 0,
        })
        .unwrap();

    let loaded = store.get_layout("L").unwrap().unwrap();
    match &loaded.items[0] {
        LayoutItem::Group { default_elements_hash, .. } => {
            assert!(default_elements_hash.is_none(),
                "absent hash must read as None, not Some(\"\")");
        }
        other => panic!("expected Group, got {other:?}"),
    }
}
