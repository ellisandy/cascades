use crate::domain::{
    DomainState, FerryStatus, RiverGauge, RoadStatus, TrailCondition, TripDecision,
    WeatherObservation,
};

/// A rendered panel: a title and zero or more text rows.
///
/// Panels have no knowledge of pixels, fonts, or geometry — that is render's job.
#[derive(Debug, Clone)]
pub struct Panel {
    pub title: String,
    pub rows: Vec<String>,
}

impl Panel {
    pub fn new(title: impl Into<String>) -> Self {
        Panel {
            title: title.into(),
            rows: Vec::new(),
        }
    }

    pub fn with_row(mut self, row: impl Into<String>) -> Self {
        self.rows.push(row.into());
        self
    }
}

/// Format a WeatherObservation into a Panel.
pub fn format_weather(obs: &WeatherObservation) -> Panel {
    Panel::new("Weather")
        .with_row(format!("{:.0}°F  {}", obs.temperature_f, obs.sky_condition))
        .with_row(format!(
            "Wind {} at {:.0} mph",
            obs.wind_direction, obs.wind_speed_mph
        ))
}

/// Format a RiverGauge reading into a Panel.
pub fn format_river(gauge: &RiverGauge) -> Panel {
    let title = if gauge.site_name.is_empty() {
        "River Gauge".to_string()
    } else {
        gauge.site_name.clone()
    };
    Panel::new(title)
        .with_row(format!("{:.1} ft", gauge.water_level_ft))
        .with_row(format!("{:.0} cfs", gauge.streamflow_cfs))
        .with_row(fmt_time(gauge.timestamp))
}

/// Format FerryStatus into a Panel.
pub fn format_ferry(status: &FerryStatus) -> Panel {
    let mut panel = Panel::new(format!("Ferry — {}", status.route));
    panel.rows.push(status.vessel_name.clone());
    for ts in status.estimated_departures.iter().take(3) {
        panel.rows.push(format!("Departs {}", fmt_time(*ts)));
    }
    panel
}

/// Format TrailCondition into a Panel.
pub fn format_trail(cond: &TrailCondition) -> Panel {
    Panel::new(cond.destination_name.clone())
        .with_row(cond.suitability_summary.clone())
}

/// Format RoadStatus into a Panel.
pub fn format_road(road: &RoadStatus) -> Panel {
    Panel::new(road.road_name.clone())
        .with_row(format!("{} — {}", road.status.to_uppercase(), road.affected_segment))
}

/// Format a TripDecision into a Panel.
pub fn format_trip_decision(destination: &str, decision: &TripDecision) -> Panel {
    match decision {
        TripDecision::Go => Panel::new(destination).with_row("GO".to_string()),
        TripDecision::Caution { warnings } => {
            let mut panel = Panel::new(destination).with_row("CAUTION".to_string());
            for w in warnings {
                panel.rows.push(format!("• {w}"));
            }
            panel
        }
        TripDecision::NoGo { reasons } => {
            let mut panel = Panel::new(destination).with_row("NO GO".to_string());
            for r in reasons {
                panel.rows.push(format!("• {r}"));
            }
            panel
        }
        TripDecision::Unknown { missing } => {
            let mut panel = Panel::new(destination).with_row("UNKNOWN".to_string());
            for m in missing {
                panel.rows.push(format!("• {m}"));
            }
            panel
        }
    }
}

/// Build all panels from the current domain state.
pub fn build_panels(state: &DomainState) -> Vec<Panel> {
    build_panels_with_destinations(state, &[], crate::evaluation::current_unix_secs())
}

/// Build all panels from the current domain state, including trip decision
/// panels for each configured destination.
pub fn build_panels_with_destinations(
    state: &DomainState,
    destinations: &[crate::config::Destination],
    now_secs: u64,
) -> Vec<Panel> {
    let mut panels = Vec::new();
    if let Some(w) = &state.weather {
        panels.push(format_weather(w));
    }
    if let Some(r) = &state.river {
        panels.push(format_river(r));
    }
    if let Some(f) = &state.ferry {
        panels.push(format_ferry(f));
    }
    if let Some(t) = &state.trail {
        panels.push(format_trail(t));
    }
    if let Some(r) = &state.road {
        panels.push(format_road(r));
    }
    for dest in destinations {
        let decision = crate::evaluation::evaluate(dest, state, now_secs);
        panels.push(format_trip_decision(&dest.name, &decision));
    }
    panels
}

