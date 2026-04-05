# Cascades Extensibility Diagnosis

> Diagnosis of the current architecture through the lens of adding new datasources.
> All file:line references are to `src/` relative to the repo root.

---

## 1. Cost to Add a New Datasource Today

Adding a hypothetical new source (e.g., a tide gauge or air-quality index) requires
touching **11 files** across every layer of the stack. Here is the exact change
list, in dependency order:

| # | File | What changes |
|---|------|-------------|
| 1 | `src/sources/<new>.rs` | New file — implement `Source` trait |
| 2 | `src/sources/mod.rs:1` | `pub mod <new>;` declaration |
| 3 | `src/domain/mod.rs:193` | Add variant to `DataPoint` enum |
| 4 | `src/domain/mod.rs:203` | Add `pub <field>: Option<NewStruct>` to `DomainState` |
| 5 | `src/domain/mod.rs:213` | Add match arm in `DomainState::apply()` |
| 6 | `src/domain/mod.rs:68` | Add `pub <signal>: bool` to `RelevantSignals` |
| 7 | `src/domain/mod.rs:183` | Add `pub <signal>_secs: Option<u64>` to `SourceAge` |
| 8 | `src/config/mod.rs:91` | Add `<new>_interval_secs` and `<new>: Option<NewSourceConfig>` to `SourceIntervals` |
| 9 | `src/lib.rs:50` | Instantiate source in `build_sources()` |
| 10 | `src/presentation/mod.rs` | Add `NewContent` struct, add field to `DataContent` or `ContextContent`, update `build_display_layout()` |
| 11 | `src/render/layout.rs` | Add rendering logic for the new content in `layout_and_render_display()` |

If the new source participates in trip evaluation (go/no-go), add:

| 12 | `src/domain/mod.rs:109` | Add threshold field(s) to `TripCriteria` |
| 13 | `src/evaluation/mod.rs:37` | Add evaluation block in `evaluate()` and `evaluate_detail()` |

**Total: 11–13 files** for a new datasource (11 minimum; +2 if evaluation criteria are needed).

---

## 2. Rendering Pipeline Coupling to Specific Datasource Types

The rendering pipeline is tightly coupled to source types at every layer. Specific
coupling points:

### `src/domain/mod.rs`

- **`DataPoint` enum (line 193):** Closed sum type — `Weather`, `River`, `Ferry`,
  `Trail`, `Road`. Every new source must add a variant here, which propagates a
  compiler-enforced exhaustive-match requirement through all downstream `match`
  statements.

- **`DomainState` struct (line 203):** Five explicit named `Option<T>` fields.
  There is no dynamic collection — the set of sources is fixed in the struct
  definition.

- **`DomainState::apply()` (line 213):** A hard-coded `match` on `DataPoint` that
  writes to the named fields. Adding a source = adding a match arm here.

- **`RelevantSignals` (line 68):** Five named booleans (`weather`, `river`, `ferry`,
  `trail`, `road`). Per-destination filtering is field-by-field, not data-driven.

- **`SourceAge` (line 183):** Five named `Option<u64>` fields — same pattern.

### `src/presentation/mod.rs`

- **`DataContent` (line 279):** Hardcodes `river: Option<RiverContent>` and
  `ferry: Option<FerryContent>` — the data zone is structurally bound to exactly
  these two sources.

- **`ContextContent` (line 302):** Hardcodes `trail: Option<TrailContent>` and
  `road: Option<RoadContent>` — the context zone is structurally bound to exactly
  these two sources.

- **`build_display_layout()` (line 326):** Computes four separate booleans
  (`any_river`, `any_ferry`, `any_trail`, `any_road`) from destination signals.
  Each is its own `if` block with hardcoded field access.

- **Five format functions** (`format_weather`, `format_river`, `format_ferry`,
  `format_trail`, `format_road`): Each takes a specific domain type, not a generic
  interface.

### `src/render/layout.rs`

