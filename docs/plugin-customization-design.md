# Plugin Customization Design

**Status:** Final design. Ready for implementation review. All four open
questions resolved — see the "Decisions" section.

## Problem

The user wants drag-and-drop control over the *inside* of a plugin — text
sizes, positions, fonts, graphics — the same way the top-level layout composer
lets them arrange plugin slots on an 800×480 canvas.

## What already exists (important context)

The premise "this is absent right now" is only half right. Phase 3
(commit `cd49efb`, "plugin decomposition") shipped the plumbing ~10 days ago:

- Dropping a plugin on the canvas does **not** create an opaque `PluginSlot`
  anymore. The admin UI calls `GET /api/admin/plugins/{id}/default_elements`,
  creates a `LayoutItem::Group` bound to the plugin instance, and inserts one
  `DataField` / `StaticText` / `StaticDivider` child per manifest entry
  (`src/layout_store/mod.rs:169` for the Group variant;
  `src/plugin_registry/mod.rs:177` for `DefaultElement`;
  `templates/admin.html` `spawnPluginAt`).
- Each child is independently selectable, movable, resizable, and already
  carries `font_family`, `bold`, `italic`, `underline`, `font_size` columns
  that the compositor honors (`src/compositor.rs:501`).
- Field-data binding survives: on boot, every `data_field` default element is
  upserted into `data_source_fields` with a stable id
  (`src/main.rs:165`).
- Five built-in plugins (`weather`, `river`, `ferry`, `trail`, `road`) ship
  `[[default_elements]]` manifests.

Decomposition is live. What isn't:

1. **No "enter group" editing scope.** A group's children render, but there's
   no focused editing mode — users select children as peers of the top-level
   layout, which is noisy.
2. **No image/icon primitive.** Plugins can't render weather icons, route
   badges, etc. outside of a Liquid template.
3. **Style controls are minimal.** Font family, size, and the three style
   toggles exist as columns but aren't exposed in the property inspector
   for in-plugin elements.
4. **No visibility conditions.** Templates do `{% if trip_decision.go %}`;
   decomposed elements have no equivalent.
5. **No asset story.** User-supplied fonts and images have nowhere to live.

## Recommended approach: complete the recursive composer

Keep the current data model (`LayoutItem` + `Group` + `parent_id`). Close the
five gaps above. Do **not** invent a second editing paradigm.

### v1 scope (~4–6 weeks)

| Work | Sizing |
|---|---|
| "Enter group" editing scope (double-click to focus; siblings dim; Escape exits) | 2–3 d |
| Property inspector: font family/size/weight/color controls for items inside a focused group | 2–3 d |
| New `LayoutItem::Image` variant + `assets` table + `POST /api/admin/assets` upload route | 1–2 w |
| `DataIcon` primitive: `{field_mapping_id, icon_map: HashMap<String, AssetId>}` for weather-condition icon swaps | 3–5 d |
| `visible_when` field on all item variants — JSONPath + single operator (`=`, `!=`, `>`, `<`, `>=`, `<=`, `exists`) + literal; no compound | ~1 w |
| "Reset to plugin defaults" action on a group (wipe + re-spawn from manifest) | 2–3 d |
| Bundle 5 OFL fonts (Inter, IBM Plex Sans, DM Serif Display, JetBrains Mono, Space Grotesk) in `fonts/`, served at `/fonts/*` + per-font CSS fallbacks in the sidecar | 2 d |

**MVP cuts.** Two natural stopping points:
- **Phase 5 alone (~1 week):** enter/exit scope + font controls + curated
  fonts. User can restyle text inside any plugin. No images yet.
- **Phase 5 + 6 (~3 weeks):** adds images + asset uploads. Feels feature-
  complete for most users; the remaining phases are polish + power features.

See the "Phased delivery" section below for the full breakdown.

### v1.1: CSS-variable theming for un-decomposed plugins

