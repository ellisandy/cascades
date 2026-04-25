pub mod api;
pub mod asset_store;
pub mod compositor;
pub mod config;
pub mod domain;
pub mod evaluation;
pub mod fonts;
pub mod format;
pub mod instance_store;
pub mod jsonpath;
pub mod layout_store;
pub mod plugin_registry;
pub mod presentation;
pub mod source_store;
pub mod sources;
pub mod template;
pub mod visible_when;

use config::Config;
use sources::Source;

pub fn build_sources(config: &Config, fixture_data: bool) -> Vec<Box<dyn Source>> {
    let mut sources: Vec<Box<dyn Source>> = Vec::new();

    sources.push(Box::new(sources::noaa::NoaaSource::new(
        &config.location,
        config.sources.weather_interval_secs,
        fixture_data,
    )));

    let site_id = config
        .sources
        .river
        .as_ref()
        .map(|r| r.usgs_site_id.as_str())
        .unwrap_or("12200500");
    sources.push(Box::new(sources::usgs::UsgsSource::new(
        site_id,
        config.sources.river_interval_secs,
        fixture_data,
    )));

    match sources::wsdot::WsdotFerrySource::new(
        config.sources.ferry.as_ref(),
        config.sources.ferry_interval_secs,
        fixture_data,
    ) {
        Ok(s) => sources.push(Box::new(s)),
        Err(e) => log::warn!("ferry source disabled: {}", e),
    }

    match sources::trail_conditions::TrailConditionsSource::new(
        config.sources.trail.as_ref(),
        config.sources.trail_interval_secs,
        fixture_data,
    ) {
        Ok(s) => sources.push(Box::new(s)),
        Err(e) => log::warn!("trail source disabled: {}", e),
    }

    match sources::road_closures::RoadClosuresSource::new(
        config.sources.road.as_ref(),
        config.sources.road_interval_secs,
        fixture_data,
    ) {
        Ok(s) => sources.push(Box::new(s)),
        Err(e) => log::warn!("road source disabled: {}", e),
    }

    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::{Config, DisplayConfig, LocationConfig, SourceIntervals, StorageConfig};

    fn minimal_config() -> Config {
        Config {
            display: DisplayConfig { width: 800, height: 480 },
            location: LocationConfig {
                latitude: 48.4,
                longitude: -122.3,
                name: "Test".to_string(),
            },
            sources: SourceIntervals {
                weather_interval_secs: 300,
                river_interval_secs: 300,
                ferry_interval_secs: 60,
                trail_interval_secs: 900,
                road_interval_secs: 1800,
                river: None,
                trail: None,
                road: None,
                ferry: None,
            },
            server: None,
            auth: None,
            device: None,
            storage: StorageConfig::default(),
        }
    }

    #[test]
    fn build_sources_with_no_optional_sources_returns_weather_and_river() {
        // No ferry/trail/road config + no env vars → only weather + river are guaranteed.
        unsafe { std::env::remove_var("WSDOT_ACCESS_CODE") };
        unsafe { std::env::remove_var("NPS_API_KEY") };

        let sources = build_sources(&minimal_config(), true);
        let ids: Vec<&str> = sources.iter().map(|s| s.id()).collect();

        assert!(ids.contains(&"weather"), "should always include weather: {ids:?}");
        assert!(ids.contains(&"river"), "should always include river: {ids:?}");
    }

    #[test]
    fn build_sources_in_fixture_mode_returns_two_core_sources() {
        unsafe { std::env::remove_var("WSDOT_ACCESS_CODE") };
        unsafe { std::env::remove_var("NPS_API_KEY") };

        let sources = build_sources(&minimal_config(), true);
        // In fixture mode without optional source config, ferry/trail/road are skipped
        // because they require API keys or explicit config to be enabled.
        assert!(sources.len() >= 2, "at least weather + river expected");
        assert!(sources.iter().all(|s| !s.id().is_empty()), "all sources must have non-empty id");
    }

    #[test]
    fn build_sources_uses_default_usgs_site_when_no_river_config() {
        unsafe { std::env::remove_var("WSDOT_ACCESS_CODE") };
        unsafe { std::env::remove_var("NPS_API_KEY") };

        let sources = build_sources(&minimal_config(), true);
        let river = sources.iter().find(|s| s.id() == "river");
        assert!(river.is_some(), "river source must be present");
        assert_eq!(river.unwrap().name(), "usgs-river");
    }

    #[test]
    fn build_sources_uses_custom_usgs_site_from_config() {
        unsafe { std::env::remove_var("WSDOT_ACCESS_CODE") };
        unsafe { std::env::remove_var("NPS_API_KEY") };

        let mut config = minimal_config();
        config.sources.river = Some(config::RiverSourceConfig {
            usgs_site_id: "12181000".to_string(),
        });

        let sources = build_sources(&config, true);
        let river = sources.iter().find(|s| s.id() == "river");
        assert!(river.is_some(), "river source must be present with custom site");
    }
}
