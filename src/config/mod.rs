use crate::domain::{RelevantSignals, TripCriteria};
use serde::Deserialize;
use std::path::Path;
use thiserror::Error;

/// Optional authentication for the web UI.
///
/// If absent, the web UI is accessible without credentials. When present,
/// a username/password login form is shown and a session cookie is required
/// to access all pages except `/health`.
///
/// Add to `config.toml`:
/// ```toml
/// [auth]
/// username = "admin"
/// password = "yourpassword"
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct AuthConfig {
    /// Username for the web UI login form.
    pub username: String,
    /// Password for the web UI login form.
    pub password: String,
}

/// Device display loop configuration for the thin HTTP fetch mode.
///
/// When present, `run()` operates as a thin client: fetch a pre-rendered PNG
/// from `image_url`, decode it, push to the hardware display, sleep, repeat.
/// The API contract: GET `image_url` returns `image/png` directly.
#[derive(Debug, Deserialize, Clone)]
pub struct DeviceConfig {
    /// URL of the pre-rendered display image served by the skagit-flats server.
    pub image_url: String,
    /// How often to fetch and refresh the display, in seconds.
    #[serde(default = "default_device_refresh_secs")]
    pub refresh_interval_secs: u64,
}

fn default_device_refresh_secs() -> u64 {
    60
}

/// HTTP server configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    /// TCP port to listen on. Defaults to 8080.
    #[serde(default = "default_server_port")]
    pub port: u16,
}

fn default_server_port() -> u16 {
    8080
}

/// Top-level runtime configuration loaded from config.toml.
/// This file is never written at runtime; changes require a restart.
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub display: DisplayConfig,
    pub location: LocationConfig,
    pub sources: SourceIntervals,
    /// HTTP server settings. If absent, defaults to port 8080.
    #[serde(default)]
    pub server: Option<ServerConfig>,
    /// Optional web UI authentication. If absent, no login is required.
    #[serde(default)]
    pub auth: Option<AuthConfig>,
    /// Device display loop config. When set, the app runs as a thin HTTP client.
    #[serde(default)]
    pub device: Option<DeviceConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DisplayConfig {
    /// Display width in pixels (800 for the Waveshare 7.5").
    pub width: u32,
    /// Display height in pixels (480 for the Waveshare 7.5").
    pub height: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LocationConfig {
    pub latitude: f64,
    pub longitude: f64,
    pub name: String,
}

/// Per-source polling intervals in seconds.
#[derive(Debug, Deserialize, Clone)]
pub struct SourceIntervals {
    pub weather_interval_secs: u64,
    pub river_interval_secs: u64,
    pub ferry_interval_secs: u64,
    #[serde(default = "default_trail_interval")]
    pub trail_interval_secs: u64,
    #[serde(default = "default_road_interval")]
    pub road_interval_secs: u64,
    #[serde(default)]
    pub river: Option<RiverSourceConfig>,
    #[serde(default)]
    pub trail: Option<TrailSourceConfig>,
    #[serde(default)]
    pub road: Option<RoadSourceConfig>,
    #[serde(default)]
    pub ferry: Option<FerrySourceConfig>,
}

fn default_trail_interval() -> u64 {
    900
}

fn default_road_interval() -> u64 {
    1800
}

/// Configuration for the USGS river gauge source.
#[derive(Debug, Deserialize, Clone)]
pub struct RiverSourceConfig {
    /// USGS site ID, e.g. "12200500" for Skagit River near Mount Vernon.
    /// Defaults to the Skagit River at Mount Vernon.
    #[serde(default = "default_usgs_site_id")]
    pub usgs_site_id: String,
}

fn default_usgs_site_id() -> String {
    "12200500".to_string()
}

/// Configuration for the trail conditions source (NPS Alerts API).
#[derive(Debug, Deserialize, Clone)]
pub struct TrailSourceConfig {
    /// NPS park code, e.g. "noca" for North Cascades. Defaults to "noca".
    #[serde(default = "default_park_code")]
    pub park_code: String,
    /// NPS API key. If absent, falls back to NPS_API_KEY env var.
    pub nps_api_key: Option<String>,
}

fn default_park_code() -> String {
    "noca".to_string()
}

/// Configuration for the road closures source (WSDOT Highway Alerts API).
#[derive(Debug, Deserialize, Clone)]
pub struct RoadSourceConfig {
    /// WSDOT access code. If absent, falls back to WSDOT_ACCESS_CODE env var.
    pub wsdot_access_code: Option<String>,
    /// WSDOT route numbers to monitor, e.g. ["020", "005"]. Defaults to ["020"].
    #[serde(default = "default_routes")]
    pub routes: Vec<String>,
}

fn default_routes() -> Vec<String> {
    vec!["020".to_string()]
}

/// Configuration for the WSDOT ferries source.
#[derive(Debug, Deserialize, Clone)]
pub struct FerrySourceConfig {
    /// WSDOT access code. If absent, falls back to WSDOT_ACCESS_CODE env var.
    pub wsdot_access_code: Option<String>,
    /// WSDOT route ID. Defaults to 9 (Anacortes / Friday Harbor).
    #[serde(default = "default_ferry_route_id")]
    pub route_id: u32,
    /// Human-readable route description.
    pub route_description: Option<String>,
}

fn default_ferry_route_id() -> u32 {
    9
}

/// Destinations configuration loaded from destinations.toml.
/// This file is written by the web UI and reloaded at runtime on change.
#[derive(Debug, Deserialize, serde::Serialize, Clone, Default)]
pub struct DestinationsConfig {
    #[serde(default)]
    pub destinations: Vec<Destination>,
}

/// A single trip destination with its relevant signals and go/no-go criteria.
#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct Destination {
    pub name: String,
    /// Which data signals matter for this destination.
    /// Controls display filtering and evaluation scope.
    #[serde(default)]
    pub signals: RelevantSignals,
    pub criteria: TripCriteria,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read '{path}': {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse '{path}': {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
}

/// Load and parse config.toml. Fails fast on any error.
pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
        path: path.to_string_lossy().into_owned(),
        source: e,
    })?;
    toml::from_str(&contents).map_err(|e| ConfigError::Parse {
        path: path.to_string_lossy().into_owned(),
        source: e,
    })
}

