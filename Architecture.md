# Architecture

## Overview

Cascades is a trip-condition rendering pipeline for e-ink displays. It polls
outdoor data sources (NOAA weather, USGS river gauges, WSDOT ferries, NPS
trail conditions, WSDOT road alerts), runs the data through a criteria-based
evaluation engine, renders Jinja HTML templates to PNGs via a headless Chrome
sidecar, dithers them for e-ink, and serves the composited frame to wall-
mounted devices on a configurable refresh cycle.

The canonical design reference lives at
[`docs/research/target-architecture.md`](docs/research/target-architecture.md).

## Process Topology

Cascades runs as **two cooperating processes**:

| Process              | Language     | Responsibility                                         | Default port |
| -------------------- | ------------ | ------------------------------------------------------ | ------------ |
| Cascades server      | Rust (Axum)  | Source polling, evaluation, compositing, HTTP API      | `8080`       |
| Render sidecar       | Bun / TS     | Stateless HTML → PNG rendering (Puppeteer + Sharp)     | `3001`       |

The server POSTs `{html, width, height, mode}` to the sidecar's `/render`
endpoint and receives PNG bytes in response. Keeping the browser out-of-
process isolates Chrome's memory footprint and lets the Rust server stay
small and restart cheaply.

## Technology Stack

**Backend (Rust)**
- `axum` 0.8 — async HTTP framework
- `tokio` 1 — async runtime
- `rusqlite` 0.31 (bundled) — local persistence
- `minijinja` 2 — Jinja2-compatible template engine
- `image` 0.24 — PNG compositing
- `ureq` 2 — blocking HTTP client (run on blocking thread pool)
- `notify` 6 — filesystem watcher for plugin hot-reload
- `serde` / `serde_json` / `toml` — config + data
- `thiserror` + `log` / `env_logger`

**Sidecar (Bun / TypeScript)**
- `puppeteer` — headless Chromium
- `sharp` — grayscale + Floyd–Steinberg dithering

## Directory Layout

```
cascades/
├── src/                         Rust backend
│   ├── main.rs                  Bootstrap: load config, open DB, spawn sources, start Axum
│   ├── lib.rs                   Public exports + build_sources() factory
│   ├── api.rs                   HTTP handlers, router, AppState (~2.8k lines)
│   ├── compositor.rs            Layout slot rendering + PNG compositing (~1.7k lines)
│   ├── config/                  TOML config structs + secrets
│   ├── domain/                  Core data types (WeatherObservation, RiverGauge, …)
│   ├── evaluation/              Criterion trait + per-source implementations
│   ├── format.rs                Format-string filters for DataField items
│   ├── instance_store/          SQLite plugin-instance persistence
│   ├── jsonpath.rs              Minimal JSONPath ($.a.b[0].c)
│   ├── layout_store/            SQLite display-layout persistence
│   ├── plugin_registry/         Plugin definitions + hot-reload from config/plugins.d
│   ├── presentation/            (Legacy) panel formatting — being superseded by templates
│   ├── source_store/            SQLite persistence for user-defined HTTP sources
│   ├── sources/                 Source trait + 5 built-ins + generic HTTP source
│   │   ├── mod.rs
│   │   ├── generic.rs           User-configured HTTP sources
│   │   ├── noaa.rs              NOAA/NWS weather
│   │   ├── usgs.rs              USGS river gauges
│   │   ├── wsdot.rs             WSDOT ferries + road alerts
│   │   ├── trail_conditions.rs  NPS trail suitability
│   │   ├── road_closures.rs     WSDOT highway alerts
│   │   ├── presets.rs           Preset definitions for common APIs
│   │   └── fixtures/            Canned data for offline/test mode
│   ├── template/                Jinja engine wrapper + render context + filters
│   └── sidecar/                 Bun render server
│       ├── server.ts            POST /render → PNG
│       └── render.test.ts
│
├── config/                      Runtime configuration
│   ├── config.toml              Main config (server, display, location, intervals)
│   ├── display.toml             Display layouts (slots)
│   ├── plugins.toml             Baseline plugin registry (usually empty)
│   ├── plugins.d/               Drop-in plugin definitions
│   └── secrets.toml             Auto-generated device API key (gitignored)
│
├── templates/                   Jinja HTML templates per plugin variant
├── tests/                       Integration tests (acceptance, compositor, visual)
├── docs/                        Design docs + research
├── scripts/dev-server.sh        Start in fixture mode
├── Cargo.toml
└── README.md
```