- **Imports (line 1–2):** Directly imports `FerryContent`, `HeroDecision`,
  `RiverContent`, `TrendArrow`, `WeatherIcon` — the render layer knows the
  concrete names of every presentation type.

- **`layout_and_render_display()`:** Pattern-matches on `HeroDecision` variants and
  reads `DisplayLayout.data.river`, `.data.ferry`, `.context.trail`, `.context.road`
  by name. Zone assignments (left vs. right column, data vs. context zone) are
  hard-wired in the function body, not driven by metadata.

### `src/evaluation/mod.rs`

- **`evaluate()` (line 37) and `evaluate_detail()` (line 189):** Explicitly evaluate
  weather, river, and road signals with per-signal logic blocks. Adding a new
  evaluable signal = adding a new code block.

---

## 3. Plugin / Datasource Abstraction

**Fetch side: clean trait abstraction.**

`src/sources/mod.rs:28` defines `Source`:

```rust
pub trait Source: Send {
    fn name(&self) -> &str;
    fn refresh_interval(&self) -> Duration;
    fn fetch(&self) -> Result<DataPoint, SourceError>;
}
```

`build_sources()` returns `Vec<Box<dyn Source>>` and the runtime scheduler treats
all sources identically — spawn one task per source, call `fetch()`, call `apply()`.
This is a well-formed extension point for fetching.

**Domain side: closed enum, no abstraction.**

The `DataPoint` enum returned by every `Source::fetch()` is a closed sum type. A
source cannot produce a domain value without modifying the enum. There is no
`DataPoint::Custom(Box<dyn Any>)` escape hatch.

**No plugin registry.** `build_sources()` (`src/lib.rs:50`) is a hardcoded
constructor list — five `sources.push(...)` calls. There is no factory, no
registration mechanism, no dynamic dispatch at the construction level.

**Summary:** The abstraction boundary stops at `fetch()`. Everything downstream
(domain state, presentation, render, evaluation) is coupled to a fixed, named set
of source types.

---

## 4. Data Contract: Sources → Domain → Presentation → Render

```
Source::fetch() → DataPoint (closed enum, 5 variants)
                       ↓
         DomainState::apply() — writes to named field
                       ↓
         DomainState { weather, river, ferry, trail, road }
                       ↓
    build_display_layout() — reads each field by name, constructs DisplayLayout
                       ↓
         DisplayLayout {
           header: HeaderContent,    // river site name, timestamps
           hero:   HeroContent,      // TripDecision + WeatherContent
           data:   DataContent,      // RiverContent + FerryContent
           context: ContextContent,  // TrailContent + RoadContent
         }
                       ↓
      render_display() → layout_and_render_display()
                       ↓
                  PixelBuffer (800×480, 1-bit)
```

The contract between each layer is static and fully compile-time checked. There is
no runtime schema, no serialization boundary, no versioning. The pipeline is:

- **Sources → Domain:** `DataPoint` enum variant. One variant per source type.
- **Domain → Presentation:** Named field reads from `DomainState`. The presentation
  layer imports concrete domain types (`WeatherObservation`, `RiverGauge`, etc.).
- **Presentation → Render:** Typed `DisplayLayout` struct with fixed 4-zone
  structure. The render layer imports concrete presentation types
  (`RiverContent`, `FerryContent`, `WeatherIcon`, etc.).
- **Evaluation → Hero zone:** `TripDecision` → `HeroDecision` (presentation) →
  rendered in the hero zone left column.

**What is NOT generic:** the zone layout itself. The display is hard-partitioned into
four zones with fixed slot assignments for specific source types. `RiverContent`
always goes in the data zone left column; `FerryContent` always goes in the data
zone right column. There is no mechanism to reflow or reroute content to a different
slot.

---

## 5. What Would Need to Change for Runtime-Configurable Datasource Selection

Today's constraint: all 5 source types are unconditionally instantiated (some are
no-ops if config/API key is absent, but they are always present in the code path).
The set of renderable source types is fixed at compile time.

