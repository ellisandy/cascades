use crate::presentation::{
    DisplayLayout, FerryContent, HeroDecision, RiverContent, TrendArrow, WeatherIcon,
};
use crate::render::font::{self, FontSize, CELL_HEIGHT, CELL_WIDTH, GLYPH_HEIGHT, GLYPH_WIDTH};
use crate::render::PixelBuffer;

// ─────────────────────────────────────────────────────────────────────────────
// Legacy grid layout (kept for backward-compatibility with render::render_panels)
// ─────────────────────────────────────────────────────────────────────────────

use crate::presentation::Panel;

/// Padding inside each panel border (pixels).
const PANEL_PADDING: u32 = 2;
/// Border thickness (pixels).
const BORDER_WIDTH: u32 = 1;
/// Height of the title bar (border + padding + glyph + padding + divider).
const TITLE_BAR_HEIGHT: u32 =
    BORDER_WIDTH + PANEL_PADDING + GLYPH_HEIGHT + PANEL_PADDING + BORDER_WIDTH;
/// Gap between panels (pixels).
const PANEL_GAP: u32 = 2;

/// A rectangle on the pixel buffer.
#[derive(Debug, Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// Compute a grid layout for `n` panels within the given dimensions.
fn compute_grid(n: usize, total_w: u32, total_h: u32) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }

    let cols = match n {
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 2,
        5 | 6 => 3,
        _ => ((n as f32).sqrt().ceil() as usize).max(1),
    };
    let rows = n.div_ceil(cols);

    let cell_w = (total_w - PANEL_GAP * (cols as u32).saturating_sub(1)) / cols as u32;
    let cell_h = (total_h - PANEL_GAP * (rows as u32).saturating_sub(1)) / rows as u32;

    let mut rects = Vec::with_capacity(n);
    for i in 0..n {
        let row = i / cols;
        let col = i % cols;
        rects.push(Rect {
            x: col as u32 * (cell_w + PANEL_GAP),
            y: row as u32 * (cell_h + PANEL_GAP),
            w: cell_w,
            h: cell_h,
        });
    }
    rects
}

/// Draw a 1-pixel border rectangle.
fn draw_border(buf: &mut PixelBuffer, r: Rect) {
    for x in r.x..r.x + r.w {
        buf.set_pixel(x, r.y, true);
        buf.set_pixel(x, r.y + r.h - 1, true);
    }
    for y in r.y..r.y + r.h {
        buf.set_pixel(r.x, y, true);
        buf.set_pixel(r.x + r.w - 1, y, true);
    }
}

/// Fill a rectangular region with a solid color.
fn fill_rect(buf: &mut PixelBuffer, r: Rect, black: bool) {
    for y in r.y..r.y + r.h {
        for x in r.x..r.x + r.w {
            buf.set_pixel(x, y, black);
        }
    }
}

/// Draw a single glyph at (x, y) using the base 8×16 font.
fn draw_glyph(buf: &mut PixelBuffer, x: u32, y: u32, ch: char, invert: bool) {
    let glyph = font::glyph(ch);
    for row in 0..GLYPH_HEIGHT {
        let bits = glyph[row as usize];
        for col in 0..GLYPH_WIDTH {
            let pixel_on = (bits >> (7 - col)) & 1 == 1;
            let draw_black = if invert { !pixel_on } else { pixel_on };
            buf.set_pixel(x + col, y + row, draw_black);
        }
    }
}

/// Draw a string using the base 8×16 font. Returns the number of chars drawn.
fn draw_text(
    buf: &mut PixelBuffer,
    x: u32,
    y: u32,
    text: &str,
    max_width: u32,
    invert: bool,
) -> usize {
    let max_chars = (max_width / CELL_WIDTH) as usize;
    let mut drawn = 0;
    for ch in text.chars().take(max_chars) {
        draw_glyph(buf, x + drawn as u32 * CELL_WIDTH, y, ch, invert);
        drawn += 1;
    }
    drawn
}

/// Render a single panel into its allocated rectangle.
fn render_panel(buf: &mut PixelBuffer, panel: &Panel, rect: Rect) {
    if rect.w < CELL_WIDTH + 2 * BORDER_WIDTH || rect.h < TITLE_BAR_HEIGHT + CELL_HEIGHT {
        return;
    }

    draw_border(buf, rect);

    let title_rect = Rect {
        x: rect.x + BORDER_WIDTH,
        y: rect.y + BORDER_WIDTH,
        w: rect.w - 2 * BORDER_WIDTH,
        h: TITLE_BAR_HEIGHT - BORDER_WIDTH,
    };
    fill_rect(buf, title_rect, true);

    let text_x = rect.x + BORDER_WIDTH + PANEL_PADDING;
    let text_y = rect.y + BORDER_WIDTH + PANEL_PADDING;
    let text_max_w = rect.w.saturating_sub(2 * (BORDER_WIDTH + PANEL_PADDING));
    draw_text(buf, text_x, text_y, &panel.title, text_max_w, true);

    let divider_y = rect.y + TITLE_BAR_HEIGHT;
    for x in rect.x + BORDER_WIDTH..rect.x + rect.w - BORDER_WIDTH {
        buf.set_pixel(x, divider_y, true);
    }

    let body_x = rect.x + BORDER_WIDTH + PANEL_PADDING;
    let body_y = divider_y + 1 + PANEL_PADDING;
    let body_w = rect.w.saturating_sub(2 * (BORDER_WIDTH + PANEL_PADDING));
    let body_h = rect
        .h
        .saturating_sub(TITLE_BAR_HEIGHT + 1 + PANEL_PADDING + BORDER_WIDTH);

    let chars_per_line = (body_w / CELL_WIDTH) as usize;
    if chars_per_line == 0 {
        return;
    }

    let max_lines = (body_h / CELL_HEIGHT) as usize;
    let mut line_idx = 0;

    for row_text in &panel.rows {
        if line_idx >= max_lines {
            break;
        }
        let wrapped = wrap_text(row_text, chars_per_line);
        for line in wrapped {
            if line_idx >= max_lines {
                break;
            }
            let ly = body_y + line_idx as u32 * CELL_HEIGHT;
            draw_text(buf, body_x, ly, &line, body_w, false);
            line_idx += 1;
        }
    }
}

