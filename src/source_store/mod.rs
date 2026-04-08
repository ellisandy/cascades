//! Data source store — SQLite-backed persistence for generic HTTP data sources.
//!
//! Manages the `data_sources` table including encrypted header storage.
//! Header values marked as "secret" are encrypted via AES-256-GCM before
//! storage and decrypted on load. GET API responses mask encrypted values.
//!
//! # Schema
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS data_sources (
//!     id                    TEXT PRIMARY KEY,
//!     name                  TEXT NOT NULL,
//!     url                   TEXT NOT NULL,
//!     method                TEXT NOT NULL DEFAULT 'GET',
//!     headers               TEXT,
//!     encrypted_headers     TEXT,
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

use crate::crypto::{self, CryptoError, EncryptionKey};

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SourceStoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("I/O error creating directory '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("data source not found: {0}")]
    NotFound(String),
}

// ─── Data types ──────────────────────────────────────────────────────────────

/// A single HTTP header: key-value pair with an optional "secret" flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub key: String,
    pub value: String,
    /// If true, the value is stored encrypted in `encrypted_headers`.
    #[serde(default)]
    pub secret: bool,
}

/// A generic HTTP data source with optional encrypted headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSource {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    /// Non-secret headers (stored plaintext).
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    /// Secret headers (values are encrypted at rest, decrypted on load).
    #[serde(default)]
    pub encrypted_headers: Vec<HeaderEntry>,
    pub body_template: Option<String>,
    pub response_root_path: Option<String>,
    #[serde(default = "default_interval")]
    pub refresh_interval_secs: i64,
    pub cached_data: Option<serde_json::Value>,
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

impl DataSource {
    /// Return all headers (plain + decrypted secret) for use in HTTP requests.
    pub fn all_headers(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for h in &self.headers {
            out.push((h.key.clone(), h.value.clone()));
        }
        for h in &self.encrypted_headers {
            out.push((h.key.clone(), h.value.clone()));
        }
        out
    }

    /// Return a masked copy for API responses: secret header values replaced with placeholder.
    pub fn masked_for_api(&self) -> DataSourceMasked {
        DataSourceMasked {
            id: self.id.clone(),
            name: self.name.clone(),
            url: self.url.clone(),
            method: self.method.clone(),
            headers: self.headers.iter().map(|h| MaskedHeader {
                key: h.key.clone(),
                value: h.value.clone(),
                secret: false,
            }).collect(),
            encrypted_headers: self.encrypted_headers.iter().map(|h| MaskedHeader {
                key: h.key.clone(),
                value: "••••••".to_string(),
                secret: true,
            }).collect(),
            body_template: self.body_template.clone(),
            response_root_path: self.response_root_path.clone(),
            refresh_interval_secs: self.refresh_interval_secs,
            cached_data: self.cached_data.clone(),
            last_fetched_at: self.last_fetched_at,
            last_error: self.last_error.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// API-safe representation with secret values masked.
#[derive(Debug, Clone, Serialize)]
pub struct DataSourceMasked {
    pub id: String,
    pub name: String,
    pub url: String,
    pub method: String,
    pub headers: Vec<MaskedHeader>,
    pub encrypted_headers: Vec<MaskedHeader>,
    pub body_template: Option<String>,
    pub response_root_path: Option<String>,
    pub refresh_interval_secs: i64,
    pub cached_data: Option<serde_json::Value>,
    pub last_fetched_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MaskedHeader {
    pub key: String,
    pub value: String,
    pub secret: bool,
}

// ─── Store ───────────────────────────────────────────────────────────────────

/// SQLite-backed store for generic data sources with encrypted header support.
pub struct SourceStore {
    conn: Mutex<Connection>,
    encryption_key: EncryptionKey,
}

impl SourceStore {
    /// Open or create the store, sharing a SQLite file with other stores.
    pub fn open(db_path: &Path, encryption_key: EncryptionKey) -> Result<Self, SourceStoreError> {
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
        Ok(Self {
            conn: Mutex::new(conn),
            encryption_key,
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
                encrypted_headers     TEXT,
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

    /// Create a new data source. Secret header values are encrypted before storage.
    pub fn create(&self, source: &DataSource) -> Result<bool, SourceStoreError> {
        let headers_json = serde_json::to_string(&source.headers)?;
        let encrypted_json = self.encrypt_headers(&source.encrypted_headers)?;
        let cached_json = source
            .cached_data
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "INSERT OR IGNORE INTO data_sources
             (id, name, url, method, headers, encrypted_headers, body_template,
              response_root_path, refresh_interval_secs, cached_data, last_fetched_at,
              last_error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                source.id,
                source.name,
                source.url,
                source.method,
                headers_json,
                encrypted_json,
                source.body_template,
                source.response_root_path,
                source.refresh_interval_secs,
                cached_json,
                source.last_fetched_at,
                source.last_error,
                source.created_at,
                source.updated_at,
            ],
        )?;
        Ok(rows == 1)
    }

    /// Get a data source by ID with encrypted headers decrypted.
    pub fn get(&self, id: &str) -> Result<Option<DataSource>, SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, url, method, headers, encrypted_headers, body_template,
                    response_root_path, refresh_interval_secs, cached_data, last_fetched_at,
                    last_error, created_at, updated_at
             FROM data_sources WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(self.row_to_source(row)?)),
            None => Ok(None),
        }
    }

    /// List all data sources with encrypted headers decrypted.
    pub fn list(&self) -> Result<Vec<DataSource>, SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, url, method, headers, encrypted_headers, body_template,
                    response_root_path, refresh_interval_secs, cached_data, last_fetched_at,
                    last_error, created_at, updated_at
             FROM data_sources ORDER BY name",
        )?;
        let mut rows = stmt.query([])?;
        let mut sources = Vec::new();
        while let Some(row) = rows.next()? {
            sources.push(self.row_to_source(row)?);
        }
        Ok(sources)
    }

    /// Update a data source's configuration (not cached data).
    /// If `encrypted_headers` contains entries, their values are encrypted.
    pub fn update(&self, source: &DataSource) -> Result<(), SourceStoreError> {
        let headers_json = serde_json::to_string(&source.headers)?;
        let encrypted_json = self.encrypt_headers(&source.encrypted_headers)?;

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE data_sources
             SET name = ?1, url = ?2, method = ?3, headers = ?4, encrypted_headers = ?5,
                 body_template = ?6, response_root_path = ?7, refresh_interval_secs = ?8,
                 updated_at = ?9
             WHERE id = ?10",
            params![
                source.name,
                source.url,
                source.method,
                headers_json,
                encrypted_json,
                source.body_template,
                source.response_root_path,
                source.refresh_interval_secs,
                source.updated_at,
                source.id,
            ],
        )?;
        Ok(())
    }

