# Cascades

Cascades is a trip-condition rendering pipeline for e-ink displays. It polls
outdoor data sources — NOAA weather, USGS river gauges, WSDOT ferries and road
alerts, NPS trail conditions — renders Liquid HTML templates via a headless
browser, and serves the resulting PNG to connected devices.

## Architecture

Two processes run together:

1. **Render sidecar** (`src/sidecar/`, Bun + TypeScript) — stateless HTTP
   server. `POST /render {html, width, height, mode}` → PNG via Puppeteer
   screenshot + Floyd-Steinberg dithering.
2. **Cascades server** (`src/`, Rust + Axum) — polls data sources, composites
   display layouts, serves API endpoints.

The compositor resolves templates by convention:
`templates/{plugin_id}_{variant}.html.jinja`.

---

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (stable channel)
- [Bun](https://bun.sh/) ≥ 1.0

### 1. Install sidecar dependencies

```bash
cd src/sidecar
bun install
```

### 2. Configure

The repo ships a working `config.toml`. No changes are needed for development.

For live data, the key sections are:

```toml
[server]
port = 8080

[display]
width = 800
height = 480

[location]
latitude = 48.4232
longitude = -122.3351
name = "Mount Vernon, WA"

[sources]
weather_interval_secs = 300
river_interval_secs = 300
ferry_interval_secs = 60
trail_interval_secs = 900
road_interval_secs = 1800
```

### 3. Start the sidecar

```bash
cd src/sidecar
bun server.ts
# Render sidecar listening on port 3001
```

Override the port: `SIDECAR_PORT=3002 bun server.ts`

### 4. Start the server

From the repo root, in a separate terminal:

```bash
cargo run
# Listening on http://0.0.0.0:8080
```

The server reads `SIDECAR_URL` (default: `http://localhost:3001`) to reach the
sidecar. Override: `SIDECAR_URL=http://localhost:3002 cargo run`

### Development shortcut (fixture mode)

Starts the server with embedded canned data — no live API calls. The sidecar
must still be running for PNG rendering.

```bash
./scripts/dev-server.sh
```

Or manually:

```bash
SKAGIT_FIXTURE_DATA=1 RUST_LOG=info cargo run
```

---

## API Endpoints

### `GET /image.png`

Legacy alias for the default display. Returns a PNG, `Content-Type: image/png`.

```bash
curl -o display.png http://localhost:8080/image.png
```

---

### `POST /api/webhook/:plugin_instance_id`

Push new data for a named plugin instance. The server stores the JSON body as
the plugin's cached data and re-renders every display that uses that instance.

```bash
curl -X POST http://localhost:8080/api/webhook/river \
  -H "Content-Type: application/json" \
  -d '{"water_level_ft": 8.3, "streamflow_cfs": 4200}'
```

**Response:** `204 No Content`

Used with plugins whose `data_strategy = "webhook"` when an external system
pushes updates rather than the server polling on a timer.

---

### `GET /api/display`

Returns the image URL and device refresh rate for the default display. Used by
TRMNL-compatible devices.

Requires `Authorization: Bearer <api_key>`.

```bash
curl -H "Authorization: Bearer <api_key>" \
  http://localhost:8080/api/display
```

**Response:**

```json
{
  "image_url": "/api/image/default?t=1712345678",
  "refresh_rate": 60
}
```

`refresh_rate` is in seconds and comes from `[server] refresh_rate_secs` in
`config.toml` (default 60).

---

### `GET /api/status`

Returns a JSON health snapshot: server version, uptime, sidecar URL, and
per-plugin-instance data freshness. No authentication required.

```bash
curl http://localhost:8080/api/status
```

**Response:**

```json
{
  "version": "0.1.0",
  "uptime_secs": 42,
  "sidecar_url": "http://localhost:3001",
  "sources": [
    {
      "id": "weather",
      "name": "Weather",
      "enabled": true,
      "last_fetched_at": 1712345678,
      "last_error": null,
      "data_age_secs": 30
    }
  ]
}
```

---

### `GET /api/image/:display_id`

Returns the latest rendered PNG for a named display. Served from an in-memory
cache; rendered on demand when the cache is cold or invalidated.

Always includes `Cache-Control: no-store`.

```bash
curl -o default.png      http://localhost:8080/api/image/default
curl -o trip-planner.png http://localhost:8080/api/image/trip-planner
```

**Responses:**
- `200 image/png` — success
- `404` — `display_id` is not defined in `config/display.toml`

---

## Display Layouts

Layouts are defined in `config/display.toml`. Each layout is a named list of
slots; slots are composited into an 800×480 frame.

```toml
[[display]]
name = "default"
slots = [
    { plugin = "river", variant = "full" },
]

[[display]]
name = "trip-planner"
slots = [
    { plugin = "weather", x = 0,   y = 0,   width = 800, height = 240, variant = "half_horizontal" },
    { plugin = "river",   x = 0,   y = 240, width = 400, height = 240, variant = "quadrant" },
    { plugin = "ferry",   x = 400, y = 240, width = 400, height = 240, variant = "quadrant" },
]
```

**Variant sizes:**

| Variant | Width | Height |
|---|---|---|
| `full` | 800 | 480 |
| `half_horizontal` | 800 | 240 |
| `half_vertical` | 400 | 480 |
| `quadrant` | 400 | 240 |

---

## Adding a Plugin

See `docs/plugin-authoring.md` for a complete walkthrough. In brief:

1. Create `config/plugins.d/myplugin.toml` with a `[[plugin]]` entry.
2. Create `templates/myplugin_full.html.jinja` (and other variant templates as needed).
3. Restart the server — or send `SIGHUP` to hot-reload `plugins.d/`.

---

## API Keys

### Device API key (`GET /api/display`)

Auto-generated at first startup and written to `config/secrets.toml`:

```bash
cat config/secrets.toml
# api_key = "<64-char hex string>"
```

To rotate: delete `config/secrets.toml` and restart. The new key is printed to
stderr and written to the file.

### Data source API keys

| Environment variable | Source |
|---|---|
| `NPS_API_KEY` | NPS Alerts API (trail conditions) |
| `WSDOT_ACCESS_CODE` | WSDOT API (ferries + road closures) |

These can also be set in `config.toml`:

```toml
[sources.trail]
nps_api_key = "..."

[sources.ferry]
wsdot_access_code = "..."

[sources.road]
wsdot_access_code = "..."
```

Missing keys are not fatal — the server starts and omits data from disabled
sources. NOAA weather and USGS river gauge require no keys.

---

## Configuration Reference

### `config.toml`

| Section | Key | Default | Description |
|---|---|---|---|
| `[server]` | `port` | `8080` | TCP port to listen on |
| `[server]` | `refresh_rate_secs` | `60` | Device refresh rate (returned in `/api/display`) |
| `[display]` | `width` | — | Display width in pixels |
| `[display]` | `height` | — | Display height in pixels |
| `[location]` | `latitude` | — | Latitude for weather/river lookups |
| `[location]` | `longitude` | — | Longitude for weather/river lookups |
| `[location]` | `name` | — | Human-readable location name |
| `[sources]` | `weather_interval_secs` | — | NOAA poll interval |
| `[sources]` | `river_interval_secs` | — | USGS poll interval |
| `[sources]` | `ferry_interval_secs` | — | WSDOT ferry poll interval |
| `[sources]` | `trail_interval_secs` | `900` | NPS trail poll interval |
| `[sources]` | `road_interval_secs` | `1800` | WSDOT road poll interval |
| `[storage]` | `db_path` | `data/cascades.db` | SQLite database path |
| `[sources.river]` | `usgs_site_id` | `12200500` | USGS gauge site ID |
| `[sources.trail]` | `park_code` | `noca` | NPS park code |
| `[sources.trail]` | `nps_api_key` | — | NPS API key (or `NPS_API_KEY` env var) |
| `[sources.ferry]` | `route_id` | `9` | WSDOT ferry route ID |
| `[sources.road]` | `routes` | `["020"]` | WSDOT route numbers to monitor |

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `SIDECAR_URL` | `http://localhost:3001` | Render sidecar base URL |
| `SKAGIT_FIXTURE_DATA` | — | Set to `1` for fixture/offline mode |
| `RUST_LOG` | — | Log verbosity (`info`, `debug`, etc.) |
| `NPS_API_KEY` | — | NPS Alerts API key |
| `WSDOT_ACCESS_CODE` | — | WSDOT API access code |

---

## Running Tests

```bash
cargo test
```

All tests are self-contained. Integration tests use an in-process Axum router
and a mock sidecar — no running processes or live network calls required.