/// Format a Unix timestamp as HH:MM (local time, best effort).
fn fmt_time(ts: u64) -> String {
    // Minimal time formatting without a time library.
    let secs = ts % 86400;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    format!("{h:02}:{m:02}")
}

// ─────────────────────────────────────────────────────────────────────────────
// Typed display model (new 4-zone layout)
// ─────────────────────────────────────────────────────────────────────────────

/// Weather condition icon for the hero zone right column.
#[derive(Debug, Clone, PartialEq)]
pub enum WeatherIcon {
    Clear,
    PartlyCloudy,
    MostlyCloudy,
    Overcast,
    Rain,
    HeavyRain,
    Drizzle,
    Snow,
    Thunderstorm,
    Fog,
    Wind,
}

impl WeatherIcon {
    /// Derive the icon from a sky-condition string.
    pub fn from_sky_condition(sky: &str) -> Self {
        let s = sky.to_lowercase();
        if s.contains("thunder") || s.contains("storm") {
            WeatherIcon::Thunderstorm
        } else if s.contains("heavy rain") || s.contains("heavy shower") {
            WeatherIcon::HeavyRain
        } else if s.contains("drizzle") || s.contains("mist") {
            WeatherIcon::Drizzle
        } else if s.contains("rain") || s.contains("shower") {
            WeatherIcon::Rain
        } else if s.contains("snow") || s.contains("sleet") || s.contains("flurr") {
            WeatherIcon::Snow
        } else if s.contains("fog") || s.contains("haze") {
            WeatherIcon::Fog
        } else if s.contains("wind") {
            WeatherIcon::Wind
        } else if s.contains("overcast") {
            WeatherIcon::Overcast
        } else if s.contains("mostly cloudy") || s.contains("mostly cloud") {
            WeatherIcon::MostlyCloudy
        } else if s.contains("partly cloudy")
            || s.contains("partly sunny")
            || s.contains("partly cloud")
        {
            WeatherIcon::PartlyCloudy
        } else if s.contains("cloudy") || s.contains("cloud") {
            WeatherIcon::Overcast
        } else {
            WeatherIcon::Clear
        }
    }
}

/// River gauge trend arrow direction.
#[derive(Debug, Clone, PartialEq)]
pub enum TrendArrow {
    Rising,
    Falling,
    Stable,
}

/// Sparkline data for the 24-hour river gauge trend.
#[derive(Debug, Clone)]
pub struct Sparkline {
    /// Raw gauge readings, oldest first (up to 24 values).
    pub values: Vec<f32>,
    /// Optional flood-stage threshold to draw as a dashed line.
    pub threshold: Option<f32>,
}

/// Content for the header strip (y=0, h=28).
#[derive(Debug, Clone)]
pub struct HeaderContent {
    /// Left label — app or destination name.
    pub app_name: String,
    /// Optional center label — primary river site name.
    pub river_site: Option<String>,
    /// Optional right label — "Updated HH:MM".
    pub last_updated: Option<String>,
}

/// Trip recommendation for the hero zone.
#[derive(Debug, Clone)]
pub enum HeroDecision {
    Go { destination: String },
    Caution { destination: String, warnings: Vec<String> },
    NoGo { destination: String, reasons: Vec<String> },
    Unknown { destination: String, missing: Vec<String> },
    /// Shown when no destinations are configured.
    AllGo,
}

/// Weather content for the hero zone right column.
#[derive(Debug, Clone)]
pub struct WeatherContent {
    pub icon: WeatherIcon,
    pub temperature_f: f32,
    pub sky_condition: String,
    pub wind_dir: String,
    pub wind_speed_mph: f32,
    pub precip_chance_pct: f32,
}

/// Hero zone content (y=30, h=202).
#[derive(Debug, Clone)]
pub struct HeroContent {
    pub decision: HeroDecision,
    pub weather: Option<WeatherContent>,
}

/// River gauge content for the data zone left column.
#[derive(Debug, Clone)]
pub struct RiverContent {
    pub site_name: String,
    pub level_ft: f32,
    pub flow_cfs: f32,
    pub trend: TrendArrow,
    pub sparkline: Option<Sparkline>,
}