    /// Delete a data source by ID.
    pub fn delete(&self, id: &str) -> Result<bool, SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute("DELETE FROM data_sources WHERE id = ?1", params![id])?;
        Ok(rows > 0)
    }

    /// Update cached data after a successful fetch.
    pub fn update_cached_data(
        &self,
        id: &str,
        data: &serde_json::Value,
        fetched_at: i64,
    ) -> Result<(), SourceStoreError> {
        let data_json = serde_json::to_string(data)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE data_sources
             SET cached_data = ?1, last_fetched_at = ?2, last_error = NULL, updated_at = ?2
             WHERE id = ?3",
            params![data_json, fetched_at, id],
        )?;
        Ok(())
    }

    /// Update last error after a failed fetch.
    pub fn update_last_error(&self, id: &str, error: &str) -> Result<(), SourceStoreError> {
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE data_sources SET last_error = ?1, updated_at = ?2 WHERE id = ?3",
            params![error, now, id],
        )?;
        Ok(())
    }

    /// Re-encrypt all encrypted headers with a new key (key rotation).
    /// Call this when the api_key in secrets.toml changes.
    pub fn rotate_encryption_key(
        &self,
        old_key: &EncryptionKey,
        new_key: &EncryptionKey,
    ) -> Result<usize, SourceStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, encrypted_headers FROM data_sources WHERE encrypted_headers IS NOT NULL",
        )?;
        let mut rows = stmt.query([])?;

        let mut updates: Vec<(String, String)> = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let enc_json: String = row.get(1)?;
            let stored: Vec<StoredEncryptedHeader> = serde_json::from_str(&enc_json)?;

            let rotated: Vec<StoredEncryptedHeader> = stored
                .into_iter()
                .map(|mut h| {
                    let plaintext = crypto::decrypt(old_key, &h.encrypted_value)?;
                    h.encrypted_value = crypto::encrypt(new_key, &plaintext)?;
                    Ok(h)
                })
                .collect::<Result<Vec<_>, CryptoError>>()?;

            let new_json = serde_json::to_string(&rotated)?;
            updates.push((id, new_json));
        }
        drop(rows);
        drop(stmt);

        let count = updates.len();
        for (id, json) in &updates {
            let now = now_secs();
            conn.execute(
                "UPDATE data_sources SET encrypted_headers = ?1, updated_at = ?2 WHERE id = ?3",
                params![json, now, id],
            )?;
        }
        Ok(count)
    }

    // ─── Internal helpers ────────────────────────────────────────────────────

    /// Encrypt header entries for storage. Returns JSON string of StoredEncryptedHeader[].
    fn encrypt_headers(&self, headers: &[HeaderEntry]) -> Result<Option<String>, SourceStoreError> {
        if headers.is_empty() {
            return Ok(None);
        }
        let stored: Vec<StoredEncryptedHeader> = headers
            .iter()
            .map(|h| {
                let encrypted_value = crypto::encrypt(&self.encryption_key, &h.value)?;
                Ok(StoredEncryptedHeader {
                    key: h.key.clone(),
                    encrypted_value,
                })
            })
            .collect::<Result<Vec<_>, CryptoError>>()?;
        Ok(Some(serde_json::to_string(&stored)?))
    }

    /// Decrypt stored encrypted headers back to HeaderEntry with secret=true.
    fn decrypt_headers(
        &self,
        json: Option<&str>,
    ) -> Result<Vec<HeaderEntry>, SourceStoreError> {
        let json = match json {
            Some(j) if !j.is_empty() => j,
            _ => return Ok(Vec::new()),
        };
        let stored: Vec<StoredEncryptedHeader> = serde_json::from_str(json)?;
        stored
            .into_iter()
            .map(|h| {
                let value = crypto::decrypt(&self.encryption_key, &h.encrypted_value)?;
                Ok(HeaderEntry {
                    key: h.key,
                    value,
                    secret: true,
                })
            })
            .collect::<Result<Vec<_>, CryptoError>>()
            .map_err(Into::into)
    }

    fn row_to_source(&self, row: &rusqlite::Row) -> Result<DataSource, SourceStoreError> {
        let headers_json: Option<String> = row.get(4)?;
        let encrypted_json: Option<String> = row.get(5)?;
        let cached_json: Option<String> = row.get(9)?;

        let headers: Vec<HeaderEntry> = match headers_json.as_deref() {
            Some(j) if !j.is_empty() => serde_json::from_str(j).unwrap_or_default(),
            _ => Vec::new(),
        };
        let encrypted_headers = self.decrypt_headers(encrypted_json.as_deref())?;
        let cached_data: Option<serde_json::Value> = cached_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok());

        Ok(DataSource {
            id: row.get(0)?,
            name: row.get(1)?,
            url: row.get(2)?,
            method: row.get(3)?,
            headers,
            encrypted_headers,
            body_template: row.get(6)?,
            response_root_path: row.get(7)?,
            refresh_interval_secs: row.get(8)?,
            cached_data,
            last_fetched_at: row.get(10)?,
            last_error: row.get(11)?,
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
        })
    }
}

