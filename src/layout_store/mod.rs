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
use crate::visible_when::VisibleWhen;

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
/// Variants correspond to the `item_type` values in the database. `parent_id`
/// (optional on every variant) links an item to a container [`LayoutItem::Group`];
/// a null `parent_id` means the item is at the layout root.
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        /// Phase 7: optional conditional-rendering clause. `None` → always render.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visible_when: Option<VisibleWhen>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bold: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        italic: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        underline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font_family: Option<String>,
        /// CSS hex color, e.g. "#ff0000". `None` → compositor default (`#000`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        /// Phase 7: optional conditional-rendering clause. `None` → always render.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visible_when: Option<VisibleWhen>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bold: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        italic: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        underline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font_family: Option<String>,
        /// CSS hex color, e.g. "#ff0000". `None` → compositor default (`#000`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        /// Phase 7: optional conditional-rendering clause. `None` → always render.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visible_when: Option<VisibleWhen>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        /// Phase 7: optional conditional-rendering clause. `None` → always render.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visible_when: Option<VisibleWhen>,
    },
    /// A data field that extracts a value from cached source data via JSONPath.
    DataField {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        /// References data_source_fields.id
        field_mapping_id: String,
        font_size: i32,
        /// Format string with `{{value}}` placeholder.
        format_string: String,
        /// Optional label displayed above/before the value.
        label: Option<String>,
        orientation: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bold: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        italic: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        underline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font_family: Option<String>,
        /// CSS hex color, e.g. "#ff0000". `None` → compositor default (`#000`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        /// Phase 7: optional conditional-rendering clause. `None` → always render.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visible_when: Option<VisibleWhen>,
    },
    /// A user-uploaded image asset placed at a fixed rectangle. The asset's
    /// raw bytes live in the [AssetStore](crate::asset_store::AssetStore);
    /// `asset_id` is the foreign key. Compositor stretches the decoded image
    /// to (`width`, `height`); aspect-preserving fit is a deferred follow-up.
    Image {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        /// References [Asset::id](crate::asset_store::Asset::id).
        asset_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        /// Phase 7: optional conditional-rendering clause. `None` → always render.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visible_when: Option<VisibleWhen>,
    },
    /// A data-driven icon: extracts a string value from cached source data via
    /// a [`FieldMapping`], then looks that value up in `icon_map` to choose
    /// which [`Asset`](crate::asset_store::Asset) to render. Used for
    /// weather-condition icons, route badges, etc.
    ///
    /// Falls back to a blank rectangle if the extracted value isn't a key in
    /// `icon_map` or the chosen asset is missing — same defensive contract as
    /// [`LayoutItem::Image`].
    DataIcon {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        /// References data_source_fields.id (same as DataField).
        field_mapping_id: String,
        /// Map of extracted value → asset id. Stored as JSON in
        /// `icon_map_json` column.
        icon_map: std::collections::HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        /// Phase 7: optional conditional-rendering clause. `None` → always render.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visible_when: Option<VisibleWhen>,
    },
    /// A container item whose children's `parent_id` points at its `id`.
    ///
    /// Groups have geometry (a visual frame) and an optional background, but
    /// no content of their own. Child coordinates are canvas-absolute; the
    /// parent's rectangle is a visual hint, not a clip region.
    Group {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        /// Plugin instance binding — when set, this is a "plugin group"
        /// whose descendants are the decomposed elements of that instance.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugin_instance_id: Option<String>,
        /// Human label shown in the outliner / z-order list.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Background mode: `"none"`, `"card"`, or `"plugin_chrome"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
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
            Self::Image { id, .. } => id,
            Self::DataIcon { id, .. } => id,
            Self::Group { id, .. } => id,
        }
    }

    pub fn z_index(&self) -> i32 {
        match self {
            Self::PluginSlot { z_index, .. } => *z_index,
            Self::StaticText { z_index, .. } => *z_index,
            Self::StaticDateTime { z_index, .. } => *z_index,
            Self::StaticDivider { z_index, .. } => *z_index,
            Self::DataField { z_index, .. } => *z_index,
            Self::Image { z_index, .. } => *z_index,
            Self::DataIcon { z_index, .. } => *z_index,
            Self::Group { z_index, .. } => *z_index,
        }
    }

    /// Returns the `parent_id` of this item if it is nested inside a [`LayoutItem::Group`].
    pub fn parent_id(&self) -> Option<&str> {
        match self {
            Self::PluginSlot { parent_id, .. } => parent_id.as_deref(),
            Self::StaticText { parent_id, .. } => parent_id.as_deref(),
            Self::StaticDateTime { parent_id, .. } => parent_id.as_deref(),
            Self::StaticDivider { parent_id, .. } => parent_id.as_deref(),
            Self::DataField { parent_id, .. } => parent_id.as_deref(),
            Self::Image { parent_id, .. } => parent_id.as_deref(),
            Self::DataIcon { parent_id, .. } => parent_id.as_deref(),
            Self::Group { parent_id, .. } => parent_id.as_deref(),
        }
    }

    /// Phase 7: optional conditional-rendering clause. `Group` items don't
    /// carry one — hiding a group separately from its children is a different
    /// (cascading) feature deliberately deferred from v1 scope.
    pub fn visible_when(&self) -> Option<&VisibleWhen> {
        match self {
            Self::PluginSlot { visible_when, .. } => visible_when.as_ref(),
            Self::StaticText { visible_when, .. } => visible_when.as_ref(),
            Self::StaticDateTime { visible_when, .. } => visible_when.as_ref(),
            Self::StaticDivider { visible_when, .. } => visible_when.as_ref(),
            Self::DataField { visible_when, .. } => visible_when.as_ref(),
            Self::Image { visible_when, .. } => visible_when.as_ref(),
            Self::DataIcon { visible_when, .. } => visible_when.as_ref(),
            Self::Group { .. } => None,
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

        // Add nullable columns for later variants. SQLite ignores if already present.
        // `parent_id` + `background` support the Group variant introduced in Phase 1
        // of the layout composer; existing `plugin_instance_id` is reused for a
        // Group's optional plugin binding.
        let columns = [
            ("field_mapping_id", "TEXT"),
            ("format_string", "TEXT"),
            ("label", "TEXT"),
            ("bold", "INTEGER"),
            ("italic", "INTEGER"),
            ("underline", "INTEGER"),
            ("font_family", "TEXT"),
            ("parent_id", "TEXT"),
            ("background", "TEXT"),
            // Phase 5: per-item foreground color (CSS hex, e.g. "#ff0000").
            ("color", "TEXT"),
            // Phase 6: foreign key into assets.id for LayoutItem::Image.
            ("asset_id", "TEXT"),
            // Phase 7: serialized VisibleWhen clause (`{path, op, value}`)
            // or NULL when the item has no condition.
            ("visible_when_json", "TEXT"),
            // Phase 7: serialized HashMap<String,String> of value → asset_id
            // for LayoutItem::DataIcon.
            ("icon_map_json", "TEXT"),
        ];
        for (col, typ) in &columns {
            let sql = format!("ALTER TABLE layout_items ADD COLUMN {col} {typ}");
            // SQLite returns an error if the column already exists; ignore it.
            let _ = conn.execute(&sql, []);
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
                    field_mapping_id, format_string, label,
                    bold, italic, underline, font_family,
                    parent_id, background, color, asset_id,
                    visible_when_json, icon_map_json
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
                row.get::<_, Option<i32>>(15)?,
                row.get::<_, Option<i32>>(16)?,
                row.get::<_, Option<i32>>(17)?,
                row.get::<_, Option<String>>(18)?,
                row.get::<_, Option<String>>(19)?,
                row.get::<_, Option<String>>(20)?,
                row.get::<_, Option<String>>(21)?,
                row.get::<_, Option<String>>(22)?,
                row.get::<_, Option<String>>(23)?,
                row.get::<_, Option<String>>(24)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut items = Vec::with_capacity(rows.len());
        for (item_id, item_type, z_index, x, y, width, height,
             plugin_instance_id, layout_variant, text_content, font_size, orientation,
             field_mapping_id, format_string, label,
             bold, italic, underline, font_family,
             parent_id, background, color, asset_id,
             visible_when_json, icon_map_json) in rows
        {
            let bold = bold.map(|v| v != 0);
            let italic = italic.map(|v| v != 0);
            let underline = underline.map(|v| v != 0);
            // Parse visible_when JSON; treat malformed as None and log so the
            // compositor doesn't panic on a hand-edited DB row. The item then
            // renders unconditionally (treating "nothing" as "always show").
            let visible_when: Option<VisibleWhen> = visible_when_json.as_deref()
                .and_then(|s| match serde_json::from_str::<VisibleWhen>(s) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        log::warn!("layout '{layout_id}' item '{item_id}': bad visible_when_json: {e}");
                        None
                    }
                });
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
                    parent_id,
                    visible_when,
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
                    bold,
                    italic,
                    underline,
                    font_family,
                    color: color.clone(),
                    parent_id,
                    visible_when,
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
                    bold,
                    italic,
                    underline,
                    font_family,
                    color: color.clone(),
                    parent_id,
                    visible_when,
                },
                "static_divider" => LayoutItem::StaticDivider {
                    id: item_id,
                    z_index,
                    x,
                    y,
                    width,
                    height,
                    orientation,
                    parent_id,
                    visible_when,
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
                    format_string: format_string
                        .unwrap_or_else(|| "{{value}}".to_string()),
                    label,
                    orientation,
                    bold,
                    italic,
                    underline,
                    font_family,
                    color,
                    parent_id,
                    visible_when,
                },
                "image" => LayoutItem::Image {
                    id: item_id,
                    z_index,
                    x,
                    y,
                    width,
                    height,
                    // Should always be present for image rows; default to ""
                    // for forward-compat / damaged rows so the read path
                    // doesn't panic.
                    asset_id: asset_id.unwrap_or_default(),
                    parent_id,
                    visible_when,
                },
                "data_icon" => {
                    // icon_map_json missing or malformed → empty map (renders
                    // as blank rect for any value, same defensive contract as
                    // missing-asset). Logs but doesn't fail the read.
                    let icon_map = icon_map_json
                        .as_deref()
                        .and_then(|s| match serde_json::from_str::<std::collections::HashMap<String, String>>(s) {
                            Ok(m) => Some(m),
                            Err(e) => {
                                log::warn!(
                                    "layout '{layout_id}' item '{item_id}': bad icon_map_json: {e}",
                                );
                                None
                            }
                        })
                        .unwrap_or_default();
                    LayoutItem::DataIcon {
                        id: item_id,
                        z_index,
                        x,
                        y,
                        width,
                        height,
                        field_mapping_id: field_mapping_id.unwrap_or_default(),
                        icon_map,
                        parent_id,
                        visible_when,
                    }
                }
                "group" => LayoutItem::Group {
                    id: item_id,
                    z_index,
                    x,
                    y,
                    width,
                    height,
                    plugin_instance_id,
                    label,
                    background,
                    parent_id,
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
            // Phase 7: serialize the optional VisibleWhen clause once per
            // item and reuse on every INSERT branch. None → NULL column.
            let vw_json: Option<String> = item
                .visible_when()
                .map(|v| serde_json::to_string(v).expect("VisibleWhen always serialises"));
            match item {
                LayoutItem::PluginSlot {
                    id, z_index, x, y, width, height, plugin_instance_id, layout_variant,
                    parent_id, visible_when: _,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          plugin_instance_id, layout_variant, parent_id, visible_when_json)
                         VALUES (?1, ?2, 'plugin_slot', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            plugin_instance_id, layout_variant, parent_id, vw_json
                        ],
                    )?;
                }
                LayoutItem::StaticText {
                    id, z_index, x, y, width, height, text_content, font_size, orientation,
                    bold, italic, underline, font_family, color, parent_id, visible_when: _,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          text_content, font_size, orientation,
                          bold, italic, underline, font_family, color, parent_id,
                          visible_when_json)
                         VALUES (?1, ?2, 'static_text', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                                 ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            text_content, font_size, orientation,
                            bold.map(|b| b as i32),
                            italic.map(|b| b as i32),
                            underline.map(|b| b as i32),
                            font_family,
                            color,
                            parent_id,
                            vw_json,
                        ],
                    )?;
                }
                LayoutItem::StaticDateTime {
                    id, z_index, x, y, width, height, font_size, format, orientation,
                    bold, italic, underline, font_family, color, parent_id, visible_when: _,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          text_content, font_size, orientation,
                          bold, italic, underline, font_family, color, parent_id,
                          visible_when_json)
                         VALUES (?1, ?2, 'static_datetime', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                                 ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            format, font_size, orientation,
                            bold.map(|b| b as i32),
                            italic.map(|b| b as i32),
                            underline.map(|b| b as i32),
                            font_family,
                            color,
                            parent_id,
                            vw_json,
                        ],
                    )?;
                }
                LayoutItem::StaticDivider {
                    id, z_index, x, y, width, height, orientation, parent_id, visible_when: _,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          orientation, parent_id, visible_when_json)
                         VALUES (?1, ?2, 'static_divider', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![id, layout.id, z_index, x, y, width, height,
                                orientation, parent_id, vw_json],
                    )?;
                }
                LayoutItem::DataField {
                    id, z_index, x, y, width, height,
                    field_mapping_id, font_size, format_string, label, orientation,
                    bold, italic, underline, font_family, color, parent_id, visible_when: _,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          field_mapping_id, font_size, format_string, label, orientation,
                          bold, italic, underline, font_family, color, parent_id,
                          visible_when_json)
                         VALUES (?1, ?2, 'data_field', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                                 ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            field_mapping_id, font_size, format_string, label, orientation,
                            bold.map(|b| b as i32),
                            italic.map(|b| b as i32),
                            underline.map(|b| b as i32),
                            font_family,
                            color,
                            parent_id,
                            vw_json,
                        ],
                    )?;
                }
                LayoutItem::Image {
                    id, z_index, x, y, width, height, asset_id, parent_id, visible_when: _,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          asset_id, parent_id, visible_when_json)
                         VALUES (?1, ?2, 'image', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            asset_id, parent_id, vw_json,
                        ],
                    )?;
                }
                LayoutItem::DataIcon {
                    id, z_index, x, y, width, height,
                    field_mapping_id, icon_map, parent_id, visible_when: _,
                } => {
                    let icon_map_json = serde_json::to_string(icon_map)
                        .expect("HashMap<String,String> always serialises");
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          field_mapping_id, icon_map_json, parent_id, visible_when_json)
                         VALUES (?1, ?2, 'data_icon', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            field_mapping_id, icon_map_json, parent_id, vw_json,
                        ],
                    )?;
                }
                LayoutItem::Group {
                    id, z_index, x, y, width, height,
                    plugin_instance_id, label, background, parent_id,
                } => {
                    tx.execute(
                        "INSERT INTO layout_items
                         (id, layout_id, item_type, z_index, x, y, width, height,
                          plugin_instance_id, label, background, parent_id)
                         VALUES (?1, ?2, 'group', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            id, layout.id, z_index, x, y, width, height,
                            plugin_instance_id, label, background, parent_id,
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
                    parent_id: None,
                    visible_when: None,
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

    // ─── Field-mapping CRUD ──────────────────────────────────────────────────

    /// Create a new field mapping. Returns the created mapping.
    pub fn create_field_mapping(
        &self,
        id: &str,
        data_source_id: &str,
        source_type: &str,
        name: &str,
        json_path: &str,
    ) -> Result<FieldMapping, LayoutStoreError> {
        let now = unix_now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO data_source_fields (id, data_source_id, source_type, name, json_path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, data_source_id, source_type, name, json_path, now],
        )?;
        Ok(FieldMapping {
            id: id.to_string(),
            data_source_id: data_source_id.to_string(),
            source_type: source_type.to_string(),
            name: name.to_string(),
            json_path: json_path.to_string(),
            created_at: now,
        })
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

    /// List all field mappings for a data source.
    pub fn list_field_mappings(
        &self,
        data_source_id: &str,
    ) -> Result<Vec<FieldMapping>, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, data_source_id, source_type, name, json_path, created_at
             FROM data_source_fields WHERE data_source_id = ?1 ORDER BY name",
        )?;
        let mappings = stmt
            .query_map(params![data_source_id], |row| {
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
        Ok(mappings)
    }

    /// Update a field mapping's name and/or json_path.  Returns the updated row,
    /// or `None` if the ID doesn't exist.
    pub fn update_field_mapping(
        &self,
        id: &str,
        name: Option<&str>,
        json_path: Option<&str>,
    ) -> Result<Option<FieldMapping>, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        if let Some(name) = name {
            conn.execute(
                "UPDATE data_source_fields SET name = ?1 WHERE id = ?2",
                params![name, id],
            )?;
        }
        if let Some(json_path) = json_path {
            conn.execute(
                "UPDATE data_source_fields SET json_path = ?1 WHERE id = ?2",
                params![json_path, id],
            )?;
        }
        drop(conn);
        self.get_field_mapping(id)
    }

    /// Upsert a field mapping keyed by `(data_source_id, json_path)`.
    ///
    /// If a mapping with that pair exists, its `name` is refreshed (if different)
    /// and the existing row is returned unchanged. Otherwise a new row is inserted
    /// with `new_id` as its primary key. This is the bootstrap path used when a
    /// plugin manifest declares `[[default_elements]]` — it gives decomposed
    /// elements a stable `field_mapping_id` across plugin reloads.
    pub fn upsert_field_mapping_by_path(
        &self,
        new_id: &str,
        data_source_id: &str,
        source_type: &str,
        name: &str,
        json_path: &str,
    ) -> Result<FieldMapping, LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        // Look up by (data_source_id, json_path).
        let existing: Option<(String, i64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, created_at, name FROM data_source_fields
                 WHERE data_source_id = ?1 AND json_path = ?2 LIMIT 1",
            )?;
            let mut rows = stmt.query(params![data_source_id, json_path])?;
            match rows.next()? {
                Some(row) => Some((row.get(0)?, row.get(1)?, row.get(2)?)),
                None => None,
            }
        };

        if let Some((id, created_at, existing_name)) = existing {
            if existing_name != name {
                conn.execute(
                    "UPDATE data_source_fields SET name = ?1 WHERE id = ?2",
                    params![name, id],
                )?;
            }
            return Ok(FieldMapping {
                id,
                data_source_id: data_source_id.to_string(),
                source_type: source_type.to_string(),
                name: name.to_string(),
                json_path: json_path.to_string(),
                created_at,
            });
        }

        let now = unix_now();
        conn.execute(
            "INSERT INTO data_source_fields (id, data_source_id, source_type, name, json_path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![new_id, data_source_id, source_type, name, json_path, now],
        )?;
        Ok(FieldMapping {
            id: new_id.to_string(),
            data_source_id: data_source_id.to_string(),
            source_type: source_type.to_string(),
            name: name.to_string(),
            json_path: json_path.to_string(),
            created_at: now,
        })
    }

    /// Delete a field mapping by ID.
    pub fn delete_field_mapping(&self, id: &str) -> Result<(), LayoutStoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM data_source_fields WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }
}

