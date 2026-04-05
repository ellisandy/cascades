# Architecture Comparison: Cascades vs TRMNL — Gap Analysis

> Synthesis of `inker-architecture.md`, `trmnl-plugin-system.md`, and
> `cascades-extensibility-diagnosis.md`. Written to inform the target architecture
> proposal (cs-hok).

---

## 1. Where Cascades Diverges from TRMNL's Approach

The divergence is not superficial — it is architectural root-and-branch.

### 1a. Rendering pipeline

| | Cascades | TRMNL/inker |
|---|---|---|
| How layout is expressed | Rust code (`layout_and_render_display()`) | Liquid template string (in DB or TSX) |
| How it runs | Compiled binary, direct pixel writes | Puppeteer screenshots rendered HTML |
| Output | `PixelBuffer` (1-bit, Rust struct) | PNG file (via Sharp), served from disk |
| Who can change a layout | A developer with Rust toolchain | Anyone who can edit a text template |
| Adding a new display format | Requires Rust code changes + recompile | Write a new `.liquid` file |

Cascades renders entirely in Rust — no HTML, no CSS, no browser. The display layout
is code, not data. This is the deepest divergence and the one that makes everything
else harder.

TRMNL's approach is: HTML → Puppeteer → Sharp → PNG. The HTML layer means the
layout is entirely decoupled from the rendering engine. A plugin author writes a
Liquid template and the rendering pipeline doesn't care what data is in it. The
platform provides the machinery (dithering, sizing, caching) and stays completely
out of the content.

### 1b. Plugin / datasource model

Cascades has no plugin concept. It has a `Source` trait (clean, well-formed), but
the contract stops there. The types a source can return are fixed in a closed enum.
There is no pathway for a "source" to influence how its data is laid out on screen
without modifying the layout code.

TRMNL's model is:
- A **plugin** is a database row (or in byos_next, a JSON registry entry).
- It owns its **Liquid templates** (one per layout variant).
- It owns its **data strategy** (polling / webhook / static).
- It owns its **settings schema** (declarative JSON → UI auto-generated).
- The platform's job is to run the data fetch, render the template, and serve the image.

Adding a plugin in TRMNL requires zero code changes to the platform. The plugin
and the platform interact only through two contracts: the data shape passed to the
template context, and the output image dimensions.

In Cascades, adding a datasource requires 11–13 file changes and a recompile.

### 1c. Layout model

Cascades has a **fixed 4-zone struct** (`HeaderContent` / `HeroContent` /
`DataContent` / `ContextContent`). Slot assignments are hardwired:
`RiverContent` always occupies `data.left`, `FerryContent` always occupies
`data.right`. There is no mechanism for a new source to claim a slot or for the
user to reconfigure the layout.

TRMNL supports four layout sizes (full / half-horizontal / half-vertical / quadrant)
plus a **mixup** system (multi-recipe compositing with named slots: `quarters`,
`top-banner`, `left-rail`, `vertical-halves`, `horizontal-halves`). Each layout
is a separate template. The layout is not in platform code — it is content.

### 1d. User-facing configurability

Cascades has no user-facing configuration surface for datasources. The config file
sets API endpoints and intervals, but there is no concept of "here are the
user-configurable fields for this source" and no UI to configure them.

TRMNL's `settingsSchema` / `configuration_template` is declarative JSON:

```json
[
  { "key": "api_key", "label": "API Key", "type": "password", "required": true },
  { "key": "city",    "label": "City",    "type": "text",     "required": true }
]
```

This schema drives the configuration UI automatically. A plugin author declares
fields; the platform renders the form, stores values encrypted where needed, and
injects them into the Liquid context as `{{ settings.api_key }}`. No UI code to
write.

---

## 2. What TRMNL Does That Makes It More Extensible/User-Friendly

In order of significance:

**1. Template-driven content.** Plugins own their layout as text templates. Neither
the platform code nor a developer needs to be involved when a plugin changes its
display. The rendering engine is a generic pipeline: template + data → image. This
separation means the plugin ecosystem can grow independently of the platform.

**2. Declarative settings schema.** One JSON blob drives: form field generation,
type coercion, required-field validation, encryption for sensitive values, Liquid
variable injection. A plugin author gets a configuration UI for free. Cascades has
none of this.

**3. Open registration.** A plugin in larapaper is a DB row. In byos_next it's a
JSON registry entry and a file. No code compilation, no pull request to the
platform, no restart. TRMNL's GitHub sync endpoint (`GET /plugins/github-plugin/:slug`)
lets users import community plugins directly from the TRMNL ecosystem. The plugin
catalog is a network effect; Cascades's hardcoded source list is an island.

