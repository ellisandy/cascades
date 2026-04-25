# Deploying Cascades on a Raspberry Pi

Cascades is the **server half** of a two-process dashboard system:

- **Cascades** (this repo) — Rust HTTP server + Bun/Puppeteer render
  sidecar. Holds the SQLite state, serves the admin UI at `/admin`,
  produces 800×480 PNGs at `/image.png`.
- **A thin display client** (e.g. [skagit-flats]) — runs on the same Pi
  (or a different one), fetches the PNG on a refresh cycle, pushes it
  to the e-ink panel via SPI.

This split lets you put cascades wherever has the most CPU, and keep the
display client (which needs the SPI bus) on the Pi attached to the panel.
Co-located on a single Pi works too, with the trade-offs noted below.

[skagit-flats]: https://github.com/ellisandy/skagit-flats

## What this installer does

`make install` builds the Rust binary, then deploys cascades + sidecar
under `/opt/cascades` as two systemd services. **It does not install
anything panel-related** — no SPI enable, no Waveshare driver, no display
loop. That's the thin client's job and lives in the thin client's repo.

## Hardware

Hardware sizing is about **where you run cascades**, not the panel:

- **Pi 4 (4GB+) or Pi 5** — comfortable. Renders take ~1–3s per slot;
  the admin editor's live preview (PR #18) feels responsive.
- **Pi 3B+ / Pi Zero 2 W** — works, but Chromium is sluggish (5–10s per
  slot on Zero 2 W). The editor preview will feel laggy. Acceptable if
  your refresh cadence is 60s+ and you don't live in the editor.
- **Pi 3 or older** — Chromium effectively doesn't fit. Don't try.
- **Generic x86_64 Linux box** (NUC, NAS, VM) — also works; pass
  `--force-non-pi` to `install.sh` to skip the Raspberry Pi check.
- **The thin display client** has no such constraints — it just needs
  SPI, a panel, and ~30 MB of RAM.

## Prerequisites on the host running cascades

- Debian-family Linux (Bookworm or Trixie tested; the apt package list
  uses names that exist in both)
- Internet access for `apt`, the Bun installer, and the Puppeteer
  Chromium download (~150 MB the first time)
- Passwordless `sudo`
- **Rust toolchain** via [rustup](https://rustup.rs):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  source "$HOME/.cargo/env"
  ```

## Installing

From a checkout on the host:

```bash
git clone https://github.com/ellisandy/cascades.git
cd cascades
make install
```

That's it. Under the hood:

1. `cargo build --release` (5–10 min on a Pi 4; 30–60 min on a Pi Zero 2 W)
2. `sudo ./scripts/install.sh` which:
   - apt-installs Chromium runtime libs
   - Installs Bun under `/home/cascades/.bun` for the service user
   - Creates the `cascades` system user
   - Lays out `/opt/cascades/{cascades,templates/,fonts/,config/,sidecar/,data/}`
   - Runs `bun install --frozen-lockfile` in the sidecar (downloads
     Chromium for the host arch on first run)
   - Installs two systemd units (`cascades-sidecar`, `cascades`)
   - Enables them on boot, restarts in dependency order, health-checks

When it finishes, the admin UI is at `http://<host-ip>:9090/admin`. Log
in with the API key from `/opt/cascades/config/secrets.toml`
(auto-generated on the Rust server's first boot).

## Pointing the thin display client at cascades

The client fetches the latest PNG on its own cadence. Two endpoints:

| URL | Auth | Notes |
|---|---|---|
| `http://<host>:9090/image.png` | None | Legacy alias for the default display |
| `http://<host>:9090/api/image/{display_id}` | `Authorization: Bearer <api_key>` | Canonical, multi-display, has cache headers |

For a co-located deploy (cascades and the client on the same Pi), use
`http://localhost:9090/...`. For the API endpoint, put the API key from
`/opt/cascades/config/secrets.toml` into the client's config.

## Idempotency

`make install` is safe to re-run any time. Preserved across re-runs:

- `/opt/cascades/data/` — SQLite (your layouts, sources, assets)
- `/opt/cascades/config/secrets.toml` — the API key
- `/opt/cascades/config.toml` — operator's customised version
- `/opt/cascades/templates/` — synced **without `--delete`** so admin-UI
  saves (PR #18) survive `git pull && make install`. The admin UI is
  the source of truth for in-place edits; the repo seeds.
- `cascades` user, group memberships, systemd unit-enable state

Overwritten (the code you just shipped):

- `/opt/cascades/cascades` — the binary
- `/opt/cascades/fonts/` — synced *with* `--delete`
- `/opt/cascades/sidecar/` (excluding `node_modules/`)
- `/opt/cascades/config/plugins.d/` and `display.toml`
- The two systemd unit files (only re-installed if changed)

If you previously ran the PR #19 installer (which had a third
`cascades-display.service`), re-running `make install` from PR #20+ will
disable + remove that leftover automatically.

## Common ops

```bash
make status            # systemctl status for both services
make logs              # tail -f both
make restart           # restart in dependency order
make uninstall         # stop + remove binary; PRESERVES data
make uninstall PURGE=1 # also wipes /opt/cascades and the cascades user
```

## File layout

```
/opt/cascades/
├── cascades                     # Rust binary (release build)
├── config.toml                  # operator config (port, intervals, location)
├── config/
│   ├── secrets.toml             # API key — auto-generated, preserved
│   ├── plugins.d/*.toml         # plugin manifests
│   └── display.toml             # default display layouts
├── templates/                   # *.html.jinja — hot-reloads on change (PR #17)
├── fonts/                       # curated font bundle
├── sidecar/                     # Bun + Puppeteer render service
│   ├── server.ts
│   ├── package.json
│   └── node_modules/            # Chromium for the host arch (~150MB)
└── data/
    └── cascades.db              # SQLite — layouts + instances + assets
```

## Caveats for upgrades

**Templates edited via the admin UI survive `make install`.** The
installer rsyncs `templates/` *without* `--delete` so a saved edit at
`/opt/cascades/templates/weather_full.html.jinja` won't be clobbered by
re-running with the repo's stock version. Trade-off: a template removed
from the repo lingers on disk until manually cleaned. If you want
in-place edits to live in git, copy them back into the repo and commit.

**`config.toml` is preserved — diff it after a `git pull`.** If a new
release adds a required key to `config.toml`, your preserved operator
config will silently miss it and the server will fail to load. After a
pull:

```bash
diff /opt/cascades/config.toml ./config.toml
```

…and merge new keys by hand. We don't auto-merge — too easy to brick a
deploy with a bad default.

## Troubleshooting

**`cascades-sidecar.service` keeps restarting**
Almost always a missing Chromium runtime lib. `journalctl -u
cascades-sidecar` will show which `.so` the dynamic loader couldn't find;
add to `APT_PKGS` in `scripts/install.sh` and re-run `make install`.

**`cargo build --release` is OOM'ing on a small Pi**
Pi Zero 2 W has 416 MB of RAM. Use:
```bash
CARGO_BUILD_JOBS=1 RUSTFLAGS="-C codegen-units=1" cargo build --release
```
…to fit. Cross-compiling from a beefier machine via `make build-arm64`
is the faster alternative if you have `cross` installed.

**Editor preview is slow**
On a Pi each preview kicks Chromium for ~1–3s (Pi 4) to ~5–10s (Pi Zero
2 W). Either accept it as the cost of co-located, or move cascades off
the Pi to a faster host and let the thin client keep talking to it over
the LAN.
