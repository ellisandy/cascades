# Plugin Layout Composer — Design

**Issue:** cs-660
**Status:** Design (review-only)
**Scope:** Extend the admin UI layout editor into a full composer with multi-element
plugin decomposition, arrangement tools, and preset-driven authoring. Implementation
is phased; this document is the architectural contract.

---

## 1. Goals and Non-Goals

### Goals

1. A single plugin instance on the canvas may be composed of **multiple visual
   elements** (labels, values, icons, dividers) rather than a single opaque slot.
2. Every element — whether standalone or nested inside a plugin — is individually
   **resizable** (drag handles + property inputs) and **freely positionable**.
3. The five **built-in plugins** (weather, river, ferry, trail, road) are
   decomposable into their constituent elements. Users can edit the layout of
   the built-ins, not just drop them as sealed blocks.
4. **Arrangement tools**: multi-select, align (L/C/R/T/M/B), distribute
   (horizontal/vertical), snap-to-grid, z-order (bring forward / send back /
   bring to front / send to back), duplicate, group/ungroup.
5. **Layout persistence**: every composition is saved via the existing layout
   API and respected by the compositor at render time. No new storage backend.

### Non-Goals

- Runtime plugin authoring (writing plugin code from the UI).
- Multi-page/multi-display management (tracked separately).
- Animation or e-ink refresh choreography.
- Responsive layouts for non-800×480 displays.

---

## 2. Current State (Baseline)

See `src/layout_store/mod.rs:60-128` for the `LayoutItem` enum and
`docs/admin-ui-design.md` for the baseline canvas UX.

What already works today:

- `LayoutItem` is a tagged enum with 5 variants (`PluginSlot`, `StaticText`,
  `StaticDateTime`, `StaticDivider`, `DataField`), each with
  `id / z_index / x / y / width / height`.
- Admin UI at `templates/admin.html` supports drag-drop from palette, property
  editing, snap-to-grid, bring-forward/send-back, duplicate.
- The compositor (`src/compositor.rs`) renders items in z-order, delegating
  plugin and text renders to a Bun sidecar and drawing dividers directly.
- `DataField` (cs-m0q) already demonstrates "a single field from a plugin's
  cached JSON, positioned independently" — it is the critical primitive that
  enables everything below.

**Key insight:** the data model already supports free positioning and
heterogeneous element types. The gap is conceptual (grouping, plugin
decomposition) and tooling (arrangement ops, multi-select).

---

## 3. Data Model

Two additive, backwards-compatible changes to `LayoutItem`:

### 3.1 `parent_id: Option<String>` on every variant

Every `LayoutItem` gains an optional `parent_id`. When set, it names the `id` of
another item (a container) that owns this item. Semantics:

- Containers are themselves `LayoutItem::PluginSlot` with a new
  `container: Option<bool>` flag (see 3.2) OR a dedicated
  `LayoutItem::Group` variant (see 3.3). We adopt the Group variant for
  clarity — `PluginSlot` stays a leaf.
- A child's `x/y` are **canvas-absolute** for backwards compatibility. The
  composer UI displays them as parent-relative when editing, but persists
  absolute coordinates. This avoids a migration and keeps the compositor
  unchanged.
- When the user drags a container, the UI translates all descendants by the
  same delta before persisting.
- Z-order is computed on the flattened item list (parent z + child z-index
  ordering within parent). Containers paint their own optional background
  (e.g. plugin chrome) before descendants; descendants paint in their
  relative z order.
- Deleting a container deletes its descendants (confirmed via modal).

### 3.2 New variant: `LayoutItem::Group`

```rust
Group {
    id: String,
    z_index: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    /// Optional plugin binding — if set, the group is a "plugin group" and
    /// its descendants are the decomposed elements of that plugin instance.
    /// If None, it's a plain user-defined group.
    plugin_instance_id: Option<String>,
    /// Human label shown in the z-order list / outliner.
    label: Option<String>,
    /// Optional background mode: "none" (transparent), "card" (white fill
    /// + 1px border), "plugin_chrome" (reserved for built-in decoration).
    background: Option<String>,
}
```

A Group has geometry — it's a visual frame — but no content of its own beyond
an optional background. Its children are other `LayoutItem`s whose `parent_id`
equals the group's `id`.

### 3.3 New migration (SQLite)

Add two columns to `layout_items`:

```sql
ALTER TABLE layout_items ADD COLUMN parent_id TEXT;
ALTER TABLE layout_items ADD COLUMN plugin_instance_id_opt TEXT;  -- for groups
ALTER TABLE layout_items ADD COLUMN label TEXT;
ALTER TABLE layout_items ADD COLUMN background TEXT;
```

