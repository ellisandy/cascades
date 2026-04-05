//! Layout store — SQLite-backed persistence for display layout configurations.
//!
//! Replaces the startup-time `display_configs: HashMap<String, DisplayConfiguration>`
//! with a mutable, runtime-editable store backed by two tables:
//!
//! ```sql
//! display_layouts(id TEXT PK, name TEXT, updated_at INTEGER)
//! layout_items(id TEXT PK, layout_id TEXT FK, item_type TEXT,
//!              z_index INT, x INT, y INT, width INT, height INT,
//!              plugin_instance_id TEXT, layout_variant TEXT,
//!              text_content TEXT, font_size INT, orientation TEXT)
//! ```
//!
//! On startup, if the store is empty, it is seeded from `config/display.toml`
//! (backwards-compatible migration).
//!
//! Thread-safe via an internal `Mutex<Connection>`; wrap in `Arc` to share.

use std::{path::Path, sync::Mutex};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::DisplayLayoutsConfig;

// ─── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum LayoutStoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("unknown item_type '{0}'")]
    InvalidItemType(String),
    #[error("I/O error creating directory '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

// ─── Public types ─────────────────────────────────────────────────────────────

/// A named display layout: a list of items ordered back-to-front by `z_index`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutConfig {
    /// Unique layout identifier (e.g. `"default"`, `"trip-planner"`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Items composited back-to-front.
    pub items: Vec<LayoutItem>,
}

/// A single item in a layout.
///
/// Three variants correspond to the three `item_type` values in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutItem {
    /// A plugin slot rendered by the compositor via the sidecar.
    PluginSlot {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        plugin_instance_id: String,
        layout_variant: String,
    },
    /// A static text element rendered directly.
    StaticText {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        text_content: String,
        font_size: i32,
        orientation: Option<String>,
    },
    /// A horizontal or vertical divider line.
    StaticDivider {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        orientation: Option<String>,
    },
}

impl LayoutItem {
    pub fn id(&self) -> &str {
        match self {
            Self::PluginSlot { id, .. } => id,
            Self::StaticText { id, .. } => id,
            Self::StaticDivider { id, .. } => id,
        }
    }

    pub fn z_index(&self) -> i32 {
        match self {
            Self::PluginSlot { z_index, .. } => *z_index,
            Self::StaticText { z_index, .. } => *z_index,
            Self::StaticDivider { z_index, .. } => *z_index,
        }
    }
}

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite-backed store for display layout configurations.
///
/// Thread-safe via an internal `Mutex<Connection>`. Wrap in `Arc` to share
/// across handlers: `Arc<LayoutStore>`.
pub struct LayoutStore {
    conn: Mutex<Connection>,
}

