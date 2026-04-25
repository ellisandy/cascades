#!/usr/bin/env python3
"""
Cascades Pi-side e-ink display refresh loop (Waveshare 7.5" V2).

Fetches /api/image/{display_id} from the local Cascades server on a fixed
interval, decodes the PNG, and pushes the buffer to the Waveshare panel.

Architecture note: this is the smallest possible Pi-side component — all
rendering, layout, and data work happens server-side in the Rust process.
This loop's only job is "keep the panel in sync with whatever /api/image
currently returns."

Failure modes (all caught and logged, never crash the loop):
- Server not yet listening    → connection refused → retry next tick.
- Server returns 5xx          → log and retry.
- PNG decode fails            → log and skip the refresh (panel keeps last good).
- SPI write fails             → log; we'll try again — could be a flaky cable.

Refresh dedup: e-ink updates are slow (~3s per cycle on the 7.5" V2) and
each one wears the panel slightly. We compare PNG bytes against the last
successfully-pushed frame and skip if identical. This makes the configured
interval a polling cadence, not a refresh cadence — checking every 60s
costs nothing if the image hasn't changed.

Configuration (env vars — set in cascades-display.service):
  CASCADES_URL            base URL of the local server (default localhost:9090)
  CASCADES_DISPLAY_ID     which layout to render            (default "default")
  CASCADES_INTERVAL_SECS  poll interval                    (default 60)
  CASCADES_SECRETS_PATH   path to secrets.toml for API key (default /opt/cascades/config/secrets.toml)
"""

import hashlib
import io
import os
import re
import sys
import time
import logging
from pathlib import Path

import requests
from PIL import Image

# ─── Config ────────────────────────────────────────────────────────────────

CASCADES_URL = os.environ.get("CASCADES_URL", "http://127.0.0.1:9090").rstrip("/")
CASCADES_DISPLAY_ID = os.environ.get("CASCADES_DISPLAY_ID", "default")
CASCADES_INTERVAL_SECS = int(os.environ.get("CASCADES_INTERVAL_SECS", "60"))
CASCADES_SECRETS_PATH = Path(
    os.environ.get("CASCADES_SECRETS_PATH", "/opt/cascades/config/secrets.toml")
)

REQUEST_TIMEOUT_SECS = 15
EXPECTED_WIDTH = 800
EXPECTED_HEIGHT = 480

# ─── Logging ───────────────────────────────────────────────────────────────

logging.basicConfig(
    level=os.environ.get("CASCADES_LOG_LEVEL", "INFO"),
    format="[display] %(message)s",
    stream=sys.stdout,
)
log = logging.getLogger("cascades.display")


def read_api_key(secrets_path: Path) -> str:
    """
    Pull the API key from the server's secrets.toml. The Rust server
    auto-generates this file on first boot, so we lazy-read it on each loop
    iteration that needs it — handles the race where this loop starts before
    the server has had a chance to write the file.

    Format we read:
        api_key = "abc123..."
    """
    if not secrets_path.exists():
        raise FileNotFoundError(
            f"secrets file not found: {secrets_path} "
            "(server may not have booted yet — will retry)"
        )
    text = secrets_path.read_text()
    match = re.search(r'^\s*api_key\s*=\s*"([^"]+)"', text, re.MULTILINE)
    if not match:
        raise ValueError(
            f"api_key entry not found in {secrets_path}; "
            "format expected: api_key = \"...\""
        )
    return match.group(1)


# ─── Display driver init ───────────────────────────────────────────────────


def init_display():
    """
    Bring up the Waveshare 7.5" V2 panel. Imports are lazy so this script
    can be unit-tested on a non-Pi (the import fails fast there).

    Returns the EPD instance (with `display`, `sleep`, etc.) ready to push
    frames. The driver does its own SPI init.
    """
    try:
        from waveshare_epd import epd7in5_V2  # type: ignore
    except ImportError as e:
        log.error(
            "waveshare_epd driver not installed. Did install.sh run? "
            "Expected at /opt/cascades/display/waveshare_epd/."
        )
        raise SystemExit(2) from e

    epd = epd7in5_V2.EPD()
    epd.init()
    return epd


# ─── PNG fetch + push ──────────────────────────────────────────────────────


def fetch_png(api_key: str) -> bytes:
    """
    Pull the latest rendered PNG. Uses the bearer-auth /api/image/{id}
    endpoint (not /image.png — same bytes, but the canonical path matches
    what the API doc lists).
    """
    url = f"{CASCADES_URL}/api/image/{CASCADES_DISPLAY_ID}"
    resp = requests.get(
        url,
        headers={"Authorization": f"Bearer {api_key}"},
        timeout=REQUEST_TIMEOUT_SECS,
    )
    resp.raise_for_status()
    return resp.content


def png_to_panel_buffer(png_bytes: bytes, epd) -> bytes:
    """
    Decode PNG → 1-bit grayscale → Waveshare buffer.

    The Cascades server already produces dithered output for `mode=device`;
    here we just convert to 1-bit ("mode=1") which the Waveshare driver
    expects. If the dimensions don't match the panel we resize — better than
    failing entirely, but log it loudly because it means a config drift.
    """
    img = Image.open(io.BytesIO(png_bytes))
    if img.size != (EXPECTED_WIDTH, EXPECTED_HEIGHT):
        log.warning(
            "PNG dimensions %s don't match panel %dx%d; resizing (this is a "
            "config drift — fix [display] width/height in config.toml)",
            img.size, EXPECTED_WIDTH, EXPECTED_HEIGHT,
        )
        img = img.resize((EXPECTED_WIDTH, EXPECTED_HEIGHT))
    if img.mode != "1":
        img = img.convert("1")
    return epd.getbuffer(img)


# ─── Main loop ─────────────────────────────────────────────────────────────


def main():
    log.info("starting display loop — url=%s display=%s interval=%ds",
             CASCADES_URL, CASCADES_DISPLAY_ID, CASCADES_INTERVAL_SECS)

    epd = init_display()
    last_hash = None
    cached_api_key = None

    while True:
        try:
            # Lazy-read the API key — handles first-boot race where this
            # service started before the Rust server wrote secrets.toml.
            if cached_api_key is None:
                cached_api_key = read_api_key(CASCADES_SECRETS_PATH)

            png = fetch_png(cached_api_key)
            digest = hashlib.sha256(png).hexdigest()
            if digest == last_hash:
                # Same bytes as last push — no panel work needed.
                log.debug("no change (hash=%s); skipping refresh", digest[:8])
            else:
                buf = png_to_panel_buffer(png, epd)
                epd.display(buf)
                last_hash = digest
                log.info("refreshed panel (%d bytes, hash=%s)",
                         len(png), digest[:8])
        except FileNotFoundError as e:
            # Server hasn't booted yet — quiet log and retry.
            log.info("waiting for server: %s", e)
        except requests.RequestException as e:
            log.warning("fetch failed: %s", e)
        except Exception as e:  # noqa: BLE001 — we genuinely want to keep the loop alive
            log.exception("unexpected error: %s", e)

        time.sleep(CASCADES_INTERVAL_SECS)


if __name__ == "__main__":
    main()