/// Word-wrap text to fit within `max_chars` columns.
fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return Vec::new();
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        if word.len() > max_chars {
            if !current_line.is_empty() {
                lines.push(current_line);
                current_line = String::new();
            }
            let mut remaining = word;
            while remaining.len() > max_chars {
                let (chunk, rest) = remaining.split_at(max_chars);
                lines.push(chunk.to_string());
                remaining = rest;
            }
            if !remaining.is_empty() {
                current_line = remaining.to_string();
            }
        } else if current_line.is_empty() {
            current_line = word.to_string();
        } else if current_line.len() + 1 + word.len() <= max_chars {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
        }
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Lay out and render all panels into the pixel buffer (legacy grid layout).
pub fn layout_and_render(panels: &[Panel], buf: &mut PixelBuffer) {
    let grid = compute_grid(panels.len(), buf.width, buf.height);
    for (panel, rect) in panels.iter().zip(grid.iter()) {
        render_panel(buf, panel, *rect);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// New 4-zone layout
// ─────────────────────────────────────────────────────────────────────────────

// Zone y-offsets and heights (800×480 canvas).
const HEADER_Y: u32 = 0;
const HEADER_H: u32 = 28;
const DIVIDER_H: u32 = 2;
const HERO_Y: u32 = HEADER_H + DIVIDER_H; // 30
const HERO_H: u32 = 202;
const DATA_Y: u32 = HERO_Y + HERO_H + DIVIDER_H; // 234
const DATA_H: u32 = 140;
const CONTEXT_Y: u32 = DATA_Y + DATA_H + DIVIDER_H; // 376
const CONTEXT_H: u32 = 102;

// Hero zone column split.
const HERO_LEFT_W: u32 = 490;
const HERO_DIVIDER_X: u32 = 492;
const HERO_RIGHT_X: u32 = 494;
const HERO_RIGHT_W: u32 = 306;

// Data zone column split.
const DATA_LEFT_W: u32 = 396;
const DATA_DIVIDER_X: u32 = 398;
const DATA_RIGHT_X: u32 = 400;
const DATA_RIGHT_W: u32 = 400;

// Context zone column split (same as data).
const CTX_LEFT_W: u32 = 396;
const CTX_DIVIDER_X: u32 = 398;
const CTX_RIGHT_X: u32 = 400;
const CTX_RIGHT_W: u32 = 400;

/// Draw a scaled glyph at (x, y) using a given FontSize.
///
/// Each pixel in the base 8×16 glyph becomes a scale×scale filled block.
fn draw_glyph_scaled(
    buf: &mut PixelBuffer,
    x: u32,
    y: u32,
    ch: char,
    size: FontSize,
    invert: bool,
) {
    let scale = size.scale();
    let g = font::glyph(ch);
    for row in 0..GLYPH_HEIGHT {
        let bits = g[row as usize];
        for col in 0..GLYPH_WIDTH {
            let pixel_on = (bits >> (7 - col)) & 1 == 1;
            let draw_black = if invert { !pixel_on } else { pixel_on };
            if draw_black {
                let px = x + col * scale;
                let py = y + row * scale;
                for dr in 0..scale {
                    for dc in 0..scale {
                        buf.set_pixel(px + dc, py + dr, true);
                    }
                }
            }
        }
    }
}

/// Draw a string at (x, y) using the given FontSize.
///
/// Returns the pixel width consumed. Clips at `max_width`.
fn draw_text_scaled(
    buf: &mut PixelBuffer,
    x: u32,
    y: u32,
    text: &str,
    size: FontSize,
    invert: bool,
    max_width: u32,
) -> u32 {
    let cell_w = size.cell_w();
    if cell_w == 0 {
        return 0;
    }
    let max_chars = (max_width / cell_w) as usize;
    let mut cursor = 0u32;
    for ch in text.chars().take(max_chars) {
        draw_glyph_scaled(buf, x + cursor, y, ch, size, invert);
        cursor += cell_w;
    }
    cursor
}

/// Draw a right-aligned string within the region [x, x+width).
fn draw_text_right(
    buf: &mut PixelBuffer,
    region_x: u32,
    y: u32,
    text: &str,
    size: FontSize,
    width: u32,
    invert: bool,
) {
    let cell_w = size.cell_w();
    let total_px = text.chars().count() as u32 * cell_w;
    let start_x = if total_px <= width {
        region_x + width - total_px
    } else {
        region_x
    };
    draw_text_scaled(buf, start_x, y, text, size, invert, width);
}

/// Draw a centered string within the region [x, x+width).
fn draw_text_centered(
    buf: &mut PixelBuffer,
    region_x: u32,
    y: u32,
    text: &str,
    size: FontSize,
    width: u32,
    invert: bool,
) {
    let cell_w = size.cell_w();
    let total_px = text.chars().count() as u32 * cell_w;
    let start_x = if total_px <= width {
        region_x + (width - total_px) / 2
    } else {
        region_x
    };
    draw_text_scaled(buf, start_x, y, text, size, invert, width);
}

/// Draw a solid filled rectangle.
fn fill_rect_xy(buf: &mut PixelBuffer, x: u32, y: u32, w: u32, h: u32, black: bool) {
    for dy in 0..h {
        for dx in 0..w {
            buf.set_pixel(x + dx, y + dy, black);
        }
    }
}

/// Draw a horizontal line of `thickness` pixels.
fn hline(buf: &mut PixelBuffer, x: u32, y: u32, len: u32, thickness: u32) {
    for dy in 0..thickness {
        for dx in 0..len {
            buf.set_pixel(x + dx, y + dy, true);
        }
    }
}

/// Draw a vertical line of `thickness` pixels.
fn vline(buf: &mut PixelBuffer, x: u32, y: u32, len: u32, thickness: u32) {
    for dy in 0..len {
        for dx in 0..thickness {
            buf.set_pixel(x + dx, y + dy, true);
        }
    }
}

/// Fill an approximate circle centered at (cx, cy) with radius r.
fn fill_circle(buf: &mut PixelBuffer, cx: i32, cy: i32, r: i32) {
    let r2 = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r2 {
                let px = cx + dx;
                let py = cy + dy;
                if px >= 0 && py >= 0 {
                    buf.set_pixel(px as u32, py as u32, true);
                }
            }
        }
    }
}

