# TRMNL Plugin System and Extensibility Model

Research into the TRMNL server plugin/datasource architecture.
Source: https://github.com/usetrmnl (repos: `plugins`, `larapaper`, `byos_next`, `trmnlp`)

---

## 1. How Plugins Are Defined and Registered

TRMNL has two server implementations with different plugin models but the same firmware-facing API contract.

### larapaper (PHP/Laravel — the hosted/reference platform)

Plugins are database rows in a `plugins` table. There is no plugin manifest, service provider, or decorator. A plugin is created either by admin-seeding native plugins or by a user via the UI / `trmnlp push`.

Key schema columns (from `database/migrations/2025_03_07_133658_create_plugins_table.php`):

```sql
id, uuid, user_id,
name,
plugin_type VARCHAR DEFAULT 'recipe',    -- 'recipe', 'image_webhook'
data_strategy VARCHAR,                   -- 'polling', 'webhook', 'static'
polling_url TEXT,
polling_verb VARCHAR,                    -- 'get' | 'post'
polling_header TEXT,
polling_body TEXT,
render_markup TEXT,                      -- Liquid template: full layout
render_markup_half_horizontal TEXT,
render_markup_half_vertical TEXT,
render_markup_quadrant TEXT,
render_markup_shared TEXT,               -- shared partial
transform_code TEXT,                     -- JS-like post-fetch transform
configuration JSONB,                     -- current user-supplied values
configuration_template JSONB,            -- field definitions (rendered in UI)
data_payload JSONB,                      -- cached fetched data
data_payload_updated_at DATETIME,
data_stale_minutes INT,
current_image VARCHAR,                   -- UUID of last-generated image
is_native BOOLEAN
```

### byos_next (TypeScript/Next.js — the BYOS reference implementation)

No Plugin table. Plugins are called **recipes** and are static TypeScript React components. Registration means adding an entry to `app/(app)/recipes/screens.json`:

```json
{
  "wikipedia": {
    "title": "Wikipedia Article",
    "published": true,
    "componentPath": "./screens/wikipedia",
    "hasDataFetch": true,
    "props": { "title": "Wikipedia (Default)", "extract": "Loading..." },
    "params": {
      "imageUrl": { "type": "string", "label": "Image URL", "default": "..." }
    },
    "tags": ["tailwind", "text", "api"],
    "category": "display-components"
  }
}
```

Each slug maps to `app/(app)/recipes/screens/<slug>/` containing `<slug>.tsx` (React component) and optionally `getData.ts` (data fetcher).

### trmnlp (Ruby gem — local dev tool for private plugins)

Plugin definition lives in `src/settings.yml`:

```yaml
strategy: polling       # polling | webhook | static
polling_url: ''
polling_verb: get
polling_headers: ''
polling_body: ''
refresh_interval: 1440
name: My Plugin
no_screen_padding: 'no'
dark_mode: 'no'
static_data: ''
```

Pushed via `trmnlp push` → `POST /api/plugin_settings/{id}/archive` with a ZIP of `settings.yml` + `.liquid` template files.

---

## 2. What a Plugin Author Must Implement

### Private/community plugin (common case)

- A **data strategy**: `polling` (server fetches a URL), `webhook` (external system pushes JSON), or `static` (hardcoded JSON).
- **Liquid templates** for each layout: `full.liquid`, `half_horizontal.liquid`, `half_vertical.liquid`, `quadrant.liquid`, plus optional `shared.liquid` partial.
- **Custom field definitions** in `configuration_template` JSON (larapaper) or `settings.yml` (trmnlp) declaring user-configurable inputs.

### Native plugin in larapaper (from `lib/` in the `plugins` repo)

```ruby
module Plugins
  class HackerNews < Base
    def locals
      { stories:, category: }   # hash injected into ERB templates
    end

    private
    def stories
      fetch_stories[..14].map { |id| fetch_item(id) }
    end
    # settings[] accesses user config values
  end
end
```

Contract: implement `locals` returning a hash. Base class provides `settings` (user config) and HTTP helpers. Views use ERB (`<%= var %>`).

### byos_next recipe

```tsx
// app/(app)/recipes/screens/wikipedia/wikipedia.tsx
export default async function Wikipedia({ title, extract, width = 800, height = 480 }) {
  "use cache";
  return <PreSatori useDoubling={true} width={width} height={height}>
    <div>...</div>
  </PreSatori>;
}
```

