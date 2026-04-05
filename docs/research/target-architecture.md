# Target Architecture: Cascades — Modern, Extensible, TRMNL-Inspired

> Informs cs-3pf (technology stack evaluation). Supersedes the ad-hoc rendering
> approach in `src/render/`. Based on analysis in `architecture-comparison.md`,
> `inker-architecture.md`, and `cascades-extensibility-diagnosis.md`.

---

## 1. Design Principles

**1. Content is data, not code.** Adding a new datasource or changing a display
layout must never require a recompile. The platform provides infrastructure;
plugins provide content.

**2. The Source trait is the extension point.** The fetch abstraction is already
correct. Everything downstream of `fetch()` must become equally generic.

**3. Decouple fetch from render.** Data freshness and display freshness are
independent concerns. A slow API should not blank the screen.

**4. One rendering primitive.** HTML → PNG is the universal interface between
content and display. Everything passes through this funnel.

**5. The evaluation model is a Cascades differentiator.** TRMNL has no go/no-go
trip planner. Preserve and generalize it — don't strip it in the name of TRMNL
compatibility.

---

## 2. Component Diagram

```
╔══════════════════════════════════════════════════════════════════════╗
║                        Plugin Registry                               ║
║   TOML config or SQLite: {id, source, templates, settings_schema}    ║
╚═══════════╤══════════════════════════════════════════════════════════╝
            │ load at startup (hot-reload on change)
            ▼
╔═══════════════════════════════════════╗
║         Scheduler (Rust)              ║
║  Per-plugin fetch loop, independent   ║
║  intervals, respects refresh_interval ║
╚═══════════╤═══════════════════════════╝
            │ calls fetch()
            ▼
╔═══════════════════════════════════════╗   ╔══════════════════════════╗
║         Source Layer (Rust)           ║   ║    Webhook Receiver      ║
║  trait Source { fetch() → Value }     ║   ║  POST /webhook/:id       ║
║  noaa, usgs, wsdot, trail, road, ...  ║   ║  updates cache directly  ║
╚═══════════╤═══════════════════════════╝   ╚═══════════╤══════════════╝
            │ Ok(serde_json::Value)                      │
            └─────────────────────┬──────────────────────┘
                                  ▼
╔══════════════════════════════════════════════════════════════════════╗
║                       Data Cache (Rust)                              ║
║  Arc<RwLock<HashMap<PluginId, CachedValue>>>                         ║
║  CachedValue { data: Value, fetched_at, error: Option<String> }      ║
║  On failure: keep last successful value, record error                ║
╚═══════════╤══════════════════════════════════════════════════════════╝
            │ read on render trigger
            ▼
╔══════════════════════════════════════════════════════════════════════╗
║                    Evaluation Engine (Rust)                          ║
║  trait Criterion { evaluate(data: &Value) → CriterionResult }        ║
║  Criteria registered by plugin at load time from settings schema     ║
║  TripDecision { go: bool, reasons: Vec<Reason> }                     ║
╚═══════════╤══════════════════════════════════════════════════════════╝
            │ TripDecision (optional — plugins opt in)
            ▼
╔══════════════════════════════════════════════════════════════════════╗
║                   Template Engine (Rust — minijinja)                 ║
║  Renders: template_str + { data, settings, trip_decision, now }      ║
║  → HTML fragment (plugin owns its layout)                            ║
║  Layout compositor wraps fragments into full-page HTML               ║
╚═══════════╤══════════════════════════════════════════════════════════╝
            │ full HTML page string
            ▼
╔══════════════════════════════════════════════════════════════════════╗
║                   Render Sidecar (Node.js/Bun)                       ║
║  POST /render { html, width, height, mode }                          ║
║  Puppeteer → raw PNG → Sharp (grayscale, dither, ≤90KB)              ║
║  Returns PNG bytes                                                   ║
╚═══════════╤══════════════════════════════════════════════════════════╝
            │ PNG bytes (per slot)
            ▼
╔══════════════════════════════════════════════════════════════════════╗
║                  Layout Compositor (Rust)                            ║
║  Slot-based: [ {plugin_id, x, y, width, height} ]                   ║
║  For single-plugin full display: one slot, 800×480                   ║
║  For multi-plugin mixup: composite N PNGs into one 800×480 frame     ║
╚═══════════╤══════════════════════════════════════════════════════════╝
            │ final 800×480 PNG
            ▼
╔══════════════════════════════════════════════════════════════════════╗
║                     Output Layer (Rust)                              ║
║  GET /api/display → image URL                                        ║
║  GET /api/image/:id → PNG bytes (with Cache-Control)                 ║
║  Framebuffer write (for directly-connected display)                  ║
╚══════════════════════════════════════════════════════════════════════╝
```