// ── Zone 1: Header ────────────────────────────────────────────────────────────

fn render_header(buf: &mut PixelBuffer, layout: &DisplayLayout) {
    let h = &layout.header;
    let pad_x = 8u32;
    let pad_y = (HEADER_H.saturating_sub(FontSize::Micro.glyph_h())) / 2;

    // Left: app name
    draw_text_scaled(
        buf,
        pad_x,
        HEADER_Y + pad_y,
        &h.app_name,
        FontSize::Small,
        false,
        400,
    );

    // Center: river site (optional)
    if let Some(site) = &h.river_site {
        draw_text_centered(buf, 200, HEADER_Y + pad_y + 3, site, FontSize::Micro, 400, false);
    }

    // Right: "Updated HH:MM" (optional)
    if let Some(ts) = &h.last_updated {
        let label = format!("Updated {ts}");
        draw_text_right(
            buf,
            0,
            HEADER_Y + pad_y + 3,
            &label,
            FontSize::Micro,
            800 - 16,
            false,
        );
    }

    // Bottom divider
    hline(buf, 0, HEADER_H, 800, DIVIDER_H);
}

// ── Zone 2: Hero ──────────────────────────────────────────────────────────────

fn render_hero(buf: &mut PixelBuffer, layout: &DisplayLayout) {
    let hero = &layout.hero;
    render_hero_left(buf, hero);
    if hero.weather.is_some() {
        // Vertical divider between GO/NO-GO and weather columns
        vline(buf, HERO_DIVIDER_X, HERO_Y, HERO_H, DIVIDER_H);
        render_hero_right(buf, hero);
    }
    // Bottom divider
    hline(buf, 0, HERO_Y + HERO_H, 800, DIVIDER_H);
}

