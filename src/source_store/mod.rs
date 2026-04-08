//! Source store — SQLite-backed persistence for user-configured generic HTTP data sources.
//!
//! Each row in `data_sources` describes an HTTP endpoint that the scheduler
//! polls on a configurable interval. The response JSON is cached in the row
//! and field mappings (in `data_source_fields`) extract individual values.
//!
//! Thread-safe via an internal `Mutex<Connection>`; wrap in `Arc` to share.

use std::{path::Path, sync::Mutex};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SourceStoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("I/O error creating directory '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

// ─── Data types ──────────────────────────────────────────────────────────────

/// A user-configured generic HTTP data source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSource {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    /// JSON-encoded array of {key, value} header pairs.
    pub headers: Option<String>,
    pub body_template: Option<String>,
    /// JSONPath applied to the response before caching.
    pub response_root_path: Option<String>,
    #[serde(default = "default_interval")]
    pub refresh_interval_secs: i64,
    pub cached_data: Option<String>,
    pub last_fetched_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn default_method() -> String {
    "GET".to_string()
}

fn default_interval() -> i64 {
    300
}

/// Configuration payload for creating or updating a generic data source.
/// Does not include cached_data, last_fetched_at, or last_error (server-managed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSourceConfig {
    pub name: String,
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    pub headers: Option<String>,
    pub body_template: Option<String>,
    pub response_root_path: Option<String>,
    #[serde(default = "default_interval")]
    pub refresh_interval_secs: i64,
}

// ─── Store ───────────────────────────────────────────────────────────────────

/// SQLite-backed store for generic data sources.
pub struct SourceStore {
    conn: Mutex<Connection>,
}

impl SourceStore {
    /// Open or create the SQLite database at `db_path` and run migrations.
    pub fn open(db_path: &Path) -> Result<Self, SourceStoreError> {
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| SourceStoreError::Io {
                    path: parent.to_string_lossy().into_owned(),
                    source: e,
                })?;
            }
        }
        let conn = Connection::open(db_path)?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn migrate(conn: &Connection) -> Result<(), SourceStoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS data_sources (
                id                    TEXT PRIMARY KEY,
                name                  TEXT NOT NULL,
                url                   TEXT NOT NULL,
                method                TEXT NOT NULL DEFAULT 'GET',
                headers               TEXT,
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

    /// Create a new data source from a config. Returns the created row.
    pub fn create(&self, config: &DataSourceConfig) -> Result<DataSource, SourceStoreError> {
        // Enforce minimum 30s refresh interval
        if config.refresh_interval_secs < 30 {
            return Err(SourceStoreError::Validation(
                "refresh_interval_secs must be at least 30".to_string(),
            ));
        }
        if config.url.is_empty() {
            return Err(SourceStoreError::Validation("url must not be empty".to_string()));
        }

        let now = unix_now();
        let id = format!("src-{}", now);
        let source = DataSource {
            id: id.clone(),
            name: config.name.clone(),
            url: config.url.clone(),
            method: config.method.clone(),
            headers: config.headers.clone(),
            body_template: config.body_template.clone(),
            response_root_path: config.response_root_path.clone(),
            refresh_interval_secs: config.refresh_interval_secs,
            cached_data: None,
            last_fetched_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        };

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO data_sources
             (id, name, url, method, headers, body_template, response_root_path,
              refresh_interval_secs, cached_data, last_fetched_at, last_error,
              created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                source.id,
                source.name,
                source.url,
                source.method,
                source.headers,
                source.body_template,
                source.response_root_path,
                source.refresh_interval_secs,
                source.cached_data,
                source.last_fetched_at,
                source.last_error,
                source.created_at,
                source.updated_at,
            ],
        )?;
        Ok(source)
    }

    /// Create a data source from a fully specified DataSource (used by preset creation).
    pub fn create_from_source(&self, source: &DataSource) -> Result<DataSource, SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO data_sources
             (id, name, url, method, headers, body_template, response_root_path,
              refresh_interval_secs, cached_data, last_fetched_at, last_error,
              created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                source.id,
                source.name,
                source.url,
                source.method,
                source.headers,
                source.body_template,
                source.response_root_path,
                source.refresh_interval_secs,
                source.cached_data,
                source.last_fetched_at,
                source.last_error,
                source.created_at,
                source.updated_at,
            ],
        )?;
        Ok(source.clone())
    }

    /// Update a data source's configuration. Returns updated row or None if not found.
    pub fn update(
        &self,
        id: &str,
        config: &DataSourceConfig,
    ) -> Result<Option<DataSource>, SourceStoreError> {
        if config.refresh_interval_secs < 30 {
            return Err(SourceStoreError::Validation(
                "refresh_interval_secs must be at least 30".to_string(),
            ));
        }

        let now = unix_now();
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "UPDATE data_sources
             SET name = ?1, url = ?2, method = ?3, headers = ?4, body_template = ?5,
                 response_root_path = ?6, refresh_interval_secs = ?7, updated_at = ?8
             WHERE id = ?9",
            params![
                config.name,
                config.url,
                config.method,
                config.headers,
                config.body_template,
                config.response_root_path,
                config.refresh_interval_secs,
                now,
                id,
            ],
        )?;
        drop(conn);

        if rows == 0 {
            return Ok(None);
        }
        self.get(id)
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

    /// List all data sources, sorted by name.
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

    /// Delete a data source by ID. Returns true if a row was deleted.
    pub fn delete(&self, id: &str) -> Result<bool, SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute("DELETE FROM data_sources WHERE id = ?1", params![id])?;
        Ok(rows > 0)
    }

    /// Update cached data and last_fetched_at for a source. Clears last_error.
    pub fn update_cached_data(
        &self,
        id: &str,
        data: &serde_json::Value,
        fetched_at: i64,
    ) -> Result<(), SourceStoreError> {
        let data_str = serde_json::to_string(data)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE data_sources
             SET cached_data = ?1, last_fetched_at = ?2, last_error = NULL, updated_at = ?2
             WHERE id = ?3",
            params![data_str, fetched_at, id],
        )?;
        Ok(())
    }

    /// Update last_error for a source. Preserves cached_data.
    pub fn update_last_error(&self, id: &str, error: &str) -> Result<(), SourceStoreError> {
        let now = unix_now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE data_sources SET last_error = ?1, updated_at = ?2 WHERE id = ?3",
            params![error, now, id],
        )?;
        Ok(())
    }
}