---

## 3. Component Responsibility Map

| Component | Owns | Does NOT own |
|---|---|---|
| **Plugin Registry** | Plugin definitions, template strings, settings schemas, source binding | Source implementations, rendering logic |
| **Scheduler** | Fetch timing, retry backoff, per-source goroutines | What to do with data, how to render |
| **Source Layer** | External API calls, parsing, `Source` trait impl | Domain state, display logic |
| **Data Cache** | Last-good data per plugin, error state, timestamp | Rendering, fetching |
| **Evaluation Engine** | Trip go/no-go logic, criterion registry | Data fetching, template rendering |
| **Template Engine** | Liquid template rendering, template context assembly | Data fetching, HTML→image conversion |
| **Render Sidecar** | HTML→PNG conversion, dithering, size constraints | Templates, data, layout geometry |
| **Layout Compositor** | Slot geometry, multi-plugin compositing | Per-plugin rendering |
| **Output Layer** | HTTP device API, framebuffer write, image serving | Everything above |

---

## 4. Plugin Model

A plugin is the unit of extensibility. It binds a data source to a display
template and declares what configuration it needs from the user.

### Plugin definition (TOML)

```toml
[[plugin]]
id = "river"
name = "USGS River Gauge"
description = "River level and flow rate from a USGS NWIS site."
source = "usgs"                          # Source trait implementation to instantiate
refresh_interval_secs = 300
data_strategy = "polling"                # polling | webhook | static

# Templates — at least one required. Others fall back to "full" if absent.
template_full             = "templates/river_full.html.liquid"
template_half_horizontal  = "templates/river_half.html.liquid"
# template_half_vertical and template_quadrant omitted → falls back to full

# Trip evaluation criteria (optional — omit if source doesn't inform go/no-go)
[[plugin.criteria]]
key = "water_level_ft"
label = "River level"
operator = "lte"               # lte, gte, eq, between
threshold = 5.5
unit = "ft"
go_direction = "below"         # below | above

# User-configurable fields (drives auto-generated config UI)
[[plugin.settings_schema]]
key = "site_id"
label = "USGS Site ID"
type = "text"
required = true
placeholder = "12150800"

[[plugin.settings_schema]]
key = "site_name"
label = "Display Name"
type = "text"
required = false
default = "River"
```

### Plugin registry location

```
config/
  plugins.toml          ← installed plugins
  plugins.d/            ← drop-in plugin files (one file per plugin)
    river.toml
    ferry.toml
    weather.toml
    trail.toml
    road.toml

templates/
  river_full.html.liquid
  river_half.html.liquid
  ferry_full.html.liquid
  weather_full.html.liquid
  ...
```

Community plugins are a directory dropped into `plugins.d/` with their templates.
No code changes. No restart required (hot-reload on file change).

### Plugin instance (runtime)

A plugin definition can be instantiated multiple times with different settings
(e.g., two river gauges at different USGS sites). Instances are stored in SQLite:

