//! Source store — SQLite-backed persistence for user-defined generic HTTP data sources.
//!
//! Generic data sources are HTTP endpoints that users configure via the admin UI.
//! Each source defines a URL, method, headers, optional body template, and a refresh
//! interval. The scheduler fetches each source on its interval and caches the response.
//!
//! # Schema
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS data_sources (
//!     id                    TEXT PRIMARY KEY,
//!     name                  TEXT NOT NULL,
//!     url                   TEXT NOT NULL,
//!     method                TEXT NOT NULL DEFAULT 'GET',
//!     headers               TEXT NOT NULL DEFAULT '{}',
//!     body_template         TEXT,
//!     response_root_path    TEXT,
//!     refresh_interval_secs INTEGER NOT NULL DEFAULT 300,
//!     cached_data           TEXT,
//!     last_fetched_at       INTEGER,
//!     last_error            TEXT,
//!     created_at            INTEGER NOT NULL,
//!     updated_at            INTEGER NOT NULL
//! );
//! ```

use std::{path::Path, sync::Mutex};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Minimum allowed refresh interval (seconds).
pub const MIN_REFRESH_INTERVAL_SECS: i64 = 30;

/// Maximum cached response size (bytes).
pub const MAX_CACHED_RESPONSE_BYTES: usize = 1_048_576; // 1 MB

// ─── Error type ──────────���─────────────────────────────���─────────────────────

#[derive(Debug, Error)]
pub enum SourceStoreError {
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
    #[error("validation error: {0}")]
    Validation(String),
}

// ─── Data types ──────────────────────────────��───────────────────────────────

/// A user-defined generic HTTP data source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSource {
    pub id: String,
    pub name: String,
    pub url: String,
    pub method: String,
    pub headers: serde_json::Value,
    pub body_template: Option<String>,
    pub response_root_path: Option<String>,
    pub refresh_interval_secs: i64,
    pub cached_data: Option<serde_json::Value>,
    pub last_fetched_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Payload for creating or updating a data source (no cached_data or timestamps).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSourceConfig {
    pub name: String,
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default = "default_headers")]
    pub headers: serde_json::Value,
    pub body_template: Option<String>,
    pub response_root_path: Option<String>,
    #[serde(default = "default_interval")]
    pub refresh_interval_secs: i64,
}

fn default_method() -> String {
    "GET".to_string()
}
fn default_headers() -> serde_json::Value {
    serde_json::json!({})
}
fn default_interval() -> i64 {
    300
}

// ─── Store ─────��─────────────────────────────���───────────────────────────────

/// SQLite-backed store for generic HTTP data sources.
///
/// Thread-safe via an internal `Mutex<Connection>`. Wrap in `Arc` to share.
pub struct SourceStore {
    conn: Mutex<Connection>,
}

