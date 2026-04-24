//! Plugin instance store — SQLite-backed persistence for plugin instances.
//!
//! A plugin instance is a concrete binding of a plugin definition to a set
//! of user settings (e.g., a specific USGS site ID for the river plugin).
//! Instances are stored durably in SQLite and cached in memory for hot reads
//! via `Arc<RwLock<HashMap>>` in the data cache layer.
//!
//! The store owns the SQLite connection and is designed to be shared across
//! threads via `Arc<InstanceStore>`. All operations take `&self` and serialise
//! through an internal `Mutex` on the connection.
//!
//! # Schema
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS plugin_instances (
//!     id                  TEXT PRIMARY KEY,
//!     plugin_id           TEXT NOT NULL,
//!     settings            TEXT NOT NULL,
//!     encrypted_settings  TEXT,
//!     cached_data         TEXT,
//!     last_fetched_at     INTEGER,
//!     last_error          TEXT
//! );
//! ```
//!
//! # Seeding
//!
//! [`seed_from_config`] migrates the 5 well-known sources (weather, river, ferry,
//! trail, road) to plugin instances using settings derived from `config.toml`.
//! It uses `INSERT OR IGNORE` so it is safe to call on every startup.

use std::{path::Path, sync::Mutex};

use rusqlite::{params, Connection};
use serde_json::Value;
use thiserror::Error;

use crate::config::Config;

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error creating directory '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

// ─── Data types ───────────────────────────────────────────────────────────────

/// A plugin instance: a concrete binding of a plugin definition to user settings.
///
/// Multiple instances of the same plugin can exist with different settings
/// (e.g., two river gauges at different USGS sites).
#[derive(Debug, Clone)]
pub struct PluginInstance {
    /// Stable unique identifier for this instance (e.g., `"river"`, `"river_2"`).
    pub id: String,
    /// The plugin definition this instance is bound to (e.g., `"river"`).
    pub plugin_id: String,
    /// Plain (non-sensitive) user settings as JSON.
    pub settings: Value,
    /// AES-encrypted sensitive settings as JSON (API keys, etc.). Nullable.
    pub encrypted_settings: Option<Value>,
    /// The last successful fetch result as JSON. Nullable until first fetch.
    pub cached_data: Option<Value>,
    /// Unix timestamp (seconds) of the last successful fetch. Nullable.
    pub last_fetched_at: Option<i64>,
    /// Error message from the last failed fetch. Nullable.
    pub last_error: Option<String>,
}

// ─── Store ────────────────────────────────────────────────────────────────────

/// SQLite-backed store for plugin instances.
///
/// Thread-safe via an internal `Mutex<Connection>`. Wrap in `Arc` to share
/// across threads: `Arc<InstanceStore>`.
pub struct InstanceStore {
    conn: Mutex<Connection>,
}

impl InstanceStore {
    /// Open or create the SQLite database at `db_path`.
    ///
    /// Creates all parent directories if they don't exist, then opens the
    /// database and runs the schema migration (idempotent `CREATE TABLE IF NOT EXISTS`).
    pub fn open(db_path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Io {
                path: parent.to_string_lossy().into_owned(),
                source: e,
            })?;
        }
        let conn = Connection::open(db_path)?;
        Self::migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn migrate(conn: &Connection) -> Result<(), StoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS plugin_instances (
                id                  TEXT PRIMARY KEY,
                plugin_id           TEXT NOT NULL,
                settings            TEXT NOT NULL,
                encrypted_settings  TEXT,
                cached_data         TEXT,
                last_fetched_at     INTEGER,
                last_error          TEXT
            );",
        )?;
        Ok(())
    }

    /// Insert a new plugin instance.
    ///
    /// Uses `INSERT OR IGNORE` — safe to call with the same `id` more than once
    /// (e.g., on every startup for the seeded instances).
    /// Returns `true` if the row was inserted, `false` if it already existed.
    pub fn create_instance(&self, instance: &PluginInstance) -> Result<bool, StoreError> {
        let settings_json = serde_json::to_string(&instance.settings)?;
        let encrypted_json = instance
            .encrypted_settings
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let cached_json = instance
            .cached_data
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "INSERT OR IGNORE INTO plugin_instances
             (id, plugin_id, settings, encrypted_settings, cached_data, last_fetched_at, last_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                instance.id,
                instance.plugin_id,
                settings_json,
                encrypted_json,
                cached_json,
                instance.last_fetched_at,
                instance.last_error,
            ],
        )?;
        Ok(rows == 1)
    }

    /// Return the plugin instance with `id`, or `None` if not found.
    pub fn get_instance(&self, id: &str) -> Result<Option<PluginInstance>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, plugin_id, settings, encrypted_settings, cached_data,
                    last_fetched_at, last_error
             FROM plugin_instances WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_instance(row)?)),
            None => Ok(None),
        }
    }

    /// Update `cached_data` and `last_fetched_at` for an existing instance.
    ///
    /// Also clears `last_error` — a successful fetch resolves the previous error.
    pub fn update_cached_data(
        &self,
        id: &str,
        data: &Value,
        fetched_at: i64,
    ) -> Result<(), StoreError> {
        let data_json = serde_json::to_string(data)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE plugin_instances
             SET cached_data = ?1, last_fetched_at = ?2, last_error = NULL
             WHERE id = ?3",
            params![data_json, fetched_at, id],
        )?;
        Ok(())
    }

    /// Update `last_error` for an existing instance.
    ///
    /// Does NOT clear `cached_data` — the last successful value is preserved
    /// for display while the error is recorded.
    pub fn update_last_error(&self, id: &str, error: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE plugin_instances SET last_error = ?1 WHERE id = ?2",
            params![error, id],
        )?;
        Ok(())
    }

    /// Return all plugin instances, sorted by `id`.
    pub fn list_instances(&self) -> Result<Vec<PluginInstance>, StoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, plugin_id, settings, encrypted_settings, cached_data,
                    last_fetched_at, last_error
             FROM plugin_instances ORDER BY id",
        )?;
        let mut rows = stmt.query([])?;
        let mut instances = Vec::new();
        while let Some(row) = rows.next()? {
            instances.push(row_to_instance(row)?);
        }
        Ok(instances)
    }
}

