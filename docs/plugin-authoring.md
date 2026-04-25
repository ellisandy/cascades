# Plugin Authoring Guide

A plugin binds a data source to one or more display templates. This guide covers
everything needed to write a plugin from scratch: the TOML definition format,
Liquid template context variables, available CSS utility classes, and a complete
end-to-end example.

---

## How plugins work

1. A TOML file in `config/plugins.d/` defines the plugin: its ID, data source,
   polling interval, template names, evaluation criteria, and settings schema.
2. Liquid template files in `templates/` render the plugin's data as HTML.
3. The compositor looks up the template by convention:
   `{plugin_id}_{variant}` → `templates/{plugin_id}_{variant}.html.liquid`.
4. The render sidecar (Bun + Puppeteer) screenshots the HTML and returns a PNG.
5. `config/plugins.d/` is hot-reloaded: drop a new `.toml` file and send `SIGHUP`
   — no restart needed. Template files require a restart to reload.

---

## Plugin TOML format

Create a file at `config/plugins.d/myplugin.toml`.

### Minimal example

```toml
[[plugin]]
id = "tide"
name = "Tide Gauge"
description = "Current tide level from a NOAA CO-OPS station."
source = "noaa_tides"
refresh_interval_secs = 600
data_strategy = "polling"

template_full = "templates/tide_full.html.liquid"
```

### Full field reference

```toml
[[plugin]]
# ── Identity ────────────────────────────────────────────────────────────────
id = "tide"                     # Stable machine ID. Must be unique. Used in
                                # template lookup and webhook URLs.
name = "Tide Gauge"             # Human-readable name shown in the UI.
description = "..."             # Short description. Optional; defaults to "".

# ── Data source ─────────────────────────────────────────────────────────────
source = "noaa_tides"           # Source implementation ID. The value must match
                                # a registered Source type in the Rust backend.
refresh_interval_secs = 600     # How often the scheduler calls fetch(). Default: 300.
data_strategy = "polling"       # One of: "polling", "webhook", "static".
                                # "polling" — server polls on refresh_interval_secs.
                                # "webhook" — data pushed via POST /api/webhook/:id.
                                # "static"  — no fetch; data set once at startup.

# ── Templates ────────────────────────────────────────────────────────────────
# Each variant maps to a template file. The compositor builds the template name
# as "{id}_{variant}" so these fields are for documentation only — the actual
# lookup uses the naming convention.
template_full             = "templates/tide_full.html.liquid"             # 800×480
template_half_horizontal  = "templates/tide_half_horizontal.html.liquid"  # 800×240
template_half_vertical    = "templates/tide_half_vertical.html.liquid"    # 400×480
template_quadrant         = "templates/tide_quadrant.html.liquid"         # 400×240

# ── Trip evaluation criteria ─────────────────────────────────────────────────
# Each [[plugin.criteria]] entry declares one evaluable metric. Empty means the
# plugin never contributes to a go/no-go decision.
[[plugin.criteria]]
key         = "level_ft"     # JSON key path into the plugin's cached data.
label       = "Tide level"   # Human-readable label for display.
operator    = "lte"          # "lte", "gte", "eq", or "between".
threshold   = 4.0            # Numeric threshold.
unit        = "ft"           # Unit suffix for display (e.g. "ft", "°F").
go_direction = "below"       # "below" or "above" — which side is safe.

# ── Settings schema ──────────────────────────────────────────────────────────
# Each [[plugin.settings_schema]] entry declares one user-configurable field.
# These fields are stored per plugin instance and passed as `settings` to templates.
[[plugin.settings_schema]]
key         = "station_id"   # Machine key stored in the instance settings JSON.
label       = "NOAA Station ID"  # Human-readable label.
type        = "text"         # "text", "number", "password", or "select".
required    = true           # Whether the field must be provided.
placeholder = "8443970"      # UI placeholder text. Optional.
default     = ""             # Default value when user provides none. Optional.
```

### Data strategies

| `data_strategy` | How data arrives |
|---|---|
| `polling` | Server calls the source's `fetch()` at `refresh_interval_secs` |
| `webhook` | External system pushes data via `POST /api/webhook/:plugin_instance_id` |
| `static` | No fetching; data set at startup and never updated |

---

## Theming knobs (Phase 9)

Plugin authors can declare a small bounded set of theming knobs alongside
data settings. The user picks values per-layout in the admin "Plugin
customization" inspector; the template binds them via CSS custom properties
and falls back to author-declared defaults via the `default` filter.

**Theming-typed fields share the `[[plugin.settings_schema]]` shape but are
stored differently from data settings:**

| Aspect | Data settings (`text`/`number`/...) | Theming knobs (`text_style`/`color`/`toggle`) |
|---|---|---|
| Storage | `instance_store.settings` (per-instance) | `Group.style_overrides` (**per-layout**) |
| Liquid binding | `{{ settings.foo }}` | `{{ style.foo }}` |
| Use for | Data fetch parameters, identity | Visual customization |

### Theming field types

