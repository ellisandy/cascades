//! Generic HTTP data source — user-configurable endpoint that implements the
//! `Source` trait. Created from the `data_sources` table rows or from presets.

use std::time::Duration;

use serde_json::Value;

use crate::jsonpath::jsonpath_extract;
use crate::sources::{Source, SourceError};

/// Maximum retry attempts on transient network errors.
const MAX_RETRIES: u32 = 4;

/// A parsed header pair for HTTP requests.
#[derive(Debug, Clone)]
pub struct HeaderPair {
    pub key: String,
    pub value: String,
}

/// A generic HTTP data source with user-configurable parameters.
pub struct GenericHttpSource {
    pub id: String,
    pub name: String,
    pub url: String,
    pub method: String,
    pub headers: Vec<HeaderPair>,
    pub body_template: Option<String>,
    pub response_root_path: Option<String>,
    pub refresh_interval: Duration,
}

impl GenericHttpSource {
    /// Build a GenericHttpSource from a DataSource row.
    pub fn from_data_source(ds: &crate::source_store::DataSource) -> Self {
        Self {
            id: ds.id.clone(),
            name: ds.name.clone(),
            url: ds.url.clone(),
            method: ds.method.clone(),
            headers: Self::parse_headers(ds.headers.as_deref()),
            body_template: ds.body_template.clone(),
            response_root_path: ds.response_root_path.clone(),
            refresh_interval: Duration::from_secs(
                std::cmp::max(ds.refresh_interval_secs, 30) as u64,
            ),
        }
    }

    /// Parse headers from the JSON string format stored in `data_sources.headers`.
    /// Expected format: `[{"key": "Accept", "value": "application/json"}, ...]`
    pub fn parse_headers(headers_json: Option<&str>) -> Vec<HeaderPair> {
        let Some(json_str) = headers_json else {
            return Vec::new();
        };
        let Ok(arr) = serde_json::from_str::<Vec<Value>>(json_str) else {
            return Vec::new();
        };
        arr.iter()
            .filter_map(|obj| {
                let key = obj.get("key")?.as_str()?.to_string();
                let value = obj.get("value")?.as_str()?.to_string();
                Some(HeaderPair { key, value })
            })
            .collect()
    }

    /// Perform the HTTP call and return the response body.
    fn http_call(&self) -> Result<String, SourceError> {
        self.call_with_retry()
    }

    fn call_with_retry(&self) -> Result<String, SourceError> {
        let mut last_err = None;
        let mut delay = Duration::from_secs(2);
        let max_delay = Duration::from_secs(300);

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                log::warn!(
                    "GenericHttpSource '{}' retry {}/{} after {:?}",
                    self.id,
                    attempt,
                    MAX_RETRIES,
                    delay,
                );
                std::thread::sleep(delay);
                delay = std::cmp::min(delay * 2, max_delay);
            }

            let result = match self.method.as_str() {
                "POST" => {
                    let mut req = ureq::post(&self.url);
                    for h in &self.headers {
                        req = req.set(&h.key, &h.value);
                    }
                    let body = self.body_template.as_deref().unwrap_or("");
                    req.timeout(Duration::from_secs(15))
                        .send_string(body)
                }
                _ => {
                    let mut req = ureq::get(&self.url);
                    for h in &self.headers {
                        req = req.set(&h.key, &h.value);
                    }
                    req.timeout(Duration::from_secs(15)).call()
                }
            };

            match result {
                Ok(response) => {
                    return response
                        .into_string()
                        .map_err(|e| SourceError::Parse(e.to_string()));
                }
                Err(e) => {
                    last_err = Some(SourceError::Network(e.to_string()));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| SourceError::Network("unknown error".to_string())))
    }
}

impl Source for GenericHttpSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn refresh_interval(&self) -> Duration {
        self.refresh_interval
    }

    fn fetch(&self) -> Result<Value, SourceError> {
        let response_text = self.http_call()?;
        let json: Value =
            serde_json::from_str(&response_text).map_err(|e| SourceError::Parse(e.to_string()))?;

        match &self.response_root_path {
            Some(path) if !path.is_empty() => {
                let extracted = jsonpath_extract(&json, path)
                    .map_err(|e| SourceError::Parse(e.to_string()))?;
                Ok(extracted.clone())
            }
            _ => Ok(json),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_headers_empty() {
        assert!(GenericHttpSource::parse_headers(None).is_empty());
        assert!(GenericHttpSource::parse_headers(Some("")).is_empty());
        assert!(GenericHttpSource::parse_headers(Some("invalid")).is_empty());
    }

    #[test]
    fn parse_headers_valid() {
        let json = r#"[{"key": "Accept", "value": "application/json"}, {"key": "X-Api-Key", "value": "secret"}]"#;
        let headers = GenericHttpSource::parse_headers(Some(json));
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].key, "Accept");
        assert_eq!(headers[0].value, "application/json");
        assert_eq!(headers[1].key, "X-Api-Key");
        assert_eq!(headers[1].value, "secret");
    }

    #[test]
    fn source_trait_methods() {
        let source = GenericHttpSource {
            id: "test-src".to_string(),
            name: "Test Source".to_string(),
            url: "https://example.com".to_string(),
            method: "GET".to_string(),
            headers: vec![],
            body_template: None,
            response_root_path: None,
            refresh_interval: Duration::from_secs(60),
        };
        assert_eq!(source.id(), "test-src");
        assert_eq!(source.name(), "Test Source");
        assert_eq!(source.refresh_interval(), Duration::from_secs(60));
    }
}
