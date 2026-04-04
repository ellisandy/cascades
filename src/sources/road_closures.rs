use crate::config::RoadSourceConfig;
use crate::domain::{DataPoint, RoadStatus};
use crate::evaluation::current_unix_secs;
use crate::sources::{Source, SourceError};
use serde::Deserialize;
use std::time::Duration;

/// WSDOT Highway Alerts API endpoint.
const WSDOT_ALERTS_URL: &str =
    "https://www.wsdot.wa.gov/Traffic/api/HighwayAlerts/HighwayAlertsREST.svc/GetAlertsAsJson";

/// Fixture JSON returned when SKAGIT_FIXTURE_DATA=1.
const FIXTURE_RESPONSE: &str = include_str!("fixtures/wsdot_highway_alerts.json");

/// Road closures source backed by the WSDOT Highway Alerts API.
///
/// Fetches all active highway alerts and filters to configured routes.
/// In fixture mode, returns static data without making network calls.
pub struct RoadClosuresSource {
    access_code: String,
    /// WSDOT route numbers to monitor, e.g. ["020", "005"].
    routes: Vec<String>,
    refresh: Duration,
    use_fixtures: bool,
}

impl RoadClosuresSource {
    pub fn new(
        config: Option<&RoadSourceConfig>,
        interval_secs: u64,
        use_fixtures: bool,
    ) -> Result<Self, SourceError> {
        let routes = config
            .and_then(|c| {
                if c.routes.is_empty() {
                    None
                } else {
                    Some(c.routes.clone())
                }
            })
            .unwrap_or_else(|| vec!["020".to_string()]);

        let access_code = if use_fixtures {
            String::new()
        } else {
            config
                .and_then(|c| c.wsdot_access_code.clone())
                .or_else(|| std::env::var("WSDOT_ACCESS_CODE").ok())
                .unwrap_or_default()
        };

        if !use_fixtures && access_code.is_empty() {
            return Err(SourceError::Other(
                "WSDOT access code not set: configure wsdot_access_code in config.toml or set WSDOT_ACCESS_CODE env var".to_string(),
            ));
        }

        Ok(Self {
            access_code,
            routes,
            refresh: Duration::from_secs(interval_secs),
            use_fixtures,
        })
    }
}

/// Raw alert from the WSDOT Highway Alerts API response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WsdotAlert {
    event_category: String,
    headline_description: String,
    #[allow(dead_code)]
    extended_description: String,
    start_roadway_location: RoadwayLocation,
    end_roadway_location: RoadwayLocation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RoadwayLocation {
    road_name: String,
    mile_post: f64,
    description: String,
}

impl Source for RoadClosuresSource {
    fn name(&self) -> &str {
        "road-closures"
    }

    fn refresh_interval(&self) -> Duration {
        self.refresh
    }

    fn fetch(&self) -> Result<DataPoint, SourceError> {
        let response_text = if self.use_fixtures {
            FIXTURE_RESPONSE.to_string()
        } else {
            self.fetch_live()?
        };

        let alerts: Vec<WsdotAlert> =
            serde_json::from_str(&response_text).map_err(|e| SourceError::Parse(e.to_string()))?;

        // Filter to configured routes and closure events.
        let closures: Vec<&WsdotAlert> = alerts
            .iter()
            .filter(|a| self.routes.contains(&a.start_roadway_location.road_name))
            .filter(|a| a.event_category == "Closure")
            .collect();

        if let Some(closure) = closures.first() {
            let road_name = format_road_name(&closure.start_roadway_location.road_name);
            let segment = format!(
                "MP {:.0} ({}) to MP {:.0} ({})",
                closure.start_roadway_location.mile_post,
                closure.start_roadway_location.description,
                closure.end_roadway_location.mile_post,
                closure.end_roadway_location.description,
            );

            // Truncate description for the e-ink display.
            let status_desc = if closure.headline_description.len() > 80 {
                format!("{}...", &closure.headline_description[..77])
            } else {
                closure.headline_description.clone()
            };

            Ok(DataPoint::Road(RoadStatus {
                road_name,
                status: status_desc,
                affected_segment: segment,
                timestamp: current_unix_secs(),
            }))
        } else {
            // No closures found for monitored routes — report open.
            let road_name = self
                .routes
                .first()
                .map(|r| format_road_name(r))
                .unwrap_or_else(|| "Highway".to_string());

            Ok(DataPoint::Road(RoadStatus {
                road_name,
                status: "No active closures".to_string(),
                affected_segment: String::new(),
                timestamp: current_unix_secs(),
            }))
        }
    }
}