| Type | Value shape | Suggested template binding |
|---|---|---|
| `color` | string (CSS hex) | `{{ style.<key> \| default("#000") }}` |
| `toggle` | bool | `{% if style.<key> \| default(false) %}` |
| `text_style` | object with `family`, `size`, `weight`, `italic`, `underline`, `color` | `{{ style.<key>.color \| default("#000") }}` etc. |

### Authoring contract

- **Always supply a `default(...)`.** The manifest's `default` field is a
  placeholder hint for the admin form; it does NOT back the rendered value.
  The template's `default(...)` filter is the *single* declaration of your
  visual default.
- **Don't expose subkeys you don't bind.** Just don't reference them. The
  admin inspector shows the full shape regardless; users can leave them blank.
- **Theming is opt-in.** A plugin with no theming-typed schema entries gets
  no Plugin Customization UI in the inspector. Existing plugins keep working
  unchanged.

### Reference example

`config/plugins.d/weather.toml`:

```toml
[[plugin.settings_schema]]
key = "temp_style"
label = "Temperature style"
type = "text_style"

[[plugin.settings_schema]]
key = "accent_color"
label = "Accent colour"
type = "color"
```

`templates/weather_full.html.liquid`:

```liquid
<style>
  .weather-root {
    --temp-color: {{ style.temp_style.color | default("#000000") }};
    --temp-size: {{ style.temp_style.size | default(96) }}px;
    --accent: {{ style.accent_color | default("#000000") }};
  }
</style>
<div class="weather-root">
  <span style="color: var(--temp-color); font-size: var(--temp-size);">
    {{ data.temperature_f | round(0) }}°F
  </span>
</div>
```

### Theming with un-decomposed plugins

If your plugin's template uses internal layout logic that shouldn't be broken
apart (flex rows, conditional trip-decision blocks), declare a single
`default_elements` entry with `kind = "plugin_slot"`. The admin's spawn-on-drop
creates a Group around your monolithic template; the Group is what holds
`style_overrides`, even though it has only one PluginSlot child.

```toml
[[plugin.default_elements]]
kind = "plugin_slot"
x = 0
y = 0
width = 800
height = 480
orientation = "full"   # selects the template variant
```

---

## Template naming convention

The compositor resolves templates by plugin instance ID and layout variant:

```
{plugin_id}_{variant}  →  templates/{plugin_id}_{variant}.html.liquid
```

| Variant | File suffix | Dimensions |
|---|---|---|
| `full` | `_full.html.liquid` | 800×480 |
| `half_horizontal` | `_half_horizontal.html.liquid` | 800×240 |
| `half_vertical` | `_half_vertical.html.liquid` | 400×480 |
| `quadrant` | `_quadrant.html.liquid` | 400×240 |

You only need to provide the variants your plugin actually uses. If a display
config requests a variant with no matching template, the compositor returns an
error for that slot.

---

## Liquid template context

Every template receives these top-level variables:

### `data`

The plugin's most recently fetched JSON, deserialized as a Liquid object. The
shape depends on what the source returned.

```liquid
{{ data.level_ft | round(1) }}   {# e.g. "4.2" #}
{{ data.station_name }}
```

If the last fetch failed and stale data is being shown, `error` is non-null
(see below).

### `settings`

The plugin instance's user-configured settings as a key→value map.

```liquid
{{ settings.station_id | default("8443970") }}
{{ settings.display_name | default("Tide") }}
```

### `trip_decision`

Set when the plugin has at least one `[[plugin.criteria]]` entry and an
evaluation has run. `null` for plugins with no criteria.

```liquid
{% if trip_decision %}
  {% if trip_decision.go %}
    <span class="value value--large">GO</span>
  {% else %}
    <span class="value value--large text--gray-50">NO GO</span>
  {% endif %}
  <span class="label">{{ trip_decision.destination | default("") }}</span>
{% endif %}
```

`trip_decision` fields:

| Field | Type | Description |
|---|---|---|
| `go` | bool | `true` if all criteria pass |
| `destination` | string or null | Destination name from evaluation |
| `results` | array | Per-criterion results (each has `key`, `pass`, `reason`) |

### `now`

Current time at render time.

| Field | Example | Description |
|---|---|---|
| `now.unix` | `1712345678` | Unix timestamp (seconds) |
| `now.iso` | `"2026-04-05T12:00:00Z"` | ISO-8601 UTC string |
| `now.local` | `"Sun Apr 5 12:00"` | Human-readable local string |

```liquid
<div class="label text--gray-50">{{ now.local }}</div>
```

### `error`

`null` on a successful fetch. Non-null string when the last fetch failed and
stale data is being shown.

```liquid
{% if error %}
  <span class="label text--gray-50">stale data</span>
{% endif %}
```

### `style`

Per-layout user customisation values for theming-typed settings fields. See
the **Theming** section below — `style` is the read-side counterpart to
`[[plugin.settings_schema]]` entries with `type = "text_style"`, `"color"`,
or `"toggle"`.

```liquid
<style>
  :root {
    --temp-color: {{ style.temp_style.color | default("#000") }};
    --temp-size:  {{ style.temp_style.size  | default(96) }}px;
    --accent:     {{ style.accent_color     | default("#000") }};
  }
</style>
```