impl SourceStore {
    /// Open or create the SQLite database at `db_path` and run migrations.
    ///
    /// Safe to open against the same file as InstanceStore/LayoutStore.
    pub fn open(db_path: &Path) -> Result<Self, SourceStoreError> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| SourceStoreError::Io {
                path: parent.to_string_lossy().into_owned(),
                source: e,
            })?;
        }
        let conn = Connection::open(db_path)?;
        Self::migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn migrate(conn: &Connection) -> Result<(), SourceStoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS data_sources (
                id                    TEXT PRIMARY KEY,
                name                  TEXT NOT NULL,
                url                   TEXT NOT NULL,
                method                TEXT NOT NULL DEFAULT 'GET',
                headers               TEXT NOT NULL DEFAULT '{}',
                body_template         TEXT,
                response_root_path    TEXT,
                refresh_interval_secs INTEGER NOT NULL DEFAULT 300,
                cached_data           TEXT,
                last_fetched_at       INTEGER,
                last_error            TEXT,
                created_at            INTEGER NOT NULL,
                updated_at            INTEGER NOT NULL
            );",
        )?;
        Ok(())
    }

    /// Create a new data source. Returns the created source.
    pub fn create(&self, config: &DataSourceConfig) -> Result<DataSource, SourceStoreError> {
        let interval = config.refresh_interval_secs.max(MIN_REFRESH_INTERVAL_SECS);
        let method = config.method.to_uppercase();
        if method != "GET" && method != "POST" {
            return Err(SourceStoreError::Validation(
                format!("method must be GET or POST, got '{}'", config.method),
            ));
        }

        let id = format!(
            "src-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let now = unix_now();
        let headers_json = serde_json::to_string(&config.headers)?;

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO data_sources
             (id, name, url, method, headers, body_template, response_root_path,
              refresh_interval_secs, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                config.name,
                config.url,
                method,
                headers_json,
                config.body_template,
                config.response_root_path,
                interval,
                now,
                now,
            ],
        )?;

        Ok(DataSource {
            id,
            name: config.name.clone(),
            url: config.url.clone(),
            method,
            headers: config.headers.clone(),
            body_template: config.body_template.clone(),
            response_root_path: config.response_root_path.clone(),
            refresh_interval_secs: interval,
            cached_data: None,
            last_fetched_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Get a data source by ID.
    pub fn get(&self, id: &str) -> Result<Option<DataSource>, SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, url, method, headers, body_template, response_root_path,
                    refresh_interval_secs, cached_data, last_fetched_at, last_error,
                    created_at, updated_at
             FROM data_sources WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_source(row)?)),
            None => Ok(None),
        }
    }

    /// List all data sources, ordered by name.
    pub fn list(&self) -> Result<Vec<DataSource>, SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, url, method, headers, body_template, response_root_path,
                    refresh_interval_secs, cached_data, last_fetched_at, last_error,
                    created_at, updated_at
             FROM data_sources ORDER BY name",
        )?;
        let mut rows = stmt.query([])?;
        let mut sources = Vec::new();
        while let Some(row) = rows.next()? {
            sources.push(row_to_source(row)?);
        }
        Ok(sources)
    }

    /// Update a data source's configuration. Returns the updated source.
    pub fn update(
        &self,
        id: &str,
        config: &DataSourceConfig,
    ) -> Result<Option<DataSource>, SourceStoreError> {
        let interval = config.refresh_interval_secs.max(MIN_REFRESH_INTERVAL_SECS);
        let method = config.method.to_uppercase();
        if method != "GET" && method != "POST" {
            return Err(SourceStoreError::Validation(
                format!("method must be GET or POST, got '{}'", config.method),
            ));
        }

        let now = unix_now();
        let headers_json = serde_json::to_string(&config.headers)?;

        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "UPDATE data_sources
             SET name = ?1, url = ?2, method = ?3, headers = ?4, body_template = ?5,
                 response_root_path = ?6, refresh_interval_secs = ?7, updated_at = ?8
             WHERE id = ?9",
            params![
                config.name,
                config.url,
                method,
                headers_json,
                config.body_template,
                config.response_root_path,
                interval,
                now,
                id,
            ],
        )?;

        if rows == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get(id)
    }

    /// Delete a data source by ID.
    pub fn delete(&self, id: &str) -> Result<bool, SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        // Also delete associated field mappings
        conn.execute(
            "DELETE FROM data_source_fields WHERE data_source_id = ?1",
            params![id],
        ).ok(); // data_source_fields might be in a different connection
        let rows = conn.execute(
            "DELETE FROM data_sources WHERE id = ?1",
            params![id],
        )?;
        Ok(rows > 0)
    }

    /// Update cached_data and last_fetched_at for a source.
    /// Also clears last_error on success.
    pub fn update_cached_data(
        &self,
        id: &str,
        data: &serde_json::Value,
        fetched_at: i64,
    ) -> Result<(), SourceStoreError> {
        let data_json = serde_json::to_string(data)?;
        if data_json.len() > MAX_CACHED_RESPONSE_BYTES {
            return Err(SourceStoreError::Validation(
                format!(
                    "response exceeds maximum size ({} bytes > {} bytes)",
                    data_json.len(),
                    MAX_CACHED_RESPONSE_BYTES
                ),
            ));
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE data_sources
             SET cached_data = ?1, last_fetched_at = ?2, last_error = NULL, updated_at = ?2
             WHERE id = ?3",
            params![data_json, fetched_at, id],
        )?;
        Ok(())
    }

    /// Update last_error for a source. Does not clear cached_data.
    pub fn update_last_error(&self, id: &str, error: &str) -> Result<(), SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE data_sources SET last_error = ?1 WHERE id = ?2",
            params![error, id],
        )?;
        Ok(())
    }
}

