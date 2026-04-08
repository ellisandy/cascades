pub mod api;
pub mod compositor;
pub mod config;
pub mod domain;
pub mod evaluation;
pub mod format;
pub mod instance_store;
pub mod jsonpath;
pub mod layout_store;
pub mod plugin_registry;
pub mod presentation;
pub mod source_store;
pub mod sources;
pub mod template;

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