If the user hasn't customised a knob (or hasn't customised the plugin at all),
the corresponding key is undefined; the `default` filter substitutes the
template's fallback. **Always supply a `default(...)`** — it's the
single declaration of your visual default.

---

## Custom Liquid filters

Cascades registers these filters via minijinja:

| Filter | Example | Output |
|---|---|---|
| `number_with_delimiter` | `{{ 12345 \| number_with_delimiter }}` | `"12,345"` |
| `round(n)` | `{{ 8.333 \| round(1) }}` | `"8.3"` |
| `default(val)` | `{{ data.name \| default("River") }}` | `"River"` if null |
| `pluralize("s")` | `{{ count \| pluralize("s") }}` | `""` if 1, `"s"` otherwise |
| `days_ago` | `{{ timestamp \| days_ago }}` | `"2 days ago"` |
| `time_of_day` | `{{ unix_ts \| time_of_day }}` | `"2:45 PM"` |

---

## TRMNL CSS utility classes

Templates render inside the TRMNL stylesheet. These utility classes are
available:

### Layout

| Class | Effect |
|---|---|
| `layout--stretch` | Stretches the container to fill its parent |
| `layout--center-x` | Centers children horizontally |
| `layout--space-between` | Distributes children with space between |

### Flex direction

| Class | Effect |
|---|---|
| `flex--col` | `flex-direction: column` |
| `flex--row` | `flex-direction: row` |

### Typography

| Class | Effect |
|---|---|
| `title` | Large title text (used inside `title_bar`) |
| `label` | Small label / caption text |
| `value` | Standard value display |
| `value--large` | Large value (e.g. "GO" / "NO GO") |
| `value--xxxlarge` | Extra-extra-extra large value (e.g. main metric) |
| `text--gray-50` | 50% gray text (secondary / muted) |

### Structure

| Class | Effect |
|---|---|
| `title_bar` | Top bar container (holds `title` and optional label) |
| `divider` | Horizontal rule separating sections |

---

## End-to-end example: Tide Gauge plugin

This example creates a minimal tide gauge plugin that shows the current tide
level and a go/no-go recommendation.

### 1. Create the plugin definition

`config/plugins.d/tide.toml`:

```toml
[[plugin]]
id = "tide"
name = "Tide Gauge"
description = "Current tide level from a NOAA CO-OPS station."
source = "noaa_tides"
refresh_interval_secs = 600
data_strategy = "polling"

template_full = "templates/tide_full.html.liquid"

[[plugin.criteria]]
key          = "level_ft"
label        = "Tide level"
operator     = "lte"
threshold    = 4.0
unit         = "ft"
go_direction = "below"

[[plugin.settings_schema]]
key         = "station_id"
label       = "NOAA Station ID"
type        = "text"
required    = true
placeholder = "8443970"

[[plugin.settings_schema]]
key         = "display_name"
label       = "Display Name"
type        = "text"
required    = false
default     = "Tide"
```

### 2. Create the template

`templates/tide_full.html.liquid`:

```html
<div class="layout--stretch flex--col">
  <div class="title_bar">
    <span class="title">{{ settings.display_name | default("Tide") }}</span>
    {% if error %}
      <span class="label text--gray-50">stale data</span>
    {% endif %}
  </div>

  <div class="flex--col layout--center-x" style="flex: 1;">
    <span class="value value--xxxlarge">{{ data.level_ft | round(1) }} ft</span>
    <span class="label">{{ data.tide_direction | default("") }}</span>
  </div>

  {% if trip_decision %}
    <div class="divider"></div>
    <div class="flex--row layout--center-x">
      {% if trip_decision.go %}
        <span class="value value--large">GO</span>
      {% else %}
        <span class="value value--large text--gray-50">NO GO</span>
      {% endif %}
      <span class="label">{{ trip_decision.destination | default("") }}</span>
    </div>
  {% endif %}

  <div class="label text--gray-50" style="text-align: right;">
    {{ now.local }}
  </div>
</div>
```

### 3. Add the display slot

In `config/display.toml`, add a slot that uses the new plugin:

```toml
[[display]]
name = "default"
slots = [
    { plugin = "tide", variant = "full" },
]
```

Or composite it with another plugin:

```toml
[[display]]
name = "coastal"
slots = [
    { plugin = "weather", x = 0,   y = 0,   width = 800, height = 240, variant = "half_horizontal" },
    { plugin = "tide",    x = 0,   y = 240, width = 800, height = 240, variant = "half_horizontal" },
]
```

### 4. Hot-reload the plugin definition

Send `SIGHUP` to the server process to reload `plugins.d/` without restarting:

```bash
kill -HUP $(pgrep cascades)
```

Template file changes require a full restart (`cargo run`).

### 5. Push test data via webhook (optional)

If you want to test before the source is implemented, push canned data via the
webhook endpoint:

```bash
curl -X POST http://localhost:8080/api/webhook/tide \
  -H "Content-Type: application/json" \
  -d '{"level_ft": 3.2, "tide_direction": "ebbing"}'
```

Then fetch the rendered image:

```bash
curl -o /tmp/tide.png http://localhost:8080/api/image/default
open /tmp/tide.png
```
