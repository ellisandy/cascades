# Inker Package Architecture and Patterns

> Research findings for cs-87w. Source: https://github.com/usetrmnl/inker (v0.3.1)

## 1. What Inker Does and What Problem It Solves

Inker is a **self-hosted server for managing e-ink displays** (TRMNL-compatible hardware). It solves the problem of users wanting full local control over their TRMNL e-ink devices rather than depending on TRMNL's cloud SaaS.

Core problems addressed:
- **Content delivery**: E-ink devices can only display static images; inker generates those images on-demand from dynamic data
- **Device management**: Auto-provisioning, API key issuance, firmware version tracking, battery/WiFi telemetry
- **Screen authoring**: A drag-and-drop designer with live widgets instead of hand-crafting HTML/images
- **Plugin system**: Connects to third-party data sources (weather, GitHub, JSON APIs, RSS) with a Liquid templating layer compatible with TRMNL's plugin ecosystem
- **Playlist rotation**: Multiple screens rotating on each device with configurable durations

**Stack**: NestJS (TypeScript backend) + React (frontend) + PostgreSQL + Redis + Puppeteer + Sharp, deployed as a single Docker container with Bun runtime.

---

## 2. HTML→Image Rendering Pipeline

The rendering pipeline has multiple entry points but converges at the same core steps.

### Entry Point A: Plugin Rendering

**File**: `/backend/src/plugins/plugin-renderer.service.ts` — `PluginRendererService`

1. `renderToPng(markup, locals, settings, width, height, mode)` — top-level call
2. `renderToHtml(markup, locals, settings)` — **LiquidJS** parses and renders the template with data: `{ ...locals, settings }`
3. `buildFullPage(innerHtml, width, height)` — wraps the fragment in a full HTML page, injecting `TRMNL_CSS` (the entire TRMNL utility CSS framework as a hardcoded string from `trmnl-css.ts`)
4. `screenRenderer.renderHtmlToPng(fullPage, width, height)` — Puppeteer headless Chromium screenshots the page
5. `screenRenderer.applyEinkProcessing(canvas, width, height, shouldNegate)` — Sharp + Floyd-Steinberg dithering

### Entry Point B: Screen Designer (Composed Widgets)

**File**: `/backend/src/screen-designer/services/screen-renderer.service.ts` — `ScreenRendererService`

1. `renderScreenDesign(screenDesignId, deviceContext, mode)` — loads design + all widgets from Postgres via Prisma
2. `renderDesignAsHtml(screenDesign, deviceContext)` — constructs HTML by iterating widgets; each widget type renders to an HTML fragment
3. `renderHtmlToPng(html, width, height)` — same Puppeteer pipeline
4. `applyEinkProcessing(canvas, width, height, negate)` — same Sharp/dithering pipeline

### Entry Point C: Screens from HTML/URL/Image Upload

**File**: `/backend/src/screens/screens.service.ts` — `ScreensService`

- `createFromHtml(html, name, width, height)` → `screenRenderer.renderHtmlToPng` → dithering → save
- `createFromUrl(url, name, width, height)` → `screenRenderer.renderUrlToPng` (Puppeteer navigates to URL) → dithering
- `createFromImage(buffer, ...)` → same dithering pipeline on uploaded image

### Puppeteer Details

- Single **shared headless Chromium browser** instance kept alive for the process lifetime; reconnects on disconnect
- Viewport set to `{width, height}` per render
- All network requests **blocked** (only `data:` URIs and local content allowed) — SSRF prevention
- Screenshot taken as raw PNG buffer

### Sharp/Dithering Pipeline

**File**: `/backend/src/screens/services/image-processor.service.ts` — `ImageProcessorService`

`processForEinkWithDithering(inputPath, outputPath, width, height, options)`:
1. Resize with `fit: 'contain'`, white background, grayscale
2. Contrast enhancement: `sharp.linear(1.2, -(128*1.2 - 128))`
3. Normalize tonal range
4. **Floyd-Steinberg dithering** at threshold 140 — implemented in raw pixel space (Float32Array, manual error diffusion):
   - 7/16 right, 5/16 below, 3/16 below-left, 1/16 below-right