**4. Multiple layout variants per plugin.** A plugin provides templates for `full`,
`half_horizontal`, `half_vertical`, and `quadrant`. The platform selects the right
one based on where in a playlist or mixup the plugin is placed. Cascades has no
equivalent — it has exactly one fixed layout. This is what makes TRMNL's mixup
system possible: multiple plugins at different sizes composited onto one screen.

**5. Data strategies beyond polling.** TRMNL supports webhook (external push) and
static data. Cascades only polls. Webhook support is particularly important for
near-realtime data (stock prices, CI status, alerts) where polling is wasteful or
too slow.

**6. Separation of data fetch and render.** TRMNL caches `data_payload` on the
plugin instance. The render pipeline uses the cached value; data refresh is a
separate cycle. If the data fetch fails, the last successful value is returned.
Cascades couples fetch and render in the same per-interval loop — if a fetch is
slow or fails, it affects the render cycle.

---

## 3. Inker's Rendering Pipeline vs Cascades's `render/` Module

### Cascades `render/layout.rs`

```
Source::fetch() → DataPoint (closed enum)
  → DomainState::apply() (named field write)
  → build_display_layout() (4-zone typed struct)
  → layout_and_render_display() (Rust pixel writes)
  → PixelBuffer (800×480, 1-bit)
```

The render layer directly imports concrete presentation types (`FerryContent`,
`RiverContent`, `TrendArrow`, `WeatherIcon`). It matches on `HeroDecision` variants
and reads named fields. It is a monolithic function that knows the entire content
universe.

### Inker `plugin-renderer.service.ts` → `screen-renderer.service.ts`

```
Liquid template + cached data locals
  → LiquidJS.render() (template + context)
  → HTML fragment
  → buildFullPage() (inject TRMNL CSS + <html>/<body> wrapper)
  → Full HTML page
  → Puppeteer.screenshot() (headless Chromium, network blocked)
  → Raw PNG buffer
  → Sharp: grayscale → contrast normalize → Floyd-Steinberg dither (threshold 140)
  → Negate if mode=device
  → Iterative size reduction if >90KB
  → Final PNG ≤ 90KB
```

The render layer is completely generic. It takes a string (HTML) and produces a
PNG. It has zero knowledge of what data is in the template. The plugin system and
the render pipeline are fully decoupled.

### The critical difference

In Cascades, adding a new visual element requires changing the render layer — you
must add a new type import, handle a new match arm, and write new pixel-drawing
code. In inker, the render layer is frozen: HTML in, PNG out, always. A new plugin
adds a Liquid template and the render layer is untouched.

The tradeoff: Cascades's native rendering is faster and produces a deterministic
output with no browser dependency. Inker's HTML pipeline is slower but infinitely
more flexible and requires no programming to author new layouts.

For a general-purpose e-ink platform that needs to support third-party plugins,
inker's approach is correct. Cascades's approach is appropriate for a single-purpose
closed system — which is exactly what it is today, and exactly what it cannot remain
if extensibility is the goal.

---

## 4. The Gap Between "Cascades Today" and "TRMNL-Like Modern Architecture"

The gap is wide. It is not a matter of adding a few features — the core data model
and rendering approach would need to change.

### What Cascades has that TRMNL-like architecture requires

| Component | Cascades today | TRMNL-like target | Gap |
|---|---|---|---|
| Fetch abstraction | `Source` trait ✓ | Same | None — keep it |
| Domain state | Closed `DataPoint` enum, named fields | Open map, dynamic dispatch | Full replacement |
| Template engine | None | LiquidJS or equivalent | New component |
| HTML render pipeline | None | Puppeteer + Sharp | New component |
| Layout model | Fixed 4-zone typed struct | Slot-based or template-driven | Full replacement |
| Plugin registry | None (5 hardcoded constructors) | Config-driven factory | New component |
| Settings schema | None | Declarative JSON → UI | New component |
| Data strategies | Polling only | Polling + webhook + static | Additions needed |
| Fetch/render decoupling | Coupled in loop | Cached data_payload | Structural change |
| Multi-layout support | One fixed layout | 4 sizes + mixup | New feature |
| User configuration | Config file only | Per-plugin settings UI | New component |

Five of the ten rows require new components that don't exist at all. Two require
full replacement of existing components. Only the fetch abstraction (`Source` trait)
carries forward without change.

### What can be preserved

- The `Source` trait and individual source implementations are clean and can be kept.
- The evaluation system (trip go/no-go) has no equivalent in TRMNL and is
  Cascades-specific value. It should be preserved but generalized.
- The data flow structure (fetch → domain → presentation → render) is correct; the
  problem is that each layer uses static types instead of dynamic dispatch.

### What must change

The most disruptive change is introducing an HTML rendering pipeline. This
determines most downstream decisions: if layout is expressed as HTML templates,
then the plugin system, the settings schema, and the multi-layout support all fall
out naturally. If layout remains as compiled Rust, none of TRMNL's extensibility
patterns are applicable.

---

