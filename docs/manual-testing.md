# Manual Testing Guide

Step-by-step instructions for running the Cascades server locally and verifying
the image endpoint by hand. A new developer should be able to see a rendered PNG
in under 5 minutes by following this guide.

---

## Prerequisites

- Rust toolchain installed (`rustup`, stable channel)
- `cargo` on your `PATH`
- A terminal and `curl`

No live API keys are needed for fixture mode (see [Fixture mode](#fixture-mode)).

---

## 1. Start the server locally

### Quick start (fixture mode — no API keys required)

```bash
./scripts/dev-server.sh
```

This builds the project and starts the server with all data sources returning
embedded fixture responses. No network calls are made. Suitable for local
development and smoke testing.

You should see:

```
Fixture mode enabled: sources return canned data (no live API calls)
Listening on http://0.0.0.0:8080
```

### Manual start (live data)

```bash
cargo run
```

The server reads `config.toml` from the current directory and starts background
polling tasks for all configured data sources. Port defaults to 8080.

#### Required `config.toml` sections

```toml
[display]
width = 800
height = 480

[location]
latitude = 48.4232
longitude = -122.3351
name = "Mount Vernon, WA"

[sources]
weather_interval_secs = 300   # NOAA weather, every 5 min
river_interval_secs = 300     # USGS gauge, every 5 min
ferry_interval_secs = 60      # WSDOT ferries, every 1 min
trail_interval_secs = 900     # NPS alerts, every 15 min
road_interval_secs = 1800     # WSDOT highway alerts, every 30 min
```

The repo ships a working `config.toml` with these defaults. You can start the
server without changing anything.

#### Optional `config.toml` sections

```toml
[server]
port = 8080          # Change port if 8080 is in use

[sources.river]
usgs_site_id = "12200500"  # Skagit River at Mount Vernon (default)

[sources.trail]
park_code = "noca"         # North Cascades (default)
# nps_api_key = "..."      # Or set NPS_API_KEY env var

[sources.road]
routes = ["020"]           # SR-20 (default)
# wsdot_access_code = "..." # Or set WSDOT_ACCESS_CODE env var

[sources.ferry]
route_id = 9               # Anacortes / Friday Harbor (default)
# wsdot_access_code = "..." # Or set WSDOT_ACCESS_CODE env var
```

#### Environment variables (optional)

| Variable | Purpose |
|----------|---------|
| `NPS_API_KEY` | NPS Alerts API key (trail conditions). Without it, trail source is disabled. |
| `WSDOT_ACCESS_CODE` | WSDOT API access code (ferries + road closures). Without it, those sources are disabled. |
| `RUST_LOG` | Log verbosity, e.g. `info`, `debug`. Defaults to silent. |

Missing API keys are not fatal — the server starts and the display omits data
from disabled sources. NOAA weather and USGS river gauge require no keys.

---

## 2. Test the image endpoint

Once the server is running, fetch the rendered PNG:

```bash
curl -o /tmp/cascades.png http://localhost:8080/image.png
```

Verify it's a valid PNG:

```bash
file /tmp/cascades.png
# Expected: PNG image data, 800 x 480 ...
```

Open it in an image viewer:

```bash
# macOS
open /tmp/cascades.png

# Linux (any of the following)
xdg-open /tmp/cascades.png
eog /tmp/cascades.png
feh /tmp/cascades.png
```

Or open it directly in a browser by navigating to:

```
http://localhost:8080/image.png
```

The image is always re-rendered from the latest cached data on each request. You
do not need to reload the server — just re-fetch the URL.

---

## 3. Fixture mode (no live API keys)

Fixture mode returns embedded JSON from `src/sources/fixtures/` instead of
making network calls. All five data sources (weather, river, ferry, trail, road)
respond immediately with canned data.

**Via `dev-server.sh` (recommended):**

```bash
./scripts/dev-server.sh
```

**Manually:**

```bash
SKAGIT_FIXTURE_DATA=1 RUST_LOG=info cargo run
```

**Run the fixture rendering tests without starting a server:**

```bash
cargo test
```

This runs the render pipeline tests (`tests/render_pipeline_tests.rs`) against
fixture data and verifies the PNG output is 800×480 and contains non-empty pixels.

---

## 4. Trigger a re-render and observe changes

There is no explicit re-render endpoint. The image is rendered on demand from
the most recently cached data. To observe a re-render:

1. **Fetch the current image** and save it:

   ```bash
   curl -o /tmp/before.png http://localhost:8080/image.png
   ```

2. **Wait for a background source to update.** Sources poll on their configured
   intervals (weather: 5 min, ferry: 1 min). Watch logs with `RUST_LOG=info`:

   ```bash
   RUST_LOG=info cargo run
   ```

   A successful source update logs:

   ```
   [INFO cascades] source 'noaa' fetched successfully
   ```

3. **Fetch again** after a source update:

   ```bash
   curl -o /tmp/after.png http://localhost:8080/image.png
   ```

4. **Compare** the two images:

   ```bash
   # macOS: open both side by side
   open /tmp/before.png /tmp/after.png

   # Or diff the raw bytes (not identical if data changed)
   cmp /tmp/before.png /tmp/after.png && echo "same" || echo "different"
   ```

In fixture mode, data is static — each fetch returns the same image. Use live
mode to observe real data updates.

---

## 5. Point a SkagitFlats device at a local Cascades instance

A SkagitFlats device is a thin client that periodically fetches the pre-rendered
PNG from a Cascades server and pushes it to e-ink hardware. To point it at your
local instance, set the `[device]` section in the device's `config.toml`:

```toml
[device]
image_url = "http://<your-machine-ip>:8080/image.png"
refresh_interval_secs = 60    # How often the device re-fetches (default: 60)
```

Replace `<your-machine-ip>` with the IP address of your development machine on
the local network (e.g. `192.168.1.42`). The device and server must be on the
same network, or the server must be reachable via the address configured.

**Steps:**

1. Start Cascades on your dev machine:

   ```bash
   ./scripts/dev-server.sh
   # or
   cargo run
   ```

2. Find your machine's local IP:

   ```bash
   # macOS
   ipconfig getifaddr en0

   # Linux
   hostname -I | awk '{print $1}'
   ```

3. Edit `config.toml` on the device:

   ```toml
   [device]
   image_url = "http://192.168.1.42:8080/image.png"
   refresh_interval_secs = 60
   ```

4. Start the device software. It will fetch `GET /image.png` every 60 seconds
   and push the result to the display.

5. Verify the device is connecting by watching server logs for incoming requests:

   ```bash
   RUST_LOG=info cargo run
   # You should see a new request logged each refresh interval
   ```

**Firewall note:** macOS may block incoming connections on port 8080. If the
device cannot reach the server, allow the connection when prompted or run:

```bash
# Temporarily allow inbound on 8080 (macOS)
sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add $(which cargo)
```
