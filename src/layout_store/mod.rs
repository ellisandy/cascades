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
    /// Unix timestamp (seconds) of last write.  Zero for layouts not yet persisted.
    #[serde(default)]
    pub updated_at: i64,
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
    /// A static date/time element rendered via the sidecar.
    #[serde(rename = "static_datetime")]
    StaticDateTime {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        font_size: i32,
        format: Option<String>,
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
    /// A data field extracted from a data source via JSONPath.
    DataField {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        field_mapping_id: String,
        font_size: i32,
        format_string: String,
        label: Option<String>,
        orientation: Option<String>,
    },
}

impl LayoutItem {
    pub fn id(&self) -> &str {
        match self {
            Self::PluginSlot { id, .. } => id,
            Self::StaticText { id, .. } => id,
            Self::StaticDateTime { id, .. } => id,
            Self::StaticDivider { id, .. } => id,
            Self::DataField { id, .. } => id,
        }
    }

    pub fn z_index(&self) -> i32 {
        match self {
            Self::PluginSlot { z_index, .. } => *z_index,
            Self::StaticText { z_index, .. } => *z_index,
            Self::StaticDateTime { z_index, .. } => *z_index,
            Self::StaticDivider { z_index, .. } => *z_index,
            Self::DataField { z_index, .. } => *z_index,
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
            );
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS data_source_fields (
                id              TEXT PRIMARY KEY,
                data_source_id  TEXT NOT NULL,
                source_type     TEXT NOT NULL DEFAULT 'generic',
                name            TEXT NOT NULL,
                json_path       TEXT NOT NULL,
                created_at      INTEGER NOT NULL
            );",
        )?;

        // Add columns for DataField items (SQLite ignores if already present).
        for col in &[
            "ALTER TABLE layout_items ADD COLUMN field_mapping_id TEXT",
            "ALTER TABLE layout_items ADD COLUMN format_string TEXT",
            "ALTER TABLE layout_items ADD COLUMN label TEXT",
        ] {
            // "duplicate column name" is harmless — the column already exists.
            match conn.execute_batch(col) {
                Ok(()) => {}
                Err(e) if e.to_string().contains("duplicate column") => {}
                Err(e) => return Err(e.into()),
            }
        }

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

        let (name, updated_at): (String, i64) = {
            let mut stmt =
                conn.prepare("SELECT name, updated_at FROM display_layouts WHERE id = ?1")?;
            let mut rows = stmt.query(params![id])?;
            match rows.next()? {
                Some(row) => (row.get(0)?, row.get(1)?),
                None => return Ok(None),
            }
        };

        let items = Self::fetch_items(&conn, id)?;

        Ok(Some(LayoutConfig { id: id.to_string(), name, items, updated_at }))
    }

    fn fetch_items(
        conn: &Connection,
        layout_id: &str,
    ) -> Result<Vec<LayoutItem>, LayoutStoreError> {
        let mut stmt = conn.prepare(
            "SELECT id, item_type, z_index, x, y, width, height,
                    plugin_instance_id, layout_variant, text_content, font_size, orientation,
                    field_mapping_id, format_string, label
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
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut items = Vec::with_capacity(rows.len());
        for (item_id, item_type, z_index, x, y, width, height,
             plugin_instance_id, layout_variant, text_content, font_size, orientation,
             field_mapping_id, format_string, label) in rows
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
                "static_datetime" => LayoutItem::StaticDateTime {
                    id: item_id,
                    z_index,
                    x,
                    y,
                    width,
                    height,
                    font_size: font_size.unwrap_or(16),
                    format: text_content,
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
                "data_field" => LayoutItem::DataField {
                    id: item_id,
                    z_index,
                    x,
                    y,
                    width,
                    height,
                    field_mapping_id: field_mapping_id.unwrap_or_default(),
                    font_size: font_size.unwrap_or(16),
                    format_string: format_string.unwrap_or_default(),
                    label,
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
                LayoutItem::StaticDateTime {
                    id, z_index, x, y, width, height, font_size, format, orientation,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          text_content, font_size, orientation)
                         VALUES (?1, ?2, 'static_datetime', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            format, font_size, orientation
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
                LayoutItem::DataField {
                    id, z_index, x, y, width, height,
                    field_mapping_id, font_size, format_string, label, orientation,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          field_mapping_id, font_size, format_string, label, orientation)
                         VALUES (?1, ?2, 'data_field', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            field_mapping_id, font_size, format_string, label, orientation
                        ],
                    )?;
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Delete a layout by ID (and all its items).
    pub fn delete_layout(&self, id: &str) -> Result<(), LayoutStoreError> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;

        // Delete all items for this layout
        tx.execute(
            "DELETE FROM layout_items WHERE layout_id = ?1",
            params![id],
        )?;

        // Delete the layout itself
        tx.execute(
            "DELETE FROM display_layouts WHERE id = ?1",
            params![id],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Get the active layout ID (the layout served by `GET /image.png`).
    pub fn get_active_layout_id(&self) -> Result<Option<String>, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT value FROM settings WHERE key = 'active_layout_id'")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Set the active layout ID.
    pub fn set_active_layout_id(&self, id: &str) -> Result<(), LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('active_layout_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![id],
        )?;
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
                updated_at: 0,
            };
            self.upsert_layout(&layout)?;
        }

        Ok(())
    }
}

/// A field mapping that extracts a single value from a data source via JSONPath.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMapping {
    pub id: String,
    pub data_source_id: String,
    pub source_type: String,
    pub name: String,
    pub json_path: String,
    pub created_at: i64,
}