## 5. Top Architectural Problems to Solve (Ranked)

### Problem 1: No HTML rendering pipeline

**Impact:** Blocks all user-authored content, all plugin templates, all TRMNL ecosystem compatibility.

The render layer is locked to Rust pixel writes. Every new visual element requires
a developer and a recompile. This is the foundational blocker — nothing else in
the TRMNL model is possible without a generic HTML→image pipeline.

**Target:** Introduce a Puppeteer (or Takumi/Satori or similar) pipeline behind the
existing rendering stage. Render layer becomes: HTML string → PNG. All current
display logic moves into a Liquid template. The render module becomes a service,
not an application layer.

---

### Problem 2: Closed `DataPoint` enum

**Impact:** Every new datasource costs 11–13 file changes and 4 compiler-enforced
exhaustion points. The platform cannot scale with the source count.

The fetch layer (`Source` trait) is already extensible. The problem is the return
type: `DataPoint` is a closed sum type. A source cannot return a new kind of data
without modifying the enum, which cascades through `DomainState`, `presentation`,
`evaluation`, and `render`.

**Target:** Replace `DataPoint` with `Box<dyn SourceValue>` or `HashMap<SourceId, Box<dyn Any>>`.
`DomainState` becomes `HashMap<SourceId, Arc<dyn SourceValue>>`. The named fields
(`weather`, `river`, `ferry`, `trail`, `road`) are eliminated. Sources register by
ID, not by type.

This is the highest-leverage code change (unblocks all downstream generalization)
that doesn't require introducing new infrastructure.

---

### Problem 3: No plugin/template system

**Impact:** No community ecosystem, no user-authored content, no runtime
extensibility. Adding content requires code.

There is no concept of a plugin, no Liquid template engine, no way to define what
appears on screen without writing Rust. This is the feature gap users would feel
most directly.

**Target:** A plugin registry (config-driven or DB-backed), a Liquid template engine
(LiquidJS or similar), and the TRMNL CSS framework. A plugin is a
`(data-strategy, template, settings-schema)` triple. The rendering pipeline takes a
plugin instance + data context → HTML → PNG. Cascades's current hardcoded sources
become the first five "built-in plugins."

---

### Problem 4: Fixed DisplayLayout zones

**Impact:** New datasources have no slot. Zone assignments are hardcoded in the
render function. Multi-plugin layouts (TRMNL's mixup model) are impossible.

`DataContent` and `ContextContent` are typed structs with named fields for specific
source types (`river`, `ferry`, `trail`, `road`). A 6th source has nowhere to go.

**Target:** Replace the 4-zone struct with a slot-based layout model. A slot is a
region (x, y, width, height) bound to a plugin instance. The layout becomes a list
of slots; the renderer composites each slot's PNG into the final image. This enables
the TRMNL mixup model and decouples slot assignment from source type.

---

### Problem 5: No declarative settings schema

**Impact:** Users cannot configure datasources without editing config files.
Community plugins can't declare their own configuration fields.

TRMNL's `settingsSchema` / `configuration_template` is what makes plugins self-contained:
the plugin describes what it needs from the user, and the platform handles the rest
(form rendering, validation, encryption, injection). Without this, every plugin
requires custom UI code.

**Target:** A `settings_schema` field on the plugin model (array of field
definitions: key, label, type, required, encrypted). The configuration UI is
generated from the schema. Settings values are injected into the Liquid context
at render time. Sensitive fields (API keys, tokens) are AES-encrypted at rest.

---

### Problem 6: Fetch/render coupling

**Impact:** Slow fetches block render. Failed fetches produce empty displays. No
graceful degradation.

Today, the scheduler calls `fetch()` and then immediately uses the result in the
render pass. There is no caching layer, no "last successful value," no separation
of concerns.

**Target:** Cache `cachedData` on the plugin instance (equivalent to TRMNL's
`data_payload`). The render pass always uses cached data. The fetch cycle runs
independently and updates the cache. A failed fetch returns the previous value,
not an empty display. This also enables webhook support (webhooks update the cache;
the render pass reads it).

---

## Summary Judgment

Cascades's architecture is internally coherent and well-engineered for what it does.
The `Source` trait, the evaluation system, and the layered pipeline are clean work.
The problem is the closed-world assumption that runs through every layer: there are
exactly five sources, exactly one layout, exactly one display, and all of this is
known at compile time.

TRMNL's model inverts this assumption. The platform provides infrastructure
(rendering, scheduling, configuration, caching). Content is data, not code. Plugins
are rows in a table, not enum variants in a sum type.

The gap between the two architectures is real and substantial. The path forward
requires introducing an HTML rendering pipeline (the foundational blocker), opening
the domain type system (the quickest high-leverage change), and building a plugin
model on top of those two foundations. The Source trait and evaluation logic are
assets — everything else between fetch and pixel needs to change.