fn row_to_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<DataSource> {
    let id: String = row.get(0)?;
    let headers_str: String = row.get(4)?;
    let headers: serde_json::Value =
        serde_json::from_str(&headers_str).unwrap_or(serde_json::json!({}));
    let cached_data: Option<serde_json::Value> = row
        .get::<_, Option<String>>(8)?
        .and_then(|s| serde_json::from_str(&s).ok());

    Ok(DataSource {
        id,
        name: row.get(1)?,
        url: row.get(2)?,
        method: row.get(3)?,
        headers,
        body_template: row.get(5)?,
        response_root_path: row.get(6)?,
        refresh_interval_secs: row.get(7)?,
        cached_data,
        last_fetched_at: row.get(9)?,
        last_error: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ─── Tests ─────���─────────────────────────────��───────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_store() -> (SourceStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let store = SourceStore::open(&db_path).unwrap();
        (store, dir)
    }

    fn test_config() -> DataSourceConfig {
        DataSourceConfig {
            name: "Test API".to_string(),
            url: "https://api.example.com/data".to_string(),
            method: "GET".to_string(),
            headers: serde_json::json!({"Authorization": "Bearer test"}),
            body_template: None,
            response_root_path: None,
            refresh_interval_secs: 60,
        }
    }

    #[test]
    fn create_and_get_source() {
        let (store, _dir) = open_store();
        let config = test_config();
        let created = store.create(&config).unwrap();
        assert!(created.id.starts_with("src-"));
        assert_eq!(created.name, "Test API");
        assert_eq!(created.url, "https://api.example.com/data");
        assert_eq!(created.method, "GET");
        assert_eq!(created.refresh_interval_secs, 60);
        assert!(created.cached_data.is_none());

        let got = store.get(&created.id).unwrap().unwrap();
        assert_eq!(got.name, "Test API");
        assert_eq!(got.headers["Authorization"], "Bearer test");
    }

    #[test]
    fn get_missing_returns_none() {
        let (store, _dir) = open_store();
        assert!(store.get("nonexistent").unwrap().is_none());
    }

    #[test]
    fn list_sources_sorted_by_name() {
        let (store, _dir) = open_store();
        let mut c1 = test_config();
        c1.name = "Zebra API".to_string();
        let mut c2 = test_config();
        c2.name = "Alpha API".to_string();
        store.create(&c1).unwrap();
        // Sleep briefly so IDs are unique
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.create(&c2).unwrap();

        let all = store.list().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "Alpha API");
        assert_eq!(all[1].name, "Zebra API");
    }

    #[test]
    fn update_source() {
        let (store, _dir) = open_store();
        let created = store.create(&test_config()).unwrap();

        let mut updated_config = test_config();
        updated_config.name = "Updated API".to_string();
        updated_config.url = "https://api.example.com/v2".to_string();
        updated_config.refresh_interval_secs = 120;

        let updated = store.update(&created.id, &updated_config).unwrap().unwrap();
        assert_eq!(updated.name, "Updated API");
        assert_eq!(updated.url, "https://api.example.com/v2");
        assert_eq!(updated.refresh_interval_secs, 120);
    }

    #[test]
    fn update_missing_returns_none() {
        let (store, _dir) = open_store();
        let result = store.update("nonexistent", &test_config()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_source() {
        let (store, _dir) = open_store();
        let created = store.create(&test_config()).unwrap();
        assert!(store.delete(&created.id).unwrap());
        assert!(store.get(&created.id).unwrap().is_none());
    }

    #[test]
    fn delete_missing_returns_false() {
        let (store, _dir) = open_store();
        assert!(!store.delete("nonexistent").unwrap());
    }

    #[test]
    fn enforces_minimum_refresh_interval() {
        let (store, _dir) = open_store();
        let mut config = test_config();
        config.refresh_interval_secs = 5; // Below minimum
        let created = store.create(&config).unwrap();
        assert_eq!(created.refresh_interval_secs, MIN_REFRESH_INTERVAL_SECS);
    }

    #[test]
    fn rejects_invalid_method() {
        let (store, _dir) = open_store();
        let mut config = test_config();
        config.method = "PATCH".to_string();
        assert!(store.create(&config).is_err());
    }

    #[test]
    fn update_cached_data_and_clear_error() {
        let (store, _dir) = open_store();
        let created = store.create(&test_config()).unwrap();

        // Set an error first
        store.update_last_error(&created.id, "timeout").unwrap();
        let with_error = store.get(&created.id).unwrap().unwrap();
        assert_eq!(with_error.last_error.as_deref(), Some("timeout"));

        // Update cached data — should clear error
        let data = serde_json::json!({"temp": 72});
        store.update_cached_data(&created.id, &data, 1_700_000_000).unwrap();
        let updated = store.get(&created.id).unwrap().unwrap();
        assert_eq!(updated.cached_data.unwrap()["temp"], 72);
        assert_eq!(updated.last_fetched_at, Some(1_700_000_000));
        assert!(updated.last_error.is_none());
    }

    #[test]
    fn update_last_error_preserves_cached_data() {
        let (store, _dir) = open_store();
        let created = store.create(&test_config()).unwrap();

        let data = serde_json::json!({"temp": 72});
        store.update_cached_data(&created.id, &data, 1_000).unwrap();
        store.update_last_error(&created.id, "API error 503").unwrap();

        let updated = store.get(&created.id).unwrap().unwrap();
        assert_eq!(updated.cached_data.unwrap()["temp"], 72);
        assert_eq!(updated.last_error.as_deref(), Some("API error 503"));
    }

    #[test]
    fn rejects_oversized_cached_data() {
        let (store, _dir) = open_store();
        let created = store.create(&test_config()).unwrap();

        // Create data larger than 1MB
        let big_string = "x".repeat(MAX_CACHED_RESPONSE_BYTES + 1);
        let data = serde_json::json!({"data": big_string});
        assert!(store.update_cached_data(&created.id, &data, 1_000).is_err());
    }

    #[test]
    fn method_is_uppercased() {
        let (store, _dir) = open_store();
        let mut config = test_config();
        config.method = "post".to_string();
        let created = store.create(&config).unwrap();
        assert_eq!(created.method, "POST");
    }
}
