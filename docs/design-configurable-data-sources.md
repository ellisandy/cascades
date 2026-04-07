# Design: Configurable Data Sources and Field-Level Layout

## Status

**Draft** -- April 2026

## Problem

The current system has five hardcoded plugin types (river, weather, ferry, trail, road), each with:
- A dedicated Rust source module in `src/sources/` implementing the `Source` trait
- Hardcoded Liquid templates per variant (`river_full.html.liquid`, etc.)
- Fixed JSON shapes baked into templates (`{{ data.water_level_ft }}`)
- Plugin instances seeded from `config.toml` via `seed_from_config()`

Adding a new data source requires writing Rust code, deploying a new binary, and creating templates. Users cannot connect to arbitrary APIs, pick individual fields from a JSON response, or control how each datum is rendered and placed on the canvas.

## Goals

1. Let users point at any HTTP/JSON endpoint and fetch data on a schedule.
2. Let users browse the fetched JSON and select individual fields.
3. Make each selected field an independently positionable canvas item with its own formatting.
4. Preserve all existing plugin functionality as "presets" that work without configuration.
5. Ship incrementally -- each phase is independently useful.

## Non-Goals

- Real-time streaming / WebSocket sources (polling only).
- Non-JSON response formats (XML, CSV, Protobuf).
- Multi-step API workflows (OAuth token refresh, pagination).
- User-defined Liquid templates (fields use format strings, not templates).

---

## 1. Data Source Abstraction

### Current State

Each source is a Rust struct implementing the `Source` trait (`src/sources/mod.rs`):

```rust
pub trait Source: Send {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn refresh_interval(&self) -> Duration;
    fn fetch(&self) -> Result<serde_json::Value, SourceError>;
}
```

Sources are compiled in. The scheduler spawns a dedicated thread per source, calls `fetch()` on the configured interval, and writes the result to `InstanceStore::update_cached_data()`.

### Proposed: GenericHttpSource

A new `GenericHttpSource` implements `Source` with user-configurable parameters stored in the `data_sources` table (see section 4). At runtime, the scheduler creates one `GenericHttpSource` per row.

```rust
pub struct GenericHttpSource {
    id: String,
    name: String,
    url: String,
    method: HttpMethod,        // GET or POST
    headers: Vec<(String, String)>,
    body_template: Option<String>,
    refresh_interval: Duration,
    /// JSONPath applied to the response before storing.
    /// e.g., "$.value.timeSeries[0]" to extract a subtree.
    response_root_path: Option<String>,
}

impl Source for GenericHttpSource {
    fn id(&self) -> &str { &self.id }
    fn name(&self) -> &str { &self.name }
    fn refresh_interval(&self) -> Duration { self.refresh_interval }

    fn fetch(&self) -> Result<Value, SourceError> {
        let resp = self.http_call()?;
        let json: Value = serde_json::from_str(&resp)?;
        match &self.response_root_path {
            Some(path) => jsonpath_extract(&json, path),
            None => Ok(json),
        }
    }
}
```

**Refresh intervals.** Stored per-source in the database. Minimum 30 seconds enforced at the API layer to prevent abuse.

**Authentication.** Supported via configurable headers. API keys go in a header value (e.g., `Authorization: Bearer <key>`). Header values containing secrets are stored in `encrypted_headers` (see schema). OAuth is out of scope -- users who need it can proxy through a middleware.

**Error handling.** Mirrors the existing pattern: on fetch failure, `last_error` is set on the source row; `cached_data` retains the previous successful value. The admin UI shows a "stale data" badge (already implemented for plugin slots).

**Retry.** Exponential backoff with jitter, matching the existing sources. Max 4 retries, max delay 5 minutes.

### Relationship to Existing Sources

The five built-in sources continue to exist as compiled Rust code. They are **not** migrated to `GenericHttpSource` because their parsers do non-trivial transformations (NWIS XML-in-JSON extraction, NWS station resolution, WSDOT nested schedule parsing). They remain as-is and their outputs feed into the same `plugin_instances.cached_data` column.

In the admin UI, built-in sources appear alongside generic sources in the data source list. The difference is that built-in sources show their specific settings (site_id, park_code) while generic sources show the HTTP configuration panel.

---

## 2. Field Selection and Mapping

### Concept

When a data source fetches JSON, the response is stored whole in `cached_data`. A **field mapping** defines a JSONPath expression that extracts a single scalar value from the cached response. Each mapping becomes a placeable canvas item.