To support user-configurable source selection at runtime, these are the required
changes, roughly in order of difficulty:

### (a) `DataPoint` — open the closed enum

**Problem:** Every source must return a named `DataPoint` variant. A 6th source
requires a 6th variant, which requires updating every `match` downstream.

**Change needed:** Either keep the enum and accept the 11-file cost per source
(current approach), or replace `DataPoint` with a trait-object or `HashMap<SourceId,
Box<dyn SourceValue>>`. The latter makes the domain generic but loses compile-time
exhaustiveness.

### (b) `DomainState` — replace named fields with a dynamic map

**Problem:** `DomainState` has five named `Option<T>` fields
(`src/domain/mod.rs:203`). A 6th source requires a 6th field.

**Change needed:** Replace with `HashMap<SourceId, Box<dyn SourceValue>>` or a
slot-indexed array. This eliminates the named-field access pattern throughout
presentation and evaluation.

### (c) `build_sources()` — replace hardcoded list with config-driven factory

**Problem:** `src/lib.rs:50` — five hardcoded `sources.push(...)` calls. The
source set cannot be changed without a code change.

**Change needed:** A source registry or factory map keyed on source type names from
config. E.g., `config.sources.enabled: ["noaa", "usgs", "wsdot-ferry"]` driving
dynamic instantiation.

### (d) `RelevantSignals` and `TripCriteria` — replace with dynamic maps

**Problem:** `RelevantSignals` (`src/domain/mod.rs:68`) is a fixed struct of
booleans. `TripCriteria` (`src/domain/mod.rs:109`) is a fixed struct of optional
thresholds. Both require code changes for each new source.

**Change needed:** `RelevantSignals` → `HashSet<SourceId>`. `TripCriteria` →
`HashMap<SourceId, Threshold>` with a generic threshold type.

### (e) `DisplayLayout` — replace fixed 4-zone typed struct with a slot system

**Problem:** `DataContent` and `ContextContent` (`src/presentation/mod.rs:279,302`)
have named fields for specific source types. Adding a 6th source has no slot.

**Change needed:** Replace typed zone structs with a slot-based layout model
(e.g., a `Vec<DisplaySlot>` with type-erased content) or define new zones. The
render layer would need a dispatch mechanism rather than field-name access.

### (f) Evaluation — generalize the per-signal evaluation blocks

**Problem:** `src/evaluation/mod.rs:37` has one hardcoded block per evaluable
signal. Adding a new evaluable source = adding a new block.

**Change needed:** Factor evaluation into a `Criterion` trait or data structure that
any source can register thresholds against. The evaluator becomes a generic loop
over registered criteria rather than a hardcoded sequence.

---

## Summary: Cost to Add a New Datasource Today

| Dimension | Current state |
|-----------|--------------|
| Files to change | 11–13 |
| Compiler-enforced exhaustion points | 4 (`DataPoint` match in `apply()`, evaluation, presentation build, render layout) |
| Runtime extensibility | None — all sources must be known at compile time |
| Fetch-layer abstraction | **Good** — `Source` trait is clean |
| Domain abstraction | **None** — closed `DataPoint` enum, named `DomainState` fields |
| Presentation abstraction | **None** — typed zone structs, hardcoded slot assignments |
| Render abstraction | **None** — concrete type imports, fixed zone rendering |
| Evaluation abstraction | **None** — hardcoded per-signal evaluation blocks |

**The architecture is correctly layered and internally consistent** but the
abstraction boundary stops at the fetch interface. The domain, presentation, render,
and evaluation layers form a single coherent but monolithic closed-world assumption
over exactly five source types.

**The smallest intervention** to reduce the 11-file cost would be to move the
`DataPoint` enum toward a trait-object or indexed variant model so that new source
values can propagate through `DomainState` without requiring field additions —
reducing the downstream cascade to presentation/render configuration only. The
`Source` trait itself needs no change.
