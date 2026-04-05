# Stack Evaluation: New Cascades Architecture

> Addresses cs-3pf. Informs implementation of the target architecture in
> `docs/research/target-architecture.md`. Supersedes any implicit stack
> assumptions in `architecture-comparison.md`.

---

## Recommendation

**Rust core + Bun render sidecar + Liquid (minijinja) templates + SQLite + React/Vite (deferred)**

| Concern | Choice |
|---|---|
| Server language | Rust (keep existing) |
| Rendering pipeline | Bun sidecar: Puppeteer + Sharp |
| Template language | Liquid via `minijinja` Rust crate |
| Data layer | SQLite via `rusqlite` |
| Frontend UI | React + Vite (deferred to v2) |

The rest of this document is the defense.

---

## 1. Rendering Pipeline: Bun Sidecar (Puppeteer + Sharp)

**Chosen: Puppeteer (headless Chromium) + Sharp, running in a Bun process as a stateless HTTP sidecar.**

### Why not pure Rust image rendering?

Cascades currently renders images with Rust's `image` crate — pixel-level drawing with hardcoded geometry. This is the core extensibility problem: every new layout requires Rust code and a recompile. The target architecture's entire value proposition is that plugins provide HTML templates and the render pipeline turns them into e-ink images.

There is no production-ready CSS layout engine in Rust. The `image` crate does pixel manipulation; it has no concept of `display: flex`, `grid--cols-4`, or the TRMNL utility classes that the existing plugin ecosystem is built on. The only paths to correct CSS rendering are:

1. Headless browser (Chromium via Puppeteer/Playwright)
2. WebKit/Blink bindings (heavy native deps, platform-specific, not Pi-friendly)
3. A minimal CSS subset renderer (would need to implement the entire TRMNL CSS framework in Rust — not viable)

Puppeteer is the right answer. It is proven by both inker (Puppeteer + Sharp, deployed as a Bun service) and the TRMNL reference implementations (larapaper uses Browsershot/Puppeteer, byos_next uses Takumi/Satori). Every self-hosted TRMNL-compatible server converges on a headless browser because there is no other way to render the TRMNL CSS framework correctly.

### Why Bun, not Node?

Bun is Node-compatible but ships as a single binary with a fast startup time and a smaller memory footprint. On a Raspberry Pi 4 (the target deployment), this matters. Bun starts in ~30ms vs Node's ~100-200ms. The sidecar is a long-lived process (Chromium stays alive between renders), so startup cost is amortized — but a smaller binary and faster boot is still better for Pi deployment.

### Sidecar model vs monolith

The sidecar (HTTP POST /render → PNG) is the correct decomposition. The Rust server handles all application logic: data fetching, caching, template rendering, evaluation. The Bun sidecar handles one thing: HTML → PNG. Keeping these separate means:

- Rust code never depends on Node.js APIs
- The sidecar can be restarted without touching the Rust process
- The interface is tiny: one HTTP endpoint, well-defined contract (see target-architecture.md §5d)
- The sidecar is ~150 lines of code

### Resource cost on Pi

Headless Chromium is the honest cost here: ~200MB RAM for the browser process. On a Pi 4 with 2GB RAM this is acceptable. On a Pi Zero (512MB) it is not. Cascades targets the Pi 4/5 class of hardware. Chromium is the tradeoff for full CSS fidelity — there is no lighter option that correctly renders the TRMNL framework.

Sharp runs in the same Bun process and adds ~30-50MB. Total sidecar footprint: ~250MB. The Rust server is a single binary with no runtime overhead.

### Floyd-Steinberg dithering

Sharp's dithering pipeline (threshold 140, error diffusion coefficients 7/16, 5/16, 3/16, 1/16) is already validated against TRMNL hardware by inker. Reimplementing this in Rust (`image` crate has basic dithering but not the exact parameters TRMNL expects) would require porting inker's pixel math and re-validating against hardware. The Sharp implementation is proven; use it.

---

## 2. Template Language: Liquid via minijinja

**Chosen: Liquid templates rendered by the `minijinja` Rust crate.**

### Why Liquid over React components?