Example: a USGS river source returns:
```json
{
  "site_id": "12200500",
  "site_name": "Skagit River Near Mount Vernon, WA",
  "water_level_ft": 11.87,
  "streamflow_cfs": 8750.0,
  "timestamp": 1774900500
}
```

A user could create field mappings:
- `$.water_level_ft` -> displayed as "11.9 ft"
- `$.streamflow_cfs` -> displayed as "8,750 cfs"
- `$.site_name` -> displayed as a title label

### JSONPath Subset

Full JSONPath is complex. We support a practical subset:

| Syntax | Example | Meaning |
|--------|---------|---------|
| `$.key` | `$.water_level_ft` | Top-level key |
| `$.a.b` | `$.properties.temperature` | Nested key |
| `$.a[0]` | `$.values[0]` | Array index |
| `$.a[0].b` | `$.timeSeries[0].sourceInfo.siteName` | Chained |

No wildcards (`$..`), filters (`[?()]`), or slices (`[0:3]`). These cover ~95% of practical API field extraction. Implementation is a simple recursive descent -- no external JSONPath library needed.

### JSON Response Explorer

The admin UI provides a **tree explorer** for the cached JSON response. When the user clicks a data source, the explorer renders the JSON structure as a collapsible tree. Clicking a leaf node auto-generates the JSONPath and prompts the user to create a field mapping.

For sources that haven't fetched yet, the UI shows a "Fetch Now" button that triggers an immediate fetch, stores the response, and populates the explorer.

---

## 3. Per-Field Rendering

### New Layout Item Type: `DataField`

Each field mapping becomes a `DataField` layout item on the canvas. It is rendered like `StaticText` (HTML snippet -> sidecar -> PNG) but its text content is resolved at render time from live data.

```rust
pub enum LayoutItem {
    PluginSlot { /* existing */ },
    StaticText { /* existing */ },
    StaticDateTime { /* existing */ },
    StaticDivider { /* existing */ },
    DataField {
        id: String,
        z_index: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        /// References data_source_fields.id
        field_mapping_id: String,
        font_size: i32,
        /// Format string with `{{value}}` placeholder.
        /// e.g., "{{value}} ft", "{{value | round(1)}} cfs"
        format_string: String,
        /// Optional label displayed above/before the value.
        label: Option<String>,
        orientation: Option<String>,
    },
}
```

### Render Pipeline

At composition time, `DataField` items:

1. Look up `field_mapping_id` in the `data_source_fields` table to get the `data_source_id` and `json_path`.
2. Read `cached_data` from the source (either `plugin_instances` for built-in sources or `data_sources` for generic ones).
3. Evaluate the JSONPath to extract the raw value.
4. Apply the format string: replace `{{value}}` with the extracted value. Support basic filters: `round(N)`, `number_with_delimiter`, `uppercase`, `lowercase`, `date(format)`.
5. Generate an HTML snippet (same pattern as `render_static_text`) and send to the sidecar.

```rust
async fn render_data_field(
    field: &DataFieldItem,
    field_store: &FieldMappingStore,
    source_cache: &SourceCache,
    sidecar_url: &str,
    mode: &str,
) -> Result<Vec<u8>, CompositorError> {
    let mapping = field_store.get_mapping(&field.field_mapping_id)?;
    let cached_json = source_cache.get_cached_data(&mapping.data_source_id)?;
    let raw_value = jsonpath_extract(&cached_json, &mapping.json_path)?;
    let formatted = apply_format(&field.format_string, &raw_value);

    let html = /* generate HTML with formatted text, font_size, dimensions */;
    call_sidecar(sidecar_url, html, width, height, &field.id, mode).await
}
```

### Format String DSL

Format strings use `{{value}}` as the placeholder with optional pipe filters:

```
{{value}} ft           -> "11.87 ft"
{{value | round(1)}}   -> "11.9"
{{value | round(0) | number_with_delimiter}} cfs  -> "8,750 cfs"
{{value | uppercase}}  -> "SKAGIT RIVER..."
{{value | date("%H:%M")}}  -> "14:30"
```

This is intentionally simpler than full Liquid. The format string is evaluated in Rust at render time, not by the Liquid engine.

---

## 4. Data Model / Storage

### Current Schema

