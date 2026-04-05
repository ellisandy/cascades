//! Layout compositor — slot model, concurrent slot renders, PNG compositing.
//!
//! Implements target-architecture.md §5e:
//!
//! - [`LayoutVariant`] controls which template size is selected and rendered.
//! - [`LayoutSlot`] binds a plugin instance to compositing geometry.
//! - [`DisplayConfiguration`] is a named list of slots loaded from
//!   `config/display.toml`.
//! - [`Compositor`] orchestrates concurrent per-slot renders and composites
//!   all slot PNGs into the final 800×480 frame.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use image::{GrayImage, ImageEncoder};
use thiserror::Error;
use tokio::task;

use crate::config::{DisplayConfigEntry, DisplaySlotEntry};
use crate::instance_store::InstanceStore;
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

/// A single compositing slot: binds a plugin instance to a position in the
/// 800×480 frame and selects a template variant for rendering.
#[derive(Debug, Clone)]
pub struct LayoutSlot {
    /// ID of the plugin instance to render (must exist in [`InstanceStore`]).
    pub plugin_instance_id: String,
    /// X offset in the final 800×480 frame.
    pub x: u32,
    /// Y offset in the final 800×480 frame.
    pub y: u32,
    /// Width of this slot in the frame (pixels copied from slot PNG).
    pub width: u32,
    /// Height of this slot in the frame (pixels copied from slot PNG).
    pub height: u32,
    /// Template variant — controls template selection and sidecar render size.
    pub layout_variant: LayoutVariant,
}

impl LayoutSlot {
    /// Build from a TOML config entry.  Missing `x`/`y` default to 0;
    /// missing `width`/`height` default to the variant's canonical dimensions.
    /// Returns `None` if the variant string is not recognised.
    pub fn from_config(entry: &DisplaySlotEntry) -> Option<Self> {
        let variant = LayoutVariant::from_str(&entry.variant)?;
        let (default_w, default_h) = variant.canonical_dimensions();
        Some(LayoutSlot {
            plugin_instance_id: entry.plugin.clone(),
            x: entry.x.unwrap_or(0),
            y: entry.y.unwrap_or(0),
            width: entry.width.unwrap_or(default_w),
            height: entry.height.unwrap_or(default_h),
            layout_variant: variant,
        })
    }
}

/// A named display configuration: an ordered list of [`LayoutSlot`]s to
/// render and composite into the 800×480 frame.
#[derive(Debug, Clone)]
pub struct DisplayConfiguration {
    /// Unique name (e.g. `"default"`, `"trip-planner"`).
    pub name: String,
    /// Slots rendered in order; later slots are composited on top.
    pub slots: Vec<LayoutSlot>,
}