fn render_hero_left(buf: &mut PixelBuffer, hero: &crate::presentation::HeroContent) {
    let glyph_h = FontSize::Hero.glyph_h();
    let cell_w = FontSize::Hero.cell_w();
    let pad_x = 12u32;

    match &hero.decision {
        HeroDecision::Go { destination: _ } | HeroDecision::AllGo => {
            let label = "GO";
            let text_w = label.chars().count() as u32 * cell_w;
            let cx = if text_w < HERO_LEFT_W {
                (HERO_LEFT_W - text_w) / 2
            } else {
                pad_x
            };
            let cy = HERO_Y + (HERO_H.saturating_sub(glyph_h)) / 2;
            draw_text_scaled(buf, cx, cy, label, FontSize::Hero, false, HERO_LEFT_W);
        }
        HeroDecision::NoGo { destination: _, reasons } => {
            // "NO GO" top-aligned (no extra top padding — space reserved for bullets)
            let label = "NO GO";
            draw_text_scaled(
                buf,
                pad_x,
                HERO_Y,
                label,
                FontSize::Hero,
                false,
                HERO_LEFT_W - pad_x,
            );

            // Reason bullets below hero text.
            // Use line_gap() instead of scale for the gap so bullets aren't
            // pushed out of the zone at large scale factors (Hero scale = 14).
            // Cap at the zone midpoint so bullets always land within the zone.
            let reasons_y = (HERO_Y + glyph_h + FontSize::Hero.line_gap())
                .min(HERO_Y + HERO_H / 2);
            let small_glyph_h = FontSize::Small.glyph_h();
            let small_step = small_glyph_h + FontSize::Small.line_gap();
            let max_reasons = 4usize;
            let shown = reasons.len().min(max_reasons);
            for (i, reason) in reasons.iter().take(shown).enumerate() {
                let ry = reasons_y + i as u32 * small_step;
                if ry + small_glyph_h > HERO_Y + HERO_H {
                    break;
                }
                // Filled 6×6 bullet square
                fill_rect_xy(buf, pad_x, ry + 6, 6, 6, true);
                draw_text_scaled(
                    buf,
                    pad_x + 10,
                    ry,
                    reason,
                    FontSize::Small,
                    false,
                    HERO_LEFT_W - pad_x - 10,
                );
            }
            if reasons.len() > max_reasons {
                let extra_y = reasons_y + max_reasons as u32 * small_step;
                if extra_y + FontSize::Micro.glyph_h() <= HERO_Y + HERO_H {
                    let more = format!("...+{} more", reasons.len() - max_reasons);
                    draw_text_scaled(
                        buf,
                        pad_x + 10,
                        extra_y,
                        &more,
                        FontSize::Micro,
                        false,
                        HERO_LEFT_W - pad_x - 10,
                    );
                }
            }
        }
        HeroDecision::Caution { destination: _, warnings } => {
            let label = "CAUTION";
            draw_text_scaled(
                buf,
                pad_x,
                HERO_Y,
                label,
                FontSize::Hero,
                false,
                HERO_LEFT_W - pad_x,
            );
            let reasons_y = (HERO_Y + glyph_h + FontSize::Hero.line_gap())
                .min(HERO_Y + HERO_H / 2);
            let small_glyph_h = FontSize::Small.glyph_h();
            let small_step = small_glyph_h + FontSize::Small.line_gap();
            let max_items = 4usize;
            for (i, warning) in warnings.iter().take(max_items).enumerate() {
                let ry = reasons_y + i as u32 * small_step;
                if ry + small_glyph_h > HERO_Y + HERO_H {
                    break;
                }
                fill_rect_xy(buf, pad_x, ry + 6, 6, 6, true);
                draw_text_scaled(
                    buf,
                    pad_x + 10,
                    ry,
                    warning,
                    FontSize::Small,
                    false,
                    HERO_LEFT_W - pad_x - 10,
                );
            }
        }
        HeroDecision::Unknown { destination: _, missing } => {
            let label = "UNKNOWN";
            draw_text_scaled(
                buf,
                pad_x,
                HERO_Y,
                label,
                FontSize::Hero,
                false,
                HERO_LEFT_W - pad_x,
            );
            let reasons_y = (HERO_Y + glyph_h + FontSize::Hero.line_gap())
                .min(HERO_Y + HERO_H / 2);
            let small_glyph_h = FontSize::Small.glyph_h();
            let small_step = small_glyph_h + FontSize::Small.line_gap();
            let max_items = 4usize;
            for (i, item) in missing.iter().take(max_items).enumerate() {
                let ry = reasons_y + i as u32 * small_step;
                if ry + small_glyph_h > HERO_Y + HERO_H {
                    break;
                }
                fill_rect_xy(buf, pad_x, ry + 6, 6, 6, false);
                draw_text_scaled(
                    buf,
                    pad_x + 10,
                    ry,
                    item,
                    FontSize::Small,
                    false,
                    HERO_LEFT_W - pad_x - 10,
                );
            }
        }
    }
}

fn render_hero_right(buf: &mut PixelBuffer, hero: &crate::presentation::HeroContent) {
    let w = hero.weather.as_ref().unwrap();
    let col_x = HERO_RIGHT_X;
    let pad = 12u32;

    // Weather icon at (col_x + pad, HERO_Y + 8), 64×64
    let icon_x = col_x + pad;
    let icon_y = HERO_Y + 8;
    draw_weather_icon(buf, icon_x, icon_y, &w.icon);

    // Temperature to the right of icon, vertically centred to icon midpoint
    let temp_str = format!("{:.0}\u{00B0}F", w.temperature_f);
    let icon_mid_y = icon_y + 32;
    let temp_glyph_h = FontSize::Large.glyph_h();
    let temp_y = icon_mid_y.saturating_sub(temp_glyph_h / 2);
    draw_text_right(
        buf,
        col_x,
        temp_y,
        &temp_str,
        FontSize::Large,
        800 - 16,
        false,
    );

    // Sky condition
    let sky_y = icon_y + 64 + 6;
    draw_text_scaled(
        buf,
        col_x + pad,
        sky_y,
        &w.sky_condition,
        FontSize::Small,
        false,
        HERO_RIGHT_W - 2 * pad,
    );

    // Wind + precipitation line
    let wind_str = format!(
        "{} {:.0}mph  PoP {:.0}%",
        w.wind_dir, w.wind_speed_mph, w.precip_chance_pct
    );
    let wind_y = sky_y + FontSize::Small.cell_h() + 4;
    draw_text_scaled(
        buf,
        col_x + pad,
        wind_y,
        &wind_str,
        FontSize::Micro,
        false,
        HERO_RIGHT_W - 2 * pad,
    );
}

// ── Zone 3: Data ──────────────────────────────────────────────────────────────

fn render_data(buf: &mut PixelBuffer, layout: &DisplayLayout) {
    let data = &layout.data;
    if data.river.is_some() || data.ferry.is_some() {
        vline(buf, DATA_DIVIDER_X, DATA_Y, DATA_H, DIVIDER_H);
    }
    if let Some(river) = &data.river {
        render_river_column(buf, river);
    }
    if let Some(ferry) = &data.ferry {
        render_ferry_column(buf, ferry);
    }
    hline(buf, 0, DATA_Y + DATA_H, 800, DIVIDER_H);
}

