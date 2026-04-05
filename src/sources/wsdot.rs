use crate::config::FerrySourceConfig;
use crate::domain::FerryStatus;
use crate::sources::{Source, SourceError};
use serde::Deserialize;
use std::time::Duration;

/// WSDOT Ferries schedule API base URL.
const WSDOT_FERRIES_API: &str =
    "https://www.wsdot.wa.gov/Ferries/API/Schedule/rest/scheduletoday";

/// Fixture JSON returned when SKAGIT_FIXTURE_DATA=1.
const FIXTURE_RESPONSE: &str = include_str!("fixtures/wsdot_ferries.json");

/// Maximum number of retry attempts on transient (5xx) errors.
const MAX_RETRIES: u32 = 4;

/// WSDOT ferries source backed by the WSDOT Ferries Schedule API.
///
/// Fetches today's schedule for a configured route, extracting vessel names
/// and estimated departure times. Uses exponential backoff on 5xx errors
/// since the WSDOT API has a history of instability.
pub struct WsdotFerrySource {
    access_code: String,
    route_id: u32,
    route_description: String,
    refresh: Duration,
    use_fixtures: bool,
}

impl WsdotFerrySource {
    pub fn new(
        config: Option<&FerrySourceConfig>,
        interval_secs: u64,
        use_fixtures: bool,
    ) -> Result<Self, SourceError> {
        let route_id = config.map(|c| c.route_id).unwrap_or(9); // default: Anacortes/Friday Harbor
        let route_description = config
            .and_then(|c| c.route_description.clone())
            .unwrap_or_else(|| "Anacortes / San Juan Islands".to_string());

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
                "WSDOT access code not set: configure wsdot_access_code in config.toml \
                 or set WSDOT_ACCESS_CODE env var"
                    .to_string(),
            ));
        }

        Ok(Self {
            access_code,
            route_id,
            route_description,
            refresh: Duration::from_secs(interval_secs),
            use_fixtures,
        })
    }
}