5. Reconstruct PNG from raw pixels via `sharp(buffer, { raw: { width, height, channels: 1 } })`

**90 KB constraint**: Iteratively scales down the output if result exceeds 90 KB (TRMNL device firmware limit). Output is 8-bit grayscale PNG (not palette-indexed — chosen for firmware 1.7.8 compatibility).

**Three render modes**:
- `device` — full e-ink processing + negate/invert (TRMNL hardware expects inverted images)
- `einkPreview` — dithering without negate (browser preview showing what device will look like)
- `preview` — raw Puppeteer screenshot, no Sharp post-processing (used for UI thumbnails)

**Thumbnails**: 200×150 JPEG, 80% quality, `fit: 'cover'`.

---

## 3. API Surface — Inputs, Outputs, Data Contracts

### Device-Facing API

**File**: `/backend/src/api/api.controller.ts`

All endpoints are `@Public()` (no auth cookie); devices identify via `HTTP_ID` header.

| Endpoint | Method | Request | Response |
|---|---|---|---|
| `/api/setup` | GET | Headers: `HTTP_ID` (MAC), `HTTP_FW_VERSION`, `battery-voltage`, `rssi`, `HTTP_MODEL` | `{ api_key, friendly_id, image_url, message }` |
| `/api/display` | GET | Headers: `HTTP_ID` (API key), `BASE64`, `battery-voltage`, `rssi`, firmware | `{ status, filename, image_url, firmware_url, update_firmware, refresh_rate, reset_firmware }` |
| `/api/log` | POST | `CreateLogDto` | Log entry |
| `/api/device-images/device/:id` | GET | Path: device ID | PNG binary |
| `/api/device-images/design/:id` | GET | Query: `battery`, `wifi`, `deviceName`, `firmwareVersion`, `macAddress`, `mode` | PNG binary, `Cache-Control: no-store` |

**`refresh_rate` logic** (from `display.service.ts`):
- Clock widget present: seconds until next minute + 3s buffer
- Date/weather/countdown widget: 60s
- Otherwise: playlist duration
- Immediate refresh flag: 1s
- Floor: 10s minimum

**`image_url` selection priority** (from `display.service.ts`):
1. Plugin instance → `/api/plugins/instances/:id/render?mode=device&t=<timestamp>`
2. Screen design with pre-rendered capture → `/uploads/captures/<captureFilename>?t=<timestamp>`
3. Screen design live render → `/api/device-images/design/:id?<deviceContextQueryParams>`
4. Uploaded screen → direct URL
5. Default welcome screen → static asset

### Plugin API

**File**: `/backend/src/plugins/plugins.controller.ts`

| Endpoint | Purpose |
|---|---|
| `GET /plugins` | List all plugin definitions |
| `POST /plugins` | Create plugin (`CreatePluginDto`) |
| `PUT /plugins/:id` | Update plugin |
| `GET /plugins/instances/all` | List all plugin instances |
| `POST /plugins/instances` | Create instance (`CreatePluginInstanceDto`) |
| `PUT /plugins/instances/:id` | Update instance settings |
| `GET /plugins/instances/:id/data` | Fetch fresh plugin data |
| `GET /plugins/instances/:id/render` | Render plugin to PNG (public; params: `layout`, `mode`) |
| `POST /plugins/preview-template` | Preview Liquid template with mock data |
| `GET /plugins/github-plugin/:slug` | Fetch TRMNL plugin template from GitHub, convert ERB→Liquid |
| `GET /plugins/recipes` | Browse TRMNL recipe gallery |
| `POST /plugins/webhooks/:slug` | Receive webhook data (public) |
| `GET /plugins/oauth/callback` | OAuth callback (public) |

---

## 4. Plugin and Template Structure

### Plugin Data Model

From `/backend/prisma/schema.prisma`:

```
Plugin {
  id, name, slug, description
  dataStrategy: 'polling' | 'webhook' | 'static'
  dataUrl: String?           // URL template with {{settings.key}} interpolation
  dataTransform: String?     // JavaScript adapter code (async, 10s timeout)
  refreshInterval: Int       // seconds between data fetches
  markupFull: String?        // Liquid template for 800×480
  markupHalfHorizontal: String?  // Liquid template for 800×240
  markupHalfVertical: String?    // Liquid template for 400×480
  markupQuadrant: String?        // Liquid template for 400×240
  settingsSchema: Json?      // Array of {key, label, type, encrypted, required}
  hasOauth: Boolean
  instances: PluginInstance[]
}

PluginInstance {
  id, pluginId
  settings: Json             // plain settings
  encryptedSettings: Json    // AES-encrypted sensitive fields
  cachedData: Json?          // last successful fetch result
  lastFetchedAt: DateTime?
  lastError: String?
}
```

### Template Selection

`PluginRendererService.selectMarkup` falls back to `markupFull` for any layout without a dedicated template:

```typescript
switch (layout) {
  case 'full': return plugin.markupFull || null;
  case 'half_horizontal': return plugin.markupHalfHorizontal || plugin.markupFull || null;
  case 'half_vertical': return plugin.markupHalfVertical || plugin.markupFull || null;
  case 'quadrant': return plugin.markupQuadrant || plugin.markupFull || null;
}
```

### Plugin Registration — Three Pathways

1. **Manual**: `POST /plugins` with custom Liquid markup
2. **TRMNL GitHub sync**: `GET /plugins/github-plugin/:slug` — fetches from TRMNL's plugin repo, converts ERB templates to Liquid
3. **Recipe gallery**: `GET /plugins/recipes` — browse and import from TRMNL's online gallery

### Data Fetching (`PluginsService.fetchData`)

1. Pre-flight: validate required settings fields, inject OAuth token if applicable
2. If `dataTransform` JS code exists: execute as `AsyncFunction` with 10s timeout; receives raw data
3. Else if `dataUrl` exists: interpolate settings into URL/headers, axios GET/POST with 15s timeout, extract via JSONPath
4. Cache result in `PluginInstance.cachedData`; on failure return last cached value

### Custom Widgets (`CustomWidgetsService`)

- Backed by `DataSource` entities (JSON API or RSS feed)
- Four display types: `value` (single field), `list` (array), `script` (user JS), `grid` (multi-cell)
- Rendered programmatically in TypeScript (no Liquid templating)

---

## 5. Dependencies and Rendering Stack

### Core Rendering

| Dependency | Role |
|---|---|
| **Puppeteer** | Headless Chromium for HTML→PNG. Single shared browser instance, reconnects on disconnect. Network interceptor blocks all external requests. |
| **Sharp** | High-performance image processing: grayscale, Floyd-Steinberg dithering, resize, compress, thumbnail generation, 90KB iterative size reduction. |
| **LiquidJS** | Liquid template engine for plugin markup. Config: `{ strictVariables: false, strictFilters: false }` |

### Custom LiquidJS Filters

Inker ports the Ruby `trmnl-liquid` gem's filters to JavaScript (registered in `PluginRendererService`):

`number_with_delimiter`, `number_to_currency`, `days_ago`, `pluralize`, `group_by`, `find_by`, `json`, `parse_json`, `append_random`, `sample`, `map_to_i`, `ordinalize`, `l_date`, `where_exp`

### TRMNL CSS Framework

**File**: `/backend/src/plugins/sync/trmnl-css.ts` — entire CSS as a hardcoded string injected into every plugin render page.

Provides:
- **Layout**: `.flex--row`, `.flex--col`, `.layout--center-x`, `.layout--stretch`
- **Grid**: `.grid--cols-1` through `.grid--cols-12` with span classes
- **Spacing**: `.m--small`, `.mt--large`, `.p--base`, `.px--medium`
- **Typography**: `.title`, `.label`, `.description`, `.value` with size variants (`.value--xxxlarge` through `.value--xxsmall`)
- **Color utilities**: `.text--gray-10` through `.text--gray-75`, `.bg--gray-5` through `.bg--gray-75`
- **Components**: `.progress-bar`, `.progress-dots`, `.table`, `.item`, `.divider`, `.title_bar`

All values are hardcoded pixels (no CSS variables) optimized for e-ink rendering.

### Full Dependency Stack