/// Internal representation of an encrypted header in the database.
#[derive(Debug, Serialize, Deserialize)]
struct StoredEncryptedHeader {
    key: String,
    encrypted_value: String,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn test_key() -> EncryptionKey {
        EncryptionKey::derive_from_api_key(
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
        )
    }

    fn make_store() -> (SourceStore, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let store = SourceStore::open(f.path(), test_key()).unwrap();
        (store, f)
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn sample_source() -> DataSource {
        let t = now();
        DataSource {
            id: "src-1".into(),
            name: "USGS Water API".into(),
            url: "https://waterservices.usgs.gov/nwis/iv/".into(),
            method: "GET".into(),
            headers: vec![HeaderEntry {
                key: "Accept".into(),
                value: "application/json".into(),
                secret: false,
            }],
            encrypted_headers: vec![HeaderEntry {
                key: "Authorization".into(),
                value: "Bearer sk-secret-key-12345".into(),
                secret: true,
            }],
            body_template: None,
            response_root_path: Some("$.value.timeSeries[0]".into()),
            refresh_interval_secs: 300,
            cached_data: None,
            last_fetched_at: None,
            last_error: None,
            created_at: t,
            updated_at: t,
        }
    }

    #[test]
    fn create_and_get_roundtrip() {
        let (store, _f) = make_store();
        let src = sample_source();
        assert!(store.create(&src).unwrap());

        let loaded = store.get("src-1").unwrap().unwrap();
        assert_eq!(loaded.id, "src-1");
        assert_eq!(loaded.name, "USGS Water API");
        assert_eq!(loaded.headers.len(), 1);
        assert_eq!(loaded.headers[0].key, "Accept");
        assert_eq!(loaded.headers[0].value, "application/json");
        // Encrypted headers are decrypted on load
        assert_eq!(loaded.encrypted_headers.len(), 1);
        assert_eq!(loaded.encrypted_headers[0].key, "Authorization");
        assert_eq!(
            loaded.encrypted_headers[0].value,
            "Bearer sk-secret-key-12345"
        );
        assert!(loaded.encrypted_headers[0].secret);
    }

    #[test]
    fn encrypted_values_not_plaintext_in_db() {
        let (store, f) = make_store();
        store.create(&sample_source()).unwrap();

        // Read raw DB value to confirm it's not plaintext
        let conn = Connection::open(f.path()).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT encrypted_headers FROM data_sources WHERE id = 'src-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!raw.contains("sk-secret-key-12345"));
        assert!(raw.contains("encrypted_value"));
    }

    #[test]
    fn list_sources() {
        let (store, _f) = make_store();
        let mut s1 = sample_source();
        s1.id = "src-a".into();
        s1.name = "Alpha".into();
        let mut s2 = sample_source();
        s2.id = "src-b".into();
        s2.name = "Beta".into();
        store.create(&s1).unwrap();
        store.create(&s2).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "Alpha");
        assert_eq!(list[1].name, "Beta");
    }