/// Deserialise a SQLite row into a [`PluginInstance`].
///
/// JSON parse failures for nullable fields silently produce `None`; an
/// unparseable `settings` column falls back to an empty JSON object and logs
/// a warning rather than propagating as a `rusqlite::Error`.
fn row_to_instance(row: &rusqlite::Row<'_>) -> rusqlite::Result<PluginInstance> {
    let id: String = row.get(0)?;
    let settings_str: String = row.get(2)?;
    let settings: Value = serde_json::from_str(&settings_str).unwrap_or_else(|e| {
        log::warn!("instance_store: failed to parse settings for instance '{}': {}", id, e);
        Value::Object(serde_json::Map::new())
    });

    let encrypted_settings: Option<Value> = row
        .get::<_, Option<String>>(3)?
        .and_then(|s| serde_json::from_str(&s).ok());

    let cached_data: Option<Value> = row
        .get::<_, Option<String>>(4)?
        .and_then(|s| serde_json::from_str(&s).ok());

    Ok(PluginInstance {
        id,
        plugin_id: row.get(1)?,
        settings,
        encrypted_settings,
        cached_data,
        last_fetched_at: row.get(5)?,
        last_error: row.get(6)?,
    })
}

// ─── Seeding from config ──────────────────────────────────────────────────────

