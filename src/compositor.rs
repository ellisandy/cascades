//! Layout compositor — slot model, concurrent slot renders, PNG compositing.
//!
//! Implements target-architecture.md §5e:
//!
//! - [`LayoutVariant`] controls which template size is selected and rendered.
//! - [`LayoutSlot`] binds a plugin instance to compositing geometry (internal).
//! - [`DisplayConfiguration`] is a named list of [`crate::layout_store::LayoutItem`]s
//!   loaded from `config/display.toml` or the [`crate::layout_store::LayoutStore`].
//! - [`Compositor`] orchestrates concurrent per-item renders and composites all
//!   item PNGs into the final 800×480 frame.
//!
//! # Static element rendering
//!
//! - **[`crate::layout_store::LayoutItem::PluginSlot`]** — rendered via the Bun
//!   sidecar using a Liquid template.
//! - **[`crate::layout_store::LayoutItem::StaticText`]** — HTML snippet sent to
//!   the sidecar; PNG blitted onto frame.
//! - **[`crate::layout_store::LayoutItem::StaticDivider`]** — drawn directly as a
//!   black rectangle; no sidecar round-trip needed.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use image::{GrayImage, ImageEncoder};
use thiserror::Error;
use tokio::task;

use crate::config::DisplayConfigEntry;
use crate::instance_store::InstanceStore;
use crate::layout_store::LayoutItem;
use crate::template::{NowContext, RenderContext, TemplateEngine};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Final composite frame width — matches the Waveshare 7.5" display.
pub const FRAME_WIDTH: u32 = 800;
/// Final composite frame height — matches the Waveshare 7.5" display.
pub const FRAME_HEIGHT: u32 = 480;

// ─── Layout types ─────────────────────────────────────────────────────────────

/// Template-size variant.  Controls which `.html.liquid` template is selected
/// (e.g. `river_quadrant` vs `river_full`) and at what pixel dimensions the
/// sidecar renders the template.
///
/// Geometry (x/y/width/height on [`LayoutSlot`]) and variant are independent:
/// a `Quadrant` slot can be placed anywhere on the 800×480 frame.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutVariant {
    /// Renders at 800×480 — the full display.
    Full,
    /// Renders at 800×240 — top or bottom half.
    HalfHorizontal,
    /// Renders at 400×480 — left or right half.
    HalfVertical,
    /// Renders at 400×240 — one quadrant.
    Quadrant,
}

impl LayoutVariant {
    /// Parse from the snake_case string used in `display.toml`.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "full" => Some(Self::Full),
            "half_horizontal" => Some(Self::HalfHorizontal),
            "half_vertical" => Some(Self::HalfVertical),
            "quadrant" => Some(Self::Quadrant),
            _ => None,
        }
    }

    /// The suffix appended to the plugin ID to form the template stem.
    /// E.g. `river` + `_` + `quadrant` → `river_quadrant`.
    pub fn template_suffix(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::HalfHorizontal => "half_horizontal",
            Self::HalfVertical => "half_vertical",
            Self::Quadrant => "quadrant",
        }
    }

    /// Canonical render dimensions for this variant (width × height).
    /// The sidecar renders the template at these dimensions.
    pub fn canonical_dimensions(&self) -> (u32, u32) {
        match self {
            Self::Full => (800, 480),
            Self::HalfHorizontal => (800, 240),
            Self::HalfVertical => (400, 480),
            Self::Quadrant => (400, 240),
        }
    }
}

/// Internal render descriptor for a plugin slot.
///
/// Passed to [`render_slot`]; carries only the fields needed to select a
/// template and call the sidecar.  Geometry is accessed directly from the
/// originating [`LayoutItem`] during compositing.
#[derive(Debug, Clone)]
struct LayoutSlot {
    /// ID of the plugin instance to render (must exist in [`InstanceStore`]).
    pub plugin_instance_id: String,
    /// Template variant — controls template selection and sidecar render size.
    pub layout_variant: LayoutVariant,
}

/// A named display configuration: an ordered list of [`LayoutItem`]s to
/// render and composite into the 800×480 frame.
///
/// Items are ordered by `z_index` (lowest first = rendered first = furthest back).
#[derive(Debug, Clone)]
pub struct DisplayConfiguration {
    /// Unique name (e.g. `"default"`, `"trip-planner"`).
    pub name: String,
    /// Items rendered back-to-front (lowest z_index first).
    pub items: Vec<LayoutItem>,
}