    #[test]
    fn update_source() {
        let (store, _f) = make_store();
        let mut src = sample_source();
        store.create(&src).unwrap();

        src.url = "https://new-url.example.com".into();
        src.encrypted_headers = vec![HeaderEntry {
            key: "X-Api-Key".into(),
            value: "new-secret-value".into(),
            secret: true,
        }];
        src.updated_at = now();
        store.update(&src).unwrap();

        let loaded = store.get("src-1").unwrap().unwrap();
        assert_eq!(loaded.url, "https://new-url.example.com");
        assert_eq!(loaded.encrypted_headers[0].value, "new-secret-value");
    }

    #[test]
    fn delete_source() {
        let (store, _f) = make_store();
        store.create(&sample_source()).unwrap();
        assert!(store.delete("src-1").unwrap());
        assert!(store.get("src-1").unwrap().is_none());
        assert!(!store.delete("src-1").unwrap());
    }

    #[test]
    fn masked_api_response() {
        let src = sample_source();
        let masked = src.masked_for_api();
        assert_eq!(masked.headers[0].value, "application/json");
        assert!(!masked.headers[0].secret);
        assert_eq!(masked.encrypted_headers[0].value, "••••••");
        assert!(masked.encrypted_headers[0].secret);
    }

    #[test]
    fn key_rotation() {
        let old_key = EncryptionKey::derive_from_api_key("old-key-aabbccdd11223344");
        let new_key = EncryptionKey::derive_from_api_key("new-key-eeff00112233aabb");

        let f = NamedTempFile::new().unwrap();
        let store = SourceStore::open(f.path(), old_key.clone()).unwrap();
        store.create(&sample_source()).unwrap();

        // Verify old key can decrypt
        let loaded = store.get("src-1").unwrap().unwrap();
        assert_eq!(
            loaded.encrypted_headers[0].value,
            "Bearer sk-secret-key-12345"
        );

        // Rotate
        let count = store.rotate_encryption_key(&old_key, &new_key).unwrap();
        assert_eq!(count, 1);

        // Old key can no longer decrypt (open new store with new key)
        let store2 = SourceStore::open(f.path(), new_key).unwrap();
        let loaded2 = store2.get("src-1").unwrap().unwrap();
        assert_eq!(
            loaded2.encrypted_headers[0].value,
            "Bearer sk-secret-key-12345"
        );
    }

    #[test]
    fn source_with_no_encrypted_headers() {
        let (store, _f) = make_store();
        let t = now();
        let src = DataSource {
            id: "plain".into(),
            name: "No secrets".into(),
            url: "https://example.com".into(),
            method: "GET".into(),
            headers: vec![],
            encrypted_headers: vec![],
            body_template: None,
            response_root_path: None,
            refresh_interval_secs: 60,
            cached_data: None,
            last_fetched_at: None,
            last_error: None,
            created_at: t,
            updated_at: t,
        };
        store.create(&src).unwrap();
        let loaded = store.get("plain").unwrap().unwrap();
        assert!(loaded.encrypted_headers.is_empty());
    }

    #[test]
    fn all_headers_combines_both() {
        let src = sample_source();
        let all = src.all_headers();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], ("Accept".into(), "application/json".into()));
        assert_eq!(
            all[1],
            ("Authorization".into(), "Bearer sk-secret-key-12345".into())
        );
    }
}