/// A field mapping that extracts a single value from a data source's cached JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMapping {
    pub id: String,
    pub data_source_id: String,
    pub source_type: String,
    pub name: String,
    pub json_path: String,
    pub created_at: i64,
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
            parent_id: None,
            visible_when: None,
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
                    bold: None,
                    italic: None,
                    underline: None,
                    font_family: None,
                    color: None,
                    parent_id: None,
                    visible_when: None,
                },
                LayoutItem::StaticDivider {
                    id: "d0".to_string(),
                    z_index: 1,
                    x: 0, y: 240, width: 800, height: 2,
                    orientation: Some("horizontal".to_string()),
                    parent_id: None,
                    visible_when: None,
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
            parent_id: None,
            visible_when: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        let decoded: LayoutItem = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, LayoutItem::PluginSlot { plugin_instance_id, .. } if plugin_instance_id == "river"));
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

    #[test]
    fn data_field_roundtrip() {
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "with-data".to_string(),
            name: "With Data Fields".to_string(),
            items: vec![
                LayoutItem::DataField {
                    id: "df0".to_string(),
                    z_index: 0,
                    x: 10,
                    y: 20,
                    width: 200,
                    height: 50,
                    field_mapping_id: "fm-123".to_string(),
                    font_size: 32,
                    format_string: "{{value}} ft".to_string(),
                    label: Some("Water Level".to_string()),
                    orientation: None,
                    bold: None,
                    italic: None,
                    underline: None,
                    font_family: None,
                    color: None,
                    parent_id: None,
                    visible_when: None,
                },
                LayoutItem::DataField {
                    id: "df1".to_string(),
                    z_index: 1,
                    x: 10,
                    y: 80,
                    width: 200,
                    height: 50,
                    field_mapping_id: "fm-456".to_string(),
                    font_size: 24,
                    format_string: "{{value | round(0) | number_with_delimiter}} cfs".to_string(),
                    label: None,
                    orientation: Some("horizontal".to_string()),
                    bold: None,
                    italic: None,
                    underline: None,
                    font_family: None,
                    color: None,
                    parent_id: None,
                    visible_when: None,
                },
            ],
            updated_at: 0,
        };
        store.upsert_layout(&layout).unwrap();

        let got = store.get_layout("with-data").unwrap().unwrap();
        assert_eq!(got.items.len(), 2);

        if let LayoutItem::DataField {
            id, field_mapping_id, font_size, format_string, label, orientation, ..
        } = &got.items[0]
        {
            assert_eq!(id, "df0");
            assert_eq!(field_mapping_id, "fm-123");
            assert_eq!(*font_size, 32);
            assert_eq!(format_string, "{{value}} ft");
            assert_eq!(label.as_deref(), Some("Water Level"));
            assert!(orientation.is_none());
        } else {
            panic!("expected DataField");
        }

        if let LayoutItem::DataField {
            id, label, orientation, ..
        } = &got.items[1]
        {
            assert_eq!(id, "df1");
            assert!(label.is_none());
            assert_eq!(orientation.as_deref(), Some("horizontal"));
        } else {
            panic!("expected DataField");
        }
    }

    #[test]
    fn data_field_serde_roundtrip() {
        let item = LayoutItem::DataField {
            id: "df0".to_string(),
            z_index: 0,
            x: 0,
            y: 0,
            width: 200,
            height: 50,
            field_mapping_id: "fm-abc".to_string(),
            font_size: 24,
            format_string: "{{value}}".to_string(),
            label: None,
            orientation: None,
            bold: None,
            italic: None,
            underline: None,
            font_family: None,
            color: None,
            parent_id: None,
            visible_when: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        let decoded: LayoutItem = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            LayoutItem::DataField { field_mapping_id, .. } if field_mapping_id == "fm-abc"
        ));
    }

    #[test]
    fn text_formatting_fields_roundtrip() {
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "fmt".to_string(),
            name: "Formatting".to_string(),
            items: vec![
                LayoutItem::StaticText {
                    id: "t0".to_string(),
                    z_index: 0,
                    x: 0, y: 0, width: 200, height: 40,
                    text_content: "Bold & italic".to_string(),
                    font_size: 20,
                    orientation: None,
                    bold: Some(true),
                    italic: Some(true),
                    underline: Some(false),
                    font_family: Some("Georgia, serif".to_string()),
                    color: None,
                    parent_id: None,
                    visible_when: None,
                },
                LayoutItem::DataField {
                    id: "df0".to_string(),
                    z_index: 1,
                    x: 0, y: 50, width: 200, height: 40,
                    field_mapping_id: "fm-x".to_string(),
                    font_size: 24,
                    format_string: "{{value}}".to_string(),
                    label: None,
                    orientation: None,
                    bold: None,
                    italic: None,
                    underline: Some(true),
                    font_family: Some("monospace".to_string()),
                    color: None,
                    parent_id: None,
                    visible_when: None,
                },
            ],
            updated_at: 0,
        };
        store.upsert_layout(&layout).unwrap();
        let got = store.get_layout("fmt").unwrap().unwrap();
        match &got.items[0] {
            LayoutItem::StaticText { bold, italic, underline, font_family, .. } => {
                assert_eq!(*bold, Some(true));
                assert_eq!(*italic, Some(true));
                assert_eq!(*underline, Some(false));
                assert_eq!(font_family.as_deref(), Some("Georgia, serif"));
            }
            _ => panic!("expected StaticText"),
        }
        match &got.items[1] {
            LayoutItem::DataField { bold, italic, underline, font_family, .. } => {
                assert!(bold.is_none());
                assert!(italic.is_none());
                assert_eq!(*underline, Some(true));
                assert_eq!(font_family.as_deref(), Some("monospace"));
            }
            _ => panic!("expected DataField"),
        }
    }

    #[test]
    fn field_mapping_crud() {
        let (store, _dir) = open_store();

        // Create
        let fm = store
            .create_field_mapping("fm-1", "river-source", "builtin", "Water Level", "$.water_level_ft")
            .unwrap();
        assert_eq!(fm.id, "fm-1");
        assert_eq!(fm.data_source_id, "river-source");
        assert_eq!(fm.source_type, "builtin");
        assert_eq!(fm.name, "Water Level");
        assert_eq!(fm.json_path, "$.water_level_ft");

        // Get
        let got = store.get_field_mapping("fm-1").unwrap().unwrap();
        assert_eq!(got.name, "Water Level");

        // Get missing
        assert!(store.get_field_mapping("nonexistent").unwrap().is_none());

        // Create a second mapping for the same source
        store
            .create_field_mapping("fm-2", "river-source", "builtin", "Streamflow", "$.streamflow_cfs")
            .unwrap();

        // List
        let mappings = store.list_field_mappings("river-source").unwrap();
        assert_eq!(mappings.len(), 2);

        // List for different source returns empty
        let empty = store.list_field_mappings("other-source").unwrap();
        assert!(empty.is_empty());

        // Delete
        store.delete_field_mapping("fm-1").unwrap();
        assert!(store.get_field_mapping("fm-1").unwrap().is_none());

        // Remaining mapping still exists
        let remaining = store.list_field_mappings("river-source").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "fm-2");
    }

    #[test]
    fn group_variant_roundtrip_through_store() {
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "with-group".to_string(),
            name: "With Group".to_string(),
            items: vec![
                LayoutItem::Group {
                    id: "g0".to_string(),
                    z_index: 0,
                    x: 20, y: 30, width: 240, height: 120,
                    plugin_instance_id: Some("weather".to_string()),
                    label: Some("Weather".to_string()),
                    background: Some("card".to_string()),
                    parent_id: None,
                },
                LayoutItem::StaticText {
                    id: "t0".to_string(),
                    z_index: 1,
                    x: 30, y: 40, width: 180, height: 24,
                    text_content: "Temp".to_string(),
                    font_size: 16,
                    orientation: None,
                    bold: None, italic: None, underline: None, font_family: None, color: None,
                    parent_id: Some("g0".to_string()),
                    visible_when: None,
                },
            ],
            updated_at: 0,
        };
        store.upsert_layout(&layout).unwrap();
        let got = store.get_layout("with-group").unwrap().unwrap();
        assert_eq!(got.items.len(), 2);

        match &got.items[0] {
            LayoutItem::Group {
                id, plugin_instance_id, label, background, parent_id, ..
            } => {
                assert_eq!(id, "g0");
                assert_eq!(plugin_instance_id.as_deref(), Some("weather"));
                assert_eq!(label.as_deref(), Some("Weather"));
                assert_eq!(background.as_deref(), Some("card"));
                assert!(parent_id.is_none());
            }
            other => panic!("expected Group, got {:?}", other),
        }
        assert_eq!(got.items[1].parent_id(), Some("g0"));
    }

    #[test]
    fn group_serde_roundtrip() {
        let item = LayoutItem::Group {
            id: "g1".to_string(),
            z_index: 5,
            x: 0, y: 0, width: 100, height: 100,
            plugin_instance_id: None,
            label: Some("Hello".to_string()),
            background: None,
            parent_id: None,
        };
        let json = serde_json::to_string(&item).unwrap();
        // None fields should be omitted by skip_serializing_if.
        assert!(json.contains(r#""type":"group""#));
        assert!(!json.contains("plugin_instance_id"));
        assert!(!json.contains("background"));
        assert!(!json.contains("parent_id"));
        let decoded: LayoutItem = serde_json::from_str(&json).unwrap();
        match decoded {
            LayoutItem::Group { id, label, .. } => {
                assert_eq!(id, "g1");
                assert_eq!(label.as_deref(), Some("Hello"));
            }
            _ => panic!("expected Group"),
        }
    }

    #[test]
    fn parent_id_roundtrip_for_non_group_variants() {
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "nested".to_string(),
            name: "Nested".to_string(),
            items: vec![
                LayoutItem::Group {
                    id: "g".to_string(),
                    z_index: 0,
                    x: 0, y: 0, width: 200, height: 200,
                    plugin_instance_id: None, label: None, background: None,
                    parent_id: None,
                },
                LayoutItem::StaticDivider {
                    id: "d".to_string(),
                    z_index: 1,
                    x: 0, y: 100, width: 200, height: 2,
                    orientation: Some("horizontal".to_string()),
                    parent_id: Some("g".to_string()),
                    visible_when: None,
                },
            ],
            updated_at: 0,
        };
        store.upsert_layout(&layout).unwrap();
        let got = store.get_layout("nested").unwrap().unwrap();
        assert_eq!(got.items[1].parent_id(), Some("g"));
    }

    #[test]
    fn upsert_field_mapping_by_path_inserts_and_deduplicates() {
        let (store, _dir) = open_store();

        // First upsert inserts a new row with the proposed id.
        let fm1 = store
            .upsert_field_mapping_by_path(
                "fm-weather-temp",
                "weather",
                "builtin",
                "Temp",
                "$.temperature_f",
            )
            .unwrap();
        assert_eq!(fm1.id, "fm-weather-temp");

        // Second call with the same (data_source_id, json_path) returns the
        // existing row, even with a different proposed id.
        let fm2 = store
            .upsert_field_mapping_by_path(
                "fm-weather-DIFFERENT",
                "weather",
                "builtin",
                "Temperature",
                "$.temperature_f",
            )
            .unwrap();
        assert_eq!(fm2.id, "fm-weather-temp");
        assert_eq!(fm2.name, "Temperature"); // name refreshed

        let mappings = store.list_field_mappings("weather").unwrap();
        assert_eq!(mappings.len(), 1);
    }

    #[test]
    fn existing_rows_have_null_parent_id() {
        // Existing rows (from the pre-Phase-1 schema) should decode with parent_id = None.
        let (store, _dir) = open_store();
        let layout = LayoutConfig {
            id: "flat".to_string(),
            name: "Flat".to_string(),
            items: vec![plugin_slot("s", "river", 0)],
            updated_at: 0,
        };
        store.upsert_layout(&layout).unwrap();
        let got = store.get_layout("flat").unwrap().unwrap();
        assert!(got.items[0].parent_id().is_none());
    }
}
