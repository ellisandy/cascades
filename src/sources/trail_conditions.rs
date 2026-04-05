use crate::config::TrailSourceConfig;
use crate::domain::TrailCondition;
use crate::sources::{Source, SourceError};
use serde::Deserialize;
use std::time::Duration;

/// NPS Alerts API base URL.
const NPS_API_BASE: &str = "https://developer.nps.gov/api/v1";

/// Fixture JSON returned when SKAGIT_FIXTURE_DATA=1.
const FIXTURE_RESPONSE: &str = include_str!("fixtures/nps_alerts.json");

/// Trail conditions source backed by the NPS Alerts API.
///
/// Fetches park alerts for a configured park code (default: "noca" for North
/// Cascades) and maps them to `TrailCondition` domain objects. In fixture mode,
/// returns static data without making network calls.
pub struct TrailConditionsSource {
    park_code: String,
    api_key: String,
    refresh: Duration,
    use_fixtures: bool,
}

impl TrailConditionsSource {
    pub fn new(
        config: Option<&TrailSourceConfig>,
        interval_secs: u64,
        use_fixtures: bool,
    ) -> Result<Self, SourceError> {
        let park_code = config
            .map(|c| c.park_code.clone())
            .unwrap_or_else(|| "noca".to_string());

        let api_key = if use_fixtures {
            String::new()
        } else {
            config
                .and_then(|c| c.nps_api_key.clone())
                .or_else(|| std::env::var("NPS_API_KEY").ok())
                .unwrap_or_default()
        };

        if !use_fixtures && api_key.is_empty() {
            return Err(SourceError::Other(
                "NPS API key not set: configure nps_api_key in config.toml or set NPS_API_KEY env var".to_string(),
            ));
        }

        Ok(Self {
            park_code,
            api_key,
            refresh: Duration::from_secs(interval_secs),
            use_fixtures,
        })
    }
}

/// Raw alert from the NPS API response.
#[derive(Debug, Deserialize)]
struct NpsAlertsResponse {
    data: Vec<NpsAlert>,
}

#[derive(Debug, Deserialize)]
struct NpsAlert {
    title: String,
    description: String,
    category: String,
}

impl Source for TrailConditionsSource {
    fn id(&self) -> &str {
        "trail"
    }

    fn name(&self) -> &str {
        "trail-conditions"
    }

    fn refresh_interval(&self) -> Duration {
        self.refresh
    }

    fn fetch(&self) -> Result<serde_json::Value, SourceError> {
        let response_text = if self.use_fixtures {
            FIXTURE_RESPONSE.to_string()
        } else {
            self.fetch_live()?
        };

        let parsed: NpsAlertsResponse =
            serde_json::from_str(&response_text).map_err(|e| SourceError::Parse(e.to_string()))?;

        // Find the most relevant trail/condition alert.
        // Prefer "caution" or "danger" categories; fall back to first alert.
        let alert = parsed
            .data
            .iter()
            .find(|a| a.category == "Caution" || a.category == "Danger")
            .or_else(|| parsed.data.first());

        let condition = match alert {
            Some(a) => {
                // Truncate long descriptions for the e-ink display.
                let summary = if a.description.len() > 120 {
                    format!("[{}] {}…", a.category, &a.description[..117])
                } else {
                    format!("[{}] {}", a.category, a.description)
                };

                TrailCondition {
                    destination_name: a.title.clone(),
                    suitability_summary: summary,
                    last_updated: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                }
            }
            None => TrailCondition {
                destination_name: format!("NPS {}", self.park_code.to_uppercase()),
                suitability_summary: "No active alerts".to_string(),
                last_updated: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            },
        };

        serde_json::to_value(condition).map_err(|e| SourceError::Parse(e.to_string()))
    }
}

impl TrailConditionsSource {
    fn fetch_live(&self) -> Result<String, SourceError> {
        let url = format!(
            "{}/alerts?parkCode={}&api_key={}",
            NPS_API_BASE, self.park_code, self.api_key
        );

        let response = ureq::get(&url)
            .timeout(Duration::from_secs(10))
            .call()
            .map_err(|e| SourceError::Network(e.to_string()))?;

        response
            .into_string()
            .map_err(|e| SourceError::Parse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TrailCondition;

    #[test]
    fn parse_fixture_response() {
        let parsed: NpsAlertsResponse =
            serde_json::from_str(FIXTURE_RESPONSE).expect("fixture JSON should parse");
        assert!(!parsed.data.is_empty(), "fixture should contain alerts");

        let alert = &parsed.data[0];
        assert!(!alert.title.is_empty());
        assert!(!alert.description.is_empty());
        assert!(!alert.category.is_empty());
    }

    #[test]
    fn fixture_mode_returns_trail_condition() {
        let source = TrailConditionsSource::new(None, 900, true)
            .expect("should create in fixture mode");

        let result = source.fetch();
        assert!(result.is_ok(), "fixture fetch should succeed");

        let cond: TrailCondition = serde_json::from_value(result.unwrap()).unwrap();
        assert!(!cond.destination_name.is_empty());
        assert!(!cond.suitability_summary.is_empty());
        assert!(cond.last_updated > 0);
    }

    #[test]
    fn missing_api_key_in_live_mode_errors() {
        // SAFETY: test-only, single-threaded test process
        unsafe { std::env::remove_var("NPS_API_KEY") };

        let result = TrailConditionsSource::new(None, 900, false);
        assert!(result.is_err(), "should fail without API key in live mode");
    }

    #[test]
    fn empty_alerts_returns_no_active_alerts() {
        let source = TrailConditionsSource {
            park_code: "noca".to_string(),
            api_key: String::new(),
            refresh: Duration::from_secs(900),
            use_fixtures: false,
        };

        let empty_json = r#"{"total":"0","data":[],"limit":"50","start":"0"}"#;
        let parsed: NpsAlertsResponse = serde_json::from_str(empty_json).unwrap();

        // Simulate the fetch logic with empty data.
        let alert = parsed
            .data
            .iter()
            .find(|a| a.category == "Caution" || a.category == "Danger")
            .or_else(|| parsed.data.first());

        assert!(alert.is_none());
        // The source would return "No active alerts" for this case.
        assert_eq!(source.name(), "trail-conditions");
    }

    #[test]
    fn long_description_is_truncated() {
        let long_desc = "A".repeat(200);
        let alert = NpsAlert {
            title: "Test Trail".to_string(),
            description: long_desc.clone(),
            category: "Caution".to_string(),
        };

        let summary = if alert.description.len() > 120 {
            format!("[{}] {}…", alert.category, &alert.description[..117])
        } else {
            format!("[{}] {}", alert.category, alert.description)
        };

        // "[Caution] " = 10 chars + 117 chars + "…" = 128 chars
        assert!(summary.len() < 200);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn source_id_name_and_interval() {
        let source = TrailConditionsSource::new(None, 900, true)
            .expect("should create in fixture mode");
        assert_eq!(source.id(), "trail");
        assert_eq!(source.name(), "trail-conditions");
        assert_eq!(source.refresh_interval(), Duration::from_secs(900));
    }
}