/// Ferry status content for the data zone right column.
#[derive(Debug, Clone)]
pub struct FerryContent {
    /// Short route description (e.g. "ANACORTES FERRY").
    pub route: String,
    pub vessel_name: String,
    /// Departure times as "HH:MM" strings; first entry is the next departure.
    pub departures: Vec<String>,
}

/// Data zone content (y=234, h=140).
#[derive(Debug, Clone)]
pub struct DataContent {
    pub river: Option<RiverContent>,
    pub ferry: Option<FerryContent>,
}

/// Trail condition for the context zone left column.
#[derive(Debug, Clone)]
pub struct TrailContent {
    pub name: String,
    pub condition: String,
}

/// Road status for the context zone right column.
#[derive(Debug, Clone)]
pub struct RoadContent {
    pub name: String,
    /// Human-readable status: "open", "closed", "restricted", etc.
    pub status: String,
    pub segment: String,
}

/// Context zone content (y=376, h=104).
#[derive(Debug, Clone)]
pub struct ContextContent {
    pub trail: Option<TrailContent>,
    pub road: Option<RoadContent>,
}

/// Full typed display layout for the redesigned 4-zone e-ink display.
///
/// Build this with [`build_display_layout`] and pass it to
/// [`crate::render::render_display`].
#[derive(Debug, Clone)]
pub struct DisplayLayout {
    pub header: HeaderContent,
    pub hero: HeroContent,
    pub data: DataContent,
    pub context: ContextContent,
}

/// Build a [`DisplayLayout`] from current domain state and destinations.
///
/// Destination decisions are evaluated and the worst-case result is shown in
/// the hero zone. Priority: NoGo > Unknown > Caution > Go. `AllGo` is shown
/// when no destinations are configured. Pass `current_unix_secs()` for
/// `now_secs` in production; pass a fixed value in tests.
pub fn build_display_layout(
    state: &DomainState,
    destinations: &[crate::config::Destination],
    now_secs: u64,
) -> DisplayLayout {
    let header = HeaderContent {
        app_name: "SKAGIT FLATS".to_string(),
        river_site: state
            .river
            .as_ref()
            .map(|r| shorten_site_name(&r.site_name)),
        last_updated: state
            .river
            .as_ref()
            .map(|r| fmt_time(r.timestamp))
            .or_else(|| state.weather.as_ref().map(|w| fmt_time(w.observation_time))),
    };

    let weather = state.weather.as_ref().map(|w| WeatherContent {
        icon: WeatherIcon::from_sky_condition(&w.sky_condition),
        temperature_f: w.temperature_f,
        sky_condition: w.sky_condition.clone(),
        wind_dir: w.wind_direction.clone(),
        wind_speed_mph: w.wind_speed_mph,
        precip_chance_pct: w.precip_chance_pct,
    });

    let decision = if destinations.is_empty() {
        HeroDecision::AllGo
    } else {
        let mut result: Option<HeroDecision> = None;
        for dest in destinations {
            let d = crate::evaluation::evaluate(dest, state, now_secs);
            match d {
                crate::domain::TripDecision::NoGo { reasons } => {
                    result = Some(HeroDecision::NoGo {
                        destination: dest.name.clone(),
                        reasons,
                    });
                    break; // NoGo is worst-case; stop looking
                }
                crate::domain::TripDecision::Unknown { missing } => {
                    if !matches!(result, Some(HeroDecision::NoGo { .. })) {
                        result = Some(HeroDecision::Unknown {
                            destination: dest.name.clone(),
                            missing,
                        });
                    }
                }
                crate::domain::TripDecision::Caution { warnings } => {
                    if !matches!(
                        result,
                        Some(HeroDecision::NoGo { .. }) | Some(HeroDecision::Unknown { .. })
                    ) {
                        result = Some(HeroDecision::Caution {
                            destination: dest.name.clone(),
                            warnings,
                        });
                    }
                }
                crate::domain::TripDecision::Go => {
                    if result.is_none() {
                        result = Some(HeroDecision::Go {
                            destination: dest.name.clone(),
                        });
                    }
                }
            }
        }
        result.unwrap_or(HeroDecision::AllGo)
    };

    let hero = HeroContent { decision, weather };

    // Determine which signals are relevant across all configured destinations.
    // When no destinations are configured, all signals are shown.
    // When destinations are configured, only show signals that at least one
    // destination declares relevant — avoiding clutter from unrelated sources.
    let any_river = destinations.is_empty() || destinations.iter().any(|d| d.signals.river);
    let any_ferry = destinations.is_empty() || destinations.iter().any(|d| d.signals.ferry);
    let any_trail = destinations.is_empty() || destinations.iter().any(|d| d.signals.trail);
    let any_road = destinations.is_empty() || destinations.iter().any(|d| d.signals.road);

    let river = if any_river {
        state.river.as_ref().map(|r| RiverContent {
            site_name: shorten_site_name(&r.site_name),
            level_ft: r.water_level_ft,
            flow_cfs: r.streamflow_cfs,
            trend: TrendArrow::Stable,
            sparkline: None,
        })
    } else {
        None
    };

    let ferry = if any_ferry {
        state.ferry.as_ref().map(|f| FerryContent {
            route: f.route.clone(),
            vessel_name: f.vessel_name.clone(),
            departures: f
                .estimated_departures
                .iter()
                .take(3)
                .map(|&ts| fmt_time(ts))
                .collect(),
        })
    } else {
        None
    };

    let data = DataContent { river, ferry };

    let trail = if any_trail {
        state.trail.as_ref().map(|t| TrailContent {
            name: t.destination_name.clone(),
            condition: t.suitability_summary.clone(),
        })
    } else {
        None
    };

    let road = if any_road {
        state.road.as_ref().map(|r| RoadContent {
            name: r.road_name.clone(),
            status: r.status.clone(),
            segment: r.affected_segment.clone(),
        })
    } else {
        None
    };

    let context = ContextContent { trail, road };

    DisplayLayout { header, hero, data, context }
}

