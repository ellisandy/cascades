# Cascades Server — User Stories

These stories define the acceptance contract for the Cascades server. Each story
describes a concrete, observable outcome that can be verified by an automated test
or manual check.

---

## 1. Starting the server

**As an operator**, when I run the server with a valid `config.toml` present, I
expect the HTTP server to start on the configured port (or 8080 by default) and
respond to requests within 5 seconds.

**Acceptance:** `GET /image.png` returns HTTP 200 within 5 seconds of startup.

---

## 2. Config error on startup

**As an operator**, when I run the server with a missing or malformed `config.toml`,
I expect the server to exit immediately with a non-zero status code and a human-
readable error message identifying which file failed and why.

**Acceptance:** Server process exits with status ≠ 0. Stderr contains the config
file path and a description of the parse or I/O error. No HTTP port is bound.

---

## 3. Fetching the display image

**As a display client**, when I send `GET /image.png` to a running server, I expect
a valid PNG image with dimensions matching the configured display size (default
800×480) and `Content-Type: image/png`.

**Acceptance:** Response status 200, `Content-Type: image/png`, PNG width=800,
height=480.

---

## 4. Data freshness — stale source triggers Unknown

**As a display client**, when a data source (e.g. weather) has not produced a fresh
reading within its staleness window (weather: 3 hours), I expect the rendered image
to reflect an `Unknown` / indeterminate trip decision rather than a stale
Go/NoGo — so the user is not misled by out-of-date data.

**Acceptance:** When all weather data is older than 3 hours (or absent), the rendered
image encodes an Unknown decision state; the displayed trip recommendation is not
"Go" or "NoGo".

---

## 5. Fixture / dev mode — offline rendering

**As a developer**, when I start the server with fixture data enabled, I expect the
server to render a complete, valid image using canned fixture data instead of making
any live network calls to external APIs.

**Acceptance:** In fixture mode, `GET /image.png` returns HTTP 200 with a valid PNG.
No outbound HTTP requests are made to NOAA, USGS, WSDOT, or NPS during the request.
The image dimensions match the configured display size.

---

## 6. Source failure — server stays up and degrades gracefully

**As an operator**, when a configured data source (e.g. USGS river gauge) is
temporarily unreachable at runtime, I expect the server to continue serving
`/image.png` without crashing, using the last known good value (or Unknown if no
value has ever been received), and to retry the failing source on its normal
interval.

**Acceptance:** After a source fetch failure, `GET /image.png` still returns HTTP
200. The previous successfully-fetched value (or Unknown if none) is used. The
server process is still running 30 seconds later.

---

## 7. Optional source disabled — missing API key

**As an operator**, when an optional source (e.g. trail conditions) is configured
but its API key is absent from both `config.toml` and the environment, I expect
the server to start successfully with that source disabled, log a warning, and
render an image that omits the trail-conditions zone rather than failing to start.

**Acceptance:** Server starts and responds to `GET /image.png` with HTTP 200.
Stderr contains a warning that the trail source was disabled. No panic or exit.

---

## 8. Multi-destination evaluation — worst-case decision shown

**As a display viewer**, when multiple destinations are configured and one evaluates
to NoGo while others evaluate to Go or Caution, I expect the hero zone of the
rendered image to reflect the worst-case decision (NoGo), ensuring the display
never misleads the user into thinking all options are safe when at least one is not.

**Acceptance:** Given destinations A (NoGo) and B (Go), the rendered image's hero
zone encodes a NoGo recommendation. The decision priority order is:
NoGo > Unknown > Caution > Go.

---

## 9. Caution / near-miss threshold

**As a display viewer**, when a trip criterion is met but the actual value is within
the configured caution margin (e.g. temperature within 5°F of maximum, river within
10% of max level), I expect the rendered image to show a Caution state rather than
a full Go — alerting the user to conditions that are acceptable but marginal.

**Acceptance:** Given `max_temp_f = 85` and observed temp = 82°F (within 5°F
margin), evaluation returns Caution, not Go. The rendered image reflects this.