impl DisplayConfiguration {
    /// Build from a TOML config entry.
    pub fn from_config(entry: &DisplayConfigEntry) -> Result<Self, CompositorError> {
        let slots = entry
            .slots
            .iter()
            .map(|s| {
                LayoutSlot::from_config(s).ok_or_else(|| CompositorError::InvalidVariant {
                    variant: s.variant.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DisplayConfiguration {
            name: entry.name.clone(),
            slots,
        })
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

    /// Render all slots in `config` concurrently, then composite them into a
    /// final 800×480 PNG.
    ///
    /// Each slot is rendered in a separate Tokio task.  All tasks are joined
    /// before compositing begins.
    pub async fn compose(
        &self,
        config: &DisplayConfiguration,
    ) -> Result<Vec<u8>, CompositorError> {
        // Spawn one task per slot — concurrent renders.
        let handles: Vec<_> = config
            .slots
            .iter()
            .map(|slot| {
                let slot = slot.clone();
                let engine = Arc::clone(&self.template_engine);
                let store = Arc::clone(&self.instance_store);
                let url = self.sidecar_url.clone();
                task::spawn(async move { render_slot(slot, engine, store, url).await })
            })
            .collect();

        // Join all tasks and collect (slot, png) pairs in original order.
        let mut rendered: Vec<(LayoutSlot, Vec<u8>)> =
            Vec::with_capacity(config.slots.len());
        for (handle, slot) in handles.into_iter().zip(config.slots.iter()) {
            let png = handle.await??;
            rendered.push((slot.clone(), png));
        }

        composite_to_png(rendered)
    }
}

// ─── Per-slot render ─────────────────────────────────────────────────────────

async fn render_slot(
    slot: LayoutSlot,
    engine: Arc<TemplateEngine>,
    store: Arc<InstanceStore>,
    sidecar_url: String,
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
    call_sidecar(&sidecar_url, html, render_w, render_h, &slot.plugin_instance_id).await
}

fn json_object_to_map(
    val: serde_json::Value,
) -> HashMap<String, serde_json::Value> {
    match val {
        serde_json::Value::Object(map) => map.into_iter().collect(),
        _ => HashMap::new(),
    }
}

// ─── Sidecar HTTP call ────────────────────────────────────────────────────────

/// POST `{base_url}/render` with `{html, width, height, mode: "device"}`.
/// Returns raw PNG bytes from the response body.
async fn call_sidecar(
    base_url: &str,
    html: String,
    width: u32,
    height: u32,
    slot_id: &str,
) -> Result<Vec<u8>, CompositorError> {
    let url = format!("{}/render", base_url);
    let slot_id = slot_id.to_string();

    let body = serde_json::json!({
        "html": html,
        "width": width,
        "height": height,
        "mode": "device"
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

/// Composite rendered slot PNGs into a final 800×480 grayscale PNG.
///
/// Slots are blitted in order; later slots overwrite earlier ones at
/// overlapping pixels.  The frame is initialised to white (255).
fn composite_to_png(
    slot_pngs: Vec<(LayoutSlot, Vec<u8>)>,
) -> Result<Vec<u8>, CompositorError> {
    // Allocate white frame.
    let pixels = vec![255u8; (FRAME_WIDTH * FRAME_HEIGHT) as usize];
    let mut frame =
        GrayImage::from_raw(FRAME_WIDTH, FRAME_HEIGHT, pixels)
            .expect("buffer size matches dimensions");

    for (slot, png_bytes) in slot_pngs {
        let slot_img = image::load_from_memory(&png_bytes)
            .map_err(|_| CompositorError::InvalidPng {
                slot: slot.plugin_instance_id.clone(),
            })?
            .to_luma8();

        let copy_w = slot.width.min(slot_img.width());
        let copy_h = slot.height.min(slot_img.height());

        for py in 0..copy_h {
            for px in 0..copy_w {
                let dst_x = slot.x + px;
                let dst_y = slot.y + py;
                if dst_x < FRAME_WIDTH && dst_y < FRAME_HEIGHT {
                    frame.put_pixel(dst_x, dst_y, *slot_img.get_pixel(px, py));
                }
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn layout_slot_from_config_defaults() {
        let entry = DisplaySlotEntry {
            plugin: "river".to_string(),
            x: None,
            y: None,
            width: None,
            height: None,
            variant: "quadrant".to_string(),
        };
        let slot = LayoutSlot::from_config(&entry).unwrap();
        assert_eq!(slot.x, 0);
        assert_eq!(slot.y, 0);
        assert_eq!(slot.width, 400);
        assert_eq!(slot.height, 240);
        assert_eq!(slot.layout_variant, LayoutVariant::Quadrant);
    }

    #[test]
    fn layout_slot_from_config_explicit() {
        let entry = DisplaySlotEntry {
            plugin: "weather".to_string(),
            x: Some(0),
            y: Some(0),
            width: Some(800),
            height: Some(240),
            variant: "half_horizontal".to_string(),
        };
        let slot = LayoutSlot::from_config(&entry).unwrap();
        assert_eq!(slot.width, 800);
        assert_eq!(slot.height, 240);
        assert_eq!(slot.layout_variant, LayoutVariant::HalfHorizontal);
    }

    #[test]
    fn layout_slot_from_config_invalid_variant() {
        let entry = DisplaySlotEntry {
            plugin: "foo".to_string(),
            x: None,
            y: None,
            width: None,
            height: None,
            variant: "not_a_variant".to_string(),
        };
        assert!(LayoutSlot::from_config(&entry).is_none());
    }

    #[test]
    fn composite_to_png_white_frame() {
        // No slots → pure white 800×480 frame.
        let png = composite_to_png(vec![]).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        let img = image::load_from_memory(&png).unwrap();
        assert_eq!(img.width(), FRAME_WIDTH);
        assert_eq!(img.height(), FRAME_HEIGHT);
        // All pixels should be white (255).
        for pixel in img.to_luma8().pixels() {
            assert_eq!(pixel.0[0], 255);
        }
    }

    #[test]
    fn composite_to_png_single_slot_placed_correctly() {
        // Create a small solid black PNG and composite it at (100, 50).
        let slot_w = 10u32;
        let slot_h = 8u32;
        let black_pixels = vec![0u8; (slot_w * slot_h) as usize];
        let slot_img = GrayImage::from_raw(slot_w, slot_h, black_pixels).unwrap();
        let mut slot_png = Vec::new();
        let enc = image::codecs::png::PngEncoder::new(&mut slot_png);
        ImageEncoder::write_image(enc, slot_img.as_raw(), slot_w, slot_h, image::ColorType::L8)
            .unwrap();

        let slot = LayoutSlot {
            plugin_instance_id: "test".to_string(),
            x: 100,
            y: 50,
            width: slot_w,
            height: slot_h,
            layout_variant: LayoutVariant::Quadrant,
        };
        let png = composite_to_png(vec![(slot, slot_png)]).unwrap();
        let frame = image::load_from_memory(&png).unwrap().to_luma8();

        // Pixel inside slot should be black.
        assert_eq!(frame.get_pixel(100, 50).0[0], 0);
        assert_eq!(frame.get_pixel(105, 54).0[0], 0);
        // Pixel outside slot should be white.
        assert_eq!(frame.get_pixel(99, 50).0[0], 255);
        assert_eq!(frame.get_pixel(110, 58).0[0], 255);
    }
}