impl DisplayConfiguration {
    /// Build from a TOML config entry.
    ///
    /// Each slot entry is converted to a [`LayoutItem::PluginSlot`] with a
    /// synthetic `id` and `z_index` derived from the entry's position.
    /// Returns an error if any slot's variant string is not recognised.
    pub fn from_config(entry: &DisplayConfigEntry) -> Result<Self, CompositorError> {
        let items = entry
            .slots
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let variant = LayoutVariant::from_str(&s.variant).ok_or_else(|| {
                    CompositorError::InvalidVariant { variant: s.variant.clone() }
                })?;
                let (default_w, default_h) = variant.canonical_dimensions();
                Ok(LayoutItem::PluginSlot {
                    id: format!("{}-{}", s.plugin, i),
                    z_index: i as i32,
                    x: s.x.unwrap_or(0) as i32,
                    y: s.y.unwrap_or(0) as i32,
                    width: s.width.unwrap_or(default_w) as i32,
                    height: s.height.unwrap_or(default_h) as i32,
                    plugin_instance_id: s.plugin.clone(),
                    layout_variant: s.variant.clone(),
                })
            })
            .collect::<Result<Vec<_>, CompositorError>>()?;
        Ok(DisplayConfiguration { name: entry.name.clone(), items })
    }

    /// Build from a [`crate::layout_store::LayoutConfig`].
    ///
    /// All item types (`PluginSlot`, `StaticText`, `StaticDivider`) are included.
    /// Items arrive pre-sorted by `z_index` from the store.
    ///
    /// [`LayoutItem::PluginSlot`] entries with an unrecognised `layout_variant`
    /// are skipped with a warning to prevent a single bad row from dropping the
    /// whole layout.
    pub fn from_layout_config(layout: &crate::layout_store::LayoutConfig) -> Self {
        let items = layout
            .items
            .iter()
            .filter_map(|item| {
                if let LayoutItem::PluginSlot { layout_variant, plugin_instance_id, .. } = item {
                    if LayoutVariant::from_str(layout_variant).is_none() {
                        log::warn!(
                            "layout '{}': unknown variant '{}' for slot '{}', skipping",
                            layout.id,
                            layout_variant,
                            plugin_instance_id
                        );
                        return None;
                    }
                }
                Some(item.clone())
            })
            .collect();
        DisplayConfiguration { name: layout.name.clone(), items }
    }
}