```sql
-- Plugin instances (src/instance_store/mod.rs)
CREATE TABLE plugin_instances (
    id                  TEXT PRIMARY KEY,
    plugin_id           TEXT NOT NULL,
    settings            TEXT NOT NULL,       -- JSON
    encrypted_settings  TEXT,                -- JSON (future)
    cached_data         TEXT,                -- JSON (last fetch result)
    last_fetched_at     INTEGER,
    last_error          TEXT
);

-- Layout items (src/layout_store/mod.rs)
CREATE TABLE layout_items (
    id                  TEXT PRIMARY KEY,
    layout_id           TEXT NOT NULL,
    item_type           TEXT NOT NULL,       -- plugin_slot|static_text|static_divider|static_datetime
    z_index             INTEGER NOT NULL DEFAULT 0,
    x                   INTEGER NOT NULL DEFAULT 0,
    y                   INTEGER NOT NULL DEFAULT 0,
    width               INTEGER NOT NULL DEFAULT 800,
    height              INTEGER NOT NULL DEFAULT 480,
    plugin_instance_id  TEXT,
    layout_variant      TEXT,
    text_content        TEXT,
    font_size           INTEGER,
    orientation         TEXT
);
```

### Proposed New Tables

```sql
-- Generic user-configurable data sources
CREATE TABLE data_sources (
    id                  TEXT PRIMARY KEY,    -- UUID
    name                TEXT NOT NULL,       -- Human-readable label
    url                 TEXT NOT NULL,       -- Endpoint URL
    method              TEXT NOT NULL DEFAULT 'GET',  -- GET or POST
    headers             TEXT,                -- JSON array of {key, value} pairs
    encrypted_headers   TEXT,                -- JSON array (for auth tokens, API keys)
    body_template       TEXT,                -- Optional POST body
    response_root_path  TEXT,                -- JSONPath to extract before storing
    refresh_interval_secs INTEGER NOT NULL DEFAULT 300,
    cached_data         TEXT,                -- JSON (last successful fetch)
    last_fetched_at     INTEGER,
    last_error          TEXT,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);

-- Field mappings: extract a single value from a source's cached_data
CREATE TABLE data_source_fields (
    id                  TEXT PRIMARY KEY,    -- UUID
    data_source_id      TEXT NOT NULL,       -- FK -> data_sources.id OR plugin_instances.id
    source_type         TEXT NOT NULL DEFAULT 'generic',  -- 'generic' or 'builtin'
    name                TEXT NOT NULL,       -- Human label, e.g., "Water Level"
    json_path           TEXT NOT NULL,       -- e.g., "$.water_level_ft"
    created_at          INTEGER NOT NULL
);
```

### Changes to Existing Tables

The `layout_items` table gains new columns for `DataField` items:

```sql
ALTER TABLE layout_items ADD COLUMN field_mapping_id TEXT;
-- References data_source_fields.id (for item_type = 'data_field')

ALTER TABLE layout_items ADD COLUMN format_string TEXT;
-- e.g., "{{value}} ft" (for item_type = 'data_field')

ALTER TABLE layout_items ADD COLUMN label TEXT;
-- Optional label text (for item_type = 'data_field')
```

The existing `item_type` values (`plugin_slot`, `static_text`, `static_divider`, `static_datetime`) are preserved. A new value `data_field` is added.

### Migration Strategy

Migrations run in `LayoutStore::migrate()` and `InstanceStore::migrate()` using `CREATE TABLE IF NOT EXISTS` and `ALTER TABLE ... ADD COLUMN` (SQLite silently ignores if column exists). No data migration is needed -- the new tables start empty and the new column on `layout_items` is nullable.

---

## 5. Admin UI

### Data Source Management Panel

A new **Sources** tab in the admin toolbar opens a side panel for managing data sources.

**Source List View:**
- Shows all data sources (built-in + generic) with name, status badge (OK/error/never-fetched), and last fetch time.
- "Add Source" button opens the source configuration form.
- Built-in sources show a "Preset" badge and limited configuration (just their existing settings).

**Source Configuration Form (generic sources):**

| Field | Description |
|-------|-------------|
| Name | Human-readable label |
| URL | Endpoint URL |
| Method | GET / POST toggle |
| Headers | Key-value pair editor (add/remove rows) |
| Body | Textarea (POST only) |
| Response Root Path | Optional JSONPath to extract a subtree |
| Refresh Interval | Dropdown: 30s, 1m, 5m, 15m, 30m, 1h |

"Test" button performs a one-shot fetch and displays the response (or error) immediately.

### JSON Response Explorer

Below the source configuration form, a collapsible JSON tree viewer displays the most recent cached response. Implementation:

```
data_sources.cached_data (JSON)
    ├─ site_id: "12200500"          [+ Add Field]
    ├─ site_name: "Skagit River..." [+ Add Field]
    ├─ water_level_ft: 11.87        [+ Add Field]
    ├─ streamflow_cfs: 8750.0       [+ Add Field]
    └─ timestamp: 1774900500        [+ Add Field]
```