## Entry Points

- **`src/main.rs`** — initializes logging, loads config and secrets, opens
  SQLite stores, loads the plugin registry (including hot-reload watcher),
  seeds default field mappings, builds the `TemplateEngine` from
  `templates/`, spawns Tokio tasks for the five built-in sources plus the
  `SourceScheduler` for user-defined HTTP sources, and starts Axum.
- **`src/lib.rs`** — re-exports public modules and provides `build_sources()`,
  which constructs the five built-in `Source` impls from config (honoring
  `SKAGIT_FIXTURE_DATA` for offline mode).
- **`src/sidecar/server.ts`** — stateless HTTP server. Exposes `POST /render`
  with three modes: `device` (dithered + negated for e-ink), `einkPreview`
  (dithered, not negated), and `preview` (raw PNG).

## Major Modules

| Module                        | Role                                                                                                                                                                 |
| ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `api.rs`                      | All HTTP handlers (40+ endpoints), router construction, shared `AppState`, API-key validation                                                                        |
| `compositor.rs`               | Owns `LayoutVariant`; renders each `LayoutItem` (`PluginSlot`, `StaticText`, `StaticDivider`, `DataField`, `Group`) concurrently; blits results into an 800×480 PNG  |
| `config/`                     | Deserializes `config.toml` and `display.toml`; secret generation; `AuthConfig`, `ServerConfig`, `SourceIntervals`, `LocationConfig`                                  |
| `domain/`                     | Core data types + `RelevantSignals` enum                                                                                                                             |
| `evaluation/`                 | `Criterion` trait with per-source impls (`MinTempCriterion`, `MaxPrecipCriterion`, `RiverLevelCriterion`, …); staleness checks; near-miss margins                    |
| `plugin_registry/`            | Loads `PluginDefinition` from TOML; `DataStrategy` (polling / webhook / static); `SettingsField` schema; `notify`-based hot-reload                                   |
| `instance_store/`             | SQLite table `plugin_instances`; caches last JSON payload + `last_fetched_at`; seeded from `config.toml`                                                             |
| `layout_store/`               | Tables `display_layouts` + `layout_items`; persists `LayoutItem` enum; seeded from `display.toml`                                                                    |
| `source_store/`               | Table `data_sources` for user-defined generic HTTP sources; enforces `MIN_REFRESH_INTERVAL_SECS` and `MAX_CACHED_RESPONSE_BYTES`                                     |
| `sources/`                    | `Source` trait + built-in implementations; fixture data embedded in module for offline runs                                                                          |
| `template/`                   | `TemplateEngine` wrapping minijinja; `RenderContext` (`data`, `settings`, `trip_decision`, `now`, `error`); custom Jinja filters                                    |
| `format.rs`                   | Format-string evaluation for `DataField` items. Filters: `round(N)`, `number_with_delimiter`, `uppercase`, `lowercase`                                               |
| `jsonpath.rs`                 | Minimal JSONPath supporting `$.a.b`, `$.a[0]`, `$.a[0].b`                                                                                                            |

## Data Flow

```
INIT
  load config.toml + display.toml + plugins.d/*.toml
  open SQLite (instance_store, layout_store, source_store)
  load plugin registry + seed default field mappings
  load Jinja templates
  spawn background tasks for built-in sources (weather / river / ferry / trail / road)
  spawn SourceScheduler for user-defined HTTP sources
  start Axum on config.server.port

POLL (per-source Tokio task)
  Source::fetch() on blocking thread pool
    ok  -> write JSON into DomainState (in-mem RwLock<HashMap>)
        -> write JSON into InstanceStore SQLite (cached_data, last_fetched_at)
    err -> log, retain last good value

RENDER (GET /api/image/:display_id  or  GET /image.png)
  load LayoutConfig from layout_store
  for each LayoutItem (concurrently via tokio::spawn):
    PluginSlot     -> evaluate criteria -> build template context -> sidecar render
    StaticText     -> html -> sidecar render
    StaticDivider  -> drawn in-memory (no sidecar)
    DataField      -> JSONPath extract + format filters -> text
  composite all PNGs into 800×480 frame (image crate)
  cache result in image_cache
  respond with Cache-Control: no-store

WEBHOOK (POST /api/webhook/:plugin_instance_id)
  parse JSON body
  write into DomainState + InstanceStore
  trigger re-render of displays using that instance
```

