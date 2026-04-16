//! Generic HTTP data source — fetches arbitrary JSON from user-configured endpoints.
//!
//! Users configure these via the admin UI. Each source defines a URL, method,
//! headers, optional request body, and an optional `response_root_path` (JSONPath)
//! for extracting a sub-tree from the response before caching.

use crate::crypto::EncryptionKey;
use crate::jsonpath::jsonpath_extract;
use crate::sources::{Source, SourceError};
use std::time::Duration;

use crate::source_store::MAX_CACHED_RESPONSE_BYTES;

/// A generic HTTP source configured at runtime via the admin UI.
pub struct GenericHttpSource {
    source_id: String,
    source_name: String,
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    response_root_path: Option<String>,
    refresh: Duration,
}

impl GenericHttpSource {
    pub fn new(
        id: String,
        name: String,
        url: String,
        method: String,
        headers: Vec<(String, String)>,
        body: Option<String>,
        response_root_path: Option<String>,
        refresh_interval_secs: u64,
    ) -> Self {
        Self {
            source_id: id,
            source_name: name,
            url,
            method,
            headers,
            body,
            response_root_path,
            refresh: Duration::from_secs(refresh_interval_secs),
        }
    }

    /// Build from a DataSource record. If an encryption key is provided,
    /// encrypted headers are decrypted and merged with plaintext headers.
    pub fn from_data_source(
        ds: &crate::source_store::DataSource,
        encryption_key: Option<&EncryptionKey>,
    ) -> Self {
        let mut headers: Vec<(String, String)> = ds
            .headers
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        // Decrypt and merge encrypted headers
        if let Some(key) = encryption_key {
            if let Some(arr) = ds.encrypted_headers.as_array() {
                for entry in arr {
                    if let (Some(k), Some(encrypted_val)) =
                        (entry["key"].as_str(), entry["value"].as_str())
                    {
                        match crate::crypto::decrypt(key, encrypted_val) {
                            Ok(plaintext) => headers.push((k.to_string(), plaintext)),
                            Err(e) => {
                                log::warn!(
                                    "failed to decrypt header '{}' for source '{}': {}",
                                    k, ds.id, e
                                );
                            }
                        }
                    }
                }
            }
        }

        Self::new(
            ds.id.clone(),
            ds.name.clone(),
            ds.url.clone(),
            ds.method.clone(),
            headers,
            ds.body_template.clone(),
            ds.response_root_path.clone(),
            ds.refresh_interval_secs as u64,
        )
    }
}

impl Source for GenericHttpSource {
    fn id(&self) -> &str {
        &self.source_id
    }

    fn name(&self) -> &str {
        &self.source_name
    }

    fn refresh_interval(&self) -> Duration {
        self.refresh
    }

    fn fetch(&self) -> Result<serde_json::Value, SourceError> {
        let mut req = match self.method.as_str() {
            "POST" => ureq::post(&self.url),
            _ => ureq::get(&self.url),
        };

        req = req.timeout(Duration::from_secs(30));

        for (key, value) in &self.headers {
            req = req.set(key, value);
        }

        let response = if let Some(body) = &self.body {
            req.set("Content-Type", "application/json")
                .send_string(body)
        } else {
            req.call()
        };

        let response = response.map_err(|e| SourceError::Network(e.to_string()))?;

        let body = response
            .into_string()
            .map_err(|e| SourceError::Parse(format!("failed to read response body: {e}")))?;

        if body.len() > MAX_CACHED_RESPONSE_BYTES {
            return Err(SourceError::Other(format!(
                "response exceeds maximum size ({} bytes > {} bytes)",
                body.len(),
                MAX_CACHED_RESPONSE_BYTES
            )));
        }

        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| SourceError::Parse(format!("invalid JSON: {e}")))?;

        // Apply response_root_path if set
        if let Some(path) = &self.response_root_path {
            if !path.is_empty() {
                let extracted = jsonpath_extract(&value, path)
                    .map_err(|e| SourceError::Parse(format!("JSONPath extraction failed: {e}")))?;
                return Ok(extracted.clone());
            }
        }

        Ok(value)
    }
}