Clicking "+ Add Field" on a leaf node:
1. Auto-populates the JSONPath (e.g., `$.water_level_ft`).
2. Opens a field configuration dialog with name, format string, font size.
3. Creates a `data_source_fields` row and a `DataField` layout item on the canvas at the next available position.

For nested objects, the tree expands on click:
```
    ├─ properties: {object}
    │   ├─ temperature: {object}
    │   │   ├─ value: 12.5          [+ Add Field]  ($.properties.temperature.value)
    │   │   └─ unitCode: "wmoUnit:degC"
    │   └─ windSpeed: {object}
    │       └─ value: 5.2           [+ Add Field]  ($.properties.windSpeed.value)
```

### Per-Field Formatting Controls

When a `DataField` item is selected on the canvas, the properties panel shows:

| Property | Control |
|----------|---------|
| Source | Read-only: source name |
| Field | Read-only: JSON path |
| Format | Text input: `{{value}} ft` |
| Label | Text input: optional label above value |
| Font Size | Number input |
| Position | x, y number inputs |
| Size | width, height number inputs |

The format string preview shows the current value with formatting applied, updating live as the user types.

### Integration with Existing Canvas

`DataField` items participate in the same drag-drop, resize, z-index, snap-to-grid, collision detection, and undo/redo systems as existing item types. They appear in the canvas with their formatted value (or a placeholder if no data is cached).

The palette's "STATIC" section gains a "Data Field" entry (or the user creates fields via the JSON explorer). The palette section could be renamed to "ELEMENTS" to accommodate the new item type.

---

## 6. Migration / Backwards Compatibility

### Existing Plugins Become "Presets"

The five built-in sources (river, weather, ferry, trail, road) and their Liquid templates continue to work unchanged. They are the default experience -- zero configuration required.

In the admin UI, a new "Source Presets" section offers one-click creation of common configurations:

| Preset | Creates |
|--------|---------|
| USGS River Gauge | Generic source pointing at NWIS API, pre-configured JSONPath fields for water level and streamflow |
| NOAA Weather | Generic source pointing at NWS API, fields for temperature, wind, conditions |
| WSDOT Ferries | Generic source pointing at WSDOT API, fields for next departure, vessel name |

Presets populate the source URL, headers, response root path, and default field mappings. Users can then customize or add additional fields.

**Key principle:** presets create generic sources, they don't wrap built-in sources. This means the built-in Rust source modules can eventually be deprecated once the generic system proves reliable, but there's no rush -- both systems coexist.

### Template Rendering Path Unchanged

`PluginSlot` items continue to render via Liquid templates through the sidecar. `DataField` items use the simpler format-string-to-HTML path. Both produce PNGs that the compositor blits onto the frame. No changes to `composite_to_png()`.

### Data Flow Comparison

**Current (PluginSlot):**
```
Source.fetch() -> InstanceStore.cached_data -> TemplateEngine.render(liquid) -> Sidecar -> PNG
```

**New (DataField):**
```
GenericHttpSource.fetch() -> data_sources.cached_data -> JSONPath extract -> format_string -> HTML -> Sidecar -> PNG
```

Both paths converge at the compositor. The sidecar and compositor are unaware of which path produced a given PNG tile.

---

## 7. Phased Implementation Plan

### Phase 1: DataField Layout Item (no new data sources)

**Scope:** Add `DataField` as a new `LayoutItem` variant that reads from existing `plugin_instances.cached_data` via JSONPath.

**What ships:**
- `DataField` variant in `LayoutItem` enum
- `data_source_fields` table for field mappings against existing plugin instances
- JSONPath extraction (simple subset)
- Format string evaluation
- Compositor renders `DataField` items via sidecar
- Admin UI: manual field creation dialog (specify source ID, JSONPath, format)
- Properties panel for DataField items

**Dependencies:** None (works with existing built-in sources).

**Value:** Users can extract individual values from existing plugin data and place them freely on the canvas. e.g., show just the water level number in large text at the top, streamflow below, without needing the full river template.

**Beads:** 3-4 implementation beads (schema + Rust types, compositor integration, admin UI field dialog, properties panel).

### Phase 2: JSON Response Explorer

**Scope:** Add the tree-based JSON explorer to the admin UI for discovering and creating field mappings visually.

**What ships:**
- JSON tree viewer component in admin.html
- "Browse Fields" button on plugin instances
- Click-to-create field mapping from tree nodes
- Auto-generated JSONPath from tree selection

**Dependencies:** Phase 1 (field mappings must exist).