/// Load and parse destinations.toml. Fails fast on any error.
pub fn load_destinations(path: &Path) -> Result<DestinationsConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Read {
        path: path.to_string_lossy().into_owned(),
        source: e,
    })?;
    toml::from_str(&contents).map_err(|e| ConfigError::Parse {
        path: path.to_string_lossy().into_owned(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parse_valid_config() {
        let toml = r#"
[display]
width = 800
height = 480

[location]
latitude = 48.4232
longitude = -122.3351
name = "Mount Vernon, WA"

[sources]
weather_interval_secs = 300
river_interval_secs = 300
ferry_interval_secs = 60
trail_interval_secs = 900
road_interval_secs = 1800

[sources.trail]
park_code = "noca"

[sources.road]
routes = ["020", "005"]
"#;
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(toml.as_bytes()).unwrap();
        let cfg = load_config(f.path()).expect("should parse");
        assert_eq!(cfg.display.width, 800);
        assert_eq!(cfg.display.height, 480);
        assert_eq!(cfg.location.name, "Mount Vernon, WA");
        assert_eq!(cfg.sources.ferry_interval_secs, 60);
        assert_eq!(cfg.sources.road_interval_secs, 1800);
        let road_cfg = cfg.sources.road.unwrap();
        assert_eq!(road_cfg.routes, vec!["020", "005"]);
    }

    #[test]
    fn parse_invalid_config_fails_fast() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"[display]\nnot valid toml !!!").unwrap();
        assert!(load_config(f.path()).is_err());
    }

    #[test]
    fn parse_valid_destinations() {
        let toml = r#"
[[destinations]]
name = "Skagit Flats Loop"

[destinations.criteria]
min_temp_f = 45.0
max_temp_f = 85.0
max_river_level_ft = 12.0
road_open_required = true
"#;
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(toml.as_bytes()).unwrap();
        let cfg = load_destinations(f.path()).expect("should parse");
        assert_eq!(cfg.destinations.len(), 1);
        assert_eq!(cfg.destinations[0].name, "Skagit Flats Loop");
        assert!(cfg.destinations[0].criteria.road_open_required);
    }

    #[test]
    fn parse_empty_destinations() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"").unwrap();
        let cfg = load_destinations(f.path()).expect("empty file is valid");
        assert!(cfg.destinations.is_empty());
    }

    #[test]
    fn config_missing_file_returns_read_error() {
        let result = load_config(Path::new("/nonexistent/config.toml"));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::Read { path, .. } => {
                assert!(path.contains("nonexistent"));
            }
            other => panic!("expected Read error, got {:?}", other),
        }
    }

    #[test]
    fn destinations_missing_file_returns_read_error() {
        let result = load_destinations(Path::new("/nonexistent/destinations.toml"));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::Read { path, .. } => {
                assert!(path.contains("nonexistent"));
            }
            other => panic!("expected Read error, got {:?}", other),
        }
    }

    #[test]
    fn config_uses_default_trail_interval() {
        let toml = r#"
[display]
width = 800
height = 480

[location]
latitude = 48.4
longitude = -122.3
name = "Test"

[sources]
weather_interval_secs = 300
river_interval_secs = 300
ferry_interval_secs = 60
"#;
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(toml.as_bytes()).unwrap();
        let cfg = load_config(f.path()).expect("should parse");
        assert_eq!(cfg.sources.trail_interval_secs, 900);
        assert_eq!(cfg.sources.road_interval_secs, 1800);
    }

    #[test]
    fn config_optional_source_configs_default_to_none() {
        let toml = r#"
[display]
width = 800
height = 480

[location]
latitude = 48.4
longitude = -122.3
name = "Test"

[sources]
weather_interval_secs = 300
river_interval_secs = 300
ferry_interval_secs = 60
"#;
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(toml.as_bytes()).unwrap();
        let cfg = load_config(f.path()).expect("should parse");
        assert!(cfg.sources.river.is_none());
        assert!(cfg.sources.trail.is_none());
        assert!(cfg.sources.road.is_none());
        assert!(cfg.sources.ferry.is_none());
    }

    #[test]
    fn destinations_multiple_entries() {
        let toml = r#"
[[destinations]]
name = "Loop A"
[destinations.criteria]
min_temp_f = 40.0
road_open_required = true

[[destinations]]
name = "Loop B"
[destinations.criteria]
max_river_level_ft = 15.0
"#;
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(toml.as_bytes()).unwrap();
        let cfg = load_destinations(f.path()).expect("should parse");
        assert_eq!(cfg.destinations.len(), 2);
        assert_eq!(cfg.destinations[0].name, "Loop A");
        assert!(cfg.destinations[0].criteria.road_open_required);
        assert_eq!(cfg.destinations[1].name, "Loop B");
        assert_eq!(cfg.destinations[1].criteria.max_river_level_ft, Some(15.0));
    }

    #[test]
    fn destinations_config_serialization_roundtrip() {
        let config = DestinationsConfig {
            destinations: vec![Destination {
                name: "Test".to_string(),
                signals: Default::default(),
                criteria: crate::domain::TripCriteria {
                    min_temp_f: Some(45.0),
                    max_temp_f: Some(85.0),
                    road_open_required: true,
                    ..Default::default()
                },
            }],
        };
        let toml_str = toml::to_string_pretty(&config).expect("should serialize");
        let parsed: DestinationsConfig = toml::from_str(&toml_str).expect("should parse");
        assert_eq!(parsed.destinations.len(), 1);
        assert_eq!(parsed.destinations[0].name, "Test");
        assert_eq!(parsed.destinations[0].criteria.min_temp_f, Some(45.0));
    }

    #[test]
    fn config_error_display() {
        let err = ConfigError::Read {
            path: "config.toml".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        let msg = err.to_string();
        assert!(msg.contains("config.toml"));
        assert!(msg.contains("not found"));
    }
}
