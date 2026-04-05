# Admin UI — Drag-Drop Canvas Interaction Model and Component Spec

**Issue:** cs-ces  
**Device:** Waveshare 7.5" e-ink, 800×480 px  
**Scope:** Design document for composing display layouts via a browser-based admin interface.

---

## 1. Overview

The admin UI lets users visually compose what gets rendered on the e-ink display.
The interface is a single-page application served at `/admin`. It has three panels:
a component palette on the left, a 1:1-scale canvas in the centre, and a property
inspector on the right.

Users drag components from the palette onto the canvas, click to select and edit
their properties, then save or preview the layout.

---

## 2. Wireframe

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│  Cascades Admin              [Layout: default ▾]   [Preview]  [Reset]  [Save]   │
├──────────────────┬──────────────────────────────────────────┬────────────────────┤
│ PLUGIN PALETTE   │                                          │ PROPERTIES         │
│                  │  ┌────────────────────────────────────┐  │                    │
│ ┌──────────────┐ │  │                                    │  │ (nothing selected) │
│ │ weather      │ │  │                                    │  │                    │
│ │ River gauge  │ │  │        800 × 480 canvas            │  │                    │
│ │ Ferry        │ │  │                                    │  │                    │
│ │ Trail        │ │  │  ┌─────────────┐ ┌─────────────┐  │  │                    │
│ │ Road         │ │  │  │  river      │ │  ferry      │  │  │                    │
│ └──────────────┘ │  │  │  quadrant   │ │  quadrant   │  │  │                    │
│                  │  │  └─────────────┘ └─────────────┘  │  │                    │
│ STATIC PALETTE   │  └────────────────────────────────────┘  │                    │
│                  │                                          │                    │
│ ┌──────────────┐ │  Grid: [Free ▾]                          │                    │
│ │ Title        │ │                                          │                    │
│ │ Label        │ │                                          │                    │
│ │ Divider      │ │                                          │                    │
│ └──────────────┘ │                                          │                    │
└──────────────────┴──────────────────────────────────────────┴────────────────────┘
```

When an element is selected, the right panel shows its editable properties:

```
┌────────────────────┐
│ PROPERTIES         │
│ ─────────────────  │
│ Type: Plugin slot  │
│                    │
│ Instance: river ▾  │
│ Variant:  full ▾   │
│                    │
│ X:   [  0]         │
│ Y:   [  0]         │
│ W:   [800]         │
│ H:   [480]         │
│                    │
│ [Delete]           │
└────────────────────┘
```

---

## 3. Canvas Model

### 3.1 Dimensions and Scale

The canvas renders at exactly 800×480 CSS pixels. On high-DPI screens the browser
will scale it up automatically. A pixel on the canvas corresponds to one pixel on
the physical display.

### 3.2 Grid Snapping

A grid selector in the toolbar controls how drag-drop placement snaps:

| Mode       | Snap X (columns)  | Snap Y (rows)   | Use case                            |
|------------|-------------------|-----------------|-------------------------------------|
| **Free**   | 1 px              | 1 px            | Pixel-perfect static element positioning |
| **Quadrant** (default) | 400 px  | 240 px  | Aligns slots to the 4 canonical quadrants |
| **Half**   | 400 px / 800 px   | 240 px / 480 px | Half-screen layouts                 |

In quadrant mode the canvas shows faint dashed grid lines at x=400 and y=240.
In half mode, lines appear at x=400, y=240 (all 4 dividers).
In free mode no grid lines are shown.

Snap is applied on drop and also when an element is nudged with arrow keys
(1 px in free mode, 1 grid cell in grid modes). Hold ⌥/Alt while dropping to
temporarily override to free placement.

### 3.3 Slot Boundaries and Visual Indicators

Each placed element has a selection outline. Colour encodes state:

| Outline colour | Meaning                                    |
|----------------|--------------------------------------------|
| Blue           | Currently selected element                 |
| Grey dashed    | Unselected element                         |
| Orange         | Element overlaps another (warning only)    |

Overlapping is permitted — the compositor blits slots back-to-front, so the
last item in the list paints over earlier ones. The admin UI preserves this
z-order and uses orange outlines to signal overlaps without blocking the user.

### 3.4 Overlap / Collision Detection

The UI computes bounding-rectangle overlaps on every property change and drag.
When two items' bounding rectangles intersect, both get orange outlines and a
tooltip: "Overlaps with: {other item name}. Lower items render on top."

No hard block is enforced. Intentional overlaps (e.g. a label on top of a plugin
slot background) are valid use-cases.

---

## 4. Component Taxonomy

### 4.1 Plugin Slots

Rendered by the compositor. Each plugin slot binds one plugin instance to a
region of the canvas and selects a template variant for rendering.

**Properties:**

| Field               | Type    | Values                                              | Notes                                    |
|---------------------|---------|-----------------------------------------------------|------------------------------------------|
| `plugin_instance_id`| string  | ID of an existing PluginInstance                    | Selected from dropdown of known instances |
| `layout_variant`    | enum    | `full`, `half_horizontal`, `half_vertical`, `quadrant` | Determines template selection and default size |
| `x`                 | u32     | 0–799                                               | Left edge                                |
| `y`                 | u32     | 0–479                                               | Top edge                                 |
| `width`             | u32     | 1–800                                               | Defaults to variant's canonical width    |
| `height`            | u32     | 1–480                                               | Defaults to variant's canonical height   |

**Variant → canonical dimensions** (default; user may override):

| Variant           | Width | Height |
|-------------------|-------|--------|
| `full`            | 800   | 480    |
| `half_horizontal` | 800   | 240    |
| `half_vertical`   | 400   | 480    |
| `quadrant`        | 400   | 240    |

When the user picks a variant from the dropdown, width/height are reset to
canonical defaults. The user may then manually adjust them.

**Palette card** shows the instance ID and plugin type (e.g. "river — River Gauge").
Dragging the card onto the canvas creates a plugin slot at the drop position,
defaulting to `quadrant` variant (smallest footprint).

### 4.2 Static Elements

Rendered directly by the server (no compositor / sidecar involved). They are
composited after plugin slots in the specified z-order.

#### Title

Large centred display text, typically used for layout headings.

| Field      | Type   | Default  | Notes                              |
|------------|--------|----------|------------------------------------|
| `text`     | string | "Title"  | Display text                       |
| `font_size`| u32    | 48       | Pixels                             |
| `x`        | u32    | 0        |                                    |
| `y`        | u32    | 0        |                                    |
| `width`    | u32    | 800      | Text centred within this box       |
| `height`   | u32    | 80       |                                    |

#### Label

Small text at an arbitrary position. Suitable for timestamps, captions, units.

| Field      | Type   | Default  | Notes                              |
|------------|--------|----------|------------------------------------|
| `text`     | string | "Label"  | Display text                       |
| `font_size`| u32    | 14       | Pixels                             |
| `x`        | u32    | 0        |                                    |
| `y`        | u32    | 0        |                                    |
| `width`    | u32    | 200      | Text left-aligned, clips to width  |
| `height`   | u32    | 20       |                                    |

#### Divider

A horizontal or vertical rule for visual separation.

| Field         | Type   | Default        | Notes                          |
|---------------|--------|----------------|--------------------------------|
| `orientation` | enum   | `horizontal`   | `horizontal` or `vertical`     |
| `x`           | u32    | 0              |                                |
| `y`           | u32    | 240            |                                |
| `width`       | u32    | 800            | For horizontal: full width     |
| `height`      | u32    | 2              | Stroke thickness               |

For a vertical divider, width becomes the stroke thickness and height the line
length. The property inspector swaps the labels accordingly.

---

## 5. Interaction Flow

```
1. User navigates to /admin
      │
      ▼