```sql
CREATE TABLE plugin_instances (
    id TEXT PRIMARY KEY,
    plugin_id TEXT NOT NULL,
    settings TEXT NOT NULL,           -- JSON: plain settings
    encrypted_settings TEXT,          -- JSON: AES-encrypted sensitive fields
    cached_data TEXT,                 -- JSON: last successful fetch result
    last_fetched_at INTEGER,          -- Unix timestamp
    last_error TEXT                   -- Last fetch error message, if any
);
```

---

## 5. API Contracts Between Layers

### 5a. Source → Data Cache

The `Source` trait changes in one way: the return type opens from a closed enum
to `serde_json::Value`. Everything else stays identical.

```rust
pub trait Source: Send {
    /// Stable identifier used as the cache key.
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn refresh_interval(&self) -> Duration;

    /// Returns arbitrary JSON on success. The shape is defined by the source
    /// and documented in its plugin definition. On error, the cache retains
    /// the previous value.
    fn fetch(&self) -> Result<serde_json::Value, SourceError>;
}
```

**`CachedValue`:**

```rust
pub struct CachedValue {
    pub data: serde_json::Value,
    pub fetched_at: SystemTime,
    pub error: Option<String>,     // non-None if last fetch failed (stale data in use)
}
```

### 5b. Data Cache → Evaluation Engine

```rust
pub trait Criterion: Send + Sync {
    fn evaluate(&self, data: &serde_json::Value) -> CriterionResult;
}

pub struct CriterionResult {
    pub key: String,
    pub label: String,
    pub value: serde_json::Value,
    pub threshold: serde_json::Value,
    pub pass: bool,
    pub reason: String,
}

pub struct TripDecision {
    pub go: bool,
    pub destination: String,
    pub results: Vec<CriterionResult>,
    pub evaluated_at: SystemTime,
}
```

Criteria are registered at plugin load time. The evaluator is a loop:
`for criterion in registered_criteria { results.push(criterion.evaluate(&cache)) }`.
No hardcoded signal names. Any plugin can register criteria against any field in
its own cached data.

### 5c. Template Engine — context contract

The template context passed to every Liquid render is:

```json
{
  "data":          { /* CachedValue.data — plugin's own fetch result */ },
  "settings":      { /* plugin instance settings (decrypted) */ },
  "trip_decision": {
    "go":          true,
    "destination": "Stevens Pass",
    "results":     [ { "key": "water_level_ft", "pass": true, "reason": "4.2 ft ≤ 5.5 ft", ... } ]
  },
  "now": {
    "unix":     1743820800,
    "iso":      "2026-04-05T12:00:00Z",
    "local":    "Sun Apr 5 12:00"
  },
  "error": null     /* or string if last fetch failed and stale data is shown */
}
```

`trip_decision` is `null` for plugins that register no criteria. Templates check
`{% if trip_decision %}` before rendering go/no-go UI.

### 5d. Template Engine → Render Sidecar

HTTP, one endpoint:

```
POST /render
Content-Type: application/json

{
  "html":   "<full HTML page string>",
  "width":  800,
  "height": 480,
  "mode":   "device"   // device | einkPreview | preview
}

→ 200 OK
Content-Type: image/png
Body: PNG bytes (≤90KB for device mode)
```

The render sidecar is a stateless HTTP server. Its only job is: receive HTML,
return PNG. It has no knowledge of plugins, templates, or data.

**Render modes:**
- `device` — full e-ink processing: grayscale → Floyd-Steinberg dither (threshold 140) → negate
- `einkPreview` — dithering without negate (what the device will look like, in-browser)
- `preview` — raw Puppeteer screenshot, no Sharp post-processing (UI thumbnails)

### 5e. Layout Compositor — slot model

```rust
pub struct LayoutSlot {
    pub plugin_instance_id: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub layout_variant: LayoutVariant,   // Full | HalfHorizontal | HalfVertical | Quadrant
}

pub enum LayoutVariant {
    Full,             // 800×480
    HalfHorizontal,   // 800×240
    HalfVertical,     // 400×480
    Quadrant,         // 400×240
}
```

A **display configuration** is a named list of slots:

```toml
[[display]]
name = "default"
slots = [
    { plugin = "river",   variant = "full" }
]

[[display]]
name = "trip-planner"
slots = [
    { plugin = "weather", x = 0,   y = 0,   width = 800, height = 240, variant = "half_horizontal" },
    { plugin = "river",   x = 0,   y = 240, width = 400, height = 240, variant = "quadrant" },
    { plugin = "ferry",   x = 400, y = 240, width = 400, height = 240, variant = "quadrant" },
]
```

For each slot, the compositor:
1. Retrieves the plugin's cached data
2. Renders the template at the slot's dimensions → HTML
3. Calls the sidecar: POST /render {html, width, height, mode}
4. Receives PNG for that slot
5. Composites all slot PNGs into the final 800×480 frame using image pixel writes

All slot renders are concurrent (Tokio tasks). The compositor joins them before
compositing.

### 5f. Output Layer — device API

```
GET /api/display
Headers: Authorization: Bearer <api_key>
→ 200 { "image_url": "/api/image/<display_id>?t=<timestamp>", "refresh_rate": 300 }

GET /api/image/:display_id
→ 200 PNG bytes, Content-Type: image/png, Cache-Control: no-store

POST /api/webhook/:plugin_instance_id
Body: arbitrary JSON
→ 200 (stores to cache, triggers re-render)
```

---

## 6. What Stays, What Changes, What Gets Added

### Stays (no changes needed)

| Component | Why |
|---|---|
| `Source` trait signature | Clean abstraction, only return type changes |
| Per-source implementations (`noaa.rs`, `usgs.rs`, etc.) | Only change: return `serde_json::Value` instead of a named enum variant |
| Scheduler (fetch loop, per-source intervals) | Sound design, keep as-is |
| Config file (TOML) | Extend with plugin registry section; existing fields preserved |
| `SourceError` type | No changes needed |

### Changes (existing components modified)

| Component | Change |
|---|---|
| `Source::fetch()` return type | `DataPoint` → `serde_json::Value` |
| `DomainState` | Replace named `Option<T>` fields with `HashMap<PluginId, CachedValue>` |
| `RelevantSignals` | Replace named booleans with `HashSet<PluginId>` |
| Evaluation | Replace hardcoded per-signal blocks with `Vec<Box<dyn Criterion>>`; criteria registered by plugins |
| Config struct | Add `[[plugin]]` array, `[[display]]` array; remove per-source top-level fields |

### Added (new components)

| Component | Description |
|---|---|
| Plugin Registry | TOML file loader + runtime registry (`HashMap<PluginId, Plugin>`) |
| Plugin Instance Store | SQLite table: settings, cached data, last fetch time, last error |
| Settings Schema | Field definition type + AES encryption for sensitive values |
| Template Engine | `minijinja` Rust crate (Jinja2-compatible, supports Liquid-like syntax); TRMNL CSS injected as `<style>` |
| Render Sidecar | Node.js/Bun process (~150 lines): Express + Puppeteer + Sharp, POST /render → PNG |
| Layout Compositor | Slot model, concurrent slot renders, PNG compositing |
| Webhook Receiver | HTTP POST endpoint → cache update → render trigger |
| Display Config | Named display layouts with slot geometry |
| Web UI (optional, later) | Plugin settings form auto-generated from `settings_schema` |

---

## 7. Template Authorship

A plugin template is a Liquid (minijinja) HTML file. The TRMNL CSS framework is
injected automatically — plugin authors use the utility classes directly.

**Example: `templates/river_full.html.liquid`**