Not every plugin benefits from being exploded into loose elements — some have
internal layout logic (flex rows, conditional trip-decision blocks, loops)
that shouldn't be destroyed. For those, let plugin authors declare a schema
of named text styles / colors / toggles (new `SettingsField.field_type`
values: `text_style`, `color`, `toggle`), and bind them in the Liquid
template via CSS custom properties:

```liquid
<style>
  :root {
    --temp-size:  {{ style.temp_style.size | default(96) }}px;
    --temp-color: {{ style.temp_style.color | default('#000') }};
  }
</style>
```

**Scope — important.** Per the resolution of open question 4, all user
customization is **per-layout**, not per-instance. Theming-knob values do
**not** live in `instance_store.settings` (that remains the home for *data*
settings like `station_name` / `site_id`). Instead, a new `style_overrides`
JSON column on the plugin `Group` row stores per-layout knob values.
Templates see them bound to `style.*` in the render context, separate from
`settings.*`.

The property inspector renders these as a bounded form — "Plugin
customization" section, below element geometry for decomposed children.
Fully backwards compatible; un-updated plugins render as today.

Cost: ~2–3 weeks to extend the schema, add the `style_overrides` column,
plumb `style` into the render context, wire the inspector, and convert
`weather` as the reference.

This is **not a separate paradigm** — it's a second set of knobs available on
any plugin slot whose group hasn't been broken apart, so the user can either
decompose the plugin *or* tweak its author-declared knobs.

### v1.2: User-uploaded fonts

Piggyback on the v1 asset pipeline (built for `LayoutItem::Image` + icon
swaps). Add a `kind` column to the assets table; serve uploaded fonts
alongside curated ones. Register `@font-face` rules dynamically in the
sidecar HTML shell. Incremental cost: ~2–3 days once the image pipeline is
shipped.

## Phased delivery

Six phases, each independently shippable and user-visible. Numbering
continues the layout-composer sequence (Phases 1–4 shipped already: Group
variant + multi-select, grouping/outliner/card backgrounds, plugin
decomposition, arrangement polish).

### Phase 5 — Editing scope + style inspector + fonts (~1 week)

The UX foundation every later phase depends on.

- "Enter group" editing mode (double-click a Group to focus; siblings dim;
  Escape exits). Outliner and canvas both honor it.
- Property inspector controls for items inside a focused group: font family,
  size, weight, italic/bold/underline, color.
- Bundle the 5 OFL fonts in `fonts/`, serve at `/fonts/*`, wire `@font-face`
  + fallbacks into the sidecar HTML shell.

**Shipping state:** user can enter any decomposed plugin and restyle its
text with curated fonts. No images, no conditions. Cleanly demos "drag-and-
drop inside a plugin."

### Phase 6 — Asset pipeline + `LayoutItem::Image` (~1.5–2 weeks)

New primitive plus the infrastructure it needs.

- `assets` table (id, filename, mime, bytes, sha256, created_at).
- `POST /api/admin/assets` upload route (multipart, 1 MB cap, MIME sniff
  on server, de-dupe by sha256).
- `GET /api/assets/:id` serving with correct `Content-Type`.
- `LayoutItem::Image { asset_id, x, y, w, h, parent_id, ... }` variant;
  compositor renders via `image::load_from_memory` (no sidecar needed for
  static images).
- Admin UI: asset library panel + drag-onto-canvas.

**Shipping state:** users can upload PNGs/SVGs and drop them into a plugin
(logos, decorations, backgrounds). Combined with Phase 5, this is the MVP
cut referenced in the v1 scope section above.

### Phase 7 — `DataIcon` + `visible_when` (~1.5 weeks)

Data-driven rendering. Two primitives that share a JSONPath-evaluation code
path, so they land together.

- `DataIcon { field_mapping_id, icon_map: HashMap<String, AssetId>, ... }` —
  resolves JSONPath → icon asset at render. Property inspector shows a map
  editor (value → asset dropdown).