fn render_river_column(buf: &mut PixelBuffer, river: &RiverContent) {
    let x = 12u32;
    let mut y = DATA_Y + 4;

    // Site name (Micro)
    draw_text_scaled(buf, x, y, &river.site_name, FontSize::Micro, false, DATA_LEFT_W - 24);
    y += FontSize::Micro.cell_h() + 2;

    // Water level (Large) + trend arrow (Medium)
    let level_str = format!("{:.1} ft", river.level_ft);
    draw_text_scaled(buf, x, y, &level_str, FontSize::Large, false, 240);

    let arrow_x = x + 240 + 8;
    let arrow_y = y + (FontSize::Large.glyph_h().saturating_sub(FontSize::Medium.glyph_h())) / 2;
    let arrow = match river.trend {
        TrendArrow::Rising => Some("^"),
        TrendArrow::Falling => Some("v"),
        TrendArrow::Stable => None,
    };
    if let Some(a) = arrow {
        draw_text_scaled(buf, arrow_x, arrow_y, a, FontSize::Medium, false, 40);
    }
    y += FontSize::Large.cell_h();

    // Flow (Medium)
    let flow_str = format!("{:.0} cfs", river.flow_cfs);
    draw_text_scaled(buf, x, y, &flow_str, FontSize::Medium, false, DATA_LEFT_W - 24);
    y += FontSize::Medium.cell_h() + 4;

    // Sparkline (if available)
    if let Some(spark) = &river.sparkline {
        let spark_x = 8u32;
        let spark_y = y;
        let spark_w = 380u32;
        let spark_h = 22u32;
        render_sparkline(buf, spark_x, spark_y, spark_w, spark_h, spark);
    }
}

fn render_ferry_column(buf: &mut PixelBuffer, ferry: &FerryContent) {
    let x = DATA_RIGHT_X + 12;
    let mut y = DATA_Y + 4;

    // Route name (Micro)
    draw_text_scaled(buf, x, y, &ferry.route, FontSize::Micro, false, DATA_RIGHT_W - 24);
    y += FontSize::Micro.cell_h() + 2;

    // Next departure (Large)
    if let Some(next) = ferry.departures.first() {
        let next_label = format!("Next: {next}");
        draw_text_scaled(buf, x, y, &next_label, FontSize::Large, false, DATA_RIGHT_W - 24);
        y += FontSize::Large.cell_h();
    }

    // Up to 2 more departures (Medium)
    for dep in ferry.departures.iter().skip(1).take(2) {
        draw_text_scaled(buf, x, y, dep, FontSize::Medium, false, DATA_RIGHT_W - 24);
        y += FontSize::Medium.cell_h();
    }

    // Vessel name (Micro, bottom-aligned)
    let vessel_y = DATA_Y + DATA_H - FontSize::Micro.cell_h() - 4;
    draw_text_right(
        buf,
        DATA_RIGHT_X,
        vessel_y,
        &ferry.vessel_name,
        FontSize::Micro,
        DATA_RIGHT_W - 16,
        false,
    );
}

// ── Zone 4: Context ───────────────────────────────────────────────────────────

fn render_context(buf: &mut PixelBuffer, layout: &DisplayLayout) {
    let ctx = &layout.context;
    if ctx.trail.is_some() || ctx.road.is_some() {
        vline(buf, CTX_DIVIDER_X, CONTEXT_Y, CONTEXT_H, DIVIDER_H);
    }
    if let Some(trail) = &ctx.trail {
        let x = 8u32;
        let mut y = CONTEXT_Y + 6;
        // Trail name
        draw_text_scaled(
            buf,
            x,
            y,
            &trail.name.to_uppercase(),
            FontSize::Micro,
            false,
            CTX_LEFT_W - 16,
        );
        y += FontSize::Micro.cell_h() + 4;
        // Condition text (word-wrapped, Small)
        let max_chars =
            ((CTX_LEFT_W - 16) / FontSize::Small.cell_w()) as usize;
        let lines = wrap_text(&trail.condition, max_chars);
        for line in lines.iter().take(3) {
            if y + FontSize::Small.glyph_h() > CONTEXT_Y + CONTEXT_H {
                break;
            }
            draw_text_scaled(buf, x, y, line, FontSize::Small, false, CTX_LEFT_W - 16);
            y += FontSize::Small.glyph_h() + FontSize::Small.line_gap();
        }
    }
    if let Some(road) = &ctx.road {
        let x = CTX_RIGHT_X + 8;
        let mut y = CONTEXT_Y + 6;
        // Road name
        draw_text_scaled(
            buf,
            x,
            y,
            &road.name.to_uppercase(),
            FontSize::Micro,
            false,
            CTX_RIGHT_W - 16,
        );
        y += FontSize::Micro.cell_h() + 4;

        let is_closed = road.status.to_lowercase().contains("closed");
        if is_closed {
            // Inverted "CLOSED" bar
            let bar_w = CTX_RIGHT_W - 16;
            let bar_h = FontSize::Medium.cell_h() + 4;
            fill_rect_xy(buf, x, y, bar_w, bar_h, true);
            draw_text_centered(
                buf,
                x,
                y + 2,
                "CLOSED",
                FontSize::Medium,
                bar_w,
                true,
            );
            y += bar_h + 4;
        } else {
            let status_str = road.status.to_uppercase();
            draw_text_scaled(buf, x, y, &status_str, FontSize::Medium, false, CTX_RIGHT_W - 16);
            y += FontSize::Medium.cell_h() + 4;
        }

        // Affected segment
        let max_chars =
            ((CTX_RIGHT_W - 16) / FontSize::Micro.cell_w()) as usize;
        let lines = wrap_text(&road.segment, max_chars);
        for line in lines.iter().take(2) {
            if y + FontSize::Micro.glyph_h() > CONTEXT_Y + CONTEXT_H {
                break;
            }
            draw_text_scaled(buf, x, y, line, FontSize::Micro, false, CTX_RIGHT_W - 16);
            y += FontSize::Micro.glyph_h() + FontSize::Micro.line_gap();
        }
    }
}