**Value:** Users can visually explore API responses and create field mappings without writing JSONPath by hand.

**Beads:** 1-2 implementation beads (tree viewer JS, integration with field creation).

### Phase 3: Generic Data Sources

**Scope:** Add the `data_sources` table and `GenericHttpSource` so users can connect to arbitrary HTTP/JSON endpoints.

**What ships:**
- `data_sources` table
- `GenericHttpSource` implementing the `Source` trait
- Source management API endpoints (CRUD)
- Scheduler integration: spawn `GenericHttpSource` instances at runtime
- Admin UI: source configuration panel (URL, method, headers, interval)
- "Test" button for one-shot fetch
- JSON explorer works for generic sources

**Dependencies:** Phase 1 + Phase 2 (field mappings and explorer).

**Value:** Users can add any HTTP/JSON API as a data source and display individual fields on the canvas.

**Beads:** 4-5 implementation beads (schema + store, HTTP fetcher, scheduler integration, admin UI source panel, API endpoints).

### Phase 4: Source Presets

**Scope:** Pre-built configurations for common APIs that users can add with one click.

**What ships:**
- Preset definitions (USGS, NOAA, WSDOT, etc.) as JSON/TOML config
- "Add Preset" button in admin UI
- Pre-populated URL, headers, response root path, and default field mappings

**Dependencies:** Phase 3 (generic sources).

**Value:** Common data sources work out of the box without manual URL/header configuration.

**Beads:** 1-2 implementation beads (preset definitions, admin UI integration).

### Phase 5: Encrypted Headers and Auth

**Scope:** Secure storage for API keys and auth tokens in header values.

**What ships:**
- AES encryption for `encrypted_headers` column
- Key management (derive from a server-side secret)
- Admin UI: header values marked as "secret" are masked and stored encrypted

**Dependencies:** Phase 3 (generic sources with headers).

**Value:** API keys are not stored in plaintext in the database.

**Beads:** 1-2 implementation beads (encryption layer, admin UI secret fields).

---

## API Endpoints (New)

### Data Sources

```
GET    /api/admin/sources              -- List all data sources
POST   /api/admin/sources              -- Create generic data source
GET    /api/admin/sources/:id          -- Get source details + cached response
PUT    /api/admin/sources/:id          -- Update source configuration
DELETE /api/admin/sources/:id          -- Delete source and its field mappings
POST   /api/admin/sources/:id/fetch    -- Trigger immediate fetch
```

### Field Mappings

```
GET    /api/admin/sources/:id/fields   -- List field mappings for a source
POST   /api/admin/sources/:id/fields   -- Create field mapping
PUT    /api/admin/fields/:id           -- Update field mapping
DELETE /api/admin/fields/:id           -- Delete field mapping + associated layout items
```

All endpoints require the same `X-Api-Key` or session cookie auth as existing admin endpoints.

---

## Key Files to Modify

| Phase | File | Change |
|-------|------|--------|
| 1 | `src/layout_store/mod.rs` | Add `DataField` variant to `LayoutItem`, `data_source_fields` table |
| 1 | `src/compositor.rs` | Handle `DataField` in `compose()` |
| 1 | `src/api.rs` | Serialize/deserialize `DataField` items, field mapping CRUD |
| 1 | `templates/admin.html` | DataField in palette, properties panel, field creation dialog |
| 2 | `templates/admin.html` | JSON tree explorer component |
| 3 | `src/sources/mod.rs` | `GenericHttpSource` implementation |
| 3 | `src/main.rs` | Scheduler spawns generic sources at startup |
| 3 | `src/api.rs` | Source CRUD endpoints |
| 3 | `src/config/mod.rs` | (No change -- generic sources are DB-only, not config.toml) |

---

## Risks and Mitigations

**Risk: JSONPath complexity creep.** Users may need features beyond the supported subset.
*Mitigation:* The simple subset covers the vast majority of flat-to-moderately-nested APIs. Document the supported syntax clearly. Add features later if demanded.

**Risk: Sidecar overload from many DataField items.** Each DataField triggers a sidecar render call.
*Mitigation:* DataField items are small (just text). Consider batching multiple DataField renders into a single sidecar call with an HTML table, or caching rendered PNGs until the underlying data changes.

**Risk: API rate limiting on user-configured endpoints.**
*Mitigation:* Enforce minimum refresh interval (30s). Show last-fetch-time and error status prominently. Users are responsible for their API quotas.

**Risk: Large JSON responses consuming memory.**
*Mitigation:* Cap cached response size at 1MB. Truncate or reject larger responses with a clear error message.