- `visible_when { path, op, value }` on all non-Group variants — `=`, `!=`,
  `>`, `<`, `>=`, `<=`, `exists`. Compositor evaluates and skips items that
  resolve to false. Property inspector: "show only when…" control with
  path picker + operator dropdown + literal input.

**Shipping state:** weather shows a sun/rain/cloud icon that updates with
conditions; precip row only appears when precip > 0; GO badge only shows
when trip_decision.go = true. Depends on Phase 6 (DataIcon needs the assets
table).

### Phase 8 — Recovery & sync affordances (~3–5 days)

Polish that prevents regret.

- "Reset to plugin defaults" action on a plugin Group — wipe children,
  respawn from manifest.
- "Plugin defaults updated" badge on groups whose `default_elements`
  manifest has changed since the user customized (detected via a content
  hash stamped on the group).

**Shipping state:** v1 rich. Users can recover bad edits and see when
authors push changes. 4–6 week total from Phase 5 start.

### Phase 9 — Theming via CSS variables (~2–3 weeks) — v1.1

Theming for plugins that stay un-decomposed.

- New `SettingsField.field_type` values: `text_style`, `color`, `toggle`.
- `style_overrides` JSON column on the plugin `Group` row (per-layout, per
  open question 4).
- Render-context plumbing: `style` bound alongside `settings`/`data`/
  `trip_decision`/`now`/`error` in the Liquid context.
- Property inspector: "Plugin customization" section below element-level
  geometry controls.
- Convert `weather` plugin as the reference implementation; document the
  pattern in `docs/plugin-authoring.md`.

**Shipping state:** users can theme any plugin the author opts into —
including template-mode plugins like `ferry` that aren't decomposed.

### Phase 10 — User-uploaded fonts (~2–3 days) — v1.2

Incremental on Phase 6's asset pipeline.

- `kind` column on `assets` (`image` | `font`).
- Accept `font/woff2`, `font/woff`, `font/ttf` in the upload route.
- Dynamic `@font-face` registration in the sidecar HTML shell.
- Font picker in the property inspector merges curated + uploaded.

**Shipping state:** fully hybrid font policy. Users bring any font, curated
set remains as sensible defaults.

### Sequencing notes

- **Strict order.** Phase 5 → 6 are foundational; 7 depends on 6. Phase 8
  could slot in before or after 7 without harm. Phase 9 is independent but
  scheduled after v1 ships so theming builds on a mature composer.
- **Pause points.** Natural places to stop and gather user feedback: after
  Phase 6 (MVP), after Phase 8 (v1 rich), after Phase 9 (theming added).
- **Parallelizable?** Only with multiple developers. Phase 7 and Phase 8 are
  independent; Phase 9 and Phase 10 could run parallel to Phase 7–8 if
  someone else picks them up.

## Alternatives considered

### Template fork + WYSIWYG

Let users fork the plugin's Liquid+CSS into a per-instance editable copy,
edited via a Monaco/CodeMirror + live-preview pane. Expressive ceiling is
much higher (add a sparkline, inject SVG). But:

- Requires Liquid/HTML/CSS literacy — different user than the composer user.
- Creates an **incoherent** state with decomposition: a forked template no
  longer matches the decomposed children. The honest resolution is "forking
  is a one-way upgrade — the composer view for that instance becomes
  disabled; reverting to default re-enables it." That's a real UX tax for a
  feature most users won't need.
- 3–5 weeks for v1, and tends to grow a WYSIWYG aspiration that's
  multi-quarter work in disguise.

**Deferred.** Revisit only if users hit the ceiling of v1+v1.1 in real usage
and ask for it. Don't build it speculatively.

### Author-declared knobs as the *primary* mechanism