Existing rows get `parent_id = NULL`, preserving flat layouts. The migration
runs idempotently in `LayoutStore::init` behind a schema version check.

### 3.4 Wire format (API)

`ItemPayload` in `src/api.rs` grows optional `parent_id`, `label`, and
`background` fields (all `skip_serializing_if = "Option::is_none"`). A new
`"group"` value is added to the `item_type` field of the tagged union.

Validation additions in `PUT /api/admin/layout/{id}`:

- `parent_id` must reference an existing item in the same payload (or be null).
- Parent chains cannot cycle (DFS check).
- A child's bounding box need not fit inside the parent's — free positioning
  is preserved. The parent's rectangle is a visual hint, not a clip region.
- `plugin_instance_id` on a `group` must reference a valid instance.

---

## 4. Built-in Plugin Decomposition

The core UX of requirement #4: users drag `weather` onto the canvas and get
**five individually editable elements** in a group, not one opaque block.

### 4.1 Plugin manifests declare a default element set

Each plugin definition in `config/plugins.d/<id>.toml` may declare a
`[[default_elements]]` array describing what items appear when the plugin is
first dropped on the canvas. Example for `weather`:

```toml
id = "weather"
name = "Weather"
source = "openweather"

[[default_elements]]
kind = "data_field"
field_path = "$.current.temp_f"
label = "Temp"
format_string = "{{value}}°F"
x = 10; y = 10; width = 120; height = 48; font_size = 36

[[default_elements]]
kind = "data_field"
field_path = "$.current.description"
x = 10; y = 60; width = 180; height = 20; font_size = 14

[[default_elements]]
kind = "static_divider"
x = 10; y = 86; width = 180; height = 2
```

When the user drags `weather` from the palette, the UI creates:

- One `Group { plugin_instance_id: "weather", ... }` at the drop position.
- One child item per `default_elements` entry, with `parent_id` pointing at
  the group and `x/y` offsets added to the drop position.

All children are regular `LayoutItem`s — the user can edit, resize, delete,
duplicate any of them with the same tools as standalone items.

### 4.2 "Traditional" plugin rendering as a fallback

For built-in plugins whose manifests do **not** declare `default_elements`, the
drag-drop behaviour is unchanged: a single `PluginSlot` is created. This
preserves backwards compatibility and lets plugin authors migrate
incrementally.

### 4.3 Field mapping bootstrap

Decomposed plugins need field mappings to exist in `data_source_fields`. The
plugin manifest's `[[default_elements]]` entries with `kind = "data_field"`
are upserted into `data_source_fields` on plugin registry reload
(`src/plugin_registry/mod.rs`), keyed by `(data_source_id, json_path)`. This
removes a manual bootstrap step.

### 4.4 Starter manifests for the five built-ins

Phase 1 of implementation ships decomposition manifests for
`weather`, `river`, `ferry`, `trail`, `road`. Each is a small TOML
authoring task; no Rust changes per-plugin.

---

## 5. Admin UI Changes

### 5.1 Outliner panel (new)

A thin vertical strip between the palette and the canvas shows the layout as a
tree:

```
▾ Default
  ▾ weather  (group)
      Temp         data_field
      Condition    data_field
      ───          divider
  ▸ river    (group)
    Title        static_text
    Updated…     static_datetime
```

- Click to select; shift-click to multi-select; drag to reorder z-index.
- Toggle ▸/▾ to collapse a group.
- Double-click a group to "enter" it (siblings dim, edits constrained to
  descendants until Escape).

Implementation: a new `<aside>` in `templates/admin.html` wired to the
existing `state.items` array (already the source of truth). No backend changes.

### 5.2 Multi-select

- Shift-click adds to selection; marquee-drag on empty canvas creates a
  rubber-band selector.
- Selection state: `state.selectedIds: Set<string>` (generalises today's
  `state.selectedId`).
- Property panel shows only fields common to all selected items. Changing a
  common field applies to all.

### 5.3 Resize handles

Eight handles per selected item (corners + edge midpoints). Dragging updates
`width`/`height` live, with snap-to-grid and Alt to disable snapping.
Shift preserves aspect ratio. Numeric property inputs remain the authoritative
edit path for precise values.

### 5.4 Arrangement toolbar

New toolbar row appears when two or more items are selected:

```
[⇤ Align Left] [═ Center H] [⇥ Align Right] [⇧ Top] [═ Middle] [⇩ Bottom]
[⟷ Distribute H] [⟺ Distribute V]
[⌃ Bring Front] [▲ Forward] [▼ Back] [⌄ Send Back]
[⌘D Duplicate] [Group] [Ungroup]
```

Semantics (all operate on `state.selectedIds`, update local state, push one
undo entry):