| Layer | Technology |
|---|---|
| Backend framework | NestJS 10.3.0, TypeScript 5.3.3 |
| Runtime | Bun |
| ORM | Prisma 5.8.0 → PostgreSQL 17 |
| Queue | BullMQ + Redis (ioredis) |
| HTTP client | Axios |
| Auth | Passport + JWT |
| QR codes | `qrcode` npm package (server-side generation) |
| Security | Helmet, compression, DNS-pinning SSRF prevention, AES encryption for plugin secrets |
| Frontend | React + Vite + TypeScript + Tailwind CSS + CodeMirror (code editing) + Leaflet (maps) + html2canvas (client-side preview) + React Router |

---

## 6. Multiple Screen Sizes and Formats

### Layout Dimensions

Hardcoded in `PluginsService`:

| Layout | Dimensions |
|---|---|
| `full` | 800 × 480 px |
| `half_horizontal` | 800 × 240 px |
| `half_vertical` | 400 × 480 px |
| `quadrant` | 400 × 240 px |

### Device Model Abstraction

**File**: `/backend/src/models/models.service.ts` — `Model` entity allows arbitrary screen sizes:

```
Model {
  width, height         // pixel dimensions
  colors                // color depth (default 2 for monochrome)
  bitDepth              // default 1
  mimeType              // output format (default image/png)
  offsetX, offsetY      // physical display orientation corrections
  rotation
  scaleFactor           // DPI scaling
}
```

BYOD devices self-register via `/api/setup` with their own dimensions. The rendering pipeline respects whatever `width`/`height` the model specifies — Puppeteer viewport and Sharp resize both take these as parameters.

### Screen Designer

`ScreenDesign` defaults to 800×480 but accepts any `width`/`height`. Widgets use absolute pixel coordinates (`x`, `y`, `width`, `height`, `zIndex`, `rotation`) within the canvas with snap-to-grid on the client side.

---

## Key Source File Map

| File | Role |
|---|---|
| `/backend/src/screen-designer/services/screen-renderer.service.ts` | Puppeteer browser lifecycle, HTML→PNG, Floyd-Steinberg dithering, 90KB constraint |
| `/backend/src/plugins/plugin-renderer.service.ts` | LiquidJS rendering, TRMNL filter registration, layout selection, calls screen-renderer |
| `/backend/src/plugins/plugins.service.ts` | Plugin CRUD, data fetching (JS adapter + URL strategy), OAuth injection, caching |
| `/backend/src/screens/services/image-processor.service.ts` | Sharp pipeline: resize, grayscale, dithering, thumbnail, format conversion |
| `/backend/src/screens/screens.service.ts` | Screen CRUD, `createFromHtml`/`createFromUrl`/`createFromImage` entry points |
| `/backend/src/api/api.controller.ts` | Device-facing `/api/display`, `/api/setup`, `/api/device-images/*` |
| `/backend/src/api/display/display.service.ts` | `image_url` selection priority, `refresh_rate` computation |
| `/backend/src/plugins/sync/trmnl-css.ts` | Hardcoded TRMNL CSS framework string |
| `/backend/src/screen-designer/services/widget-templates.service.ts` | 16 built-in widget type definitions, seeding |
| `/backend/src/data-sources/data-sources.service.ts` | JSON API + RSS fetching, JSONPath extraction, caching, field metadata |
| `/backend/prisma/schema.prisma` | Full data model: Device, ScreenDesign, ScreenWidget, Plugin, PluginInstance, DataSource, Playlist, Model |

---

## Summary: Rendering Pipeline (End-to-End)

```
Plugin Liquid template + data locals
        ↓ LiquidJS render
HTML fragment
        ↓ buildFullPage (inject TRMNL CSS + viewport)
Full HTML page
        ↓ Puppeteer headless Chromium screenshot (network blocked)
Raw PNG buffer (color)
        ↓ Sharp: grayscale + contrast normalization
Grayscale PNG
        ↓ Floyd-Steinberg dithering (threshold 140)
1-bit dithered PNG
        ↓ Negate if mode=device (hardware inversion)
        ↓ Iterative quality reduction if >90KB
Final PNG ≤90KB
        ↓ Served to device via /api/display image_url
E-ink display renders image
```