/// Remove common verbose suffixes from USGS site names.
fn shorten_site_name(name: &str) -> String {
    let s = name
        .trim_end_matches(", WA")
        .trim_end_matches(", Washington")
        .trim_end_matches(", Wa")
        .trim_end_matches(" WA");
    if s.len() > 32 {
        format!("{}...", &s[..29])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Destination;
    use crate::domain::{
        FerryStatus, RiverGauge, RoadStatus, TrailCondition, TripCriteria, WeatherObservation,
    };

    fn sample_weather() -> WeatherObservation {
        WeatherObservation {
            temperature_f: 52.0,
            wind_speed_mph: 10.0,
            wind_direction: "SW".to_string(),
            sky_condition: "Mostly Cloudy".to_string(),
            precip_chance_pct: 0.0,
            observation_time: 0,
        }
    }

    fn sample_river() -> RiverGauge {
        RiverGauge {
            site_id: "12200500".to_string(),
            site_name: "Skagit River Near Mount Vernon, WA".to_string(),
            water_level_ft: 11.87,
            streamflow_cfs: 8750.0,
            timestamp: 46800, // 13:00 UTC
        }
    }

    fn sample_ferry() -> FerryStatus {
        FerryStatus {
            route: "Anacortes / San Juan Islands".to_string(),
            vessel_name: "MV Samish".to_string(),
            estimated_departures: vec![37800, 45000, 52200], // 10:30, 12:30, 14:30
        }
    }

    fn sample_trail() -> TrailCondition {
        TrailCondition {
            destination_name: "Cascade Pass Trail".to_string(),
            suitability_summary: "Snow above 5000ft".to_string(),
            last_updated: 0,
        }
    }

    fn sample_road() -> RoadStatus {
        RoadStatus {
            road_name: "SR-20".to_string(),
            status: "closed".to_string(),
            affected_segment: "Newhalem to Rainy Pass".to_string(),
            timestamp: 0,
        }
    }

    #[test]
    fn weather_panel_has_expected_rows() {
        let panel = format_weather(&sample_weather());
        assert_eq!(panel.title, "Weather");
        assert_eq!(panel.rows.len(), 2);
        assert!(panel.rows[0].contains("52°F"));
        assert!(panel.rows[0].contains("Mostly Cloudy"));
        assert!(panel.rows[1].contains("SW"));
        assert!(panel.rows[1].contains("10 mph"));
    }

    #[test]
    fn river_panel_shows_level_and_flow() {
        let panel = format_river(&sample_river());
        assert!(panel.title.contains("Skagit River"));
        assert_eq!(panel.rows.len(), 3);
        assert!(panel.rows[0].contains("11.9 ft"));
        assert!(panel.rows[1].contains("8750 cfs"));
        // Third row is the formatted timestamp
        assert!(panel.rows[2].contains(":"));
    }

    #[test]
    fn river_panel_empty_name_uses_default_title() {
        let mut gauge = sample_river();
        gauge.site_name = String::new();
        let panel = format_river(&gauge);
        assert_eq!(panel.title, "River Gauge");
    }

    #[test]
    fn ferry_panel_shows_vessel_and_departures() {
        let panel = format_ferry(&sample_ferry());
        assert!(panel.title.contains("Ferry"));
        assert!(panel.title.contains("Anacortes"));
        assert_eq!(panel.rows[0], "MV Samish");
        // Should show up to 3 departure times
        assert!(panel.rows.len() >= 2);
        assert!(panel.rows[1].starts_with("Departs"));
    }

    #[test]
    fn ferry_panel_limits_to_three_departures() {
        let mut status = sample_ferry();
        status.estimated_departures = vec![1000, 2000, 3000, 4000, 5000];
        let panel = format_ferry(&status);
        // 1 vessel name row + 3 departure rows = 4 total
        assert_eq!(panel.rows.len(), 4);
    }

    #[test]
    fn trail_panel_shows_condition() {
        let panel = format_trail(&sample_trail());
        assert_eq!(panel.title, "Cascade Pass Trail");
        assert_eq!(panel.rows.len(), 1);
        assert!(panel.rows[0].contains("Snow above 5000ft"));
    }

    #[test]
    fn road_panel_shows_status_and_segment() {
        let panel = format_road(&sample_road());
        assert_eq!(panel.title, "SR-20");
        assert_eq!(panel.rows.len(), 1);
        assert!(panel.rows[0].contains("CLOSED"));
        assert!(panel.rows[0].contains("Newhalem to Rainy Pass"));
    }

    #[test]
    fn trip_decision_go_panel() {
        let panel = format_trip_decision("Test Dest", &TripDecision::Go);
        assert_eq!(panel.title, "Test Dest");
        assert_eq!(panel.rows[0], "GO");
    }

    #[test]
    fn trip_decision_no_go_panel_lists_reasons() {
        let decision = TripDecision::NoGo {
            reasons: vec!["Too cold".to_string()],
        };
        let panel = format_trip_decision("Test Dest", &decision);
        assert_eq!(panel.rows[0], "NO GO");
        assert!(panel.rows[1].contains("Too cold"));
    }

    #[test]
    fn trip_decision_nogo_multiple_reasons() {
        let decision = TripDecision::NoGo {
            reasons: vec![
                "Too cold".to_string(),
                "River too high".to_string(),
                "Road closed".to_string(),
            ],
        };
        let panel = format_trip_decision("Test", &decision);
        // 1 "NO GO" + 3 reason rows
        assert_eq!(panel.rows.len(), 4);
        assert!(panel.rows[1].starts_with('•'));
        assert!(panel.rows[2].starts_with('•'));
        assert!(panel.rows[3].starts_with('•'));
    }

    #[test]
    fn build_panels_empty_state() {
        let state = DomainState::default();
        let panels = build_panels(&state);
        assert!(panels.is_empty());
    }

    #[test]
    fn build_panels_weather_only() {
        let state = DomainState {
            weather: Some(sample_weather()),
            ..Default::default()
        };
        let panels = build_panels(&state);
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0].title, "Weather");
    }

    #[test]
    fn build_panels_all_sources() {
        let state = DomainState {
            weather: Some(sample_weather()),
            river: Some(sample_river()),
            ferry: Some(sample_ferry()),
            trail: Some(sample_trail()),
            road: Some(sample_road()),
        };
        let panels = build_panels(&state);
        assert_eq!(panels.len(), 5);
    }

    #[test]
    fn build_panels_with_destinations_adds_trip_panels() {
        let state = DomainState {
            weather: Some(sample_weather()),
            ..Default::default()
        };
        let destinations = vec![
            Destination {
                name: "Skagit Loop".to_string(),
                signals: Default::default(),
                criteria: TripCriteria {
                    min_temp_f: Some(40.0),
                    ..Default::default()
                },
            },
            Destination {
                name: "Baker Lake".to_string(),
                signals: Default::default(),
                criteria: TripCriteria {
                    max_temp_f: Some(90.0),
                    ..Default::default()
                },
            },
        ];
        let panels = build_panels_with_destinations(&state, &destinations, 0);
        // 1 weather panel + 2 destination panels
        assert_eq!(panels.len(), 3);
        assert_eq!(panels[1].title, "Skagit Loop");
        assert_eq!(panels[2].title, "Baker Lake");
    }

    #[test]
    fn panel_builder_pattern() {
        let panel = Panel::new("Title")
            .with_row("Row 1")
            .with_row("Row 2")
            .with_row("Row 3");
        assert_eq!(panel.title, "Title");
        assert_eq!(panel.rows.len(), 3);
    }

    #[test]
    fn fmt_time_formats_correctly() {
        // 13:00 UTC = 46800 seconds into the day
        assert_eq!(fmt_time(46800), "13:00");
        // 00:00
        assert_eq!(fmt_time(0), "00:00");
        // 23:59
        assert_eq!(fmt_time(86340), "23:59");
    }

    // ── New typed display model tests ──

    #[test]
    fn weather_icon_clear_conditions() {
        assert_eq!(WeatherIcon::from_sky_condition("Sunny"), WeatherIcon::Clear);
        assert_eq!(WeatherIcon::from_sky_condition("Clear"), WeatherIcon::Clear);
    }

    #[test]
    fn weather_icon_rain_detection() {
        assert_eq!(WeatherIcon::from_sky_condition("Rain"), WeatherIcon::Rain);
        assert_eq!(
            WeatherIcon::from_sky_condition("Heavy Rain"),
            WeatherIcon::HeavyRain
        );
        assert_eq!(WeatherIcon::from_sky_condition("Drizzle"), WeatherIcon::Drizzle);
    }

    #[test]
    fn weather_icon_cloud_variants() {
        assert_eq!(
            WeatherIcon::from_sky_condition("Mostly Cloudy"),
            WeatherIcon::MostlyCloudy
        );
        assert_eq!(
            WeatherIcon::from_sky_condition("Partly Cloudy"),
            WeatherIcon::PartlyCloudy
        );
        assert_eq!(
            WeatherIcon::from_sky_condition("Overcast"),
            WeatherIcon::Overcast
        );
    }

    #[test]
    fn weather_icon_special_conditions() {
        assert_eq!(
            WeatherIcon::from_sky_condition("Thunderstorm"),
            WeatherIcon::Thunderstorm
        );
        assert_eq!(WeatherIcon::from_sky_condition("Snow"), WeatherIcon::Snow);
        assert_eq!(WeatherIcon::from_sky_condition("Fog"), WeatherIcon::Fog);
    }

    #[test]
    fn build_display_layout_empty_state() {
        let state = DomainState::default();
        let layout = build_display_layout(&state, &[], 0);
        assert_eq!(layout.header.app_name, "SKAGIT FLATS");
        assert!(layout.header.river_site.is_none());
        assert!(layout.hero.weather.is_none());
        assert!(layout.data.river.is_none());
        assert!(layout.data.ferry.is_none());
        assert!(layout.context.trail.is_none());
        assert!(layout.context.road.is_none());
    }

    #[test]
    fn build_display_layout_no_destinations_shows_all_go() {
        let state = DomainState {
            weather: Some(sample_weather()),
            ..Default::default()
        };
        let layout = build_display_layout(&state, &[], 0);
        assert!(matches!(layout.hero.decision, HeroDecision::AllGo));
    }

    #[test]
    fn build_display_layout_go_decision() {
        let state = DomainState {
            weather: Some(sample_weather()),
            ..Default::default()
        };
        let destinations = vec![Destination {
            name: "Test Loop".to_string(),
            signals: Default::default(),
            criteria: TripCriteria {
                min_temp_f: Some(40.0),
                ..Default::default()
            },
        }];
        let layout = build_display_layout(&state, &destinations, 0);
        assert!(matches!(layout.hero.decision, HeroDecision::Go { .. }));
    }

    #[test]
    fn build_display_layout_nogo_decision() {
        let state = DomainState {
            weather: Some(sample_weather()),
            ..Default::default()
        };
        let destinations = vec![Destination {
            name: "Cold Dest".to_string(),
            signals: Default::default(),
            criteria: TripCriteria {
                min_temp_f: Some(80.0), // 52°F fails this
                ..Default::default()
            },
        }];
        let layout = build_display_layout(&state, &destinations, 0);
        assert!(matches!(
            layout.hero.decision,
            HeroDecision::NoGo { .. }
        ));
    }

    #[test]
    fn build_display_layout_river_content() {
        let state = DomainState {
            river: Some(sample_river()),
            ..Default::default()
        };
        let layout = build_display_layout(&state, &[], 0);
        let river = layout.data.river.expect("river should be present");
        assert!((river.level_ft - 11.87).abs() < 0.01);
        assert!((river.flow_cfs - 8750.0).abs() < 1.0);
        assert_eq!(river.trend, TrendArrow::Stable);
    }

    #[test]
    fn build_display_layout_ferry_departures() {
        let state = DomainState {
            ferry: Some(sample_ferry()),
            ..Default::default()
        };
        let layout = build_display_layout(&state, &[], 0);
        let ferry = layout.data.ferry.expect("ferry should be present");
        assert_eq!(ferry.vessel_name, "MV Samish");
        // 3 departures → 3 formatted strings
        assert_eq!(ferry.departures.len(), 3);
        // 37800 = 10:30
        assert_eq!(ferry.departures[0], "10:30");
    }

    #[test]
    fn build_display_layout_context_trail_road() {
        let state = DomainState {
            trail: Some(sample_trail()),
            road: Some(sample_road()),
            ..Default::default()
        };
        let layout = build_display_layout(&state, &[], 0);
        let trail = layout.context.trail.expect("trail should be present");
        let road = layout.context.road.expect("road should be present");
        assert_eq!(trail.name, "Cascade Pass Trail");
        assert!(road.status.contains("closed"));
    }

    #[test]
    fn shorten_site_name_strips_wa_suffix() {
        assert_eq!(
            shorten_site_name("Skagit River Near Mount Vernon, WA"),
            "Skagit River Near Mount Vernon"
        );
    }

    // ── Signal relevance filtering in build_display_layout ──

    #[test]
    fn river_hidden_when_no_destination_has_river_relevant() {
        use crate::domain::RelevantSignals;
        let state = DomainState {
            river: Some(sample_river()),
            ..Default::default()
        };
        let destinations = vec![Destination {
            name: "Island Trip".to_string(),
            signals: RelevantSignals {
                weather: true,
                river: false,
                ferry: true,
                trail: false,
                road: false,
            },
            criteria: Default::default(),
        }];
        let layout = build_display_layout(&state, &destinations, 0);
        // river data exists but no destination considers it relevant → hidden
        assert!(layout.data.river.is_none());
    }

    #[test]
    fn river_shown_when_any_destination_has_river_relevant() {
        use crate::domain::RelevantSignals;
        let state = DomainState {
            river: Some(sample_river()),
            ..Default::default()
        };
        let destinations = vec![
            Destination {
                name: "Island Trip".to_string(),
                signals: RelevantSignals {
                    weather: true,
                    river: false,
                    ferry: true,
                    trail: false,
                    road: false,
                },
                criteria: Default::default(),
            },
            Destination {
                name: "Valley Loop".to_string(),
                signals: RelevantSignals {
                    weather: true,
                    river: true,
                    ferry: false,
                    trail: false,
                    road: true,
                },
                criteria: Default::default(),
            },
        ];
        let layout = build_display_layout(&state, &destinations, 0);
        // at least one destination has river relevant → river shown
        assert!(layout.data.river.is_some());
    }

    #[test]
    fn all_signals_shown_when_no_destinations_configured() {
        let state = DomainState {
            river: Some(sample_river()),
            ferry: Some(sample_ferry()),
            trail: Some(sample_trail()),
            road: Some(sample_road()),
            ..Default::default()
        };
        let layout = build_display_layout(&state, &[], 0);
        // no destinations → full dashboard mode → all signals shown
        assert!(layout.data.river.is_some());
        assert!(layout.data.ferry.is_some());
        assert!(layout.context.trail.is_some());
        assert!(layout.context.road.is_some());
    }

    #[test]
    fn ferry_hidden_when_no_destination_has_ferry_relevant() {
        use crate::domain::RelevantSignals;
        let state = DomainState {
            ferry: Some(sample_ferry()),
            ..Default::default()
        };
        let destinations = vec![Destination {
            name: "Mountain Hike".to_string(),
            signals: RelevantSignals {
                weather: true,
                river: false,
                ferry: false,
                trail: true,
                road: true,
            },
            criteria: Default::default(),
        }];
        let layout = build_display_layout(&state, &destinations, 0);
        assert!(layout.data.ferry.is_none());
    }
}