- **Align left**: set each item's `x` to `min(selected.x)`.
- **Align center H**: set each item's `x` to
  `mid(selected.bounds) − item.width/2`.
- **Align right**: set each item's `x` to
  `max(selected.right) − item.width`.
- **Top/middle/bottom**: analogous for `y`.
- **Distribute H**: sort selected by `x`, fix endpoints, space interior
  items so gaps between right-edge and next left-edge are equal.
- **Distribute V**: analogous for y.
- **Bring front / send back**: walk `state.items` sorted by z, ensure the
  selected stack is at the top / bottom of the list, preserving their
  relative order.
- **Group**: create a new `Group` item with bounds = union of selected, set
  each selected item's `parent_id` to the new group.
- **Ungroup**: clear `parent_id` on descendants of every selected group,
  delete the group container.
- **Duplicate**: for each selected, clone with a new UUID and offset
  (+10,+10). When duplicating a group, deep-copy descendants with
  new UUIDs too, preserving the parent chain.

### 5.5 Snap-to-grid hardening

Current snap toggle (`state.snapToGrid`) snaps to 10px. Add a grid size
selector (5 / 10 / 20 / 40 / 80 / variant-grid) and expose the active grid
visually (faint dashed lines when `snap ≠ off`). Arrow keys nudge by 1px or
one grid cell when snap is on.

### 5.6 Smart guides (stretch)

When dragging a single item, compare edges against all other items' edges; if
within 4px, snap and draw a magenta guide. Implementation is local — no
backend changes. Marked stretch to keep Phase 1 scope-contained.

---

## 6. Compositor

No changes are required for Phase 1. The compositor already renders all item
types in z-order and is agnostic to `parent_id`.

Phase 2 adds one optional capability:

### 6.1 Group backgrounds

When a `Group` has `background: Some("card")`, the compositor draws a white
rectangle with a 1px black border at the group's bounds before descendants.
This is a direct `imageops::overlay` of a pre-filled tile — no sidecar round
trip. Runtime cost is O(items) and negligible.

### 6.2 Order of operations

```
sorted_items = flatten(items, by parent_id).sort_by(z_index_ascending)
for item in sorted_items:
    if item is Group and item.background != None:
        draw_group_background(frame, item)
    elif item is Group:
        continue  # pure visual container, no pixels
    else:
        render_item(frame, item)  # existing path
```

Flattening is a two-pass in-memory operation; no SQL changes.

---

## 7. API Changes

All changes are additive to `src/api.rs`:

### 7.1 `ItemPayload` additions

```rust
pub struct ItemPayload {
    // ... existing fields ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
}
```

### 7.2 New `item_type` value: `"group"`

Dispatch in `ItemPayload::to_layout_item` gains a Group arm. All existing
variants keep working.

### 7.3 Validation on layout write

`validate_layout_payload` (new helper) runs on `PUT /api/admin/layout/{id}`:

1. All `parent_id`s reference existing items in the payload.
2. No parent-chain cycles (DFS).
3. Group items' `plugin_instance_id_opt` references a valid instance.
4. Existing coord/variant checks are preserved.

On failure: `422 Unprocessable Entity` with `{ "errors": [ ... ] }`.

### 7.4 New endpoint: `GET /api/admin/plugins/{id}/default_elements`

Returns the `default_elements` array from the plugin manifest so the admin UI
can spawn the decomposed elements client-side. Keeps the decomposition rules
server-authoritative while letting the UI avoid a round-trip per element.

```json
[
  { "kind": "data_field", "field_path": "$.current.temp_f", "x": 10, ... },
  ...
]
```

If the plugin has no `default_elements`, returns `[]` (UI falls back to
single `PluginSlot` spawn).

---

## 8. Phased Implementation Plan

Each phase is independently shippable and testable.

### Phase 1 — Data model + minimal UI (smallest shippable slice)

1. Schema migration: `parent_id`, `label`, `background`, `plugin_instance_id_opt`
   columns on `layout_items`.
2. `LayoutItem::Group` variant + (de)serialization + store read/write.
3. `ItemPayload` additions + `"group"` type + validation.
4. Admin UI: multi-select (shift-click + marquee), resize handles, basic
   align/distribute toolbar.
5. Unit tests: new variant round-trips through SQL and API; validation
   catches cycles and bad parent_ids.

**Exit criteria:** User can shift-select two static labels, click "Align
left", save, reload, and the alignment persists. Existing layouts load
unchanged.

### Phase 2 — Grouping and outliner

1. Outliner panel in admin.html.
2. Group / Ungroup toolbar buttons.
3. Group drag translates descendants.
4. Group delete confirms and cascades.
5. Group background rendering in the compositor.

**Exit criteria:** User can group three items, drag the group as a unit,
apply a "card" background, save, and the composite renders with the card.

