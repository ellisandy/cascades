pub mod config;
pub mod domain;
pub mod evaluation;
pub mod presentation;
pub mod render;
pub mod sources;

use config::{Config, Destination};
use domain::DomainState;
use render::PixelBuffer;
use sources::Source;

/// Fetch live data from all configured sources, evaluate conditions,
/// build the display layout, and render to a PixelBuffer.
///
/// When `fixture_data` is true, each source returns canned responses from
/// embedded JSON files instead of making network calls. Use this in tests
/// and CI where live APIs are unavailable.
///
/// Returns an 800×480 1-bit PixelBuffer ready for display or PNG export.
pub fn render_current_state(config: &Config, fixture_data: bool) -> PixelBuffer {
    render_current_state_with_destinations(config, &[], fixture_data)
}

/// Like [`render_current_state`], but with explicit destinations for trip evaluation.
pub fn render_current_state_with_destinations(
    config: &Config,
    destinations: &[Destination],
    fixture_data: bool,
) -> PixelBuffer {
    let all_sources: Vec<Box<dyn Source>> = build_sources(config, fixture_data);

    let mut state = DomainState::default();
    for source in &all_sources {
        match source.fetch() {
            Ok(point) => state.apply(point),
            Err(e) => log::warn!("source '{}' fetch failed: {}", source.name(), e),
        }
    }

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let layout = presentation::build_display_layout(&state, destinations, now_secs);
    render::render_display(&layout, config.display.width, config.display.height)
}

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