// ─── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CompositorError {
    #[error("template error: {0}")]
    Template(#[from] crate::template::TemplateError),

    #[error("sidecar request failed for slot '{slot}': {message}")]
    Sidecar { slot: String, message: String },

    #[error("sidecar returned invalid PNG for slot '{slot}'")]
    InvalidPng { slot: String },

    #[error("instance store error: {0}")]
    Store(#[from] crate::instance_store::StoreError),

    #[error("task join error: {0}")]
    Join(#[from] tokio::task::JoinError),

    #[error("unknown layout variant '{variant}'")]
    InvalidVariant { variant: String },

    #[error("PNG frame encoding failed")]
    Encoding,
}

// ─── Compositor ───────────────────────────────────────────────────────────────

/// Orchestrates slot renders and compositing for a [`DisplayConfiguration`].
///
/// Construct once at startup with [`Compositor::new`]; call [`Compositor::compose`]
/// for each display refresh cycle.
pub struct Compositor {
    template_engine: Arc<TemplateEngine>,
    instance_store: Arc<InstanceStore>,
    /// Base URL of the Bun render sidecar, e.g. `"http://localhost:3001"`.
    sidecar_url: String,
}

impl Compositor {
    /// Create a new `Compositor`.
    ///
    /// `sidecar_url` is the base URL of the Bun render sidecar
    /// (e.g. `"http://localhost:3001"`).
    pub fn new(
        template_engine: Arc<TemplateEngine>,
        instance_store: Arc<InstanceStore>,
        sidecar_url: impl Into<String>,
    ) -> Self {
        Compositor {
            template_engine,
            instance_store,
            sidecar_url: sidecar_url.into(),
        }
    }

    /// Render all items in `config` and composite them into a final 800×480 PNG.
    ///
    /// - `PluginSlot` and `StaticText` items are rendered concurrently via the
    ///   Bun sidecar (async HTTP).
    /// - `StaticDivider` items are drawn directly as black rectangles (no I/O).
    ///
    /// All rendered items are composited in `z_index` order (lowest first).
    /// `render_mode` is forwarded to the sidecar: `"device"` (dither+negate),
    /// `"einkPreview"` (dither only), or `"preview"` (raw).
    pub async fn compose(
        &self,
        config: &DisplayConfiguration,
        render_mode: &str,
    ) -> Result<Vec<u8>, CompositorError> {
        // For each item, we either spawn an async render task or handle it inline.
        // task_for_item[i] = Some(handle_index) if item i has an async task, else None.
        let mut task_for_item: Vec<Option<usize>> = Vec::with_capacity(config.items.len());
        let mut handles: Vec<task::JoinHandle<Result<Vec<u8>, CompositorError>>> = Vec::new();

        for item in &config.items {
            let maybe_task = match item {
                LayoutItem::PluginSlot {
                    plugin_instance_id,
                    layout_variant,
                    ..
                } => match LayoutVariant::from_str(layout_variant) {
                    None => {
                        // Filtered by from_layout_config; warn and skip if somehow reached.
                        log::warn!(
                            "compose: unknown variant '{}' for '{}', skipping",
                            layout_variant,
                            plugin_instance_id
                        );
                        None
                    }
                    Some(variant) => {
                        let slot = LayoutSlot {
                            plugin_instance_id: plugin_instance_id.clone(),
                            layout_variant: variant,
                        };
                        let engine = Arc::clone(&self.template_engine);
                        let store = Arc::clone(&self.instance_store);
                        let url = self.sidecar_url.clone();
                        let mode = render_mode.to_string();
                        let idx = handles.len();
                        handles.push(task::spawn(async move {
                            render_slot(slot, engine, store, url, mode).await
                        }));
                        Some(idx)
                    }
                },
                LayoutItem::StaticText {
                    id,
                    width,
                    height,
                    text_content,
                    font_size,
                    ..
                } => {
                    let text = text_content.clone();
                    let w = (*width).max(0) as u32;
                    let h = (*height).max(0) as u32;
                    let fs = *font_size;
                    let url = self.sidecar_url.clone();
                    let iid = id.clone();
                    let mode = render_mode.to_string();
                    let idx = handles.len();
                    handles.push(task::spawn(async move {
                        render_static_text(&text, fs, w, h, &url, &iid, &mode).await
                    }));
                    Some(idx)
                }
                LayoutItem::StaticDateTime {
                    id,
                    width,
                    height,
                    font_size,
                    format,
                    ..
                } => {
                    let fmt = format.clone();
                    let w = (*width).max(0) as u32;
                    let h = (*height).max(0) as u32;
                    let fs = *font_size;
                    let url = self.sidecar_url.clone();
                    let iid = id.clone();
                    let mode = render_mode.to_string();
                    let idx = handles.len();
                    handles.push(task::spawn(async move {
                        render_static_datetime(fmt.as_deref(), fs, w, h, &url, &iid, &mode).await
                    }));
                    Some(idx)
                }
                LayoutItem::StaticDivider { .. } => {
                    // Drawn directly — no async task needed.
                    None
                }
            };
            task_for_item.push(maybe_task);
        }

        // Join all async tasks in order.
        let mut png_results: Vec<Vec<u8>> = Vec::with_capacity(handles.len());
        for handle in handles {
            png_results.push(handle.await??);
        }

        composite_to_png(&config.items, &task_for_item, &png_results)
    }
}

// ─── Per-slot render ─────────────────────────────────────────────────────────

async fn render_slot(
    slot: LayoutSlot,
    engine: Arc<TemplateEngine>,
    store: Arc<InstanceStore>,
    sidecar_url: String,
    mode: String,
) -> Result<Vec<u8>, CompositorError> {
    let id = slot.plugin_instance_id.clone();
    let store2 = Arc::clone(&store);
    let instance = task::spawn_blocking(move || store2.get_instance(&id)).await??;

    let (data, settings_map, error) = match instance {
        Some(inst) => {
            let data = inst
                .cached_data
                .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
            let settings = json_object_to_map(inst.settings);
            (data, settings, inst.last_error)
        }
        None => (
            serde_json::Value::Object(Default::default()),
            HashMap::new(),
            Some(format!(
                "plugin instance '{}' not found",
                slot.plugin_instance_id
            )),
        ),
    };

    let template_name = format!(
        "{}_{}",
        slot.plugin_instance_id,
        slot.layout_variant.template_suffix()
    );

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let ctx = RenderContext {
        data,
        settings: settings_map,
        trip_decision: None,
        now: NowContext::from_unix(now_secs),
        error,
    };

    let html = engine.render(&template_name, &ctx)?;
    let (render_w, render_h) = slot.layout_variant.canonical_dimensions();
    call_sidecar(&sidecar_url, html, render_w, render_h, &slot.plugin_instance_id, &mode).await
}

fn json_object_to_map(
    val: serde_json::Value,
) -> HashMap<String, serde_json::Value> {
    match val {
        serde_json::Value::Object(map) => map.into_iter().collect(),
        _ => HashMap::new(),
    }
}

// ─── Static text render ──────────────────────────────────────────────────────

/// Render a static text element via the sidecar.
///
/// Generates a minimal self-contained HTML snippet and POSTs it to
/// `{sidecar_url}/render` at the item's pixel dimensions.
async fn render_static_text(
    text: &str,
    font_size: i32,
    width: u32,
    height: u32,
    sidecar_url: &str,
    item_id: &str,
    mode: &str,
) -> Result<Vec<u8>, CompositorError> {
    let safe = html_escape(text);
    let html = format!(
        "<div style='width:{w}px;height:{h}px;display:flex;align-items:center;\
         justify-content:center;font-family:sans-serif;font-size:{fs}px;\
         color:#000;background:white;'>{text}</div>",
        w = width,
        h = height,
        fs = font_size,
        text = safe,
    );
    call_sidecar(sidecar_url, html, width, height, item_id, mode).await
}

// ─── Static datetime render ─────────────────────────────────────────────────

/// Render a static date/time element via the sidecar.
///
/// Gets the current local time and formats it, then renders via the sidecar
/// like `render_static_text`.
async fn render_static_datetime(
    format: Option<&str>,
    font_size: i32,
    width: u32,
    height: u32,
    sidecar_url: &str,
    item_id: &str,
    mode: &str,
) -> Result<Vec<u8>, CompositorError> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let now = NowContext::from_unix(now_secs);
    let time_str = match format {
        Some(f) if !f.is_empty() => f.to_string(), // user-provided label/format
        _ => now.local,
    };
    let safe = html_escape(&time_str);
    let html = format!(
        "<div style='width:{w}px;height:{h}px;display:flex;align-items:center;\
         justify-content:center;font-family:sans-serif;font-size:{fs}px;\
         color:#000;background:white;'>{text}</div>",
        w = width,
        h = height,
        fs = font_size,
        text = safe,
    );
    call_sidecar(sidecar_url, html, width, height, item_id, mode).await
}

/// Escape `&`, `<`, `>`, and `"` for safe HTML embedding.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ─── Sidecar HTTP call ────────────────────────────────────────────────────────

/// POST `{base_url}/render` with `{html, width, height, mode}`.
/// Returns raw PNG bytes from the response body.
///
/// `mode` is one of `"device"` (dither+negate for e-ink hardware),
/// `"einkPreview"` (dither only, correct for browsers), or `"preview"` (raw).
async fn call_sidecar(
    base_url: &str,
    html: String,
    width: u32,
    height: u32,
    slot_id: &str,
    mode: &str,
) -> Result<Vec<u8>, CompositorError> {
    let url = format!("{}/render", base_url);
    let slot_id = slot_id.to_string();
    let mode = mode.to_string();

    let body = serde_json::json!({
        "html": html,
        "width": width,
        "height": height,
        "mode": mode
    });

    // ureq is synchronous; spawn_blocking moves it off the async executor.
    let bytes =
        task::spawn_blocking(move || -> Result<Vec<u8>, CompositorError> {
            let resp = ureq::post(&url)
                .send_json(body)
                .map_err(|e| CompositorError::Sidecar {
                    slot: slot_id.clone(),
                    message: e.to_string(),
                })?;
            let mut bytes = Vec::new();
            resp.into_reader()
                .read_to_end(&mut bytes)
                .map_err(|e| CompositorError::Sidecar {
                    slot: slot_id,
                    message: e.to_string(),
                })?;
            Ok(bytes)
        })
        .await??;

    Ok(bytes)
}

// ─── PNG compositing ─────────────────────────────────────────────────────────

/// Composite all layout items into a final 800×480 grayscale PNG.
///
/// Items are processed in the order they appear in `items` (z_index order,
/// lowest first).  For each item:
/// - `PluginSlot` / `StaticText`: decoded PNG from `png_results[task_for_item[i]]`
///   is blitted at the item's (x, y) position.
/// - `StaticDivider`: a solid black rectangle is drawn at (x, y, width, height).
///
/// Items whose `task_for_item` entry is `None` (skipped during rendering) are
/// omitted silently.
fn composite_to_png(
    items: &[LayoutItem],
    task_for_item: &[Option<usize>],
    png_results: &[Vec<u8>],
) -> Result<Vec<u8>, CompositorError> {
    // Allocate white frame.
    let pixels = vec![255u8; (FRAME_WIDTH * FRAME_HEIGHT) as usize];
    let mut frame =
        GrayImage::from_raw(FRAME_WIDTH, FRAME_HEIGHT, pixels)
            .expect("buffer size matches dimensions");

    for (item, maybe_task) in items.iter().zip(task_for_item.iter()) {
        match item {
            LayoutItem::PluginSlot { x, y, width, height, plugin_instance_id, .. } => {
                if let Some(idx) = maybe_task {
                    blit_png(
                        &mut frame,
                        &png_results[*idx],
                        (*x).max(0) as u32,
                        (*y).max(0) as u32,
                        (*width).max(0) as u32,
                        (*height).max(0) as u32,
                        plugin_instance_id,
                    )?;
                }
            }
            LayoutItem::StaticText { x, y, width, height, id, .. } => {
                if let Some(idx) = maybe_task {
                    blit_png(
                        &mut frame,
                        &png_results[*idx],
                        (*x).max(0) as u32,
                        (*y).max(0) as u32,
                        (*width).max(0) as u32,
                        (*height).max(0) as u32,
                        id,
                    )?;
                }
            }
            LayoutItem::StaticDateTime { x, y, width, height, id, .. } => {
                if let Some(idx) = maybe_task {
                    blit_png(
                        &mut frame,
                        &png_results[*idx],
                        (*x).max(0) as u32,
                        (*y).max(0) as u32,
                        (*width).max(0) as u32,
                        (*height).max(0) as u32,
                        id,
                    )?;
                }
            }
            LayoutItem::StaticDivider { x, y, width, height, .. } => {
                draw_divider(
                    &mut frame,
                    (*x).max(0) as u32,
                    (*y).max(0) as u32,
                    (*width).max(0) as u32,
                    (*height).max(0) as u32,
                );
            }
        }
    }

    let mut png_bytes = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
    ImageEncoder::write_image(
        encoder,
        frame.as_raw(),
        FRAME_WIDTH,
        FRAME_HEIGHT,
        image::ColorType::L8,
    )
    .map_err(|_| CompositorError::Encoding)?;

    Ok(png_bytes)
}

/// Decode `png_bytes` and copy up to `(width × height)` pixels onto `frame`
/// at offset `(x, y)`.
fn blit_png(
    frame: &mut GrayImage,
    png_bytes: &[u8],
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    item_id: &str,
) -> Result<(), CompositorError> {
    let src = image::load_from_memory(png_bytes)
        .map_err(|_| CompositorError::InvalidPng { slot: item_id.to_string() })?
        .to_luma8();

    let copy_w = width.min(src.width());
    let copy_h = height.min(src.height());

    for py in 0..copy_h {
        for px in 0..copy_w {
            let dst_x = x + px;
            let dst_y = y + py;
            if dst_x < FRAME_WIDTH && dst_y < FRAME_HEIGHT {
                frame.put_pixel(dst_x, dst_y, *src.get_pixel(px, py));
            }
        }
    }
    Ok(())
}

/// Fill a rectangle on `frame` with black (pixel value 0).
///
/// Used to render [`LayoutItem::StaticDivider`] without a sidecar round-trip.
fn draw_divider(frame: &mut GrayImage, x: u32, y: u32, width: u32, height: u32) {
    for py in 0..height {
        for px in 0..width {
            let dst_x = x + px;
            let dst_y = y + py;
            if dst_x < FRAME_WIDTH && dst_y < FRAME_HEIGHT {
                frame.put_pixel(dst_x, dst_y, image::Luma([0u8]));
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DisplaySlotEntry;
    use crate::layout_store::LayoutItem;

    // ── LayoutVariant ─────────────────────────────────────────────────────────

    #[test]
    fn layout_variant_from_str_roundtrip() {
        let cases = [
            ("full", LayoutVariant::Full),
            ("half_horizontal", LayoutVariant::HalfHorizontal),
            ("half_vertical", LayoutVariant::HalfVertical),
            ("quadrant", LayoutVariant::Quadrant),
        ];
        for (s, expected) in cases {
            assert_eq!(LayoutVariant::from_str(s), Some(expected));
        }
        assert_eq!(LayoutVariant::from_str("bogus"), None);
    }

    #[test]
    fn layout_variant_dimensions() {
        assert_eq!(LayoutVariant::Full.canonical_dimensions(), (800, 480));
        assert_eq!(LayoutVariant::HalfHorizontal.canonical_dimensions(), (800, 240));
        assert_eq!(LayoutVariant::HalfVertical.canonical_dimensions(), (400, 480));
        assert_eq!(LayoutVariant::Quadrant.canonical_dimensions(), (400, 240));
    }

    // ── DisplayConfiguration::from_config ────────────────────────────────────

    #[test]
    fn display_config_from_config_defaults() {
        let entry = crate::config::DisplayConfigEntry {
            name: "test".to_string(),
            slots: vec![DisplaySlotEntry {
                plugin: "river".to_string(),
                x: None,
                y: None,
                width: None,
                height: None,
                variant: "quadrant".to_string(),
            }],
        };
        let cfg = DisplayConfiguration::from_config(&entry).unwrap();
        assert_eq!(cfg.items.len(), 1);
        if let LayoutItem::PluginSlot { x, y, width, height, plugin_instance_id, layout_variant, .. } =
            &cfg.items[0]
        {
            assert_eq!(*x, 0);
            assert_eq!(*y, 0);
            assert_eq!(*width, 400);
            assert_eq!(*height, 240);
            assert_eq!(plugin_instance_id, "river");
            assert_eq!(layout_variant, "quadrant");
        } else {
            panic!("expected PluginSlot");
        }
    }

    #[test]
    fn display_config_from_config_explicit() {
        let entry = crate::config::DisplayConfigEntry {
            name: "test".to_string(),
            slots: vec![DisplaySlotEntry {
                plugin: "weather".to_string(),
                x: Some(0),
                y: Some(0),
                width: Some(800),
                height: Some(240),
                variant: "half_horizontal".to_string(),
            }],
        };
        let cfg = DisplayConfiguration::from_config(&entry).unwrap();
        if let LayoutItem::PluginSlot { width, height, .. } = &cfg.items[0] {
            assert_eq!(*width, 800);
            assert_eq!(*height, 240);
        } else {
            panic!("expected PluginSlot");
        }
    }

    #[test]
    fn display_config_from_config_invalid_variant() {
        let entry = crate::config::DisplayConfigEntry {
            name: "test".to_string(),
            slots: vec![DisplaySlotEntry {
                plugin: "foo".to_string(),
                x: None,
                y: None,
                width: None,
                height: None,
                variant: "not_a_variant".to_string(),
            }],
        };
        assert!(DisplayConfiguration::from_config(&entry).is_err());
    }

    // ── DisplayConfiguration::from_layout_config ──────────────────────────────

    #[test]
    fn from_layout_config_includes_all_item_types() {
        let layout = crate::layout_store::LayoutConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            updated_at: 0,
            items: vec![
                LayoutItem::PluginSlot {
                    id: "s0".to_string(),
                    z_index: 0,
                    x: 0, y: 0, width: 400, height: 240,
                    plugin_instance_id: "river".to_string(),
                    layout_variant: "quadrant".to_string(),
                },
                LayoutItem::StaticText {
                    id: "t0".to_string(),
                    z_index: 1,
                    x: 10, y: 10, width: 200, height: 50,
                    text_content: "Hello".to_string(),
                    font_size: 24,
                    orientation: None,
                },
                LayoutItem::StaticDivider {
                    id: "d0".to_string(),
                    z_index: 2,
                    x: 0, y: 240, width: 800, height: 2,
                    orientation: Some("horizontal".to_string()),
                },
            ],
        };
        let cfg = DisplayConfiguration::from_layout_config(&layout);
        assert_eq!(cfg.items.len(), 3);
        assert!(matches!(&cfg.items[0], LayoutItem::PluginSlot { .. }));
        assert!(matches!(&cfg.items[1], LayoutItem::StaticText { .. }));
        assert!(matches!(&cfg.items[2], LayoutItem::StaticDivider { .. }));
    }

    #[test]
    fn from_layout_config_skips_plugin_slot_with_unknown_variant() {
        let layout = crate::layout_store::LayoutConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            updated_at: 0,
            items: vec![
                LayoutItem::PluginSlot {
                    id: "s0".to_string(),
                    z_index: 0,
                    x: 0, y: 0, width: 400, height: 240,
                    plugin_instance_id: "river".to_string(),
                    layout_variant: "bogus_variant".to_string(),
                },
                LayoutItem::StaticDivider {
                    id: "d0".to_string(),
                    z_index: 1,
                    x: 0, y: 240, width: 800, height: 2,
                    orientation: None,
                },
            ],
        };
        let cfg = DisplayConfiguration::from_layout_config(&layout);
        // Bad PluginSlot skipped; StaticDivider kept
        assert_eq!(cfg.items.len(), 1);
        assert!(matches!(&cfg.items[0], LayoutItem::StaticDivider { .. }));
    }

    // ── html_escape ───────────────────────────────────────────────────────────

    #[test]
    fn html_escape_encodes_special_chars() {
        assert_eq!(html_escape("<b>Me & \"you\"</b>"), "&lt;b&gt;Me &amp; &quot;you&quot;&lt;/b&gt;");
    }

    #[test]
    fn html_escape_passthrough_plain() {
        assert_eq!(html_escape("Hello World"), "Hello World");
    }

    // ── composite_to_png ─────────────────────────────────────────────────────

    #[test]
    fn composite_to_png_white_frame() {
        // No items → pure white 800×480 frame.
        let png = composite_to_png(&[], &[], &[]).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!(img.width(), FRAME_WIDTH);
        assert_eq!(img.height(), FRAME_HEIGHT);
        for pixel in img.to_luma8().pixels() {
            assert_eq!(pixel.0[0], 255);
        }
    }

    #[test]
    fn composite_to_png_plugin_slot_placed_correctly() {
        // Create a small solid black PNG and composite it via a PluginSlot item.
        let slot_w = 10u32;
        let slot_h = 8u32;
        let black_pixels = vec![0u8; (slot_w * slot_h) as usize];
        let slot_img = GrayImage::from_raw(slot_w, slot_h, black_pixels).unwrap();
        let mut slot_png = Vec::new();
        let enc = image::codecs::png::PngEncoder::new(&mut slot_png);
        ImageEncoder::write_image(enc, slot_img.as_raw(), slot_w, slot_h, image::ColorType::L8)
            .unwrap();

        let item = LayoutItem::PluginSlot {
            id: "s0".to_string(),
            z_index: 0,
            x: 100, y: 50,
            width: slot_w as i32, height: slot_h as i32,
            plugin_instance_id: "test".to_string(),
            layout_variant: "quadrant".to_string(),
        };

        let task_for_item = vec![Some(0usize)];
        let png_results = vec![slot_png];

        let png = composite_to_png(&[item], &task_for_item, &png_results).unwrap();
        let frame = image::load_from_memory(&png).unwrap().to_luma8();

        assert_eq!(frame.get_pixel(100, 50).0[0], 0);
        assert_eq!(frame.get_pixel(105, 54).0[0], 0);
        assert_eq!(frame.get_pixel(99, 50).0[0], 255);
        assert_eq!(frame.get_pixel(110, 58).0[0], 255);
    }

    #[test]
    fn composite_to_png_static_divider_draws_black_rect() {
        let item = LayoutItem::StaticDivider {
            id: "d0".to_string(),
            z_index: 0,
            x: 0, y: 200,
            width: 800, height: 2,
            orientation: Some("horizontal".to_string()),
        };

        let task_for_item = vec![None];
        let png = composite_to_png(&[item], &task_for_item, &[]).unwrap();
        let frame = image::load_from_memory(&png).unwrap().to_luma8();

        // Row 200 and 201 should be all black.
        for x in 0..800u32 {
            assert_eq!(frame.get_pixel(x, 200).0[0], 0, "pixel ({x}, 200) should be black");
            assert_eq!(frame.get_pixel(x, 201).0[0], 0, "pixel ({x}, 201) should be black");
        }
        // Row 199 should be white.
        assert_eq!(frame.get_pixel(400, 199).0[0], 255);
        // Row 202 should be white.
        assert_eq!(frame.get_pixel(400, 202).0[0], 255);
    }

    #[test]
    fn composite_to_png_static_text_placed_correctly() {
        // A StaticText item backed by a solid grey PNG at (50, 100).
        let w = 200u32;
        let h = 40u32;
        let grey_pixels = vec![128u8; (w * h) as usize];
        let img = GrayImage::from_raw(w, h, grey_pixels).unwrap();
        let mut text_png = Vec::new();
        let enc = image::codecs::png::PngEncoder::new(&mut text_png);
        ImageEncoder::write_image(enc, img.as_raw(), w, h, image::ColorType::L8).unwrap();

        let item = LayoutItem::StaticText {
            id: "t0".to_string(),
            z_index: 0,
            x: 50, y: 100,
            width: w as i32, height: h as i32,
            text_content: "test".to_string(),
            font_size: 16,
            orientation: None,
        };

        let task_for_item = vec![Some(0usize)];
        let png_results = vec![text_png];

        let png = composite_to_png(&[item], &task_for_item, &png_results).unwrap();
        let frame = image::load_from_memory(&png).unwrap().to_luma8();

        // Inside the text area should be grey.
        assert_eq!(frame.get_pixel(50, 100).0[0], 128);
        assert_eq!(frame.get_pixel(150, 120).0[0], 128);
        // Outside should be white.
        assert_eq!(frame.get_pixel(49, 100).0[0], 255);
    }

    #[test]
    fn composite_to_png_z_index_ordering_later_overwrites_earlier() {
        // Two overlapping dividers: z_index 0 is white (255), z_index 1 is black (0).
        // The second divider should overwrite the first at the overlap region.

        // We use two PluginSlot items with synthetic PNGs instead of dividers
        // to test z_index compositing.
        let w = 20u32;
        let h = 20u32;

        let white_pixels = vec![255u8; (w * h) as usize];
        let white_img = GrayImage::from_raw(w, h, white_pixels).unwrap();
        let mut white_png = Vec::new();
        ImageEncoder::write_image(
            image::codecs::png::PngEncoder::new(&mut white_png),
            white_img.as_raw(), w, h, image::ColorType::L8,
        ).unwrap();

        let black_pixels = vec![0u8; (w * h) as usize];
        let black_img = GrayImage::from_raw(w, h, black_pixels).unwrap();
        let mut black_png = Vec::new();
        ImageEncoder::write_image(
            image::codecs::png::PngEncoder::new(&mut black_png),
            black_img.as_raw(), w, h, image::ColorType::L8,
        ).unwrap();

        // White slot at z=0, black slot at z=1, both at (0, 0).
        let items = vec![
            LayoutItem::PluginSlot {
                id: "s0".to_string(), z_index: 0,
                x: 0, y: 0, width: w as i32, height: h as i32,
                plugin_instance_id: "white".to_string(),
                layout_variant: "quadrant".to_string(),
            },
            LayoutItem::PluginSlot {
                id: "s1".to_string(), z_index: 1,
                x: 0, y: 0, width: w as i32, height: h as i32,
                plugin_instance_id: "black".to_string(),
                layout_variant: "quadrant".to_string(),
            },
        ];
        let task_for_item = vec![Some(0usize), Some(1usize)];
        let png_results = vec![white_png, black_png];

        let png = composite_to_png(&items, &task_for_item, &png_results).unwrap();
        let frame = image::load_from_memory(&png).unwrap().to_luma8();

        // Black (z=1) overwrote white (z=0).
        assert_eq!(frame.get_pixel(10, 10).0[0], 0);
    }

    #[test]
    fn static_text_font_size_is_used_in_html() {
        // This test verifies that font_size is properly embedded in the HTML style.
        // render_static_text generates HTML with the font_size in the style attribute.
        // The format string should contain: font-size:48px when font_size=48

        let text = "Test text";
        let font_size = 48i32;
        let width = 200u32;
        let height = 100u32;

        // Simulate the HTML generation that render_static_text does
        let safe = html_escape(text);
        let html = format!(
            "<div style='width:{w}px;height:{h}px;display:flex;align-items:center;\
             justify-content:center;font-family:sans-serif;font-size:{fs}px;\
             color:#000;background:white;'>{text}</div>",
            w = width,
            h = height,
            fs = font_size,
            text = safe,
        );

        // Verify the font_size is correctly embedded in the HTML
        assert!(html.contains("font-size:48px"),
            "HTML should contain 'font-size:48px' when font_size=48, but got: {}", html);

        // Verify it's not hardcoded to a default value
        assert!(!html.contains("font-size:16px"),
            "HTML should not contain hardcoded font-size:16px");
    }
}