Plus `getData.ts`:
```ts
export default async function fetchData(params?: Record<string, unknown>) {
  // fetch external APIs, return props object
  return { title, extract, thumbnail };
}
```

---

## 3. Data Flow: Plugin → Template → Rendered Image

### larapaper

1. Firmware calls `GET /api/display`.
2. `$plugin->updateDataPayload()` fetches `polling_url` using `polling_verb`/`polling_header`/`polling_body`. Response is JSON-parsed via `ResponseParserRegistry`, stored in `data_payload`.
3. `$plugin->render(size: 'full', device: $device)` renders the Liquid template (`render_markup`) with `data_payload` in the Liquid context. Custom filters from the `trmnl-liquid` gem (date, number, localization) are applied.
4. Rendered HTML → `GenerateScreenJob::dispatchSync()` → `ImageGenerationService::generateImage()` → **Browsershot** (headless Chrome via Puppeteer) renders at 800×480.
5. `bnussbau/trmnl-pipeline` library runs `ImageStage`: convert to BMP or PNG (dithering, bit depth, rotation).
6. Image UUID stored in `plugins.current_image` and `devices.current_screen_image`.
7. API responds with `{ image_url, refresh_rate, filename }`.

### byos_next

1. Firmware calls `GET /api/display`; server selects screen slug from device config/playlist.
2. Response returns `{ image_url: "/api/bitmap/<slug>.bmp?width=800&height=480&grayscale=2", refresh_rate }`.
3. Firmware fetches `GET /api/bitmap/<slug>.bmp` → `renderRecipeBitmap()`.
4. `buildRecipeElement({ slug })`: loads `screens.json`, dynamically imports `<slug>.tsx`, runs `getData.ts` (10s timeout), merges `screen_configs` DB row for user params.
5. React element rendered to PNG via **Takumi** (`@takumi-rs/core`, default) or **Satori** (`next/og`).
6. PNG → `renderBmp()` with Floyd-Steinberg dithering → 1-bit BMP at 800×480.
7. Raw BMP bytes returned as `image/bmp`.

---

## 4. How the Server Schedules/Refreshes Multiple Plugins

### larapaper

No proactive scheduling. Refresh is **pull-driven**: firmware polls `GET /api/display` every `refresh_rate` seconds. On each poll:

```
device.isSleepModeActive()   → return sleep image
device.isPauseActive()       → return sleep image until pause_until timestamp
device.getNextPlaylistItem() → iterate playlists
  Playlist.isActiveNow():      checks weekdays[] and active_from/active_until time window
  PlaylistItem.getNextItem():  round-robins through items ordered by last_displayed_at
```

If `plugin->isDataStale()` is true (`data_payload_updated_at + data_stale_minutes < now()`), `updateDataPayload()` re-fetches and `GenerateScreenJob::dispatchSync()` re-renders synchronously before responding.

### byos_next

`calculateRefreshRate()` reads a JSONB `refresh_schedule` on the device row:

```json
{
  "default_refresh_rate": 180,
  "time_ranges": [
    { "start_time": "09:00", "end_time": "22:00", "refresh_rate": 300 }
  ]
}
```

In playlist mode, `getActivePlaylistItem()` checks `days_of_week` (JSONB array) and `start_time`/`end_time` per item, cycling via `current_playlist_index` on the device row.

No background scheduler in either implementation. Rendering is fully on-demand, triggered by device poll.

---

## 5. User-Facing Configuration Model

### larapaper

`configuration_template` JSONB defines custom fields:

```json
{
  "custom_fields": [
    {
      "name": "API Key",
      "keyname": "api_key",
      "field_type": "password",
      "optional": false,
      "description": "Your OpenAI API key"
    },
    {
      "name": "Model",
      "keyname": "model",
      "field_type": "select",
      "options": ["gpt-4o", "gpt-4o-mini", "o3"],
      "default": "gpt-4o"
    }
  ]
}
```

Known field types: `password`, `select`, `text`, `author_bio`, `copyable`, `copyable_webhook_url`.

User-supplied values stored in `configuration` JSONB. In Liquid: `{{ trmnl.plugin_settings.custom_fields_values.<keyname> }}`. In Ruby native plugins: `settings['keyname']`.

Plugins with missing required fields (`hasMissingRequiredConfigurationFields()`) show a UI warning but can still be added to a playlist.

### byos_next

Params defined in `screens.json` per recipe:

```json
"params": {
  "imageUrl": {
    "type": "string",
    "label": "Image URL",
    "description": "URL of the image",
    "default": "https://...",
    "placeholder": "https://..."
  }
}
```

Types: `"string" | "number" | "boolean"`. Values stored in a `screen_configs` table keyed by `screen_id` with a `params` JSONB column. UI calls `updateScreenParams(slug, params, definitions)` (a Next.js Server Action) which upserts into `screen_configs`.

### trmnlp (local dev overrides)

User config in `.trmnlp.yml`:

```yaml
custom_fields:
  station: "{{ env.ICAO }}"   # env var interpolation
variables:
  trmnl:
    plugin_settings:
      instance_name: "Kevin Bacon Facts"
```

---

## 6. Multi-Tenancy and Per-Device Customization

### larapaper

- Plugins belong to a `user_id`. Each user has independent plugin instances.
- `assign_new_devices: boolean` on `User` enables auto-provisioning new devices with a default mirror.
- A device can have a `mirror_device_id` — it displays whatever image the mirrored device is showing.
- Playlists belong to a device (`device_id`), scoped per-device. Multiple playlists with non-overlapping time windows (`active_from`, `active_until`, `weekdays[]`).
- Cache invalidation: `ImageGenerationService::resetIfNotCacheable($plugin, $device)` clears `current_image` when a device with different dimensions exists, preventing cross-device cache poisoning.
- Sleep mode is per-device (`sleep_mode_enabled`, `sleep_mode_from`, `sleep_mode_to`), evaluated in the user's timezone.

### byos_next

Multi-tenancy added in migration `0009_add_user_tenancy.sql`. PostgreSQL **Row Level Security** enforces isolation:

```sql
CREATE POLICY devices_select_policy ON devices
  FOR SELECT
  USING (user_id = current_setting('app.current_user_id', true) OR user_id IS NULL);
```

Same pattern applied to `playlists`, `mixups`, `screen_configs`. App connects as `postgres` but issues `SET ROLE byos_app` before queries. The `byos_app` role has `NOBYPASSRLS`.

Screen params (`screen_configs`) are per-user — each user can configure different param values for the same recipe slug.

**Mixup (multi-screen layout) model:**

A `mixups` table stores a layout ID (`quarters`, `top-banner`, `left-rail`, `vertical-halves`, `horizontal-halves`) and a `mixup_slots` table maps slot IDs to recipe slugs. Device `display_mode` enum (`screen`, `playlist`, `mixup`) selects the content path. Each slot is rendered independently at proportional dimensions and composited onto the 800×480 canvas.

---

## Key Source Files

| Repo | File | Notes |
|------|------|-------|
| `usetrmnl/plugins` | `lib/hacker_news/hacker_news.rb` | Native plugin Ruby class pattern |
| `usetrmnl/plugins` | `lib/hacker_news/views/full.html.erb` | ERB view template |
| `usetrmnl/plugins` | `README.md` | Official plugin architecture |
| `usetrmnl/larapaper` | `routes/api.php` | `GET /api/display` scheduling logic |
| `usetrmnl/larapaper` | `app/Models/Plugin.php` | `isDataStale()`, `updateDataPayload()`, `render()`, Liquid rendering |
| `usetrmnl/larapaper` | `app/Jobs/GenerateScreenJob.php` | Image generation job |
| `usetrmnl/larapaper` | `app/Services/ImageGenerationService.php` | Browsershot → BMP pipeline |
| `usetrmnl/larapaper` | `database/migrations/2025_03_07_133658_create_plugins_table.php` | Plugin schema |
| `usetrmnl/byos_next` | `app/api/display/route.ts` | Display API with playlist/mixup dispatch |
| `usetrmnl/byos_next` | `app/api/bitmap/[[...slug]]/route.ts` | On-demand BMP rendering |
| `usetrmnl/byos_next` | `lib/recipes/recipe-renderer.ts` | Takumi/Satori render pipeline |
| `usetrmnl/byos_next` | `app/(app)/recipes/screens.json` | Recipe registry |
| `usetrmnl/byos_next` | `migrations/0009_add_user_tenancy.sql` | PostgreSQL RLS multi-tenancy |
| `usetrmnl/trmnlp` | `lib/trmnlp/config/plugin.rb` | `settings.yml` parsing |
| `usetrmnl/trmnlp` | `lib/trmnlp/context.rb` | Liquid render context construction |
