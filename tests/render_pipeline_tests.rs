use cascades::config::{Config, DisplayConfig, LocationConfig, SourceIntervals};
use cascades::render_current_state;

fn fixture_config() -> Config {
    Config {
        display: DisplayConfig {
            width: 800,
            height: 480,
        },
        location: LocationConfig {
            latitude: 48.4232,
            longitude: -122.3351,
            name: "Mount Vernon, WA".to_string(),
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
        auth: None,
        device: None,
    }
}

#[test]
fn render_current_state_fixture_returns_800x480() {
    let config = fixture_config();
    let buf = render_current_state(&config, true);
    assert_eq!(buf.width, 800);
    assert_eq!(buf.height, 480);
}

#[test]
fn render_current_state_fixture_has_black_pixels() {
    let config = fixture_config();
    let buf = render_current_state(&config, true);
    let has_black = buf.pixels.iter().any(|&b| b != 0);
    assert!(has_black, "rendered buffer should contain black pixels");
}

#[test]
fn render_current_state_fixture_png_is_valid() {
    let config = fixture_config();
    let buf = render_current_state(&config, true);
    let png = buf.to_png();
    // PNG magic bytes: 89 50 4E 47
    assert!(png.starts_with(b"\x89PNG"), "to_png() should return valid PNG");
}