2. Page loads. Left panel shows:
   - Plugin palette: one card per PluginInstance (fetched from GET /api/admin/plugins)
   - Static palette: fixed cards for Title, Label, Divider
   Canvas loads current layout (fetched from GET /api/admin/layout/{id})
      │
      ▼
3. User drags a palette card onto the canvas
   - On dragstart: card shows a ghost image at 50% opacity
   - On drop: element created at snapped drop position with default dimensions
   - Element is immediately selected; right panel shows its properties
      │
      ▼
4. User adjusts properties in the right panel
   - All fields are live-update: changes are reflected on canvas immediately
   - Overlap detection runs after each change
   - No network call on individual property edits (all local state)
      │
      ▼
5. User clicks toolbar actions:
   ├─ [Preview] → POST /api/admin/preview with current layout JSON
   │              Response: PNG returned as blob; shown in a modal overlay
   ├─ [Save]    → PUT /api/admin/layout/{id} with current layout JSON
   │              On success: toast "Saved". Layout is now live for device polling.
   └─ [Reset]   → Discards unsaved changes, reloads from server (GET /api/admin/layout/{id})
```

### 5.1 Element Selection

- Click an element → selects it; right panel updates to show its properties
- Click canvas background → deselects all
- Delete/Backspace key → removes selected element (with undo: Ctrl+Z)
- Escape → deselects

### 5.2 Z-Order Management

Items render back-to-front (index 0 at back, last index at front). A small
z-order list below the canvas shows items by name; users can drag rows to
reorder. "Bring to front" / "Send to back" context menu items provide quick
access.

### 5.3 Layout Name and ID

A dropdown in the toolbar lists saved layouts. Selecting a different layout
loads it. A "New layout…" option in the dropdown prompts for a name, then
creates an empty layout with that name as its slug ID
(e.g. "My Layout" → `my-layout`).

---

## 6. Layout State (Data Model)

A layout is a JSON document stored server-side. It maps 1:1 to what the
compositor needs.

```json
{
  "id": "default",
  "name": "Default",
  "items": [
    {
      "type": "plugin_slot",
      "plugin_instance_id": "river",
      "layout_variant": "full",
      "x": 0,
      "y": 0,
      "width": 800,
      "height": 480
    },
    {
      "type": "static_label",
      "text": "Updated: 12:30",
      "font_size": 14,
      "x": 10,
      "y": 460,
      "width": 200,
      "height": 20
    }
  ]
}
```

**Item types and their fields:**

| type              | Required fields                                                  |
|-------------------|------------------------------------------------------------------|
| `plugin_slot`     | `plugin_instance_id`, `layout_variant`, `x`, `y`, `width`, `height` |
| `static_title`    | `text`, `font_size`, `x`, `y`, `width`, `height`                |
| `static_label`    | `text`, `font_size`, `x`, `y`, `width`, `height`                |
| `static_divider`  | `orientation`, `x`, `y`, `width`, `height`                      |

Items are ordered back-to-front: index 0 renders first (furthest back); the last
item renders on top.

**Mapping to compositor:** `plugin_slot` items map directly to `LayoutSlot` /
`DisplayConfiguration`. Static element items need a new rendering path in the
compositor (see §8).

---

## 7. Toolbar

```
[Layout: default ▾]    [Preview]    [Reset]    [Save]
```

| Control       | Action                                                               |
|---------------|----------------------------------------------------------------------|
| Layout picker | Dropdown of saved layouts; "New layout…" to create                  |
| Preview       | POST /api/admin/preview → modal with rendered PNG                   |
| Reset         | Revert to last saved state (GET /api/admin/layout/{id})             |
| Save          | PUT /api/admin/layout/{id}; layout is live for device polling       |

After Save, the in-memory `DisplayConfiguration` for that display ID is updated
so the next device poll returns the new layout immediately.

---

## 8. API Surface Required Downstream

New endpoints needed in `api.rs`. All `/api/admin/…` routes require
`Authorization: Bearer <api_key>` (same credential as `GET /api/display`).

### 8.1 Get Plugin Instances

```
GET /api/admin/plugins
```

Returns the list of known plugin instances so the palette can populate itself.

**Response:**
```json
[
  { "id": "river",   "plugin_id": "river",   "display_name": "River Gauge" },
  { "id": "weather", "plugin_id": "weather", "display_name": "Weather" },
  { "id": "ferry",   "plugin_id": "ferry",   "display_name": "Ferry" }
]
```

Source: `InstanceStore::list_instances()` (already implemented).

### 8.2 List Layouts

```
GET /api/admin/layouts
```

Returns all saved layout IDs and names.

**Response:**
```json
[
  { "id": "default",      "name": "Default" },
  { "id": "trip-planner", "name": "Trip Planner" }
]
```

### 8.3 Get Layout

```
GET /api/admin/layout/{id}
```

Returns the full layout JSON (§6 data model). 404 if the layout does not exist.

### 8.4 Save Layout

```
PUT /api/admin/layout/{id}
Content-Type: application/json

