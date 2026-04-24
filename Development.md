# Development Guidelines

These are the working conventions for Cascades. For the "what" of the system,
see [Architecture.md](Architecture.md).

## Prerequisites

- Rust stable (via `rustup`)
- Bun ≥ 1.0 (for the render sidecar)

## Running Locally

Cascades needs both the Rust server and the Bun sidecar running.

```bash
# One-time: install sidecar deps
cd src/sidecar && bun install

# Terminal 1 — sidecar
cd src/sidecar
bun server.ts

# Terminal 2 — server
cargo run
```

**Fixture mode** — no live API calls, good for UI work and tests:

```bash
./scripts/dev-server.sh
# equivalent to:
SKAGIT_FIXTURE_DATA=1 RUST_LOG=info cargo run
```

**Override the sidecar URL:**

```bash
SIDECAR_URL=http://localhost:3002 cargo run
```

**Release build:**

```bash
cargo build --release   # → target/release/cascades
```

## Testing

Tests live in `tests/`:

- `server_acceptance_tests.rs` — end-to-end HTTP acceptance tests mapped to
  user stories. Uses an in-process Axum router and a mock sidecar, so no
  external processes required.
- `compositor_tests.rs` — PNG blitting and layout-slot rendering.
- `template_visual_tests.rs` — regression tests for Liquid template output.

Unit tests are colocated with their modules (notably `format.rs` and
`jsonpath.rs`).

```bash
cargo test                         # everything
cargo test <name>                  # single test by name substring
cargo test -- --nocapture          # show stdout
cargo test -- --test-threads=1     # run serially (occasionally needed)
```

Test databases use `tempfile`, so they don't leak between runs. Fixture JSON
is embedded in the source files under `src/sources/fixtures/` — don't add
new fixtures that depend on a live API.

**Before opening a PR:** run `cargo test` and `cargo clippy`. If you touched
the sidecar, run `cd src/sidecar && bun test`.

## Code Conventions

**Error handling.** Use `thiserror` for module-level error enums. Name them
`Error` within a module or `XyzError` at crate level (see
`sources::SourceError`). Sources must not panic — log and retain last good
value. HTTP handlers return proper status codes (404 for missing layouts,
401 for bad auth, etc.), not generic 500s.

**Logging.** Use `log::{info, warn, debug, error}` macros. `env_logger` is
initialized in `main.rs`. Drive verbosity with `RUST_LOG` (e.g.
`RUST_LOG=cascades=debug,info`).

**Concurrency.**
- Shared state lives in `Arc<RwLock<...>>` or `Arc<Mutex<...>>`.
- Use `tokio::spawn` for background tasks and per-item concurrent work.
- Use `tokio::task::spawn_blocking` for CPU-bound or blocking I/O
  (`ureq` HTTP calls, template rendering, SQLite).
- No global mutable state — pass `AppState` by `Arc` clone.

**Module layout.** Each major component is a module with a `mod.rs`. Trait
definitions go at the top of the module. Keep handlers in `api.rs` thin;
business logic belongs in the feature module.

**Naming.**
- Plugin IDs are snake_case (`river`, `weather`, `trail`).
- Sources are named after the provider (`NoaaSource`, `UsgsSource`).
- Criterion types end in `Criterion` (`MinTempCriterion`).
- Stores are explicitly named (`InstanceStore`, `LayoutStore`,
  `SourceStore`, `SourceScheduler`).

**Database.** Stores wrap `Mutex<Connection>` internally. Create schema in
`open()` if not present — no separate migration tool yet. Prefer a single
statement per operation; reserve transactions for genuine multi-write
sequences.

**Comments.** Default to none. Add one only when the *why* is non-obvious
(a hidden constraint, a workaround, a subtle invariant). Don't restate
what the code says.

## Adding a Plugin

1. Drop a TOML file in `config/plugins.d/` defining `[[plugin]]` with its
   id, source, refresh interval, data strategy, template paths, criteria,
   and settings schema. See existing plugins (`weather.toml`, `river.toml`,
   `ferry.toml`, `trail.toml`, `road.toml`) as references, and
   [`docs/plugin-authoring.md`](docs/plugin-authoring.md) for the full
   field reference.
2. Add Liquid templates under `templates/` for each variant you support
   (`_full`, `_half_horizontal`, `_half_vertical`, `_quadrant`).
3. If the source type is new (not covered by the generic HTTP source),
   implement the `Source` trait in `src/sources/` and wire it into
   `build_sources()` in `src/lib.rs`.
4. The registry hot-reloads on file change — no restart needed for pure
   TOML/template edits.

## Documentation

Design and user docs live in `docs/`:

- `plugin-authoring.md` — full plugin TOML reference, template context,
  available CSS utilities.
- `layout-composer-design.md` — the admin-UI layout composer's element
  decomposition, grouping, alignment, and drag-drop model.
- `admin-ui-design.md` — canvas editor, palette, property inspector,
  preview panel.
- `design-configurable-data-sources.md` — generic HTTP sources, field
  mapping, JSONPath extraction.
- `test-cases.md` — manual test checklist.
- `user-stories.md` — feature stories driving the roadmap.
- `research/target-architecture.md` — authoritative architectural
  reference (component diagram, data flow, design principles).
- `research/architecture-comparison.md`, `research/inker-architecture.md`,
  `research/cascades-extensibility-diagnosis.md` — background research.

Update the relevant doc when you change the behavior it describes. Don't
create new top-level docs for things that belong in an existing one.

## Git Workflow

**Commit messages** follow a lightweight Conventional-Commits style:

```
feat: layout composer Phase 4 — arrangement polish (cs-3wq)
fix: canvas item labels back to black text
docs: plugin layout composer design (cs-660)
chore: add config/secrets.toml to .gitignore
```

- Prefix: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`.
- Capital letter after the colon.
- Em-dash (` — `) to separate scope from detail when useful.
- Optional short issue id in parens at the end (e.g. `(cs-3wq)`).
- Terse but descriptive; assume the reader knows the codebase.

**Branching.** Work on a feature branch; open a PR against `main`. Don't
push directly to `main`.

**Secrets.** `config/secrets.toml` is gitignored and auto-generated. Never
commit real API keys. Pass source credentials through environment variables
or local config.