Shared state is held as `Arc<RwLock<...>>` — notably `DomainState`
(source data) and the image cache (rendered PNG bytes). There is no global
mutable state outside these locks.

## Configuration System

Configuration is layered TOML, with SQLite as the source of truth at runtime
(TOML seeds the DB on first boot).

**`config/config.toml`** — main config
- `[server]` port, refresh_rate_secs
- `[display]` width, height
- `[location]` latitude, longitude, name
- `[sources]` per-source polling intervals
- `[sources.river|trail|ferry|road]` per-source settings (site IDs, route IDs, API keys)
- `[storage]` db_path
- `[auth]` (optional) username/password for admin UI
- `[device]` (optional) thin-client settings for wall-mounted devices

**`config/display.toml`** — an array of `[[display]]` layouts, each with a
name and `slots = [{ plugin, variant, x, y, width, height }, …]`. Seeded into
`layout_store` on first boot.

**`config/plugins.d/*.toml`** — one plugin definition per file:
`[[plugin]]` with `id`, `name`, `source`, `refresh_interval_secs`,
`data_strategy`, template paths per variant, `[[plugin.criteria]]`,
`[[plugin.settings_schema]]`, `[[plugin.default_elements]]`. Watched for
changes via `notify`; reloaded on file change or SIGHUP.

**`config/secrets.toml`** — auto-generated on first boot; contains the
64-char hex device API key. Gitignored.

**Environment variables**
- `SIDECAR_URL` (default `http://localhost:3001`)
- `SKAGIT_FIXTURE_DATA=1` — use canned fixtures, skip live HTTP
- `RUST_LOG` — e.g. `info`, `debug`
- `NPS_API_KEY`, `WSDOT_ACCESS_CODE` — source credentials

## Built-in Sources

| Source        | API                          | Auth              | Default interval |
| ------------- | ---------------------------- | ----------------- | ---------------- |
| NOAA weather  | `api.weather.gov`            | User-Agent        | 300 s            |
| USGS river    | `waterservices.usgs.gov`     | —                 | 300 s            |
| WSDOT ferries | `wsdot.wa.gov/API`           | `access_code`     | 60 s             |
| WSDOT roads   | `wsdot.wa.gov/API`           | `access_code`     | 1800 s           |
| NPS trails    | `api.nps.gov`                | `api_key`         | 900 s            |
| Generic HTTP  | user-configured              | custom headers    | 300 s (min 30 s) |

All sources use `ureq` from a Tokio blocking thread pool. Errors never
crash the task; the last good value is retained and re-served until the
next successful fetch.

## Key Patterns

**Plugin system.** Trait-based (`Source`) plus TOML-defined plugin metadata.
New plugins can be added by dropping a file into `config/plugins.d/` — no
recompile for pure template/criteria changes. The file watcher reloads the
registry on change.

**Slot-based compositor.** Each layout is a list of `LayoutItem`s with
explicit geometry (`x`, `y`, `width`, `height`, `z_index`). Items render
concurrently; the compositor blits results into a single frame.

**Evaluation engine.** Each plugin's criteria implement the `Criterion`
trait. Criteria produce pass/fail plus near-miss margins (e.g. "within 5 °F"
or "within 10 % of target flow"). Stale data trips a separate status.

**Template context.** Every Jinja template receives:
- `data` — cached source JSON
- `settings` — the instance's user-configured settings
- `trip_decision` — go / no-go plus per-criterion results
- `now` — timestamp context (`unix`, `iso`, `local` strings)
- `error` — optional error from last failed fetch

**Caching.** Source data cached in `DomainState` (RAM) and `instance_store`
(SQLite). Rendered PNGs cached in-process keyed by display ID.
