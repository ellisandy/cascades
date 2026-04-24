//! Asset store — SQLite-backed persistence for user-uploaded image assets.
//!
//! Phase 6 introduces user-supplied images (logos, decorations, backgrounds)
//! that drop onto layouts via `LayoutItem::Image`. Storage is content-addressed
//! by SHA-256 so re-uploading identical bytes returns the existing id rather
//! than duplicating the BLOB.
//!
//! # Schema
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS assets (
//!     id          TEXT PRIMARY KEY,        -- "asset-<unix_millis>" — user-facing
//!     filename    TEXT NOT NULL,           -- original upload filename, for UI display
//!     mime        TEXT NOT NULL,           -- "image/png" | "image/jpeg"
//!     bytes       BLOB NOT NULL,           -- raw file bytes; capped at 1 MiB upstream
//!     sha256      TEXT NOT NULL UNIQUE,    -- hex-encoded; UNIQUE drives dedup
//!     created_at  INTEGER NOT NULL         -- unix seconds
//! );
//! ```

use std::{path::Path, sync::Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Hard cap on a single asset's stored bytes. Enforced both at the upload
/// route (axum `DefaultBodyLimit`) and here as a defence-in-depth check.
pub const MAX_ASSET_BYTES: usize = 1_048_576; // 1 MiB

/// MIME types accepted by the asset pipeline. SVG is deliberately excluded for
/// v1 — the compositor decodes assets via `image::load_from_memory`, which is
/// raster-only. SVG support is a v1.x deferral.
pub const ALLOWED_MIMES: &[&str] = &["image/png", "image/jpeg"];

#[derive(Debug, Error)]
pub enum AssetStoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("I/O error creating directory '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("validation error: {0}")]
    Validation(String),
}

/// A stored asset — bytes + metadata.
#[derive(Debug, Clone)]
pub struct Asset {
    pub id: String,
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub created_at: i64,
}

/// Lightweight summary used by the admin asset library list (omits `bytes`).
#[derive(Debug, Clone, Serialize)]
pub struct AssetSummary {
    pub id: String,
    pub filename: String,
    pub mime: String,
    pub size: i64,
    pub created_at: i64,
}

/// SQLite-backed asset store. Thread-safe via an internal `Mutex<Connection>`;
/// wrap in `Arc` for shared use.
pub struct AssetStore {
    conn: Mutex<Connection>,
}

impl AssetStore {
    /// Open or create the SQLite database at `db_path` and run migrations.
    /// Safe to open against the same file as the other stores.
    pub fn open(db_path: &Path) -> Result<Self, AssetStoreError> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| AssetStoreError::Io {
                path: parent.to_string_lossy().into_owned(),
                source: e,
            })?;
        }
        let conn = Connection::open(db_path)?;
        Self::migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn migrate(conn: &Connection) -> Result<(), AssetStoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS assets (
                id          TEXT PRIMARY KEY,
                filename    TEXT NOT NULL,
                mime        TEXT NOT NULL,
                bytes       BLOB NOT NULL,
                sha256      TEXT NOT NULL UNIQUE,
                created_at  INTEGER NOT NULL
            );",
        )?;
        Ok(())
    }

    /// Insert a new asset, or — if the bytes already exist (matched by SHA-256)
    /// — return the existing asset's id without writing again. Validates size,
    /// MIME, and refuses empty bytes.
    pub fn insert_or_get(
        &self,
        bytes: &[u8],
        filename: &str,
        mime: &str,
    ) -> Result<String, AssetStoreError> {
        if bytes.is_empty() {
            return Err(AssetStoreError::Validation("empty asset bytes".into()));
        }
        if bytes.len() > MAX_ASSET_BYTES {
            return Err(AssetStoreError::Validation(format!(
                "asset exceeds {} byte cap (got {})",
                MAX_ASSET_BYTES,
                bytes.len()
            )));
        }
        if !ALLOWED_MIMES.contains(&mime) {
            return Err(AssetStoreError::Validation(format!(
                "unsupported MIME '{mime}'; allowed: {ALLOWED_MIMES:?}"
            )));
        }
        let sha256 = hex_sha256(bytes);

        let conn = self.conn.lock().unwrap();
        // Dedup: if the hash already exists, return that id. Same bytes = same
        // asset, regardless of filename — saves disk and avoids confusing the
        // user with two ids for one image.
        if let Some(existing_id) = conn
            .query_row(
                "SELECT id FROM assets WHERE sha256 = ?1",
                params![sha256],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(existing_id);
        }

        let id = format!(
            "asset-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let now = unix_now();
        conn.execute(
            "INSERT INTO assets (id, filename, mime, bytes, sha256, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, filename, mime, bytes, sha256, now],
        )?;
        Ok(id)
    }

    /// Fetch an asset by id. Returns `Ok(None)` if not found.
    pub fn get(&self, id: &str) -> Result<Option<Asset>, AssetStoreError> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT id, filename, mime, bytes, sha256, created_at
                 FROM assets WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Asset {
                        id: r.get(0)?,
                        filename: r.get(1)?,
                        mime: r.get(2)?,
                        bytes: r.get(3)?,
                        sha256: r.get(4)?,
                        created_at: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// List all assets as summaries (no bytes). Most-recent first.
    pub fn list(&self) -> Result<Vec<AssetSummary>, AssetStoreError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, filename, mime, length(bytes), created_at
             FROM assets ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(AssetSummary {
                    id: r.get(0)?,
                    filename: r.get(1)?,
                    mime: r.get(2)?,
                    size: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// MIME-sniff a small byte buffer. Returns one of the `ALLOWED_MIMES` strings
/// or `None` if the magic bytes don't match a supported format. Hand-rolled to
/// avoid pulling in an extra crate for two signatures.
pub fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    // JPEG: FF D8 FF (covers JFIF, EXIF, raw — common variants)
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    None
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (tempfile::TempDir, AssetStore) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = AssetStore::open(&dir.path().join("test.db")).unwrap();
        (dir, store)
    }

    /// Minimal valid 1×1 PNG — 67 bytes. Used so tests never touch the
    /// filesystem and the file stays self-contained.
    fn one_pixel_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // signature
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR len + tag
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1×1
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, // depth/color/etc + crc
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, // IDAT len + tag
            0x78, 0x9C, 0x62, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01,
            0x0D, 0x0A, 0x2D, 0xB4, // IDAT data + crc
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82, // IEND
        ]
    }

    #[test]
    fn sniff_recognizes_png_and_jpeg() {
        assert_eq!(sniff_mime(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]), Some("image/png"));
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_mime(b"GIF89a"), None);
        assert_eq!(sniff_mime(&[]), None);
    }

    #[test]
    fn insert_and_get_roundtrip() {
        let (_d, store) = tmp_store();
        let png = one_pixel_png();
        let id = store.insert_or_get(&png, "logo.png", "image/png").unwrap();
        let got = store.get(&id).unwrap().unwrap();
        assert_eq!(got.filename, "logo.png");
        assert_eq!(got.mime, "image/png");
        assert_eq!(got.bytes, png);
        assert_eq!(got.sha256.len(), 64);
    }

    #[test]
    fn dedup_returns_same_id_for_same_bytes() {
        let (_d, store) = tmp_store();
        let png = one_pixel_png();
        let id1 = store.insert_or_get(&png, "a.png", "image/png").unwrap();
        // Different filename — should still dedupe on content hash.
        let id2 = store.insert_or_get(&png, "b.png", "image/png").unwrap();
        assert_eq!(id1, id2);
        // And the stored filename keeps the *first* upload's name (ids are
        // immutable; renames would require a separate API).
        assert_eq!(store.get(&id1).unwrap().unwrap().filename, "a.png");
    }

    #[test]
    fn rejects_empty_oversized_and_unknown_mime() {
        let (_d, store) = tmp_store();
        // empty
        assert!(matches!(
            store.insert_or_get(&[], "x.png", "image/png"),
            Err(AssetStoreError::Validation(_))
        ));
        // oversized
        let big = vec![0u8; MAX_ASSET_BYTES + 1];
        assert!(matches!(
            store.insert_or_get(&big, "big.png", "image/png"),
            Err(AssetStoreError::Validation(_))
        ));
        // unknown MIME
        let png = one_pixel_png();
        assert!(matches!(
            store.insert_or_get(&png, "x.svg", "image/svg+xml"),
            Err(AssetStoreError::Validation(_))
        ));
    }

    #[test]
    fn get_unknown_id_returns_none() {
        let (_d, store) = tmp_store();
        assert!(store.get("asset-doesnotexist").unwrap().is_none());
    }

    #[test]
    fn list_returns_summaries_sorted_newest_first() {
        let (_d, store) = tmp_store();
        let a = store.insert_or_get(&one_pixel_png(), "first.png", "image/png").unwrap();
        // Tweak one byte to get a distinct hash without growing the test.
        let mut other = one_pixel_png();
        other[20] ^= 0x01;
        // Sleep 1s to guarantee distinct created_at — the table sorts by
        // created_at DESC and the unix-second granularity could otherwise tie.
        std::thread::sleep(std::time::Duration::from_secs(1));
        let b = store.insert_or_get(&other, "second.png", "image/png").unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        // Newest first.
        assert_eq!(list[0].id, b);
        assert_eq!(list[1].id, a);
        assert!(list[0].size > 0);
    }
}