### Phase 3 — Plugin decomposition

1. Plugin manifest schema: `[[default_elements]]`.
2. Field-mapping upsert on plugin registry reload.
3. `GET /api/admin/plugins/{id}/default_elements`.
4. Admin UI: drag of a plugin palette card with non-empty default_elements
   creates a `Group + children`, not a `PluginSlot`.
5. Decomposition manifests for the five built-ins.

**Exit criteria:** Dropping `weather` yields five editable elements inside
a weather group; each can be individually moved/resized/deleted; saving
and reloading preserves the decomposition.

### Phase 4 — Arrangement polish

1. Bring-to-front / send-to-back (absolute z-order).
2. Duplicate (including group-aware deep clone).
3. Smart guides on drag.
4. Grid size selector + visible grid.
5. Undo stack > 1 step.

### Phase 5 — Nice-to-haves (separate beads)

- Layout templates/presets library.
- Constraint-based positioning (fill parent, align to siblings).
- Element locking.
- Plugin manifest editor in admin UI.

---

## 9. Testing Strategy

Per `AGENTS.md` core rule, tests ride alongside each phase. For this work:

- **`src/layout_store/`**: migration is idempotent on existing DBs; new columns
  round-trip through `save_layout` / `get_layout`; Group variant serialises
  with correct JSON tag.
- **`src/api.rs`**: validation rejects cycles, dangling parents, bad group
  plugin instance ids; accepts well-formed payloads.
- **`src/compositor.rs`**: flatten-and-sort produces correct render order when
  groups and non-groups interleave; group-card background paints once, before
  descendants.
- **Integration**: `PUT /api/admin/layout/{id}` with a grouped payload
  round-trips via `GET`; `POST /api/admin/preview/{id}` renders without
  panicking.
- **Admin UI**: no automated tests today; Phase 1 adds a basic Playwright
  smoke for multi-select + align. Manual test steps for
  `docs/manual-testing.md`.

---

## 10. Risks

1. **Migration risk** — adding four columns to a hot table. Mitigation:
   schema version check in `LayoutStore::init` runs the `ALTER TABLE` exactly
   once. Backwards-compatible: old rows have `parent_id = NULL` and behave
   identically to today.

2. **Serialization churn** — `ItemPayload` is serde-tagged. Adding a new
   `"group"` variant is safe, but existing clients sending unknown fields must
   be tolerated. Current code uses `#[serde(deny_unknown_fields)]`? Check and
   relax if necessary before Phase 1 ships.

3. **Plugin manifest schema drift** — `default_elements` is additive; plugins
   that don't declare it continue to render as opaque slots. No forced
   migration for third-party plugin authors.

4. **Performance** — Group rendering adds one `imageops::overlay` per group
   with a background. With typical layouts (< 20 items, < 5 groups), this is
   sub-millisecond. The compositor's bottleneck remains sidecar HTTP, not
   pixel math.

5. **Cycle detection bugs** — DFS with a visited set. Tested with a payload
   that references its own id as parent and with a 3-item cycle.

6. **UX complexity** — the composer gets genuinely richer. Mitigation:
   progressive disclosure (toolbar appears only on multi-select), an
   "Advanced" toggle to show/hide outliner, and default behaviour matches
   today (single-select, property-panel editing).

---

## 11. Out-of-Scope / Follow-ups

The following are explicitly deferred to new beads:

- **Layout templates / preset gallery** — save a layout as a template, drop
  templates from a gallery. Requires template storage + a curation surface.
- **Responsive layouts** — computed positions based on content length or
  other signals.
- **Element-level visibility conditions** — hide an element when its data
  field is missing.
- **Cross-plugin data sharing** — a DataField in plugin A sourced from plugin
  B's cache.
- **Undo redo history > 1** — today's single-step undo suffices for Phase 1;
  full history is Phase 4.

---

## 12. Open Questions (for review)

1. **Group coordinates: absolute or parent-relative?** This doc specifies
   absolute (for zero-migration compat). The alternative — parent-relative
   — gives cleaner semantics for group drag and constraint-based layout, at
   the cost of a one-time migration. Recommendation: stay with absolute;
   revisit in Phase 5 if constraint layouts land.

2. **Is `Group` a separate variant or a mode of `PluginSlot`?** This doc
   chose separate variant for clarity. `PluginSlot { container: true }` is
   the alternative. Separate variant wins because the compositor path for
   groups (no sidecar call) is fundamentally different from plugin slots.

3. **Should decomposition be reversible?** "Collapse to plugin slot" (undoing
   a decomposition) is useful when users mess up the default elements.
   Proposed: a "Reset to default elements" action in the group's context
   menu restores the manifest default. Implementation is straightforward;
   flagged for Phase 3 polish.