// ── Sparkline ─────────────────────────────────────────────────────────────────

fn render_sparkline(
    buf: &mut PixelBuffer,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    spark: &crate::presentation::Sparkline,
) {
    if spark.values.is_empty() || w < 4 || h < 4 {
        return;
    }

    let n = spark.values.len();
    let min_v = spark.values.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_v = spark.values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let range = (max_v - min_v).max(0.001);

    // Map value to pixel y within [y+1 .. y+h-2] (inner area).
    let px_y = |v: f32| -> u32 {
        let frac = (v - min_v) / range;
        // Invert: high values → small y (top)
        let offset = ((1.0 - frac) * (h - 3) as f32) as u32;
        y + 1 + offset.min(h - 3)
    };

    // Optional flood-threshold dashed line
    if let Some(thresh) = spark.threshold
        && thresh >= min_v && thresh <= max_v
    {
        let ty = px_y(thresh);
        let mut dash = 0u32;
        while dash + 3 < w {
            hline(buf, x + dash, ty, 4.min(w - dash), 2);
            dash += 6;
        }
    }

    // Draw polyline with 2px pen
    let step = (w as f32) / (n - 1).max(1) as f32;
    for i in 1..n {
        let x0 = x + ((i - 1) as f32 * step) as u32;
        let x1 = x + (i as f32 * step) as u32;
        let y0 = px_y(spark.values[i - 1]);
        let y1 = px_y(spark.values[i]);
        draw_line_thick(buf, x0, y0, x1, y1, 2);
    }

    // Mark the current (rightmost) reading with a 4×4 filled square
    let last_x = x + w.saturating_sub(4);
    let last_y = px_y(*spark.values.last().unwrap());
    fill_rect_xy(buf, last_x, last_y.saturating_sub(2), 4, 4, true);
}

/// Draw a thick line from (x0, y0) to (x1, y1) using a `pen` × `pen` square brush.
fn draw_line_thick(buf: &mut PixelBuffer, x0: u32, y0: u32, x1: u32, y1: u32, pen: u32) {
    let dx = (x1 as i32 - x0 as i32).abs();
    let dy = (y1 as i32 - y0 as i32).abs();
    let sx: i32 = if x0 < x1 { 1 } else { -1 };
    let sy: i32 = if y0 < y1 { 1 } else { -1 };
    let mut err = dx - dy;
    let mut cx = x0 as i32;
    let mut cy = y0 as i32;

    loop {
        for py in 0..pen as i32 {
            for px in 0..pen as i32 {
                let fx = cx + px;
                let fy = cy + py;
                if fx >= 0 && fy >= 0 {
                    buf.set_pixel(fx as u32, fy as u32, true);
                }
            }
        }
        if cx == x1 as i32 && cy == y1 as i32 {
            break;
        }
        let e2 = 2 * err;
        if e2 > -dy {
            err -= dy;
            cx += sx;
        }
        if e2 < dx {
            err += dx;
            cy += sy;
        }
    }
}

// ── Weather icons (programmatic 64×64 pixel art) ──────────────────────────────

fn draw_weather_icon(buf: &mut PixelBuffer, x: u32, y: u32, icon: &WeatherIcon) {
    let cx = x as i32 + 32;
    let cy = y as i32 + 32;

    match icon {
        WeatherIcon::Clear => draw_icon_sun(buf, cx, cy),
        WeatherIcon::PartlyCloudy => {
            draw_icon_sun_small(buf, cx - 10, cy - 10);
            draw_icon_cloud(buf, cx + 2, cy + 8, 28, 16);
        }
        WeatherIcon::MostlyCloudy => {
            draw_icon_sun_tiny(buf, cx - 14, cy - 14);
            draw_icon_cloud(buf, cx - 4, cy + 2, 34, 20);
        }
        WeatherIcon::Overcast => {
            draw_icon_cloud(buf, cx, cy, 40, 24);
        }
        WeatherIcon::Rain => {
            draw_icon_cloud(buf, cx, cy - 8, 36, 20);
            draw_rain_drops(buf, x + 10, y + 36, 44, 24, 6);
        }
        WeatherIcon::HeavyRain => {
            draw_icon_cloud(buf, cx, cy - 8, 36, 20);
            draw_rain_drops(buf, x + 6, y + 36, 52, 24, 9);
        }
        WeatherIcon::Drizzle => {
            draw_icon_cloud(buf, cx, cy - 8, 36, 20);
            draw_rain_drops(buf, x + 14, y + 36, 36, 20, 4);
        }
        WeatherIcon::Snow => {
            draw_icon_cloud(buf, cx, cy - 8, 36, 20);
            draw_snow_dots(buf, x + 8, y + 38, 48, 20);
        }
        WeatherIcon::Thunderstorm => {
            draw_icon_cloud(buf, cx, cy - 12, 36, 20);
            draw_lightning(buf, x + 28, y + 32);
        }
        WeatherIcon::Fog => {
            for i in 0..4u32 {
                hline(buf, x + 4, y + 18 + i * 10, 56, 3);
            }
        }
        WeatherIcon::Wind => {
            for i in 0..3u32 {
                hline(buf, x + 4, y + 20 + i * 12, 48, 3);
                hline(buf, x + 52, y + 24 + i * 12, 8, 3);
            }
        }
    }
}

