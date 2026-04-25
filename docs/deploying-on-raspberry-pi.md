# Deploying Cascades on a Raspberry Pi

This is the all-on-Pi deployment recipe: server, render sidecar, and the
e-ink push loop all run on the same Raspberry Pi. The web admin UI
(including PR-C's plugin editor) is reachable over your LAN; the Pi
drives the Waveshare 7.5" V2 panel directly via SPI.

For the alternative "Pi as thin client, server elsewhere" topology, see
the discussion in PR #19.

## Hardware tested

- **Raspberry Pi 4 (4GB or 8GB)** or **Pi 5** — recommended. Chromium needs
  the RAM and the cores; renders take ~1–3s per slot on Pi 4.
- **Pi 3B+ / Pi Zero 2 W** — Chromium will run, but renders are sluggish
  (5–10s per slot). Acceptable if your refresh interval is generous.
- **Pi 3 or older** — don't try. Chromium effectively doesn't fit.
- **Waveshare 7.5" V2** e-ink HAT (`epd7in5_V2` driver). Other panels would
  need a different driver in `scripts/pi-display-loop.py`.

## Prerequisites on the Pi

- Raspberry Pi OS Bookworm (64-bit) — Lite or Desktop both fine
- Internet access (for `apt`, Bun installer, Waveshare driver clone, Puppeteer Chromium download)
- Root via `sudo`
- **Rust toolchain** — install via [rustup](https://rustup.rs):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  source "$HOME/.cargo/env"
  ```

## One-shot install

From a checkout of the repo on the Pi:

```bash
git clone https://github.com/ellisandy/cascades.git
cd cascades
make install
```

That's it. Under the hood `make install` runs:

1. `cargo build --release` — produces `target/release/cascades` (~5–10 min on Pi 4).
2. `sudo ./scripts/install.sh` which:
   - apt-installs Chromium runtime libs, Python+PIL, SPI tools
   - Installs Bun for the `cascades` service user
   - Creates the `cascades` system user (in `spi`, `gpio`, `video` groups)
   - Lays out `/opt/cascades/` with the binary, templates, fonts, sidecar source
   - `bun install` in the sidecar (downloads ARM64 Chromium — ~150MB the first time)
   - Enables the SPI bus
   - Vendors the Waveshare driver into `/opt/cascades/display/waveshare_epd/`
   - Installs three systemd units (`cascades-sidecar`, `cascades`, `cascades-display`)
   - Enables them on boot and restarts them in dependency order

When it finishes, the admin UI is at `http://<pi-ip>:9090/admin`. Log in
with the API key from `/opt/cascades/config/secrets.toml` (auto-generated
on first boot of the Rust server).

## Idempotency

`make install` is safe to re-run any time. State that's preserved across
re-installs:

- `/opt/cascades/data/` — SQLite database (your layouts, sources, assets)
- `/opt/cascades/config/secrets.toml` — the API key
- `/opt/cascades/config.toml` — the operator's own config file (only copied if missing)
- group memberships, the systemd unit-enable state, the SPI-enabled flag

State that's overwritten (because it's the code you just shipped):

- `/opt/cascades/cascades` — the binary
- `/opt/cascades/templates/` and `/opt/cascades/fonts/` — synced with `--delete`
- `/opt/cascades/sidecar/` (excluding `node_modules/`)
- `/opt/cascades/config/plugins.d/` and `/opt/cascades/config/display.toml`
- The three systemd unit files (only re-installed if they actually changed)

The script exits 0 on a re-run that has nothing to do, so it's safe to
wire into a CI/CD step or a `git pull && make install` cron.

## Common ops

```bash
make status        # systemctl status for all three services
make logs          # tail -f for all three (Ctrl-C to exit)
make restart       # restart in dependency order
make uninstall     # stop services + remove binary; PRESERVES data
make uninstall PURGE=1  # also wipes /opt/cascades and the cascades user
```

## What the install configures

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
│   └── node_modules/            # Chromium for ARM64 lives here (~150MB)
├── display/
│   ├── loop.py                  # the e-ink push loop
│   └── waveshare_epd/           # vendored driver
└── data/
    └── cascades.db              # SQLite — layouts + instances + assets
```

## Configuration knobs

Most behavior is configurable without re-installing. Edit and restart:

- **`/opt/cascades/config.toml`** — server port, refresh intervals,
  location, source-specific options. `sudo systemctl restart cascades`
  to apply.
- **systemd unit env vars** — bump `RUST_LOG`, change `SIDECAR_URL`, etc.
  by editing `/etc/systemd/system/cascades.service`, then
  `sudo systemctl daemon-reload && sudo systemctl restart cascades`.
- **Display loop interval** — `CASCADES_INTERVAL_SECS` in
  `cascades-display.service` (default 60s).
- **Templates** — edit in-place under `/opt/cascades/templates/` (or via
  the admin editor at `/admin/plugins/{id}/edit`). The hot-reload watcher
  picks them up immediately, no restart.

## Troubleshooting

**"`cargo build --release` is using a lot of swap"**
Pi 4 with 2GB RAM may swap during the link step. Use a 4GB+ board, or
cross-compile from a faster machine and copy the binary over.

**`cascades-sidecar.service` keeps restarting**
Almost always a missing Chromium runtime lib. `journalctl -u
cascades-sidecar` will show which `.so` the dynamic loader couldn't find;
add to `APT_PKGS` in `scripts/install.sh` and re-run `make install`.

**Display panel flashes but doesn't update**
Check `journalctl -u cascades-display`. Common causes:
- `secrets.toml` missing — server hasn't booted yet, loop will retry
- SPI not enabled — `sudo raspi-config`, Interface Options → SPI → Enable, reboot
- Image dimensions don't match panel — check `[display]` in `config.toml`

**Editor preview is slow**
On Pi 4 each preview request kicks Chromium for ~1–3s. The 200ms debounce
helps but you'll still feel it. Either: (a) accept it as the cost of
all-on-Pi, or (b) move to the thin-client topology and put the server
on a faster host.

## Caveats for upgrades

**Templates edited via the admin UI survive `make install`.** The
installer rsyncs `templates/` *without* `--delete`, so a saved edit at
`/opt/cascades/templates/weather_full.html.jinja` won't be clobbered by
re-running the installer with the repo's stock version. The trade-off:
the admin UI is the source of truth for any in-place edits — if you want
those to live in git, copy them back into the repo and commit. (The
installer does honour the repo for *new* template files added by an
upgrade.)

**`config.toml` is preserved across upgrades — diff it after pulling.**
If a new release adds a required key to `config.toml`, your preserved
operator config will silently miss it and the server will fail to load
on the next restart. After a `git pull`, diff:

```bash
diff /opt/cascades/config.toml ./config.toml
```

…and merge new keys by hand. We don't auto-merge because it's surgical
and one wrong default could brick the deploy.