The TRMNL plugin ecosystem — both the hosted platform and inker — uses Liquid templates. A plugin author dropping a new source into Cascades should be able to adapt an existing TRMNL plugin template with minimal changes. React components (byos_next's model) require:

- Node.js toolchain
- TypeScript knowledge
- A build step (npm run build) before a plugin template can be used
- Runtime in a Node process (can't run in Rust)

Liquid templates are plain text files. They can be edited in any text editor, dropped into the `plugins.d/` directory, and picked up by hot-reload without any build step. The plugin authorship story is: write HTML + a handful of Liquid tags. That is the lowest possible barrier to entry.

### Why minijinja over other Rust template engines?

`minijinja` is a Jinja2-compatible template engine in pure Rust with no native dependencies. TRMNL uses Liquid (a Ruby/JS variant of Jinja2); the syntax is nearly identical for the common cases (variable output, conditionals, loops, filters). Plugin templates written for TRMNL or inker require at most cosmetic adaptation to run under minijinja.

Alternatives considered:

| Engine | Issue |
|---|---|
| `tera` | Jinja2-compatible but diverges on filter names; no path from TRMNL templates |
| `handlebars` | Different syntax entirely; breaks TRMNL ecosystem compatibility |
| LiquidJS (Node) | Would require template rendering in the sidecar, coupling data and HTML concerns |
| `liquid` Rust crate | Less maintained than minijinja, thinner ecosystem |

minijinja is actively maintained, supports custom filters (needed to port the TRMNL Liquid filter set: `number_with_delimiter`, `days_ago`, `pluralize`, etc.), and has no native dependencies — important for cross-compilation to ARM (Pi).

### The template authorship contract

A plugin template receives a well-defined Liquid context (see target-architecture.md §5c):

```
{{ data.field_name }}           ← plugin's fetch result
{{ settings.site_name }}        ← user-configured settings
{% if trip_decision.go %}       ← optional go/no-go evaluation
{{ now.local }}                 ← current time
{% if error %}stale{% endif %}  ← error state
```

This is learnable in an afternoon by anyone who has used Liquid, Jinja2, or Handlebars. It is a better authorship story than React + TypeScript + NestJS decorators (inker's native plugin path).

---

## 3. Server Language: Rust (keep existing)

**Chosen: Keep the Rust core. Add the Bun sidecar for rendering only.**

### Why not rewrite in TypeScript/Bun?

Inker is a full TypeScript/Bun rewrite of the TRMNL server concept. It is an excellent piece of software. It is also the wrong model for Cascades for the following reasons:

**Operational dependencies**: inker requires PostgreSQL + Redis. For a self-hosted Raspberry Pi appliance, this means the user must run three services (inker, postgres, redis) or use Docker Compose. The Cascades target is a single binary + one SQLite file. That is a fundamentally different deployment model.

**Existing codebase**: The five Cascades sources (`noaa.rs`, `usgs.rs`, `wsdot.rs`, `trail_conditions.rs`, `road_closures.rs`) are working and correct. The `Source` trait, scheduler, and evaluation logic are sound. A rewrite discards this work to end up at a heavier dependency tree.

**Runtime footprint**: A Rust binary is ~5-10MB, uses ~20-30MB RAM at idle. A NestJS/TypeScript application is ~150MB installed, uses ~100-200MB RAM at idle. On a Pi with 2GB RAM (after Chromium's 200MB), this matters.

**Cascades' differentiator**: The evaluation engine (go/no-go trip planner) is not present in inker or any TRMNL implementation. It is Cascades' core value. The `Criterion` trait and `TripDecision` type are already well-modeled in Rust. A TypeScript rewrite gains nothing here and loses the type safety that makes the criterion dispatch correct.

### Why not Go?

Go would require a full rewrite (zero existing Go code) for a server whose Rust implementation is mostly correct. Go has no advantage over Rust for this workload — the bottleneck is Chromium startup and e-ink image generation, not the Rust HTTP server. The existing Rust code should be migrated to the new architecture, not discarded.

### What the Rust core does and does not do

Rust handles: HTTP server (axum), scheduler (Tokio tasks), data fetching (ureq), data cache (Arc<RwLock<HashMap>>), evaluation engine (Criterion trait), template rendering (minijinja), plugin registry (TOML loader), instance store (rusqlite), PNG compositing (image crate), output layer.

Bun handles: one thing only — POST /render { html } → PNG. This is the sidecar boundary.

---

## 4. Data Layer: SQLite

**Chosen: SQLite via `rusqlite` for plugin instance storage.**

### What needs to be persisted?

1. Plugin instance settings (user config, encrypted sensitive fields)
2. Cached fetch data per instance
3. Last fetch timestamp and last error
4. Display configurations (named slot layouts)

This is not a high-write workload. A typical Cascades installation has 5-20 plugin instances, each writing cached data every 5-60 minutes. SQLite handles this trivially.

### Why not PostgreSQL?

PostgreSQL is the right choice for inker (multi-user, multi-device, cloud deployment). For Cascades (single device, self-hosted, Pi appliance), PostgreSQL means running a separate service, managing connection pooling, handling startup ordering, and dealing with a 300MB installation footprint. The Cascades user is configuring a home e-ink display, not operating a database server.

### Why not an in-memory store?

Plugin settings must survive server restarts. Cached data should survive restarts (avoids a blank screen on startup if the upstream API is temporarily down). SQLite gives durability with zero operational overhead.

### SQLite schema

As defined in target-architecture.md §4:

```sql
CREATE TABLE plugin_instances (
    id TEXT PRIMARY KEY,
    plugin_id TEXT NOT NULL,
    settings TEXT NOT NULL,           -- JSON
    encrypted_settings TEXT,          -- JSON: AES-encrypted sensitive fields
    cached_data TEXT,                 -- JSON: last successful fetch
    last_fetched_at INTEGER,          -- Unix timestamp
    last_error TEXT
);
```

The display configuration lives in TOML files (consistent with existing Cascades config style) and is not stored in SQLite. Only runtime state (instance settings, cache) goes in the database.

---

## 5. Frontend: React + Vite (Deferred)

**Deferred to v2. Not a blocker for the rendering pipeline or plugin system.**

When the web UI is built, React + Vite is the right choice:

- The settings form is auto-generated from `settings_schema` (a JSON array of field definitions). This is exactly the kind of data-driven form rendering that React excels at.
- The screen designer (drag-and-drop slot layout, live preview) benefits from React's component model and state management.
- Vite produces a static bundle that the Rust server can serve as static files — no separate Node process required in production.

Alternative considered: **HTMX + server-side rendering** (no build step, simpler). This is viable for the settings form but insufficient for the screen designer, which needs client-side drag-and-drop with live preview. Since the screen designer is in scope for v2, using HTMX now and migrating to React later adds unnecessary churn.

Alternative considered: **Preact** (smaller bundle, React-compatible). The bundle size difference (~3KB vs ~45KB) is irrelevant for a local self-hosted UI. Use React for ecosystem breadth.

The `settings_schema` contract is fully defined in target-architecture.md. The frontend can be built independently against that contract without affecting the server implementation.

---

## 6. Full Stack Summary

```
┌─────────────────────────────────────────────────────────┐
│  Rust core (single binary, ~10MB)                        │
│                                                          │
│  axum          HTTP server + device API                  │
│  tokio          Scheduler, async fetch tasks             │
│  ureq           HTTP client for data sources             │
│  minijinja      Liquid template rendering                │
│  rusqlite       Plugin instance store (SQLite)           │
│  notify         Filesystem hot-reload (plugin changes)   │
│  image          PNG compositing (slot layout)            │
│  serde_json     Open data model (source → cache → tmpl)  │
│  toml           Plugin registry and display config       │
└─────────────────────────────────────────────────────────┘
                           │
                    POST /render
                           │
┌─────────────────────────────────────────────────────────┐
│  Bun sidecar (~150 lines, long-lived process)            │
│                                                          │
│  Bun HTTP       Lightweight HTTP server                  │
│  Puppeteer      Headless Chromium: HTML → raw PNG        │
│  Sharp          Grayscale → Floyd-Steinberg dither →     │
│                 ≤90KB PNG (threshold 140, negate device) │
└─────────────────────────────────────────────────────────┘
                           │
                      PNG bytes
                           │
┌──────────────────────────┐
│  E-ink device / browser  │
└──────────────────────────┘
```

**Deployment artifact**: `cascades` binary + `cascades-render` Bun script + SQLite file + TOML configs. No Docker required (though a Compose file is a reasonable convenience for users who want it). Chromium must be installed on the host (it is the only system dependency).

**Pi 4 footprint** (estimated):
- Rust server: ~30MB RAM idle
- Bun + Chromium sidecar: ~250MB RAM (Chromium dominates)
- Total: ~280MB on a 2GB Pi 4 — acceptable

---

## 7. What This Stack Deliberately Does Not Do

**No JavaScript data transforms**: Inker supports `dataTransform` — arbitrary user JavaScript executed against fetch results. This is powerful but adds an eval()-style execution surface, a 10s timeout mechanism, and complexity in the data pipeline. Cascades achieves the same result through the `Source` trait: transformations happen in Rust code, which is type-safe and compiled. Community plugins that need data transformation write a Rust source implementation, not a JavaScript snippet.

**No plugin marketplace sync**: The inker `GET /plugins/github-plugin/:slug` endpoint fetches plugins from the TRMNL GitHub repo. This is a useful feature for an online service; for a self-hosted appliance it adds network dependency and a trust surface. Plugin installation is: drop files into `plugins.d/`. A marketplace layer can be added later without architectural changes.

**No multi-user / RLS**: SQLite without Row Level Security is the deliberate choice. Cascades is a single-owner device. Multi-user support would require either PostgreSQL (with RLS as byos_next uses) or a custom tenant isolation layer. Neither is appropriate for a Pi appliance.

**No Redis**: Inker uses BullMQ + Redis for job queuing. Cascades' scheduler is a set of Tokio tasks with per-source intervals — the same behavior with zero infrastructure.