---

## 10. No destinations configured — server still serves image

**As an operator**, when `destinations.toml` is absent or empty, I expect the server
to start and serve `/image.png` without error, rendering a layout that omits
destination-specific evaluation zones rather than panicking or refusing to serve.

**Acceptance:** `GET /image.png` returns HTTP 200 with a valid PNG when
`destinations.toml` is missing or contains zero destinations.

---

## 11. Device client mode — thin client fetches and refreshes

**As a device operator**, when the `[device]` section is configured with a remote
`image_url` and `refresh_interval_secs`, I expect the device client to periodically
fetch the rendered PNG from that URL and push it to the local hardware display,
without running a local HTTP server itself.

**Acceptance:** In device-client mode, no HTTP server is bound on the local port.
The client fetches from `image_url` at approximately the configured interval. On each
successful fetch, the display is updated with the new image.

---

## 12. Source polling — background refresh without client requests

**As an operator**, when the server is running and no HTTP clients are connected,
I expect each data source to continue fetching updates on its configured interval so
that when a client eventually calls `GET /image.png`, it receives data no older than
one polling interval.

**Acceptance:** After server startup, wait one full `weather_interval_secs` with no
HTTP requests. Then call `GET /image.png`. The weather observation in the rendered
state is no older than `weather_interval_secs + a small buffer`.

---

## 13. Health status — operator checks server and source state

**As an operator**, when I send `GET /api/status`, I expect a JSON response
showing the server version, uptime, sidecar URL, and per-source data freshness
(last fetch time, data age, last error), so I can quickly diagnose whether a
source is stale or erroring without grepping logs.

**Acceptance:** `GET /api/status` returns HTTP 200, `Content-Type: application/json`,
and a body with `version`, `uptime_secs`, `sidecar_url`, and `sources` fields.
Each source entry includes `id`, `enabled`, `last_fetched_at`, `last_error`, and
`data_age_secs`.

---

## 15. Webhook — external system pushes plugin data

**As an integrator**, when I `POST` a JSON payload to
`/api/webhook/:plugin_instance_id`, I expect the server to store that payload
as the plugin instance's cached data and return `204 No Content`, so that the
next display render uses the newly pushed data.

**Acceptance:** `POST /api/webhook/river` with valid JSON returns `204 No
Content`. A subsequent `GET /api/image/default` reflects the pushed data in the
rendered PNG.

---

## 16. Display API — TRMNL device polls for image URL and refresh rate

**As a TRMNL device**, when I send `GET /api/display` with a valid
`Authorization: Bearer <api_key>` header, I expect a JSON response containing
`image_url` (a relative URL to the current PNG) and `refresh_rate` (seconds),
so I know where to fetch the image and how often to refresh.

**Acceptance:** `GET /api/display` with the correct Bearer token returns HTTP
200, `Content-Type: application/json`, and a body with `image_url` and
`refresh_rate` fields. Without a valid token, the response is `401 Unauthorized`.

---

## 17. Named display images

**As a display client**, when I send `GET /api/image/:display_id` with a valid
display name (e.g. `default`, `trip-planner`), I expect the latest rendered PNG
for that display with `Cache-Control: no-store`. When the display name is
unknown, I expect `404 Not Found`.

**Acceptance:** `GET /api/image/default` returns `200 image/png` with
`Cache-Control: no-store` and a valid PNG. `GET /api/image/unknown` returns 404.

---

## 18. Multi-slot compositor — display layout with multiple plugins

**As an operator**, when I configure a display with multiple slots (e.g.
`trip-planner` with weather, river, and ferry slots), I expect the server to
render each slot independently and composite them into a single 800×480 PNG,
with each plugin's content placed at the correct position.

**Acceptance:** `GET /api/image/trip-planner` returns a valid 800×480 PNG. The
pixels in the weather region, river region, and ferry region all contain
non-trivial content (not uniform white or black).