/// Full sun: circle with rays.
fn draw_icon_sun(buf: &mut PixelBuffer, cx: i32, cy: i32) {
    fill_circle(buf, cx, cy, 14);
    // 8 rays
    let offsets: [(i32, i32); 8] = [
        (0, -22), (0, 22), (-22, 0), (22, 0),
        (-16, -16), (16, -16), (-16, 16), (16, 16),
    ];
    for (dx, dy) in offsets {
        fill_rect_xy(
            buf,
            (cx + dx - 1).max(0) as u32,
            (cy + dy - 1).max(0) as u32,
            3,
            3,
            true,
        );
    }
}

/// Smaller sun (for partly cloudy).
fn draw_icon_sun_small(buf: &mut PixelBuffer, cx: i32, cy: i32) {
    fill_circle(buf, cx, cy, 9);
    let offsets: [(i32, i32); 4] = [(0, -16), (0, 16), (-16, 0), (16, 0)];
    for (dx, dy) in offsets {
        fill_rect_xy(
            buf,
            (cx + dx - 1).max(0) as u32,
            (cy + dy - 1).max(0) as u32,
            2,
            2,
            true,
        );
    }
}

/// Tiny sun peeking behind cloud.
fn draw_icon_sun_tiny(buf: &mut PixelBuffer, cx: i32, cy: i32) {
    fill_circle(buf, cx, cy, 7);
}

/// Cloud shape: 3 overlapping circles + rectangle base.
fn draw_icon_cloud(buf: &mut PixelBuffer, cx: i32, cy: i32, half_w: i32, half_h: i32) {
    // Three bumps on top
    fill_circle(buf, cx - half_w / 3, cy - half_h / 2, half_h / 2);
    fill_circle(buf, cx, cy - half_h * 2 / 3, half_h * 2 / 3);
    fill_circle(buf, cx + half_w / 3, cy - half_h / 3, half_h / 2);
    // Rectangular body
    let bx = (cx - half_w).max(0) as u32;
    let by = (cy - half_h / 2).max(0) as u32;
    let bw = (2 * half_w) as u32;
    let bh = (half_h / 2 + 4) as u32;
    fill_rect_xy(buf, bx, by, bw, bh, true);
}

/// Rain drops: short diagonal lines.
fn draw_rain_drops(buf: &mut PixelBuffer, x: u32, y: u32, area_w: u32, area_h: u32, count: u32) {
    let spacing = area_w / count.max(1);
    for i in 0..count {
        let dx = x + i * spacing;
        let offset = (i % 3) * 6;
        let dy = y + offset;
        if dy + 8 <= y + area_h {
            draw_line_thick(buf, dx, dy, dx + 3, dy + 8, 2);
        }
    }
}

/// Snow dots: small filled squares.
fn draw_snow_dots(buf: &mut PixelBuffer, x: u32, y: u32, area_w: u32, area_h: u32) {
    let cols = 5u32;
    let rows = 2u32;
    let col_step = area_w / cols;
    let row_step = area_h / rows;
    for r in 0..rows {
        for c in 0..cols {
            let offset = if r == 0 { 0 } else { col_step / 2 };
            let sx = x + c * col_step + offset;
            let sy = y + r * row_step;
            fill_rect_xy(buf, sx, sy, 4, 4, true);
        }
    }
}