impl LayoutStore {
    /// Create a new field mapping.
    pub fn create_field_mapping(&self, mapping: &FieldMapping) -> Result<(), LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO data_source_fields (id, data_source_id, source_type, name, json_path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                mapping.id,
                mapping.data_source_id,
                mapping.source_type,
                mapping.name,
                mapping.json_path,
                mapping.created_at,
            ],
        )?;
        Ok(())
    }

    /// Get a field mapping by ID.
    pub fn get_field_mapping(&self, id: &str) -> Result<Option<FieldMapping>, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, data_source_id, source_type, name, json_path, created_at
             FROM data_source_fields WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(FieldMapping {
                id: row.get(0)?,
                data_source_id: row.get(1)?,
                source_type: row.get(2)?,
                name: row.get(3)?,
                json_path: row.get(4)?,
                created_at: row.get(5)?,
            })),
            None => Ok(None),
        }
    }

    /// List all field mappings for a given data source.
    pub fn list_field_mappings(
        &self,
        data_source_id: &str,
    ) -> Result<Vec<FieldMapping>, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, data_source_id, source_type, name, json_path, created_at
             FROM data_source_fields WHERE data_source_id = ?1
             ORDER BY name",
        )?;
        let rows = stmt.query_map(params![data_source_id], |row| {
            Ok(FieldMapping {
                id: row.get(0)?,
                data_source_id: row.get(1)?,
                source_type: row.get(2)?,
                name: row.get(3)?,
                json_path: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Update a field mapping (name and json_path).
    pub fn update_field_mapping(
        &self,
        id: &str,
        name: &str,
        json_path: &str,
    ) -> Result<bool, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE data_source_fields SET name = ?1, json_path = ?2 WHERE id = ?3",
            params![name, json_path, id],
        )?;
        Ok(updated > 0)
    }

    /// Delete a field mapping by ID.
    pub fn delete_field_mapping(&self, id: &str) -> Result<bool, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM data_source_fields WHERE id = ?1",
            params![id],
        )?;
        Ok(deleted > 0)
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
            updated_at: 0,
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
            updated_at: 0,
        };
        store.upsert_layout(&layout).unwrap();

        let updated = LayoutConfig {
            id: "default".to_string(),
            name: "Default v2".to_string(),
            items: vec![
                plugin_slot("s0", "weather", 0),
                plugin_slot("s1", "river", 1),
            ],
            updated_at: 0,
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
            updated_at: 0,
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
                updated_at: 0,
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
            updated_at: 0,
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
            updated_at: 0,
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
            updated_at: 0,
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

    #[test]
    fn data_field_roundtrip() {
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "df-test".to_string(),
            name: "DataField Test".to_string(),
            items: vec![
                LayoutItem::DataField {
                    id: "df-0".to_string(),
                    z_index: 0,
                    x: 10, y: 20, width: 200, height: 40,
                    field_mapping_id: "fm-123".to_string(),
                    font_size: 24,
                    format_string: "{{value}} ft".to_string(),
                    label: Some("Water Level".to_string()),
                    orientation: None,
                },
            ],
            updated_at: 0,
        };
        store.upsert_layout(&layout).unwrap();
        let got = store.get_layout("df-test").unwrap().unwrap();
        assert_eq!(got.items.len(), 1);
        match &got.items[0] {
            LayoutItem::DataField { field_mapping_id, format_string, label, font_size, .. } => {
                assert_eq!(field_mapping_id, "fm-123");
                assert_eq!(format_string, "{{value}} ft");
                assert_eq!(label.as_deref(), Some("Water Level"));
                assert_eq!(*font_size, 24);
            }
            other => panic!("expected DataField, got {:?}", other),
        }
    }

    #[test]
    fn field_mapping_crud() {
        let (store, _dir) = open_store();

        // Create
        let mapping = FieldMapping {
            id: "fm-1".to_string(),
            data_source_id: "river".to_string(),
            source_type: "builtin".to_string(),
            name: "Water Level".to_string(),
            json_path: "$.water_level_ft".to_string(),
            created_at: 1000,
        };
        store.create_field_mapping(&mapping).unwrap();

        // Get
        let got = store.get_field_mapping("fm-1").unwrap().unwrap();
        assert_eq!(got.name, "Water Level");
        assert_eq!(got.json_path, "$.water_level_ft");

        // List
        let list = store.list_field_mappings("river").unwrap();
        assert_eq!(list.len(), 1);

        // Update
        assert!(store.update_field_mapping("fm-1", "River Level", "$.level").unwrap());
        let updated = store.get_field_mapping("fm-1").unwrap().unwrap();
        assert_eq!(updated.name, "River Level");
        assert_eq!(updated.json_path, "$.level");

        // Delete
        assert!(store.delete_field_mapping("fm-1").unwrap());
        assert!(store.get_field_mapping("fm-1").unwrap().is_none());
        assert!(!store.delete_field_mapping("fm-1").unwrap()); // already deleted
    }

    #[test]
    fn active_layout_id_defaults_to_none() {
        let (store, _dir) = open_store();
        assert!(store.get_active_layout_id().unwrap().is_none());
    }

    #[test]
    fn set_and_get_active_layout_id() {
        let (store, _dir) = open_store();
        store.set_active_layout_id("my-layout").unwrap();
        assert_eq!(store.get_active_layout_id().unwrap().unwrap(), "my-layout");
    }

    #[test]
    fn set_active_layout_id_overwrites_previous() {
        let (store, _dir) = open_store();
        store.set_active_layout_id("first").unwrap();
        store.set_active_layout_id("second").unwrap();
        assert_eq!(store.get_active_layout_id().unwrap().unwrap(), "second");
    }
}