impl LayoutStore {
    /// Open or create the SQLite database at `db_path` and run migrations.
    ///
    /// Safe to open against the same file as [`crate::instance_store::InstanceStore`];
    /// SQLite serialises concurrent writes from separate connections.
    pub fn open(db_path: &Path) -> Result<Self, LayoutStoreError> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| LayoutStoreError::Io {
                path: parent.to_string_lossy().into_owned(),
                source: e,
            })?;
        }
        let conn = Connection::open(db_path)?;
        Self::migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn migrate(conn: &Connection) -> Result<(), LayoutStoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS display_layouts (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS layout_items (
                id                  TEXT PRIMARY KEY,
                layout_id           TEXT NOT NULL,
                item_type           TEXT NOT NULL,
                z_index             INTEGER NOT NULL DEFAULT 0,
                x                   INTEGER NOT NULL DEFAULT 0,
                y                   INTEGER NOT NULL DEFAULT 0,
                width               INTEGER NOT NULL DEFAULT 800,
                height              INTEGER NOT NULL DEFAULT 480,
                plugin_instance_id  TEXT,
                layout_variant      TEXT,
                text_content        TEXT,
                font_size           INTEGER,
                orientation         TEXT
            );",
        )?;
        Ok(())
    }

    /// Returns `true` if at least one layout row exists.
    pub fn has_any_layouts(&self) -> Result<bool, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM display_layouts", [], |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Return all layouts ordered by `id`.
    pub fn list_layouts(&self) -> Result<Vec<LayoutConfig>, LayoutStoreError> {
        let ids: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare("SELECT id FROM display_layouts ORDER BY id")?;
            stmt.query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut result = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Some(layout) = self.get_layout(id)? {
                result.push(layout);
            }
        }
        Ok(result)
    }

    /// Return the layout with `id`, or `None` if not found.
    pub fn get_layout(&self, id: &str) -> Result<Option<LayoutConfig>, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();

        let name: String = {
            let mut stmt =
                conn.prepare("SELECT name FROM display_layouts WHERE id = ?1")?;
            let mut rows = stmt.query(params![id])?;
            match rows.next()? {
                Some(row) => row.get(0)?,
                None => return Ok(None),
            }
        };

        let items = Self::fetch_items(&conn, id)?;

        Ok(Some(LayoutConfig { id: id.to_string(), name, items }))
    }

    fn fetch_items(
        conn: &Connection,
        layout_id: &str,
    ) -> Result<Vec<LayoutItem>, LayoutStoreError> {
        let mut stmt = conn.prepare(
            "SELECT id, item_type, z_index, x, y, width, height,
                    plugin_instance_id, layout_variant, text_content, font_size, orientation
             FROM layout_items
             WHERE layout_id = ?1
             ORDER BY z_index, id",
        )?;

        let rows = stmt.query_map(params![layout_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, i32>(4)?,
                row.get::<_, i32>(5)?,
                row.get::<_, i32>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i32>>(10)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut items = Vec::with_capacity(rows.len());
        for (item_id, item_type, z_index, x, y, width, height,
             plugin_instance_id, layout_variant, text_content, font_size, orientation) in rows
        {
            let item = match item_type.as_str() {
                "plugin_slot" => LayoutItem::PluginSlot {
                    id: item_id,
                    z_index,
                    x,
                    y,
                    width,
                    height,
                    plugin_instance_id: plugin_instance_id.unwrap_or_default(),
                    layout_variant: layout_variant
                        .unwrap_or_else(|| "full".to_string()),
                },
                "static_text" => LayoutItem::StaticText {
                    id: item_id,
                    z_index,
                    x,
                    y,
                    width,
                    height,
                    text_content: text_content.unwrap_or_default(),
                    font_size: font_size.unwrap_or(16),
                    orientation,
                },
                "static_divider" => LayoutItem::StaticDivider {
                    id: item_id,
                    z_index,
                    x,
                    y,
                    width,
                    height,
                    orientation,
                },
                other => {
                    return Err(LayoutStoreError::InvalidItemType(other.to_string()))
                }
            };
            items.push(item);
        }
        Ok(items)
    }

    /// Insert or fully replace a layout (atomically replaces all items).
    pub fn upsert_layout(&self, layout: &LayoutConfig) -> Result<(), LayoutStoreError> {
        let now = unix_now();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        tx.execute(
            "INSERT INTO display_layouts (id, name, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, updated_at = excluded.updated_at",
            params![layout.id, layout.name, now],
        )?;

        tx.execute(
            "DELETE FROM layout_items WHERE layout_id = ?1",
            params![layout.id],
        )?;

        for item in &layout.items {
            match item {
                LayoutItem::PluginSlot {
                    id, z_index, x, y, width, height, plugin_instance_id, layout_variant,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          plugin_instance_id, layout_variant)
                         VALUES (?1, ?2, 'plugin_slot', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            plugin_instance_id, layout_variant
                        ],
                    )?;
                }
                LayoutItem::StaticText {
                    id, z_index, x, y, width, height, text_content, font_size, orientation,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          text_content, font_size, orientation)
                         VALUES (?1, ?2, 'static_text', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            text_content, font_size, orientation
                        ],
                    )?;
                }
                LayoutItem::StaticDivider {
                    id, z_index, x, y, width, height, orientation,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height, orientation)
                         VALUES (?1, ?2, 'static_divider', ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![id, layout.id, z_index, x, y, width, height, orientation],
                    )?;
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Seed from TOML display config.  No-op if layouts already exist.
    ///
    /// Converts each `[[display]]` entry to a `LayoutConfig` with
    /// `PluginSlot` items, preserving the slot order as `z_index`.
    pub fn seed_from_toml(
        &self,
        toml_layouts: &DisplayLayoutsConfig,
    ) -> Result<(), LayoutStoreError> {
        if self.has_any_layouts()? {
            return Ok(());
        }

        for entry in &toml_layouts.displays {
            let mut items = Vec::new();
            for (j, slot) in entry.slots.iter().enumerate() {
                let (default_w, default_h) = variant_canonical_dims(&slot.variant);
                items.push(LayoutItem::PluginSlot {
                    id: format!("{}-slot-{}", entry.name, j),
                    z_index: j as i32,
                    x: slot.x.unwrap_or(0) as i32,
                    y: slot.y.unwrap_or(0) as i32,
                    width: slot.width.unwrap_or(default_w) as i32,
                    height: slot.height.unwrap_or(default_h) as i32,
                    plugin_instance_id: slot.plugin.clone(),
                    layout_variant: slot.variant.clone(),
                });
            }
            let layout = LayoutConfig {
                id: entry.name.clone(),
                name: entry.name.clone(),
                items,
            };
            self.upsert_layout(&layout)?;
        }

        Ok(())
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn variant_canonical_dims(variant: &str) -> (u32, u32) {
    match variant {
        "full" => (800, 480),
        "half_horizontal" => (800, 240),
        "half_vertical" => (400, 480),
        "quadrant" => (400, 240),
        _ => (800, 480),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_store() -> (LayoutStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let store = LayoutStore::open(&path).unwrap();
        (store, dir)
    }

    fn plugin_slot(id: &str, plugin: &str, z: i32) -> LayoutItem {
        LayoutItem::PluginSlot {
            id: id.to_string(),
            z_index: z,
            x: 0,
            y: 0,
            width: 800,
            height: 480,
            plugin_instance_id: plugin.to_string(),
            layout_variant: "full".to_string(),
        }
    }

    #[test]
    fn open_creates_tables() {
        let (store, _dir) = open_store();
        assert!(!store.has_any_layouts().unwrap());
    }

    #[test]
    fn upsert_and_get_layout() {
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "default".to_string(),
            name: "Default".to_string(),
            items: vec![plugin_slot("s0", "river", 0)],
        };
        store.upsert_layout(&layout).unwrap();

        let got = store.get_layout("default").unwrap().unwrap();
        assert_eq!(got.id, "default");
        assert_eq!(got.name, "Default");
        assert_eq!(got.items.len(), 1);
        assert!(matches!(&got.items[0], LayoutItem::PluginSlot { plugin_instance_id, .. } if plugin_instance_id == "river"));
    }

    #[test]
    fn get_layout_returns_none_for_missing_id() {
        let (store, _dir) = open_store();
        assert!(store.get_layout("nonexistent").unwrap().is_none());
    }

    #[test]
    fn upsert_replaces_existing_items() {
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "default".to_string(),
            name: "Default".to_string(),
            items: vec![plugin_slot("s0", "river", 0)],
        };
        store.upsert_layout(&layout).unwrap();

        let updated = LayoutConfig {
            id: "default".to_string(),
            name: "Default v2".to_string(),
            items: vec![
                plugin_slot("s0", "weather", 0),
                plugin_slot("s1", "river", 1),
            ],
        };
        store.upsert_layout(&updated).unwrap();

        let got = store.get_layout("default").unwrap().unwrap();
        assert_eq!(got.name, "Default v2");
        assert_eq!(got.items.len(), 2);
    }

    #[test]
    fn has_any_layouts_returns_true_after_insert() {
        let (store, _dir) = open_store();
        assert!(!store.has_any_layouts().unwrap());

        store.upsert_layout(&LayoutConfig {
            id: "x".to_string(),
            name: "X".to_string(),
            items: vec![],
        }).unwrap();

        assert!(store.has_any_layouts().unwrap());
    }

    #[test]
    fn list_layouts_returns_all() {
        let (store, _dir) = open_store();
        for name in &["alpha", "beta", "gamma"] {
            store.upsert_layout(&LayoutConfig {
                id: name.to_string(),
                name: name.to_string(),
                items: vec![plugin_slot(&format!("{}-s0", name), "river", 0)],
            }).unwrap();
        }
        let all = store.list_layouts().unwrap();
        assert_eq!(all.len(), 3);
        // Ordered by id
        assert_eq!(all[0].id, "alpha");
        assert_eq!(all[2].id, "gamma");
    }

    #[test]
    fn seed_from_toml_populates_empty_store() {
        use crate::config::{DisplayConfigEntry, DisplayLayoutsConfig, DisplaySlotEntry};
        let (store, _dir) = open_store();
        let toml = DisplayLayoutsConfig {
            displays: vec![
                DisplayConfigEntry {
                    name: "default".to_string(),
                    slots: vec![DisplaySlotEntry {
                        plugin: "river".to_string(),
                        x: None,
                        y: None,
                        width: None,
                        height: None,
                        variant: "full".to_string(),
                    }],
                },
            ],
        };
        store.seed_from_toml(&toml).unwrap();
        assert!(store.has_any_layouts().unwrap());

        let layout = store.get_layout("default").unwrap().unwrap();
        assert_eq!(layout.items.len(), 1);
        if let LayoutItem::PluginSlot { plugin_instance_id, width, height, .. } = &layout.items[0] {
            assert_eq!(plugin_instance_id, "river");
            assert_eq!(*width, 800);
            assert_eq!(*height, 480);
        } else {
            panic!("expected PluginSlot");
        }
    }

    #[test]
    fn seed_from_toml_is_noop_when_layouts_exist() {
        use crate::config::{DisplayConfigEntry, DisplayLayoutsConfig, DisplaySlotEntry};
        let (store, _dir) = open_store();

        store.upsert_layout(&LayoutConfig {
            id: "existing".to_string(),
            name: "Existing".to_string(),
            items: vec![],
        }).unwrap();

        let toml = DisplayLayoutsConfig {
            displays: vec![DisplayConfigEntry {
                name: "new".to_string(),
                slots: vec![DisplaySlotEntry {
                    plugin: "river".to_string(),
                    x: None, y: None, width: None, height: None,
                    variant: "full".to_string(),
                }],
            }],
        };

        store.seed_from_toml(&toml).unwrap();

        // "new" was NOT seeded because layouts already existed.
        assert!(store.get_layout("new").unwrap().is_none());
        assert!(store.get_layout("existing").unwrap().is_some());
    }

    #[test]
    fn items_ordered_by_z_index() {
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "multi".to_string(),
            name: "Multi".to_string(),
            items: vec![
                plugin_slot("s2", "ferry", 2),
                plugin_slot("s0", "river", 0),
                plugin_slot("s1", "weather", 1),
            ],
        };
        store.upsert_layout(&layout).unwrap();
        let got = store.get_layout("multi").unwrap().unwrap();
        assert_eq!(got.items[0].z_index(), 0);
        assert_eq!(got.items[1].z_index(), 1);
        assert_eq!(got.items[2].z_index(), 2);
    }

    #[test]
    fn static_text_and_divider_roundtrip() {
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "static".to_string(),
            name: "Static".to_string(),
            items: vec![
                LayoutItem::StaticText {
                    id: "t0".to_string(),
                    z_index: 0,
                    x: 10, y: 20, width: 200, height: 50,
                    text_content: "Hello".to_string(),
                    font_size: 24,
                    orientation: None,
                },
                LayoutItem::StaticDivider {
                    id: "d0".to_string(),
                    z_index: 1,
                    x: 0, y: 240, width: 800, height: 2,
                    orientation: Some("horizontal".to_string()),
                },
            ],
        };
        store.upsert_layout(&layout).unwrap();
        let got = store.get_layout("static").unwrap().unwrap();
        assert_eq!(got.items.len(), 2);
        assert!(matches!(&got.items[0], LayoutItem::StaticText { text_content, .. } if text_content == "Hello"));
        assert!(matches!(&got.items[1], LayoutItem::StaticDivider { orientation: Some(o), .. } if o == "horizontal"));
    }

    #[test]
    fn layout_item_serde_roundtrip() {
        let item = LayoutItem::PluginSlot {
            id: "s0".to_string(),
            z_index: 0,
            x: 0, y: 0, width: 800, height: 480,
            plugin_instance_id: "river".to_string(),
            layout_variant: "full".to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let decoded: LayoutItem = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, LayoutItem::PluginSlot { plugin_instance_id, .. } if plugin_instance_id == "river"));
    }
}