```html
<div class="layout--stretch flex--col">
  <div class="title_bar">
    <span class="title">{{ settings.site_name | default: "River" }}</span>
    {% if error %}
      <span class="label text--gray-50">stale data</span>
    {% endif %}
  </div>

  <div class="flex--col layout--center-x" style="flex: 1;">
    <span class="value value--xxxlarge">{{ data.water_level_ft | round: 1 }} ft</span>
    <span class="label">{{ data.streamflow_cfs | round: 0 | number_with_delimiter }} cfs</span>
  </div>

  {% if trip_decision %}
    <div class="divider"></div>
    <div class="flex--row layout--center-x">
      {% if trip_decision.go %}
        <span class="value value--large">GO</span>
      {% else %}
        <span class="value value--large text--gray-50">NO GO</span>
      {% endif %}
      <span class="label">{{ trip_decision.destination }}</span>
    </div>
  {% endif %}

  <div class="label text--gray-50" style="text-align: right;">
    {{ now.local }}
  </div>
</div>
```

No Rust code changed. No recompile. Drop a new `.html.liquid` file and a plugin
definition block, restart (or wait for hot-reload).

---

## 8. Migration Path

The migration has three independent phases. Each phase is shippable; none require
the others to be complete first.

### Phase 1: Open the type system (no new infrastructure)

Change `DataPoint` to `serde_json::Value`. Update `DomainState` to a `HashMap`.
Generalize `RelevantSignals` and evaluation. Port the five sources to return JSON.

**Result:** 11-file cost to add a new source drops to ~2 files (new source file +
plugin definition). No rendering changes. The display still uses the old Rust pixel
renderer for the five existing sources.

### Phase 2: Introduce the rendering pipeline (new infrastructure)

Add the render sidecar. Add the template engine. Port the five existing display
layouts to Liquid templates. The existing Rust `render/` module becomes dead code
and can be deleted.

**Result:** Display authoring no longer requires code. New plugins get a template
file, not a Rust file. The existing sources now render via the new pipeline.

### Phase 3: Plugin model and multi-layout (new features)

Add plugin registry, instance store, settings schema, display config, compositor,
and webhook receiver. Wire the web UI for settings configuration.

**Result:** Community plugins can be installed by dropping files. Multiple plugins
can share one screen. Webhook datasources work. The evaluation engine is fully
data-driven.

---

## 9. Technology Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Template engine (Rust) | `minijinja` | Jinja2-compatible, pure Rust, no native deps, supports custom filters. TRMNL plugins use Liquid (Ruby/JS variant of Jinja2) — minijinja templates are cross-compatible with minimal adaptation. |
| HTML→PNG | Puppeteer + Sharp (Node.js/Bun sidecar) | Proven by inker; full CSS support including TRMNL framework; Floyd-Steinberg dithering in Sharp is well-tested at threshold 140 with the exact TRMNL dither parameters. Puppeteer is the only viable option for full CSS compatibility. |
| Sidecar runtime | Bun | Single binary, fast startup, compatible with Node.js ecosystem, smaller footprint than Node on Raspberry Pi. |
| Plugin/instance store | SQLite (via `rusqlite`) | Zero-dependency, file-based, adequate for single-device use. No Postgres operational overhead. |
| Image compositing | `image` crate (Rust) | Already available in the ecosystem; compositing slot PNGs into a final frame is pixel-copy, not complex image processing. |
| Plugin config format | TOML | Consistent with existing Cascades config style. Human-editable, no schema boilerplate. |

---

## 10. What This Architecture Does NOT Include

**A full web UI for plugin authoring** is out of scope for the first version.
Settings are configured via TOML files. A web UI can be built later using the
`settings_schema` as its data model — the contract is defined, the UI is optional.

**A plugin marketplace / GitHub sync** (inker's `GET /plugins/github-plugin/:slug`)
is out of scope for the first version. The mechanism for importing community
plugins is: copy files. The marketplace layer can be added later without
architectural changes.

**Multi-device support** is out of scope. Cascades is a single-display appliance.
The output layer serves one device. Adding multi-device support would require
per-device display configurations — the slot model supports it, but the device
enrollment flow is not designed here.

**Color e-ink support** is out of scope. The pipeline assumes 1-bit dithered output.
The `mode` parameter and Sharp pipeline can be extended for grayscale or color
displays when needed.