Make author-declared CSS-variable customization the main path and leave
Phase 3's decomposition as a secondary "break apart" fallback. Works, but
throws away momentum: Phase 3 already shipped, users already get
drag-and-drop, and the "similar to the top-level composer" directive from
the ask maps most cleanly to decomposition. Folding this into v1.1 preserves
its value without demoting what's already live.

## Non-goals for v1

- **No template editor / source-level HTML+CSS editing.**
- **No compound expressions in `visible_when`** — single `path op value`
  only. `&&` / `||` / parens wait until we see a concrete plugin that
  needs them.
- **No "save my layout as the new plugin default" round-trip** — one-way
  edits only. Plugin authors still own manifests.
- **No mobile editing.** Desktop-only admin UI.

## Decisions

These were the four open questions at draft time. All resolved.

1. **Conditional visibility — how far do we go?** Single comparison
   operators (`=`, `!=`, `>`, `<`, `>=`, `<=`, `exists`). One JSONPath, one
   operator, one literal. No compound expressions, no parser. Covers
   `precip_chance_pct > 0` (real case in current templates) without
   committing to a DSL. Firm stop at compound.
2. **Two-paradigm drift between Liquid templates and `default_elements`.**
   Decomposition is the default authoring surface for user-customizable
   plugins. Liquid templates remain first-class but bounded — for plugins
   that need loops, custom filters, or complex inline strings (currently
   just `ferry`). Template-mode plugins are *not* user-customizable in
   v1/v1.1 — accepted cliff. If the cliff bothers users later, we add a
   repeater primitive to `default_elements` rather than a template-
   annotation system.
3. **Font policy.** Curated set in v1 (5 OFL-licensed fonts: Inter, IBM
   Plex Sans, DM Serif Display, JetBrains Mono, Space Grotesk — ~400 KB
   bundled). Image-asset pipeline ships in v1 for `LayoutItem::Image` +
   icon swaps. User-uploaded fonts land in v1.2, piggybacking on the image
   asset pipeline with a `kind` column (~2–3 days incremental once images
   ship).
4. **Customization scope — per-layout vs per-instance.** Everything
   per-layout. Element positions, font/size/weight, and v1.1's theming-knob
   values all live on the layout, not on the plugin instance.
   `instance_store.settings` retains its original role — data
   configuration (`station_name`, `site_id`, `park_code`). Visual
   attributes move to layout via `layout_items` columns (already there for
   element-level style) and a new `style_overrides` JSON column on the
   plugin `Group` (v1.1). Multi-display consistency is solved later via a
   "copy layout" action if needed, not by linking customization to the
   instance.

## Risks

- **Conditional-rendering gap grows into an ad-hoc rules engine.** Mitigate
  by holding the `path op value` boundary and refusing compound-expression
  expansion until we have concrete plugins that need it.
- **Manifest drift:** when a plugin author adds a new `default_element`, it
  doesn't reach existing customized groups until the user runs "reset to
  defaults" (destructive) or we build a real sync/diff UI (deferred).
- **Outliner clutter.** A decomposed plugin is 5–7 items; three plugins on a
  display is 15–20 entries. Phase 2's collapse helps but doesn't eliminate
  this.
- **Render cost.** 5× the sidecar calls per plugin vs. a template render.
  Almost certainly fine at 300s refresh; revisit if someone builds a
  60s-refresh display with 4 decomposed plugins.

## Summary

Phase 3 already made plugins decomposable. The v1 ask is to finish the UX
(enter-group scope, property inspector, images, conditional visibility, asset
uploads). v1.1 adds CSS-variable theming for plugins users leave
un-decomposed. Template forking stays on the shelf until we have signal that
the ceiling is real.

Six phases, numbered to continue the layout-composer sequence. Phase 5
(~1 week) is a usable demo; Phase 5+6 (~3 weeks) is the MVP most users would
consider feature-complete; Phases 5–8 (~4–6 weeks) is v1 rich. Phase 9 adds
theming (~2–3 weeks, v1.1); Phase 10 adds font uploads (~2–3 days, v1.2).