{ "id": "default", "name": "Default", "items": [ … ] }
```

- Validates the layout (known plugin instance IDs, valid variants, coords in bounds).
- Persists to storage (new `admin_layouts` SQLite table or separate JSON file in `config/layouts/`).
- Updates the in-memory `DisplayConfiguration` so the next `GET /api/image/{id}` uses the new layout.
- Returns 200 with the saved layout or 422 with validation errors.

### 8.5 Preview Layout

```
POST /api/admin/preview
Content-Type: application/json

{ "id": "preview", "name": "Preview", "items": [ … ] }
```

- Constructs a `DisplayConfiguration` from the request body without persisting it.
- Calls `Compositor::compose` to render a PNG.
- Returns `Content-Type: image/png` with the rendered image.
- Returns 422 if the layout JSON is invalid, 500 if the compositor fails.

### 8.6 Admin UI HTML

```
GET /admin
```

Returns the single-page admin application HTML (inline, no external deps).
No authentication required (same policy as `GET /`).

---

## 9. Static Element Rendering (Compositor Extension)

Static elements (title, label, divider) currently have no render path. The
compositor needs to be extended to handle them. Two options:

**Option A — HTML sidecar (recommended)**  
Each static element is expressed as a small HTML snippet rendered by the Bun
sidecar at the element's exact pixel dimensions. This reuses the existing
`call_sidecar` machinery. A static divider at `(0, 240, 800, 2)` would produce
a 800×2 PNG.

Pros: consistent rendering pipeline, font rendering for free, no new image crate
dependency. Cons: many sidecar round-trips for layouts with many static elements.

**Option B — Rust `image` crate**  
Render text/lines directly in Rust. The `imageproc` crate provides line drawing;
`rusttype` or `ab_glyph` provide font rasterisation.

Pros: no sidecar dependency, faster. Cons: font management complexity.

**Recommendation:** Start with Option A (HTML sidecar) to ship quickly. Migrate
to Option B if sidecar latency becomes a problem.

---

## 10. Live Preview Flow

After the user clicks Save, the display layout is live. The device continues to
poll `GET /api/display` (bearer-authenticated) and `GET /api/image/default` as
before. No device changes required.

The admin Preview modal reuses `GET /api/image/{id}` after Save, or calls
`POST /api/admin/preview` before Save for a non-destructive preview.

---

## 11. Open Questions

1. **Layout persistence format** — SQLite table vs JSON files in `config/layouts/`.
   JSON files are simpler and git-trackable; SQLite is consistent with plugin
   instance storage. Recommend SQLite (new `admin_layouts` table) for consistency.

2. **Authentication for `/admin` page** — currently proposed as open (same as `GET /`).
   Could add basic auth or the same Bearer token. Deferred to implementer; the
   admin page does expose save/preview, so some protection may be desirable.

3. **Undo/redo depth** — basic Ctrl+Z for single-step undo is specified. A full
   undo stack could be added later; file a separate bead if needed.

4. **Multi-display support** — the layout picker supports multiple named layouts
   (one per display). The device poll currently only uses `"default"`. Wiring
   additional display IDs to device API responses is out of scope for this design.