/// Raw schedule response from WSDOT Ferries API.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ScheduleResponse {
    terminal_combos: Vec<TerminalCombo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TerminalCombo {
    departing_terminal_name: String,
    arriving_terminal_name: String,
    times: Vec<ScheduleTime>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ScheduleTime {
    departing_time: String,
    vessel_name: String,
}

impl Source for WsdotFerrySource {
    fn id(&self) -> &str {
        "ferry"
    }

    fn name(&self) -> &str {
        "wsdot-ferries"
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

        parse_schedule(&response_text, &self.route_description)
    }
}

/// Parse a WSDOT schedule response into a serde_json::Value.
fn parse_schedule(json: &str, route_description: &str) -> Result<serde_json::Value, SourceError> {
    let schedule: ScheduleResponse =
        serde_json::from_str(json).map_err(|e| SourceError::Parse(e.to_string()))?;

    let combo = schedule.terminal_combos.first().ok_or_else(|| {
        SourceError::Parse("no terminal combos in schedule response".to_string())
    })?;

    // Use the first vessel name found, or "Unknown" if schedule is empty.
    let vessel_name = combo
        .times
        .first()
        .map(|t| t.vessel_name.clone())
        .unwrap_or_else(|| "Unknown".to_string());

    // Extract departure timestamps from the WSDOT date format: /Date(1234567890000-0700)/
    let departures: Vec<u64> = combo
        .times
        .iter()
        .filter_map(|t| parse_wsdot_date(&t.departing_time))
        .collect();

    let route = if combo.departing_terminal_name.is_empty() {
        route_description.to_string()
    } else {
        format!(
            "{} → {}",
            combo.departing_terminal_name, combo.arriving_terminal_name
        )
    };

    let status = FerryStatus {
        route,
        vessel_name,
        estimated_departures: departures,
    };

    serde_json::to_value(status).map_err(|e| SourceError::Parse(e.to_string()))
}

/// Parse a WSDOT date string like "/Date(1711652400000-0700)/" to Unix epoch seconds.
fn parse_wsdot_date(s: &str) -> Option<u64> {
    // Format: /Date(MILLISECONDS±OFFSET)/
    let inner = s.strip_prefix("/Date(")?.strip_suffix(")/")?;

    // Split at the timezone offset (+ or - after the milliseconds).
    // The milliseconds portion is always digits, so find the first +/- that isn't
    // at position 0 (negative timestamps are not expected for ferry schedules).
    let millis_str = inner
        .find(|c: char| (c == '+' || c == '-') && !inner.starts_with(c))
        .map(|pos| &inner[..pos])
        .unwrap_or(inner);

    let millis: u64 = millis_str.parse().ok()?;
    Some(millis / 1000)
}

impl WsdotFerrySource {
    /// Fetch live schedule data from the WSDOT Ferries API with retry/backoff.
    fn fetch_live(&self) -> Result<String, SourceError> {
        let url = format!(
            "{}/{}?apiaccesscode={}",
            WSDOT_FERRIES_API, self.route_id, self.access_code
        );

        self.get_with_retry(&url)
    }

    /// HTTP GET with exponential backoff on transient errors.
    /// WSDOT API has a history of instability — treat 5xx as transient.
    fn get_with_retry(&self, url: &str) -> Result<String, SourceError> {
        let mut last_err = None;
        let mut delay = Duration::from_secs(2);
        let max_delay = Duration::from_secs(300); // 5 minutes

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                log::warn!(
                    "WSDOT ferry retry {}/{} after {:?} for {}",
                    attempt,
                    MAX_RETRIES,
                    delay,
                    url
                );
                std::thread::sleep(delay);
                delay = std::cmp::min(delay * 2, max_delay);
            }

            match ureq::get(url)
                .timeout(Duration::from_secs(15))
                .call()
            {
                Ok(response) => {
                    return response
                        .into_string()
                        .map_err(|e| SourceError::Parse(e.to_string()));
                }
                Err(ureq::Error::Status(code, _)) if code >= 500 => {
                    log::warn!("WSDOT ferry API returned {} (transient)", code);
                    last_err = Some(SourceError::Network(format!(
                        "HTTP {} from WSDOT ferry API",
                        code
                    )));
                }
                Err(e) => {
                    last_err = Some(SourceError::Network(e.to_string()));
                }
            }
        }

        Err(last_err.unwrap_or_else(|| SourceError::Network("unknown error".to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::FerryStatus;

    #[test]
    fn parse_fixture_response() {
        let schedule: ScheduleResponse =
            serde_json::from_str(FIXTURE_RESPONSE).expect("fixture JSON should parse");
        assert!(!schedule.terminal_combos.is_empty());
        assert!(!schedule.terminal_combos[0].times.is_empty());
        assert_eq!(schedule.terminal_combos[0].times[0].vessel_name, "MV Samish");
    }

    #[test]
    fn fixture_mode_returns_ferry_status() {
        let source =
            WsdotFerrySource::new(None, 60, true).expect("should create in fixture mode");

        let result = source.fetch();
        assert!(result.is_ok(), "fixture fetch should succeed: {:?}", result);

        let status: FerryStatus = serde_json::from_value(result.unwrap()).unwrap();
        assert!(status.route.contains("Anacortes"));
        assert_eq!(status.vessel_name, "MV Samish");
        assert_eq!(status.estimated_departures.len(), 3);
        assert!(status.estimated_departures[0] > 0);
    }

    #[test]
    fn empty_schedule_returns_unknown_vessel() {
        let json = r#"{
            "ScheduleID": 1,
            "ScheduleName": "Test",
            "TerminalCombos": [{
                "DepartingTerminalID": 1,
                "DepartingTerminalName": "Anacortes",
                "ArrivingTerminalID": 10,
                "ArrivingTerminalName": "Friday Harbor",
                "Times": []
            }]
        }"#;

        let result = parse_schedule(json, "Test Route");
        assert!(result.is_ok());

        let status: FerryStatus = serde_json::from_value(result.unwrap()).unwrap();
        assert_eq!(status.vessel_name, "Unknown");
        assert!(status.estimated_departures.is_empty());
        assert!(status.route.contains("Anacortes"));
    }

    #[test]
    fn missing_terminal_combos_returns_error() {
        let json = r#"{"ScheduleID": 1, "ScheduleName": "Test", "TerminalCombos": []}"#;

        let result = parse_schedule(json, "Test Route");
        assert!(result.is_err());
        match result.unwrap_err() {
            SourceError::Parse(msg) => assert!(msg.contains("no terminal combos")),
            other => panic!("expected Parse error, got {:?}", other),
        }
    }

    #[test]
    fn invalid_json_returns_parse_error() {
        let result = parse_schedule("not json", "Test");
        assert!(result.is_err());
        match result.unwrap_err() {
            SourceError::Parse(_) => {}
            other => panic!("expected Parse error, got {:?}", other),
        }
    }

    #[test]
    fn parse_wsdot_date_format() {
        // /Date(1711652400000-0700)/ = 1711652400 seconds
        assert_eq!(
            parse_wsdot_date("/Date(1711652400000-0700)/"),
            Some(1711652400)
        );
        // No offset
        assert_eq!(
            parse_wsdot_date("/Date(1711652400000)/"),
            Some(1711652400)
        );
        // Invalid format
        assert_eq!(parse_wsdot_date("not a date"), None);
        assert_eq!(parse_wsdot_date(""), None);
    }

    #[test]
    fn missing_access_code_in_live_mode_errors() {
        // SAFETY: test-only, single-threaded test process
        unsafe { std::env::remove_var("WSDOT_ACCESS_CODE") };

        let result = WsdotFerrySource::new(None, 60, false);
        assert!(result.is_err(), "should fail without access code in live mode");
    }

    #[test]
    fn source_id_name_and_interval() {
        let source =
            WsdotFerrySource::new(None, 60, true).expect("should create in fixture mode");
        assert_eq!(source.id(), "ferry");
        assert_eq!(source.name(), "wsdot-ferries");
        assert_eq!(source.refresh_interval(), Duration::from_secs(60));
    }
}
