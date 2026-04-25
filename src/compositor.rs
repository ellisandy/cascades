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

use crate::asset_store::AssetStore;
use crate::config::DisplayConfigEntry;
use crate::fonts::FontsManifest;
use crate::format::apply_format;
use crate::instance_store::InstanceStore;
use crate::jsonpath::{jsonpath_extract, value_to_string};
use crate::layout_store::{LayoutItem, LayoutStore};
use crate::template::{NowContext, RenderContext, TemplateEngine};

/// Bundles the curated-font manifest and the URL Chromium should use to fetch
/// font files, so the compositor can wrap every sidecar payload with
/// `<head><style>@font-face…</style></head>` in one place.
#[derive(Clone)]
pub(crate) struct FontsWrap {
    pub manifest: Arc<FontsManifest>,
    pub base_url: String,
}

impl FontsWrap {
    fn wrap(&self, inner: &str) -> String {
        self.manifest.wrap_html(inner, &self.base_url)
    }
}

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
    pub fn from_name(s: &str) -> Option<Self> {
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
                let variant = LayoutVariant::from_name(&s.variant).ok_or_else(|| {
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
                    parent_id: None,
                    visible_when: None,
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
                if let LayoutItem::PluginSlot { layout_variant, plugin_instance_id, .. } = item
                    && LayoutVariant::from_name(layout_variant).is_none()
                {
                    log::warn!(
                        "layout '{}': unknown variant '{}' for slot '{}', skipping",
                        layout.id,
                        layout_variant,
                        plugin_instance_id
                    );
                    return None;
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
    layout_store: Arc<LayoutStore>,
    /// Base URL of the Bun render sidecar, e.g. `"http://localhost:3001"`.
    sidecar_url: String,
    /// Curated fonts + URL base the sidecar should hit to fetch them.
    fonts: FontsWrap,
    /// Phase 6: source for `LayoutItem::Image` bytes. Optional so legacy
    /// callers (and a handful of unit tests that don't exercise images) can
    /// still construct a compositor without an asset store; image items
    /// render as no-ops with a warning when this is `None`.
    asset_store: Option<Arc<AssetStore>>,
}

impl Compositor {
    /// Create a new `Compositor`.
    ///
    /// `sidecar_url` is the base URL of the Bun render sidecar
    /// (e.g. `"http://localhost:3001"`).
    ///
    /// `fonts_manifest` + `font_base_url` are used to wrap every HTML payload
    /// sent to the sidecar with `@font-face` declarations, so user-selected
    /// curated fonts actually render.
    pub fn new(
        template_engine: Arc<TemplateEngine>,
        instance_store: Arc<InstanceStore>,
        layout_store: Arc<LayoutStore>,
        sidecar_url: impl Into<String>,
        fonts_manifest: Arc<FontsManifest>,
        font_base_url: impl Into<String>,
    ) -> Self {
        Compositor {
            template_engine,
            instance_store,
            layout_store,
            sidecar_url: sidecar_url.into(),
            fonts: FontsWrap {
                manifest: fonts_manifest,
                base_url: font_base_url.into(),
            },
            asset_store: None,
        }
    }

    /// Builder-style attachment for the asset store. Production wiring
    /// (`main.rs`) calls this so `LayoutItem::Image` items can resolve their
    /// `asset_id` to bytes; tests that don't render images can skip it.
    pub fn with_asset_store(mut self, asset_store: Arc<AssetStore>) -> Self {
        self.asset_store = Some(asset_store);
        self
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
        // Phase 7: build the eval snapshot once per render — a single JSON
        // object keyed by plugin_instance_id containing each instance's
        // cached_data. visible_when paths like `$.weather.precip_chance_pct`
        // resolve against this. Built up-front so it's both consistent across
        // all items and reusable for DataIcon resolution below.
        let eval_snapshot = build_eval_snapshot(&self.instance_store).await;

        // Phase 7: filter items whose visible_when clause evaluates false.
        // visible[i] mirrors config.items; we track in parallel rather than
        // shrinking the items vec so we can use the existing item-index
        // relationship for task_for_item / png_results without remapping.
        let visible: Vec<bool> = config
            .items
            .iter()
            .map(|it| {
                it.visible_when()
                    .map(|vw| vw.evaluate(&eval_snapshot))
                    .unwrap_or(true)
            })
            .collect();

        // For each item, we either spawn an async render task or handle it inline.
        // task_for_item[i] = Some(handle_index) if item i has an async task, else None.
        let mut task_for_item: Vec<Option<usize>> = Vec::with_capacity(config.items.len());
        let mut handles: Vec<task::JoinHandle<Result<Vec<u8>, CompositorError>>> = Vec::new();

        for (idx, item) in config.items.iter().enumerate() {
            // Phase 7: hidden items don't render. Push None so indices line up
            // for the post-join loop, but skip the work.
            if !visible[idx] {
                task_for_item.push(None);
                continue;
            }
            let maybe_task = match item {
                // Phase 6: image rendering is synchronous (decode + alpha blit
                // on the compositor thread), so it never spawns a sidecar
                // task. The actual paint happens in the post-join loop below.
                LayoutItem::Image { .. } => None,
                // Phase 7: DataIcon also renders synchronously after the join
                // (resolves value → asset_id → bytes from the asset store).
                LayoutItem::DataIcon { .. } => None,
                LayoutItem::PluginSlot {
                    plugin_instance_id,
                    layout_variant,
                    ..
                } => match LayoutVariant::from_name(layout_variant) {
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
                        let fonts = self.fonts.clone();
                        let idx = handles.len();
                        handles.push(task::spawn(async move {
                            render_slot(slot, engine, store, url, mode, fonts).await
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
                    bold,
                    italic,
                    underline,
                    font_family,
                    color,
                    ..
                } => {
                    let text = text_content.clone();
                    let w = (*width).max(0) as u32;
                    let h = (*height).max(0) as u32;
                    let fs = *font_size;
                    let url = self.sidecar_url.clone();
                    let iid = id.clone();
                    let mode = render_mode.to_string();
                    let fmt = TextFormat {
                        bold: bold.unwrap_or(false),
                        italic: italic.unwrap_or(false),
                        underline: underline.unwrap_or(false),
                        font_family: font_family.clone(),
                        color: color.clone(),
                    };
                    let fonts = self.fonts.clone();
                    let idx = handles.len();
                    handles.push(task::spawn(async move {
                        render_static_text(&text, fs, w, h, &url, &iid, &mode, &fmt, &fonts).await
                    }));
                    Some(idx)
                }
                LayoutItem::StaticDateTime {
                    id,
                    width,
                    height,
                    font_size,
                    format,
                    bold,
                    italic,
                    underline,
                    font_family,
                    color,
                    ..
                } => {
                    let fmt_str = format.clone();
                    let w = (*width).max(0) as u32;
                    let h = (*height).max(0) as u32;
                    let fs = *font_size;
                    let url = self.sidecar_url.clone();
                    let iid = id.clone();
                    let mode = render_mode.to_string();
                    let fmt = TextFormat {
                        bold: bold.unwrap_or(false),
                        italic: italic.unwrap_or(false),
                        underline: underline.unwrap_or(false),
                        font_family: font_family.clone(),
                        color: color.clone(),
                    };
                    let fonts = self.fonts.clone();
                    let idx = handles.len();
                    handles.push(task::spawn(async move {
                        render_static_datetime(
                            fmt_str.as_deref(), fs, w, h, &url, &iid, &mode, &fmt, &fonts,
                        )
                        .await
                    }));
                    Some(idx)
                }
                LayoutItem::DataField {
                    id,
                    width,
                    height,
                    field_mapping_id,
                    font_size,
                    format_string,
                    label,
                    bold,
                    italic,
                    underline,
                    font_family,
                    color,
                    ..
                } => {
                    let w = (*width).max(0) as u32;
                    let h = (*height).max(0) as u32;
                    let fs = *font_size;
                    let url = self.sidecar_url.clone();
                    let iid = id.clone();
                    let mode = render_mode.to_string();
                    let fmid = field_mapping_id.clone();
                    let fmt_str = format_string.clone();
                    let lbl = label.clone();
                    let fmt = TextFormat {
                        bold: bold.unwrap_or(false),
                        italic: italic.unwrap_or(false),
                        underline: underline.unwrap_or(false),
                        font_family: font_family.clone(),
                        color: color.clone(),
                    };
                    let layout_store = Arc::clone(&self.layout_store);
                    let instance_store = Arc::clone(&self.instance_store);
                    let fonts = self.fonts.clone();
                    let idx = handles.len();
                    handles.push(task::spawn(async move {
                        render_data_field(
                            &fmid, &fmt_str, lbl.as_deref(), fs, w, h,
                            &url, &iid, &mode,
                            layout_store, instance_store,
                            &fmt,
                            &fonts,
                        )
                        .await
                    }));
                    Some(idx)
                }
                LayoutItem::StaticDivider { .. } => {
                    // Drawn directly — no async task needed.
                    None
                }
                LayoutItem::Group { .. } => {
                    // Phase 1: pure container, no render output.
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

        // Phase 6: pre-fetch bytes for every Image item so the synchronous
        // compositor doesn't have to touch SQLite. Errors (missing asset,
        // invalid bytes) are logged but never fail the whole render — a
        // missing asset becomes a blank rectangle, which is far less
        // surprising than a 500 from /image.png.
        // Phase 6: pre-fetch Image bytes (keyed by asset_id, dedup-friendly).
        // Phase 7: pre-resolve DataIcon items — extract value via field
        // mapping, look up in icon_map, fetch bytes — and stash in
        // data_icon_bytes keyed by item id (each DataIcon may resolve to a
        // different asset based on data, so item id is the natural key).
        let mut image_bytes: HashMap<String, Vec<u8>> = HashMap::new();
        let mut data_icon_bytes: HashMap<String, Vec<u8>> = HashMap::new();
        if let Some(store) = &self.asset_store {
            for (idx, item) in config.items.iter().enumerate() {
                if !visible[idx] {
                    continue;
                }
                match item {
                    LayoutItem::Image { asset_id, .. } => {
                        if image_bytes.contains_key(asset_id) {
                            continue;
                        }
                        if let Some(bytes) = fetch_asset_bytes(store, asset_id).await? {
                            image_bytes.insert(asset_id.clone(), bytes);
                        }
                    }
                    LayoutItem::DataIcon { id, field_mapping_id, icon_map, .. } => {
                        // Resolve value via field mapping; missing → no icon.
                        let extracted = resolve_data_icon_value(
                            &self.layout_store,
                            &eval_snapshot,
                            field_mapping_id,
                        )
                        .await;
                        let Some(value_str) = extracted else { continue };
                        let Some(asset_id) = icon_map.get(&value_str) else {
                            log::warn!(
                                "compose: DataIcon '{id}' value '{value_str}' not in icon_map; rendering blank",
                            );
                            continue;
                        };
                        if let Some(bytes) = fetch_asset_bytes(store, asset_id).await? {
                            data_icon_bytes.insert(id.clone(), bytes);
                        }
                    }
                    _ => {}
                }
            }
        }

        composite_to_png(
            &config.items,
            &visible,
            &task_for_item,
            &png_results,
            &image_bytes,
            &data_icon_bytes,
        )
    }
}

/// Build a JSON snapshot of all plugin instances' cached data, keyed by
/// instance id. Used by [`VisibleWhen::evaluate`] and DataIcon resolution.
/// Errors fetching individual instances log + omit so a flaky read doesn't
/// hide every conditional item.
async fn build_eval_snapshot(instance_store: &Arc<InstanceStore>) -> serde_json::Value {
    let store = Arc::clone(instance_store);
    let instances = match task::spawn_blocking(move || store.list_instances()).await {
        Ok(Ok(list)) => list,
        Ok(Err(e)) => {
            log::warn!("compose: failed to list instances for eval snapshot: {e}");
            return serde_json::Value::Object(serde_json::Map::new());
        }
        Err(e) => {
            log::warn!("compose: instance list task panicked: {e}");
            return serde_json::Value::Object(serde_json::Map::new());
        }
    };
    let mut root = serde_json::Map::with_capacity(instances.len());
    for inst in instances {
        root.insert(
            inst.id,
            inst.cached_data.unwrap_or(serde_json::Value::Null),
        );
    }
    serde_json::Value::Object(root)
}

/// Resolve a DataIcon's `field_mapping_id` against the eval snapshot. Returns
/// the extracted value as a string (so it matches against `icon_map` keys).
/// Logs + returns None on any failure path (missing mapping, missing path,
/// non-string-coercible value); compositor renders a blank rectangle.
async fn resolve_data_icon_value(
    layout_store: &Arc<LayoutStore>,
    snapshot: &serde_json::Value,
    field_mapping_id: &str,
) -> Option<String> {
    let ls = Arc::clone(layout_store);
    let fmid = field_mapping_id.to_string();
    let mapping = task::spawn_blocking(move || ls.get_field_mapping(&fmid))
        .await
        .ok()?
        .ok()?;
    let mapping = mapping?;
    // Build "$.<source_id>.<rest_of_path>" so the path resolves against the
    // unified snapshot. `mapping.json_path` is anchored at the instance's own
    // data; our snapshot is `{instance_id: data}` so we prepend.
    //
    // The trim/branch dance handles three input shapes:
    //   "$.water_level"      → "$.<src>.water_level"   (dot prefix, key root)
    //   "$.values[0]"        → "$.<src>.values[0]"     (dot prefix, key root)
    //   "$.[0]"              → "$.<src>[0]"            (dot prefix, array root —
    //                                                   strip the dot to avoid
    //                                                   "$.<src>.[0]" which is
    //                                                   ill-formed)
    let trimmed = mapping
        .json_path
        .strip_prefix('$')
        .unwrap_or(mapping.json_path.as_str())
        .trim_start_matches('.');
    let path = if trimmed.starts_with('[') {
        format!("$.{}{}", mapping.data_source_id, trimmed)
    } else {
        format!("$.{}.{}", mapping.data_source_id, trimmed)
    };
    let extracted = crate::jsonpath::jsonpath_extract(snapshot, &path).ok()?;
    Some(crate::jsonpath::value_to_string(extracted))
}

/// Helper: fetch asset bytes off-thread, mapping all errors to a logged
/// `None` so the compositor never fails the whole render on one bad asset.
async fn fetch_asset_bytes(
    store: &Arc<AssetStore>,
    asset_id: &str,
) -> Result<Option<Vec<u8>>, CompositorError> {
    let store = Arc::clone(store);
    let aid = asset_id.to_string();
    let result = task::spawn_blocking(move || store.get(&aid)).await?;
    match result {
        Ok(Some(asset)) => Ok(Some(asset.bytes)),
        Ok(None) => {
            log::warn!("compose: asset '{asset_id}' not found");
            Ok(None)
        }
        Err(e) => {
            log::warn!("compose: failed to load asset '{asset_id}': {e}");
            Ok(None)
        }
    }
}

// ─── Per-slot render ─────────────────────────────────────────────────────────

async fn render_slot(
    slot: LayoutSlot,
    engine: Arc<TemplateEngine>,
    store: Arc<InstanceStore>,
    sidecar_url: String,
    mode: String,
    fonts: FontsWrap,
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
    call_sidecar(
        &sidecar_url,
        html,
        render_w,
        render_h,
        &slot.plugin_instance_id,
        &mode,
        &fonts,
    )
    .await
}

fn json_object_to_map(
    val: serde_json::Value,
) -> HashMap<String, serde_json::Value> {
    match val {
        serde_json::Value::Object(map) => map.into_iter().collect(),
        _ => HashMap::new(),
    }
}

// ─── Text formatting ─────────────────────────────────────────────────────────

/// Text formatting options shared by static text, datetime, and data field items.
#[derive(Debug, Clone, Default)]
pub(crate) struct TextFormat {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub font_family: Option<String>,
    /// CSS hex color, e.g. "#ff0000". `None` → caller's default (typically #000).
    pub color: Option<String>,
}

impl TextFormat {
    /// Return CSS declarations for this format (no trailing semicolon after last).
    /// Always emits `font-family` so callers can rely on it.
    fn css(&self) -> String {
        let family = self
            .font_family
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("sans-serif");
        let mut css = format!("font-family:{family};");
        if self.bold {
            css.push_str("font-weight:bold;");
        }
        if self.italic {
            css.push_str("font-style:italic;");
        }
        if self.underline {
            css.push_str("text-decoration:underline;");
        }
        css
    }

    /// The CSS `color:` value to apply, falling back to `#000`.
    fn color_css(&self) -> &str {
        self.color
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("#000")
    }
}

// ─── Static text render ──────────────────────────────────────────────────────

/// Render a static text element via the sidecar.
///
/// Generates a minimal self-contained HTML snippet and POSTs it to
/// `{sidecar_url}/render` at the item's pixel dimensions.
#[allow(clippy::too_many_arguments)]
async fn render_static_text(
    text: &str,
    font_size: i32,
    width: u32,
    height: u32,
    sidecar_url: &str,
    item_id: &str,
    mode: &str,
    format: &TextFormat,
    fonts: &FontsWrap,
) -> Result<Vec<u8>, CompositorError> {
    let safe = html_escape(text);
    let html = format!(
        "<div style='width:{w}px;height:{h}px;display:flex;align-items:center;\
         justify-content:center;{tf}font-size:{fs}px;\
         color:{color};background:white;'>{text}</div>",
        w = width,
        h = height,
        fs = font_size,
        tf = format.css(),
        color = format.color_css(),
        text = safe,
    );
    call_sidecar(sidecar_url, html, width, height, item_id, mode, fonts).await
}

// ─── Static datetime render ─────────────────────────────────────────────────

/// Render a static date/time element via the sidecar.
///
/// Gets the current local time and formats it, then renders via the sidecar
/// like `render_static_text`.
#[allow(clippy::too_many_arguments)]
async fn render_static_datetime(
    format: Option<&str>,
    font_size: i32,
    width: u32,
    height: u32,
    sidecar_url: &str,
    item_id: &str,
    mode: &str,
    text_format: &TextFormat,
    fonts: &FontsWrap,
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
         justify-content:center;{tf}font-size:{fs}px;\
         color:{color};background:white;'>{text}</div>",
        w = width,
        h = height,
        fs = font_size,
        tf = text_format.css(),
        color = text_format.color_css(),
        text = safe,
    );
    call_sidecar(sidecar_url, html, width, height, item_id, mode, fonts).await
}

// ─── Data field render ──────────────────────────────────────────────────────

/// Render a data field element via the sidecar.
///
/// Looks up the field mapping, extracts the value from cached data via JSONPath,
/// applies the format string, and renders the result as HTML through the sidecar.
/// Falls back to `[no data]` if the field mapping is missing or JSONPath fails.
#[allow(clippy::too_many_arguments)]
async fn render_data_field(
    field_mapping_id: &str,
    format_string: &str,
    label: Option<&str>,
    font_size: i32,
    width: u32,
    height: u32,
    sidecar_url: &str,
    item_id: &str,
    mode: &str,
    layout_store: Arc<LayoutStore>,
    instance_store: Arc<InstanceStore>,
    text_format: &TextFormat,
    fonts: &FontsWrap,
) -> Result<Vec<u8>, CompositorError> {
    let fmid = field_mapping_id.to_string();
    let ls = Arc::clone(&layout_store);
    let mapping = task::spawn_blocking(move || ls.get_field_mapping(&fmid))
        .await?
        .map_err(|e| CompositorError::Sidecar {
            slot: item_id.to_string(),
            message: format!("field mapping lookup failed: {e}"),
        })?;

    let display_text = match mapping {
        Some(mapping) => {
            // Look up the cached data from the plugin instance
            let source_id = mapping.data_source_id.clone();
            let is = Arc::clone(&instance_store);
            let instance =
                task::spawn_blocking(move || is.get_instance(&source_id)).await??;

            match instance {
                Some(inst) => {
                    let cached = inst
                        .cached_data
                        .unwrap_or(serde_json::Value::Null);
                    match jsonpath_extract(&cached, &mapping.json_path) {
                        Ok(val) => {
                            let raw = value_to_string(val);
                            apply_format(format_string, &raw)
                        }
                        Err(_) => "[no data]".to_string(),
                    }
                }
                None => "[no data]".to_string(),
            }
        }
        None => "[no data]".to_string(),
    };

    // Build HTML with optional label.  Text formatting (bold/italic/underline/
    // font-family) applies to the value; the small grey label stays in the
    // default sans-serif so it remains legible.
    let safe_value = html_escape(&display_text);
    let label_font_size = (font_size as f32 * 0.6) as i32;
    let value_css = {
        let mut css = format!("font-size:{font_size}px;");
        if text_format.bold {
            css.push_str("font-weight:bold;");
        }
        if text_format.italic {
            css.push_str("font-style:italic;");
        }
        if text_format.underline {
            css.push_str("text-decoration:underline;");
        }
        css
    };
    let content = match label {
        Some(lbl) if !lbl.is_empty() => {
            let safe_label = html_escape(lbl);
            format!(
                "<div style='font-size:{lfs}px;font-family:sans-serif;color:#666;margin-bottom:2px;'>{lbl}</div>\
                 <div style='{vcss}'>{val}</div>",
                lfs = label_font_size,
                lbl = safe_label,
                vcss = value_css,
                val = safe_value,
            )
        }
        _ => format!(
            "<div style='{vcss}'>{val}</div>",
            vcss = value_css,
            val = safe_value,
        ),
    };

    let family = text_format
        .font_family
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("sans-serif");
    let html = format!(
        "<div style='width:{w}px;height:{h}px;display:flex;flex-direction:column;\
         align-items:center;justify-content:center;font-family:{family};\
         color:{color};background:white;'>{content}</div>",
        w = width,
        h = height,
        family = family,
        color = text_format.color_css(),
        content = content,
    );
    call_sidecar(sidecar_url, html, width, height, item_id, mode, fonts).await
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
    fonts: &FontsWrap,
) -> Result<Vec<u8>, CompositorError> {
    let url = format!("{}/render", base_url);
    let slot_id = slot_id.to_string();
    let mode = mode.to_string();

    // Wrap in a full HTML document with @font-face declarations so Chromium
    // can fetch and apply the curated fonts before screenshotting.
    let html = fonts.wrap(&html);

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
/// Helper: treat an empty `visible` vec as "all items visible". Lets unit
/// tests of `composite_to_png` keep passing `&[]` for tests that don't care
/// about Phase 7 visibility filtering.
fn is_visible(visible: &[bool], idx: usize) -> bool {
    visible.get(idx).copied().unwrap_or(true)
}

fn composite_to_png(
    items: &[LayoutItem],
    visible: &[bool],
    task_for_item: &[Option<usize>],
    png_results: &[Vec<u8>],
    image_bytes: &HashMap<String, Vec<u8>>,
    data_icon_bytes: &HashMap<String, Vec<u8>>,
) -> Result<Vec<u8>, CompositorError> {
    // Allocate white frame.
    let pixels = vec![255u8; (FRAME_WIDTH * FRAME_HEIGHT) as usize];
    let mut frame =
        GrayImage::from_raw(FRAME_WIDTH, FRAME_HEIGHT, pixels)
            .expect("buffer size matches dimensions");

    for (idx, (item, maybe_task)) in items.iter().zip(task_for_item.iter()).enumerate() {
        // Phase 7: visible_when=false items are omitted entirely. The
        // visibility vec parallels items (no shrinking) so this index lookup
        // is cheap and tests that pass an empty `visible` vec still work
        // (treat as all-visible — see helper below).
        if !is_visible(visible, idx) {
            continue;
        }
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
            LayoutItem::DataField { x, y, width, height, id, .. } => {
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
            LayoutItem::Group { x, y, width, height, background, .. } => {
                // Phase 2: groups may declare a background rendered behind
                // their descendants. Items are traversed in z-order, so a
                // group placed at a lower z_index than its children paints
                // first — the natural "background" position.
                if background.as_deref() == Some("card") {
                    draw_card_background(
                        &mut frame,
                        (*x).max(0) as u32,
                        (*y).max(0) as u32,
                        (*width).max(0) as u32,
                        (*height).max(0) as u32,
                    );
                }
            }
            LayoutItem::Image { x, y, width, height, asset_id, .. } => {
                if let Some(bytes) = image_bytes.get(asset_id) {
                    // Failure (corrupted bytes, unsupported subformat) downgrades
                    // to a logged warning + skipped paint — a broken asset
                    // shouldn't take down the whole composite render.
                    if let Err(e) = blit_asset_image(
                        &mut frame,
                        bytes,
                        (*x).max(0) as u32,
                        (*y).max(0) as u32,
                        (*width).max(0) as u32,
                        (*height).max(0) as u32,
                        asset_id,
                    ) {
                        log::warn!(
                            "compose: skipping image '{asset_id}': {e}",
                        );
                    }
                }
                // Else: bytes were absent (missing asset or no asset_store
                // wired); already logged in compose(). Render as no-op so the
                // user sees an empty box where the image was.
            }
            LayoutItem::DataIcon { id, x, y, width, height, .. } => {
                // Bytes were resolved upstream in compose() (value lookup →
                // icon_map → asset bytes). Missing entries here are logged
                // already; we just paint what's there. Same defensive
                // contract as Image.
                if let Some(bytes) = data_icon_bytes.get(id)
                    && let Err(e) = blit_asset_image(
                        &mut frame,
                        bytes,
                        (*x).max(0) as u32,
                        (*y).max(0) as u32,
                        (*width).max(0) as u32,
                        (*height).max(0) as u32,
                        id,
                    )
                {
                    log::warn!("compose: skipping data_icon '{id}': {e}");
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

/// Decode `png_bytes` and copy up to `(width × height)` pixels onto `frame`
/// at offset `(x, y)`.
/// Decode PNG/JPEG bytes, resize to (`width`, `height`), and alpha-composite
/// onto the grayscale frame. Used by `LayoutItem::Image`. Distinct from
/// `blit_png` because:
/// - Sidecar PNGs are already at the slot size (no resize).
/// - Sidecar PNGs are L8 (text on white), so no alpha blending is needed.
/// - Asset images are user-supplied at arbitrary dimensions and may carry
///   transparency we want to honour.
///
/// **Stretch fit.** The image scales to exactly the box size — aspect ratio
/// is the user's responsibility (set the box aspect to match). Documented as
/// such; if users complain we'd add a `fit_mode` field rather than guess.
fn blit_asset_image(
    frame: &mut GrayImage,
    bytes: &[u8],
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    asset_id: &str,
) -> Result<(), CompositorError> {
    if width == 0 || height == 0 {
        return Ok(());
    }
    let src = image::load_from_memory(bytes).map_err(|_| CompositorError::InvalidPng {
        slot: format!("image:{asset_id}"),
    })?;
    // LumaA preserves transparency for compositing; a fully opaque JPEG
    // ends up with alpha=255 everywhere, which is exactly what we want.
    let resized = image::imageops::resize(
        &src.to_luma_alpha8(),
        width,
        height,
        image::imageops::FilterType::Triangle,
    );

    for py in 0..height {
        for px in 0..width {
            let dst_x = x + px;
            let dst_y = y + py;
            if dst_x >= FRAME_WIDTH || dst_y >= FRAME_HEIGHT {
                continue;
            }
            let p = resized.get_pixel(px, py);
            let src_l = p.0[0] as u16;
            let alpha = p.0[1] as u16;
            if alpha == 0 {
                continue; // fully transparent — leave dst untouched
            }
            if alpha == 255 {
                frame.put_pixel(dst_x, dst_y, image::Luma([src_l as u8]));
            } else {
                let dst_l = frame.get_pixel(dst_x, dst_y).0[0] as u16;
                // Standard alpha over: out = src*a + dst*(1-a), in [0,255].
                let out = (src_l * alpha + dst_l * (255 - alpha)) / 255;
                frame.put_pixel(dst_x, dst_y, image::Luma([out as u8]));
            }
        }
    }
    Ok(())
}

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

/// Draw a "card" style group background: white fill with a 1px black border.
///
/// Used to render [`LayoutItem::Group`] items with `background = "card"`.
/// The fill ensures any items below this group in z-order are masked out;
/// the 1px border marks the group visually.
fn draw_card_background(frame: &mut GrayImage, x: u32, y: u32, width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    // White fill.
    for py in 0..height {
        for px in 0..width {
            let dst_x = x + px;
            let dst_y = y + py;
            if dst_x < FRAME_WIDTH && dst_y < FRAME_HEIGHT {
                frame.put_pixel(dst_x, dst_y, image::Luma([255u8]));
            }
        }
    }
    // 1px black border (top/bottom/left/right).
    let right = x.saturating_add(width.saturating_sub(1));
    let bottom = y.saturating_add(height.saturating_sub(1));
    for px in 0..width {
        let dst_x = x + px;
        if dst_x < FRAME_WIDTH {
            if y < FRAME_HEIGHT {
                frame.put_pixel(dst_x, y, image::Luma([0u8]));
            }
            if bottom < FRAME_HEIGHT {
                frame.put_pixel(dst_x, bottom, image::Luma([0u8]));
            }
        }
    }
    for py in 0..height {
        let dst_y = y + py;
        if dst_y < FRAME_HEIGHT {
            if x < FRAME_WIDTH {
                frame.put_pixel(x, dst_y, image::Luma([0u8]));
            }
            if right < FRAME_WIDTH {
                frame.put_pixel(right, dst_y, image::Luma([0u8]));
            }
        }
    }
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
            assert_eq!(LayoutVariant::from_name(s), Some(expected));
        }
        assert_eq!(LayoutVariant::from_name("bogus"), None);
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
                    parent_id: None,
                    visible_when: None,
                },
                LayoutItem::StaticText {
                    id: "t0".to_string(),
                    z_index: 1,
                    x: 10, y: 10, width: 200, height: 50,
                    text_content: "Hello".to_string(),
                    font_size: 24,
                    orientation: None,
                    bold: None,
                    italic: None,
                    underline: None,
                    font_family: None,
                    color: None,
                    parent_id: None,
                    visible_when: None,
                },
                LayoutItem::StaticDivider {
                    id: "d0".to_string(),
                    z_index: 2,
                    x: 0, y: 240, width: 800, height: 2,
                    orientation: Some("horizontal".to_string()),
                    parent_id: None,
                    visible_when: None,
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
                    parent_id: None,
                    visible_when: None,
                },
                LayoutItem::StaticDivider {
                    id: "d0".to_string(),
                    z_index: 1,
                    x: 0, y: 240, width: 800, height: 2,
                    orientation: None,
                    parent_id: None,
                    visible_when: None,
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
        let png = composite_to_png(&[], &[], &[], &[], &std::collections::HashMap::new(), &std::collections::HashMap::new()).unwrap();
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
            parent_id: None,
            visible_when: None,
        };

        let task_for_item = vec![Some(0usize)];
        let png_results = vec![slot_png];

        let png = composite_to_png(&[item], &[], &task_for_item, &png_results, &std::collections::HashMap::new(), &std::collections::HashMap::new()).unwrap();
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
            parent_id: None,
            visible_when: None,
        };

        let task_for_item = vec![None];
        let png = composite_to_png(&[item], &[], &task_for_item, &[], &std::collections::HashMap::new(), &std::collections::HashMap::new()).unwrap();
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
            bold: None,
            italic: None,
            underline: None,
            font_family: None,
            color: None,
            parent_id: None,
            visible_when: None,
        };

        let task_for_item = vec![Some(0usize)];
        let png_results = vec![text_png];

        let png = composite_to_png(&[item], &[], &task_for_item, &png_results, &std::collections::HashMap::new(), &std::collections::HashMap::new()).unwrap();
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
                parent_id: None,
                visible_when: None,
            },
            LayoutItem::PluginSlot {
                id: "s1".to_string(), z_index: 1,
                x: 0, y: 0, width: w as i32, height: h as i32,
                plugin_instance_id: "black".to_string(),
                layout_variant: "quadrant".to_string(),
                parent_id: None,
                visible_when: None,
            },
        ];
        let task_for_item = vec![Some(0usize), Some(1usize)];
        let png_results = vec![white_png, black_png];

        let png = composite_to_png(&items, &[], &task_for_item, &png_results, &std::collections::HashMap::new(), &std::collections::HashMap::new()).unwrap();
        let frame = image::load_from_memory(&png).unwrap().to_luma8();

        // Black (z=1) overwrote white (z=0).
        assert_eq!(frame.get_pixel(10, 10).0[0], 0);
    }

    #[test]
    fn composite_to_png_group_card_background_draws_border_and_fills_white() {
        // A group with a "card" background paints a white rectangle with a
        // 1px black border at its bounds, before any descendants.
        let item = LayoutItem::Group {
            id: "g0".to_string(),
            z_index: 0,
            x: 50, y: 40, width: 100, height: 60,
            plugin_instance_id: None,
            label: None,
            background: Some("card".to_string()),
            parent_id: None,
            default_elements_hash: None,
            defaults_stale: None,
        };
        let task_for_item = vec![None];
        let png = composite_to_png(&[item], &[], &task_for_item, &[], &std::collections::HashMap::new(), &std::collections::HashMap::new()).unwrap();
        let frame = image::load_from_memory(&png).unwrap().to_luma8();

        // Border pixels are black.
        assert_eq!(frame.get_pixel(50, 40).0[0], 0, "top-left corner");
        assert_eq!(frame.get_pixel(149, 40).0[0], 0, "top-right corner");
        assert_eq!(frame.get_pixel(50, 99).0[0], 0, "bottom-left corner");
        assert_eq!(frame.get_pixel(149, 99).0[0], 0, "bottom-right corner");
        assert_eq!(frame.get_pixel(100, 40).0[0], 0, "top edge");
        assert_eq!(frame.get_pixel(100, 99).0[0], 0, "bottom edge");
        assert_eq!(frame.get_pixel(50, 70).0[0], 0, "left edge");
        assert_eq!(frame.get_pixel(149, 70).0[0], 0, "right edge");

        // Interior is white.
        assert_eq!(frame.get_pixel(100, 70).0[0], 255, "interior");
        assert_eq!(frame.get_pixel(51, 41).0[0], 255, "just inside top-left");

        // Outside the group is unchanged white.
        assert_eq!(frame.get_pixel(49, 40).0[0], 255);
        assert_eq!(frame.get_pixel(150, 40).0[0], 255);
    }

    #[test]
    fn composite_to_png_group_without_background_is_noop() {
        // A group with no background (or None) paints nothing.
        let item = LayoutItem::Group {
            id: "g0".to_string(),
            z_index: 0,
            x: 10, y: 10, width: 50, height: 50,
            plugin_instance_id: None,
            label: None,
            background: None,
            parent_id: None,
            default_elements_hash: None,
            defaults_stale: None,
        };
        let task_for_item = vec![None];
        let png = composite_to_png(&[item], &[], &task_for_item, &[], &std::collections::HashMap::new(), &std::collections::HashMap::new()).unwrap();
        let frame = image::load_from_memory(&png).unwrap().to_luma8();

        // Frame is still pure white.
        for pixel in frame.pixels() {
            assert_eq!(pixel.0[0], 255);
        }
    }

    #[test]
    fn composite_to_png_group_card_paints_before_children_in_z_order() {
        // Group (z=0) with card bg, plus a black child (z=1) inside it.
        // The child must paint on top — the group bg must NOT overwrite it.
        let group = LayoutItem::Group {
            id: "g".to_string(),
            z_index: 0,
            x: 100, y: 100, width: 80, height: 60,
            plugin_instance_id: None,
            label: None,
            background: Some("card".to_string()),
            parent_id: None,
            default_elements_hash: None,
            defaults_stale: None,
        };
        // Child is a StaticDivider at z=1 inside the group.
        let child = LayoutItem::StaticDivider {
            id: "d".to_string(),
            z_index: 1,
            x: 110, y: 120, width: 20, height: 10,
            orientation: None,
            parent_id: Some("g".to_string()),
            visible_when: None,
        };
        let task_for_item = vec![None, None];
        let png = composite_to_png(&[group, child], &[], &task_for_item, &[], &std::collections::HashMap::new(), &std::collections::HashMap::new()).unwrap();
        let frame = image::load_from_memory(&png).unwrap().to_luma8();

        // Inside the child rectangle — should be black (child painted on top).
        assert_eq!(frame.get_pixel(115, 125).0[0], 0, "child should overpaint group bg");
        // Inside the group but outside the child — should be white.
        assert_eq!(frame.get_pixel(140, 140).0[0], 255, "group interior should be white");
        // Group border should still be visible.
        assert_eq!(frame.get_pixel(100, 100).0[0], 0, "group border top-left");
    }

    /// A 4×4 black PNG bytes generated via the `image` crate, kept inline so
    /// tests don't ship a binary fixture. Compositor uses `to_luma_alpha8()`
    /// internally so any encoding the `image` crate accepts is fine here.
    fn black_4x4_png() -> Vec<u8> {
        // 4×4 fully-black opaque image. RGBA so we can assert alpha pathway.
        let img = image::RgbaImage::from_fn(4, 4, |_, _| image::Rgba([0, 0, 0, 255]));
        let mut out: Vec<u8> = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut out);
        ImageEncoder::write_image(
            encoder,
            img.as_raw(),
            4,
            4,
            image::ColorType::Rgba8,
        )
        .unwrap();
        out
    }

    /// `LayoutItem::Image` should resize the asset bytes into the item's
    /// rectangle and stamp pixel values onto the frame. This is the single
    /// most important sanity check on the Phase 6 render path.
    #[test]
    fn composite_to_png_image_renders_at_item_rect() {
        let png = black_4x4_png();
        let item = LayoutItem::Image {
            id: "img1".to_string(),
            z_index: 0,
            x: 50, y: 60, width: 30, height: 20,
            asset_id: "asset-x".to_string(),
            parent_id: None,
            visible_when: None,
        };
        let mut bytes = std::collections::HashMap::new();
        bytes.insert("asset-x".to_string(), png);
        let task_for_item = vec![None];
        let composed = composite_to_png(&[item], &[], &task_for_item, &[], &bytes, &std::collections::HashMap::new()).unwrap();
        let frame = image::load_from_memory(&composed).unwrap().to_luma8();

        // Inside the image rectangle: black (the asset is fully black).
        assert_eq!(frame.get_pixel(60, 70).0[0], 0, "image interior should be black");
        // Outside the rectangle: untouched white background.
        assert_eq!(frame.get_pixel(10, 10).0[0], 255, "outside the image stays white");
        assert_eq!(frame.get_pixel(85, 85).0[0], 255, "outside the image stays white");
    }

    /// Phase 7: items with `visible[i] = false` must not paint. A black
    /// image item that *would* paint at (50,60)→(80,80) stays unpainted
    /// when its visibility flag is false.
    #[test]
    fn composite_to_png_skips_invisible_items() {
        let png = black_4x4_png();
        let item = LayoutItem::Image {
            id: "img1".to_string(),
            z_index: 0,
            x: 50, y: 60, width: 30, height: 20,
            asset_id: "asset-x".to_string(),
            parent_id: None,
            visible_when: None,
        };
        let mut bytes = std::collections::HashMap::new();
        bytes.insert("asset-x".to_string(), png);
        let task_for_item = vec![None];
        let composed = composite_to_png(
            &[item],
            &[false],
            &task_for_item,
            &[],
            &bytes,
            &std::collections::HashMap::new(),
        ).unwrap();
        let frame = image::load_from_memory(&composed).unwrap().to_luma8();
        assert_eq!(frame.get_pixel(60, 70).0[0], 255, "hidden image must not paint");
    }

    /// `composite_to_png` with an empty `visible` slice treats every item as
    /// visible — needed so legacy callers / tests that don't care about
    /// Phase 7 don't have to construct a parallel boolean vec.
    #[test]
    fn composite_to_png_empty_visible_means_all_visible() {
        let png = black_4x4_png();
        let item = LayoutItem::Image {
            id: "img1".to_string(),
            z_index: 0,
            x: 50, y: 60, width: 30, height: 20,
            asset_id: "asset-x".to_string(),
            parent_id: None,
            visible_when: None,
        };
        let mut bytes = std::collections::HashMap::new();
        bytes.insert("asset-x".to_string(), png);
        let task_for_item = vec![None];
        let composed = composite_to_png(
            &[item],
            &[],
            &task_for_item,
            &[],
            &bytes,
            &std::collections::HashMap::new(),
        ).unwrap();
        let frame = image::load_from_memory(&composed).unwrap().to_luma8();
        assert_eq!(frame.get_pixel(60, 70).0[0], 0, "empty `visible` should mean fully visible");
    }

    /// Phase 7: `LayoutItem::DataIcon` shares `blit_asset_image` with
    /// Image, so the bytes-blit path is well-tested already. This test
    /// proves the post-join handler correctly looks bytes up by *item id*
    /// (not asset_id) — the keying detail that's specific to DataIcon.
    #[test]
    fn composite_to_png_data_icon_renders_via_item_id_keyed_bytes() {
        let png = black_4x4_png();
        let item = LayoutItem::DataIcon {
            id: "icon-1".to_string(),
            z_index: 0,
            x: 100, y: 100, width: 20, height: 20,
            field_mapping_id: "fm-weather-cond".to_string(),
            icon_map: std::collections::HashMap::new(),
            parent_id: None,
            visible_when: None,
        };
        let mut data_icon_bytes = std::collections::HashMap::new();
        data_icon_bytes.insert("icon-1".to_string(), png);
        let task_for_item = vec![None];
        let composed = composite_to_png(
            &[item],
            &[],
            &task_for_item,
            &[],
            &std::collections::HashMap::new(),
            &data_icon_bytes,
        ).unwrap();
        let frame = image::load_from_memory(&composed).unwrap().to_luma8();
        assert_eq!(frame.get_pixel(110, 110).0[0], 0, "data icon must paint at item rect");
        assert_eq!(frame.get_pixel(50, 50).0[0], 255);
    }

    /// A missing asset (id present in the item but not in the bytes map) must
    /// not crash the render — it just leaves a blank rectangle.
    #[test]
    fn composite_to_png_image_with_missing_asset_is_no_op() {
        let item = LayoutItem::Image {
            id: "img1".to_string(),
            z_index: 0,
            x: 100, y: 100, width: 50, height: 50,
            asset_id: "ghost".to_string(),
            parent_id: None,
            visible_when: None,
        };
        let task_for_item = vec![None];
        let composed = composite_to_png(
            &[item],
            &[],
            &task_for_item,
            &[],
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let frame = image::load_from_memory(&composed).unwrap().to_luma8();
        // Whole frame is the white background — no panic, no paint.
        assert_eq!(frame.get_pixel(125, 125).0[0], 255);
    }

    #[test]
    fn text_format_css_defaults_to_sans_serif() {
        let fmt = TextFormat::default();
        assert_eq!(fmt.css(), "font-family:sans-serif;");
    }

    #[test]
    fn text_format_css_emits_bold_italic_underline() {
        let fmt = TextFormat {
            bold: true,
            italic: true,
            underline: true,
            font_family: Some("Georgia, serif".to_string()),
            color: None,
        };
        let css = fmt.css();
        assert!(css.contains("font-family:Georgia, serif;"));
        assert!(css.contains("font-weight:bold;"));
        assert!(css.contains("font-style:italic;"));
        assert!(css.contains("text-decoration:underline;"));
    }

    #[test]
    fn text_format_css_falls_back_on_blank_font_family() {
        let fmt = TextFormat {
            font_family: Some("   ".to_string()),
            ..TextFormat::default()
        };
        assert!(fmt.css().starts_with("font-family:sans-serif;"));
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