impl RoadClosuresSource {
    fn fetch_live(&self) -> Result<String, SourceError> {
        let url = format!("{}?AccessCode={}", WSDOT_ALERTS_URL, self.access_code);

        let response = ureq::get(&url)
            .timeout(Duration::from_secs(15))
            .call()
            .map_err(|e| SourceError::Network(e.to_string()))?;

        response
            .into_string()
            .map_err(|e| SourceError::Parse(e.to_string()))
    }
}

/// Map a WSDOT route number to a human-readable road name.
fn format_road_name(route_num: &str) -> String {
    match route_num {
        "005" => "I-5".to_string(),
        "020" => "SR-20 North Cascades Hwy".to_string(),
        "530" => "SR-530".to_string(),
        "009" => "SR-9".to_string(),
        "011" => "SR-11 Chuckanut Dr".to_string(),
        other => format!("SR-{}", other.trim_start_matches('0')),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fixture_response() {
        let alerts: Vec<WsdotAlert> =
            serde_json::from_str(FIXTURE_RESPONSE).expect("fixture JSON should parse");
        assert!(!alerts.is_empty(), "fixture should contain alerts");
        assert_eq!(alerts[0].event_category, "Closure");
        assert_eq!(alerts[0].start_roadway_location.road_name, "020");
    }

    #[test]
    fn fixture_mode_returns_road_status() {
        let source =
            RoadClosuresSource::new(None, 1800, true).expect("should create in fixture mode");

        let result = source.fetch();
        assert!(result.is_ok(), "fixture fetch should succeed");

        match result.unwrap() {
            DataPoint::Road(status) => {
                assert_eq!(status.road_name, "SR-20 North Cascades Hwy");
                assert!(!status.status.is_empty());
                assert!(status.affected_segment.contains("MP"));
            }
            other => panic!("expected DataPoint::Road, got {:?}", other),
        }
    }

    #[test]
    fn missing_access_code_in_live_mode_errors() {
        // SAFETY: test-only, single-threaded test process
        unsafe { std::env::remove_var("WSDOT_ACCESS_CODE") };

        let result = RoadClosuresSource::new(None, 1800, false);
        assert!(result.is_err(), "should fail without access code in live mode");
    }

    #[test]
    fn empty_alerts_returns_no_closures() {
        let source = RoadClosuresSource {
            access_code: String::new(),
            routes: vec!["020".to_string()],
            refresh: Duration::from_secs(1800),
            use_fixtures: false,
        };

        let empty_json = "[]";
        let alerts: Vec<WsdotAlert> = serde_json::from_str(empty_json).unwrap();

        let closures: Vec<&WsdotAlert> = alerts
            .iter()
            .filter(|a| source.routes.contains(&a.start_roadway_location.road_name))
            .filter(|a| a.event_category == "Closure")
            .collect();

        assert!(closures.is_empty());
        assert_eq!(source.name(), "road-closures");
    }

    #[test]
    fn format_road_name_known_routes() {
        assert_eq!(format_road_name("005"), "I-5");
        assert_eq!(format_road_name("020"), "SR-20 North Cascades Hwy");
        assert_eq!(format_road_name("530"), "SR-530");
        assert_eq!(format_road_name("042"), "SR-42");
    }

    #[test]
    fn filters_to_closures_only() {
        let alerts: Vec<WsdotAlert> =
            serde_json::from_str(FIXTURE_RESPONSE).expect("fixture JSON should parse");

        let routes = vec!["020".to_string()];
        let closures: Vec<&WsdotAlert> = alerts
            .iter()
            .filter(|a| routes.contains(&a.start_roadway_location.road_name))
            .filter(|a| a.event_category == "Closure")
            .collect();

        // Fixture has one closure and one construction alert for route 020.
        // Only the closure should match.
        assert_eq!(closures.len(), 1);
        assert_eq!(closures[0].event_category, "Closure");
    }
}