/// Lightning bolt.
fn draw_lightning(buf: &mut PixelBuffer, x: u32, y: u32) {
    // Simple zigzag: top to bottom
    let pts: [(u32, u32); 5] = [(x + 10, y), (x + 4, y + 12), (x + 8, y + 12), (x + 2, y + 24), (x + 12, y + 10)];
    for w in pts.windows(2) {
        draw_line_thick(buf, w[0].0, w[0].1, w[1].0, w[1].1, 3);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point for new 4-zone layout
// ─────────────────────────────────────────────────────────────────────────────

/// Render a [`DisplayLayout`] into a 800×480 1-bit pixel buffer.
pub fn layout_and_render_display(layout: &DisplayLayout, buf: &mut PixelBuffer) {
    render_header(buf, layout);
    render_hero(buf, layout);
    render_data(buf, layout);
    render_context(buf, layout);
}

/// Render a startup/loading screen for the 800×480 e-ink display.
///
/// Shown on first boot while data sources complete their initial fetch.
/// Displays the app name in the header and a centered "Loading data..." message.
pub fn layout_and_render_startup(buf: &mut PixelBuffer) {
    let pad_x = 8u32;
    let pad_y = (HEADER_H.saturating_sub(FontSize::Small.glyph_h())) / 2;

    // Header: app name
    draw_text_scaled(buf, pad_x, HEADER_Y + pad_y, "SKAGIT FLATS", FontSize::Small, false, 400);

    // Header divider
    hline(buf, 0, HEADER_H, 800, DIVIDER_H);

    // Center "Loading data..." in the area below the header
    let body_y = HEADER_H + DIVIDER_H;
    let body_h = 480u32.saturating_sub(body_y);
    let text = "Loading data...";
    let text_w = text.len() as u32 * FontSize::Medium.cell_w();
    let text_h = FontSize::Medium.glyph_h();
    let x = (800u32.saturating_sub(text_w)) / 2;
    let y = body_y + (body_h.saturating_sub(text_h)) / 2;
    draw_text_scaled(buf, x, y, text, FontSize::Medium, false, 800);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_single_panel_fills_screen() {
        let rects = compute_grid(1, 800, 480);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[0].y, 0);
        assert_eq!(rects[0].w, 800);
        assert_eq!(rects[0].h, 480);
    }

    #[test]
    fn grid_two_panels_side_by_side() {
        let rects = compute_grid(2, 800, 480);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].x, 0);
        assert!(rects[1].x > 0);
    }

    #[test]
    fn grid_four_panels_2x2() {
        let rects = compute_grid(4, 800, 480);
        assert_eq!(rects.len(), 4);
        assert_eq!(rects[0].x, rects[2].x);
        assert_eq!(rects[1].x, rects[3].x);
    }

    #[test]
    fn wrap_text_basic() {
        let lines = wrap_text("hello world", 20);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn wrap_text_breaks_at_word_boundary() {
        let lines = wrap_text("hello world foo", 11);
        assert_eq!(lines, vec!["hello world", "foo"]);
    }

    #[test]
    fn wrap_text_long_word() {
        let lines = wrap_text("abcdefghij", 5);
        assert_eq!(lines, vec!["abcde", "fghij"]);
    }

    #[test]
    fn draw_glyph_scaled_sets_pixels() {
        let mut buf = PixelBuffer::new(200, 200);
        draw_glyph_scaled(&mut buf, 0, 0, 'A', FontSize::Medium, false);
        // 'A' glyph should have black pixels in the scaled area
        let has_black = (0..FontSize::Medium.glyph_w())
            .any(|x| (0..FontSize::Medium.glyph_h()).any(|y| buf.get_pixel(x, y)));
        assert!(has_black);
    }

    #[test]
    fn draw_text_scaled_hero_fits_go() {
        let mut buf = PixelBuffer::new(800, 480);
        let w = draw_text_scaled(&mut buf, 0, 0, "GO", FontSize::Hero, false, 490);
        // "GO" = 2 chars × Hero cell_w (112+4=116) = 232px (spacing capped at 4px)
        assert_eq!(w, 2 * FontSize::Hero.cell_w());
    }

    #[test]
    fn new_layout_renders_with_go_decision() {
        use crate::presentation::{
            DataContent, DisplayLayout, HeaderContent, HeroContent, HeroDecision,
            ContextContent,
        };
        let layout = DisplayLayout {
            header: HeaderContent {
                app_name: "SKAGIT FLATS".to_string(),
                river_site: None,
                last_updated: None,
            },
            hero: HeroContent {
                decision: HeroDecision::AllGo,
                weather: None,
            },
            data: DataContent { river: None, ferry: None },
            context: ContextContent { trail: None, road: None },
        };
        let mut buf = PixelBuffer::new(800, 480);
        layout_and_render_display(&layout, &mut buf);
        // Header divider should be visible at y=28–29
        assert!(buf.get_pixel(0, 28));
    }

    #[test]
    fn sparkline_renders_values() {
        use crate::presentation::Sparkline;
        let mut buf = PixelBuffer::new(400, 50);
        let spark = Sparkline {
            values: vec![1.0, 2.0, 1.5, 3.0, 2.5, 1.0, 4.0],
            threshold: None,
        };
        render_sparkline(&mut buf, 5, 5, 380, 22, &spark);
        assert!(buf.pixels.iter().any(|&b| b != 0));
    }

    #[test]
    fn sparkline_with_threshold() {
        use crate::presentation::Sparkline;
        let mut buf = PixelBuffer::new(400, 50);
        let spark = Sparkline {
            values: vec![1.0, 2.0, 3.0, 2.0, 1.0],
            threshold: Some(2.0),
        };
        render_sparkline(&mut buf, 5, 5, 380, 22, &spark);
        assert!(buf.pixels.iter().any(|&b| b != 0));
    }

    #[test]
    fn nogo_bullets_render_within_hero_zone() {
        use crate::presentation::{
            ContextContent, DataContent, DisplayLayout, HeaderContent, HeroContent, HeroDecision,
        };
        // Verify that reason bullets render within the hero zone (y = HERO_Y..HERO_Y+HERO_H).
        // With the line_gap fix, reasons_y is capped at HERO_Y + HERO_H / 2, ensuring
        // all bullets fall inside the zone regardless of scale factor.
        let layout = DisplayLayout {
            header: HeaderContent {
                app_name: "SKAGIT FLATS".to_string(),
                river_site: None,
                last_updated: None,
            },
            hero: HeroContent {
                decision: HeroDecision::NoGo {
                    destination: "Test".to_string(),
                    reasons: vec![
                        "Reason one".to_string(),
                        "Reason two".to_string(),
                        "Reason three".to_string(),
                        "Reason four".to_string(),
                    ],
                },
                weather: None,
            },
            data: DataContent { river: None, ferry: None },
            context: ContextContent { trail: None, road: None },
        };
        let mut buf = PixelBuffer::new(800, 480);
        layout_and_render_display(&layout, &mut buf);

        // Check that there are black pixels in the lower half of the hero zone
        // (where bullets are positioned after the line_gap fix).
        let hero_lower_start = HERO_Y + HERO_H / 2;
        let hero_bottom = HERO_Y + HERO_H;
        let has_bullets = (hero_lower_start..hero_bottom)
            .any(|y| (0..800u32).any(|x| buf.get_pixel(x, y)));
        assert!(has_bullets, "Expected bullet pixels in hero zone lower half (y={}..{})", hero_lower_start, hero_bottom);
    }
}