/// Migrate the 5 well-known sources to plugin instances using settings from `config`.
///
/// Uses `INSERT OR IGNORE` on each instance, so it is safe to call on every
/// startup without overwriting user-modified instance settings.
///
/// Seeded instances (using values from `config.toml` where present):
///
/// | Instance ID | Plugin   | Key settings                                          |
/// |-------------|----------|-------------------------------------------------------|
/// | `weather`   | weather  | *(none required — uses lat/long)*                     |
/// | `river`     | river    | `site_id` (default `"12200500"`)                      |
/// | `ferry`     | ferry    | `route_id` (default `9`), description, access code    |
/// | `trail`     | trail    | `park_code` (default `"noca"`), NPS API key           |
/// | `road`      | road     | `routes` (default `"020"`), access code               |
///
/// Credential fields (`wsdot_access_code`, `nps_api_key`) are stored in plain
/// `settings` here because encryption is not yet implemented. They will be moved
/// to `encrypted_settings` when the encryption layer is added.
pub fn seed_from_config(store: &InstanceStore, config: &Config) -> Result<(), StoreError> {
    // weather — no required settings; station selection uses lat/long from config
    store.create_instance(&PluginInstance {
        id: "weather".to_string(),
        plugin_id: "weather".to_string(),
        settings: serde_json::json!({}),
        encrypted_settings: None,
        cached_data: None,
        last_fetched_at: None,
        last_error: None,
    })?;

    // river — site_id from config, default to Skagit River at Mount Vernon
    let site_id = config
        .sources
        .river
        .as_ref()
        .map(|r| r.usgs_site_id.as_str())
        .unwrap_or("12200500");
    store.create_instance(&PluginInstance {
        id: "river".to_string(),
        plugin_id: "river".to_string(),
        settings: serde_json::json!({ "site_id": site_id, "site_name": "River" }),
        encrypted_settings: None,
        cached_data: None,
        last_fetched_at: None,
        last_error: None,
    })?;

    // ferry — route_id, route_description, and wsdot_access_code from config
    let (route_id, route_description, ferry_access_code) = config
        .sources
        .ferry
        .as_ref()
        .map(|f| {
            (
                f.route_id,
                f.route_description
                    .clone()
                    .unwrap_or_else(|| "Anacortes / Friday Harbor".to_string()),
                f.wsdot_access_code.clone(),
            )
        })
        .unwrap_or((9, "Anacortes / Friday Harbor".to_string(), None));
    store.create_instance(&PluginInstance {
        id: "ferry".to_string(),
        plugin_id: "ferry".to_string(),
        settings: serde_json::json!({
            "route_id": route_id,
            "route_description": route_description,
            "wsdot_access_code": ferry_access_code
        }),
        encrypted_settings: None,
        cached_data: None,
        last_fetched_at: None,
        last_error: None,
    })?;

    // trail — park_code and nps_api_key from config
    let (park_code, nps_api_key) = config
        .sources
        .trail
        .as_ref()
        .map(|t| (t.park_code.as_str(), t.nps_api_key.clone()))
        .unwrap_or(("noca", None));
    store.create_instance(&PluginInstance {
        id: "trail".to_string(),
        plugin_id: "trail".to_string(),
        settings: serde_json::json!({
            "park_code": park_code,
            "nps_api_key": nps_api_key
        }),
        encrypted_settings: None,
        cached_data: None,
        last_fetched_at: None,
        last_error: None,
    })?;

    // road — routes and wsdot_access_code from config
    let (routes, road_access_code) = config
        .sources
        .road
        .as_ref()
        .map(|r| (r.routes.join(","), r.wsdot_access_code.clone()))
        .unwrap_or_else(|| ("020".to_string(), None));
    store.create_instance(&PluginInstance {
        id: "road".to_string(),
        plugin_id: "road".to_string(),
        settings: serde_json::json!({
            "routes": routes,
            "wsdot_access_code": road_access_code
        }),
        encrypted_settings: None,
        cached_data: None,
        last_fetched_at: None,
        last_error: None,
    })?;

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_store() -> (InstanceStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let store = InstanceStore::open(&db_path).unwrap();
        (store, dir)
    }

    fn minimal_instance(id: &str, plugin_id: &str) -> PluginInstance {
        PluginInstance {
            id: id.to_string(),
            plugin_id: plugin_id.to_string(),
            settings: serde_json::json!({ "key": "value" }),
            encrypted_settings: None,
            cached_data: None,
            last_fetched_at: None,
            last_error: None,
        }
    }

    #[test]
    fn open_creates_db_file() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("sub").join("cascades.db");
        // Sub-directory does not exist yet — open should create it.
        let store = InstanceStore::open(&db_path).unwrap();
        assert!(db_path.exists());
        // Empty store has no instances.
        assert_eq!(store.list_instances().unwrap().len(), 0);
    }

    #[test]
    fn create_and_get_instance() {
        let (store, _dir) = open_store();
        let inst = minimal_instance("river", "river");
        let inserted = store.create_instance(&inst).unwrap();
        assert!(inserted, "first insert should return true");

        let retrieved = store.get_instance("river").unwrap().expect("should exist");
        assert_eq!(retrieved.id, "river");
        assert_eq!(retrieved.plugin_id, "river");
        assert_eq!(retrieved.settings["key"], "value");
        assert!(retrieved.cached_data.is_none());
        assert!(retrieved.last_fetched_at.is_none());
        assert!(retrieved.last_error.is_none());
    }

    #[test]
    fn get_instance_returns_none_for_missing_id() {
        let (store, _dir) = open_store();
        let result = store.get_instance("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn create_instance_is_idempotent() {
        let (store, _dir) = open_store();
        let inst = minimal_instance("weather", "weather");

        let first = store.create_instance(&inst).unwrap();
        assert!(first);

        let second = store.create_instance(&inst).unwrap();
        assert!(!second, "duplicate insert should return false (OR IGNORE)");

        // Only one row should exist.
        assert_eq!(store.list_instances().unwrap().len(), 1);
    }

    #[test]
    fn update_cached_data_sets_data_and_clears_error() {
        let (store, _dir) = open_store();
        store.create_instance(&minimal_instance("river", "river")).unwrap();

        // Set an error first to verify it gets cleared.
        store.update_last_error("river", "network timeout").unwrap();
        let with_error = store.get_instance("river").unwrap().unwrap();
        assert_eq!(with_error.last_error.as_deref(), Some("network timeout"));

        // Now update cached data — should clear the error.
        let data = serde_json::json!({ "water_level_ft": 8.5 });
        store.update_cached_data("river", &data, 1_700_000_000).unwrap();

        let updated = store.get_instance("river").unwrap().unwrap();
        assert_eq!(updated.cached_data.unwrap()["water_level_ft"], 8.5);
        assert_eq!(updated.last_fetched_at, Some(1_700_000_000));
        assert!(updated.last_error.is_none(), "error should be cleared after successful fetch");
    }

    #[test]
    fn update_last_error_preserves_cached_data() {
        let (store, _dir) = open_store();
        store.create_instance(&minimal_instance("weather", "weather")).unwrap();

        // First a successful fetch.
        let data = serde_json::json!({ "temperature_f": 55.0 });
        store.update_cached_data("weather", &data, 1_000).unwrap();

        // Then a failure — cached_data should not be cleared.
        store.update_last_error("weather", "API error 503").unwrap();

        let updated = store.get_instance("weather").unwrap().unwrap();
        assert_eq!(
            updated.cached_data.unwrap()["temperature_f"],
            55.0,
            "cached data should survive an error update"
        );
        assert_eq!(updated.last_error.as_deref(), Some("API error 503"));
    }

    #[test]
    fn list_instances_returns_sorted_by_id() {
        let (store, _dir) = open_store();
        store.create_instance(&minimal_instance("weather", "weather")).unwrap();
        store.create_instance(&minimal_instance("river", "river")).unwrap();
        store.create_instance(&minimal_instance("ferry", "ferry")).unwrap();

        let instances = store.list_instances().unwrap();
        assert_eq!(instances.len(), 3);
        assert_eq!(instances[0].id, "ferry");
        assert_eq!(instances[1].id, "river");
        assert_eq!(instances[2].id, "weather");
    }

    #[test]
    fn instance_with_encrypted_settings_roundtrips() {
        let (store, _dir) = open_store();
        let inst = PluginInstance {
            id: "trail".to_string(),
            plugin_id: "trail".to_string(),
            settings: serde_json::json!({ "park_code": "noca" }),
            encrypted_settings: Some(serde_json::json!({ "nps_api_key": "ENCRYPTED_BLOB" })),
            cached_data: None,
            last_fetched_at: None,
            last_error: None,
        };
        store.create_instance(&inst).unwrap();

        let retrieved = store.get_instance("trail").unwrap().unwrap();
        let enc = retrieved.encrypted_settings.unwrap();
        assert_eq!(enc["nps_api_key"], "ENCRYPTED_BLOB");
    }

    // ── seed_from_config tests ────────────────────────────────────────────────

    fn minimal_config() -> Config {
        use crate::config::*;
        Config {
            display: DisplayConfig { width: 800, height: 480 },
            location: LocationConfig {
                latitude: 48.4,
                longitude: -122.3,
                name: "Test".to_string(),
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

    #[test]
    fn seed_from_config_creates_five_instances() {
        let (store, _dir) = open_store();
        let config = minimal_config();

        seed_from_config(&store, &config).unwrap();

        let instances = store.list_instances().unwrap();
        assert_eq!(instances.len(), 5);

        let ids: Vec<&str> = instances.iter().map(|i| i.id.as_str()).collect();
        assert!(ids.contains(&"ferry"));
        assert!(ids.contains(&"river"));
        assert!(ids.contains(&"road"));
        assert!(ids.contains(&"trail"));
        assert!(ids.contains(&"weather"));
    }

    #[test]
    fn seed_from_config_uses_default_site_ids() {
        let (store, _dir) = open_store();
        let config = minimal_config();
        seed_from_config(&store, &config).unwrap();

        let river = store.get_instance("river").unwrap().unwrap();
        assert_eq!(river.settings["site_id"], "12200500");

        let ferry = store.get_instance("ferry").unwrap().unwrap();
        assert_eq!(ferry.settings["route_id"], 9);
        assert_eq!(ferry.settings["route_description"], "Anacortes / Friday Harbor");
        assert!(ferry.settings["wsdot_access_code"].is_null());

        let trail = store.get_instance("trail").unwrap().unwrap();
        assert_eq!(trail.settings["park_code"], "noca");
        assert!(trail.settings["nps_api_key"].is_null());

        let road = store.get_instance("road").unwrap().unwrap();
        assert_eq!(road.settings["routes"], "020");
        assert!(road.settings["wsdot_access_code"].is_null());
    }

    #[test]
    fn seed_from_config_respects_config_values() {
        use crate::config::*;
        let (store, _dir) = open_store();
        let mut config = minimal_config();
        config.sources.river = Some(RiverSourceConfig { usgs_site_id: "12150800".to_string() });
        config.sources.trail = Some(TrailSourceConfig {
            park_code: "mora".to_string(),
            nps_api_key: Some("test-nps-key".to_string()),
        });
        config.sources.road = Some(RoadSourceConfig {
            wsdot_access_code: Some("road-access-code".to_string()),
            routes: vec!["020".to_string(), "002".to_string()],
        });
        config.sources.ferry = Some(FerrySourceConfig {
            wsdot_access_code: Some("ferry-access-code".to_string()),
            route_id: 14,
            route_description: Some("Edmonds / Kingston".to_string()),
        });

        seed_from_config(&store, &config).unwrap();

        let river = store.get_instance("river").unwrap().unwrap();
        assert_eq!(river.settings["site_id"], "12150800");

        let trail = store.get_instance("trail").unwrap().unwrap();
        assert_eq!(trail.settings["park_code"], "mora");
        assert_eq!(trail.settings["nps_api_key"], "test-nps-key");

        let road = store.get_instance("road").unwrap().unwrap();
        assert_eq!(road.settings["routes"], "020,002");
        assert_eq!(road.settings["wsdot_access_code"], "road-access-code");

        let ferry = store.get_instance("ferry").unwrap().unwrap();
        assert_eq!(ferry.settings["route_id"], 14);
        assert_eq!(ferry.settings["route_description"], "Edmonds / Kingston");
        assert_eq!(ferry.settings["wsdot_access_code"], "ferry-access-code");
    }

    #[test]
    fn seed_from_config_is_idempotent() {
        let (store, _dir) = open_store();
        let config = minimal_config();

        seed_from_config(&store, &config).unwrap();
        seed_from_config(&store, &config).unwrap();

        // Still exactly 5 instances, no duplicates.
        assert_eq!(store.list_instances().unwrap().len(), 5);
    }
}