fn row_to_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<DataSource> {
    Ok(DataSource {
        id: row.get(0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        method: row.get(3)?,
        headers: row.get(4)?,
        body_template: row.get(5)?,
        response_root_path: row.get(6)?,
        refresh_interval_secs: row.get(7)?,
        cached_data: row.get(8)?,
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

// ─── Tests ──────────────────────────────────────────────────────────────────

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

    fn test_config(name: &str) -> DataSourceConfig {
        DataSourceConfig {
            name: name.to_string(),
            url: "https://example.com/api".to_string(),
            method: "GET".to_string(),
            headers: None,
            body_template: None,
            response_root_path: None,
            refresh_interval_secs: 300,
        }
    }

    fn test_source(id: &str, name: &str) -> DataSource {
        let now = unix_now();
        DataSource {
            id: id.to_string(),
            name: name.to_string(),
            url: "https://example.com/api".to_string(),
            method: "GET".to_string(),
            headers: None,
            body_template: None,
            response_root_path: None,
            refresh_interval_secs: 300,
            cached_data: None,
            last_fetched_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn create_and_get() {
        let (store, _dir) = open_store();
        let ds = store.create(&test_config("Test Source")).unwrap();
        assert!(!ds.id.is_empty());
        assert_eq!(ds.name, "Test Source");

        let retrieved = store.get(&ds.id).unwrap().expect("should exist");
        assert_eq!(retrieved.name, "Test Source");
        assert_eq!(retrieved.url, "https://example.com/api");
        assert_eq!(retrieved.method, "GET");
        assert!(retrieved.cached_data.is_none());
    }

    #[test]
    fn create_from_source_works() {
        let (store, _dir) = open_store();
        let src = test_source("test-1", "Test Source");
        store.create_from_source(&src).unwrap();

        let retrieved = store.get("test-1").unwrap().expect("should exist");
        assert_eq!(retrieved.id, "test-1");
        assert_eq!(retrieved.name, "Test Source");
    }

    #[test]
    fn create_rejects_short_interval() {
        let (store, _dir) = open_store();
        let mut cfg = test_config("Bad");
        cfg.refresh_interval_secs = 10;
        let err = store.create(&cfg).unwrap_err();
        assert!(err.to_string().contains("at least 30"));
    }

    #[test]
    fn get_returns_none_for_missing() {
        let (store, _dir) = open_store();
        assert!(store.get("nonexistent").unwrap().is_none());
    }

    #[test]
    fn list_returns_sorted_by_name() {
        let (store, _dir) = open_store();
        store.create_from_source(&test_source("c", "Zebra")).unwrap();
        store.create_from_source(&test_source("a", "Alpha")).unwrap();
        store.create_from_source(&test_source("b", "Mid")).unwrap();

        let sources = store.list().unwrap();
        assert_eq!(sources.len(), 3);
        assert_eq!(sources[0].name, "Alpha");
        assert_eq!(sources[1].name, "Mid");
        assert_eq!(sources[2].name, "Zebra");
    }

    #[test]
    fn delete_removes_source() {
        let (store, _dir) = open_store();
        let ds = store.create(&test_config("Deletable")).unwrap();
        assert!(store.delete(&ds.id).unwrap());
        assert!(store.get(&ds.id).unwrap().is_none());
        assert!(!store.delete(&ds.id).unwrap());
    }

    #[test]
    fn update_cached_data_and_clear_error() {
        let (store, _dir) = open_store();
        let ds = store.create(&test_config("Source")).unwrap();

        store.update_last_error(&ds.id, "timeout").unwrap();
        let s = store.get(&ds.id).unwrap().unwrap();
        assert_eq!(s.last_error.as_deref(), Some("timeout"));

        let data = serde_json::json!({"temp": 55});
        store
            .update_cached_data(&ds.id, &data, 1_700_000_000)
            .unwrap();
        let s = store.get(&ds.id).unwrap().unwrap();
        assert!(s.cached_data.is_some());
        assert_eq!(s.last_fetched_at, Some(1_700_000_000));
        assert!(s.last_error.is_none());
    }

    #[test]
    fn update_source_config() {
        let (store, _dir) = open_store();
        let ds = store.create(&test_config("Original")).unwrap();

        let updated_config = DataSourceConfig {
            name: "Updated".to_string(),
            url: "https://new.example.com".to_string(),
            method: "POST".to_string(),
            headers: None,
            body_template: Some("{}".to_string()),
            response_root_path: None,
            refresh_interval_secs: 60,
        };
        let updated = store.update(&ds.id, &updated_config).unwrap().unwrap();
        assert_eq!(updated.name, "Updated");
        assert_eq!(updated.url, "https://new.example.com");
        assert_eq!(updated.method, "POST");
    }

    #[test]
    fn update_nonexistent_returns_none() {
        let (store, _dir) = open_store();
        let result = store.update("no-such-id", &test_config("X")).unwrap();
        assert!(result.is_none());
    }
}
