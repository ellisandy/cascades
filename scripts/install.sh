#!/usr/bin/env bash
#
# Cascades Raspberry Pi installer — fully idempotent.
#
# Re-running this script after a code update should be safe and should
# converge the host to the desired state without clobbering user data
# (SQLite database, secrets.toml, custom config).
#
# What it does, in order:
#   1.  Sanity-checks: root, Pi (or --force-non-pi), pre-built Rust binary present.
#   2.  apt-installs system packages (Bun-Chromium runtime libs, Python+PIL, SPI tools).
#   3.  Installs Bun for the cascades user if missing.
#   4.  Creates the `cascades` service user and adds it to `spi`/`gpio` groups.
#   5.  Lays out /opt/cascades/{cascades,templates/,fonts/,config/,sidecar/,display/,data/}.
#       - Templates and fonts: rsync-copied (overwrite is fine — hot-reloads in-place).
#       - config/secrets.toml: PRESERVED if present (auto-generated on first server boot).
#       - data/: PRESERVED (SQLite lives there).
#   6.  `bun install` in /opt/cascades/sidecar (downloads Chromium for ARM64; ~150MB).
#   7.  Enables SPI on the Pi (raspi-config nonint do_spi 0).
#   8.  Vendors the Waveshare 7.5" V2 driver into /opt/cascades/display/waveshare_epd/.
#   9.  Installs the three systemd units, daemon-reloads, enables them on boot.
#   10. Restarts services in the correct order to pick up new bits.
#
# Re-run any time. The only state mutation that isn't strictly idempotent is
# the systemctl restart at the end — that's what you want, otherwise an
# install of new code wouldn't take effect until the next reboot.
#
# Usage:
#   sudo ./scripts/install.sh
#   sudo ./scripts/install.sh --skip-display       # don't install the e-ink loop
#   sudo ./scripts/install.sh --force-non-pi       # for testing on a non-Pi host

set -euo pipefail

# ─── Constants ────────────────────────────────────────────────────────────

INSTALL_DIR="/opt/cascades"
SERVICE_USER="cascades"
SERVICE_GROUP="cascades"
SERVICE_HOME="/home/${SERVICE_USER}"
SYSTEMD_DIR="/etc/systemd/system"

# Repo paths (resolved relative to this script's location, so the script
# can be invoked from anywhere).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

WAVESHARE_REPO="https://github.com/waveshareteam/e-Paper.git"
WAVESHARE_REF="master"

# Arg parsing
SKIP_DISPLAY=0
FORCE_NON_PI=0
for arg in "$@"; do
    case "$arg" in
        --skip-display)  SKIP_DISPLAY=1 ;;
        --force-non-pi)  FORCE_NON_PI=1 ;;
        --help|-h)
            sed -n '3,40p' "$0"
            exit 0
            ;;
        *)
            echo "unknown option: $arg" >&2
            echo "see --help" >&2
            exit 1
            ;;
    esac
done

# ─── Helpers ──────────────────────────────────────────────────────────────

log()  { printf '\e[1;36m[install]\e[0m %s\n' "$*"; }
warn() { printf '\e[1;33m[warn]\e[0m %s\n' "$*" >&2; }
die()  { printf '\e[1;31m[error]\e[0m %s\n' "$*" >&2; exit 1; }
step() { printf '\n\e[1;32m▶\e[0m \e[1m%s\e[0m\n' "$*"; }

# Run a command as the cascades service user, preserving its HOME so things
# like Bun's installer find ~/.bun. `sudo -u` resets HOME by default.
as_service_user() {
    sudo -u "${SERVICE_USER}" -H "$@"
}

# ─── Step 0: preflight ────────────────────────────────────────────────────

step "Preflight checks"

[[ $EUID -eq 0 ]] || die "must run as root (try: sudo $0)"

if [[ "${FORCE_NON_PI}" -ne 1 ]]; then
    if [[ ! -r /proc/device-tree/model ]] \
       || ! grep -qi "raspberry pi" /proc/device-tree/model 2>/dev/null; then
        die "this doesn't look like a Raspberry Pi.
     Pass --force-non-pi to override (you'll lose --skip-display safety)."
    fi
    log "Detected: $(tr -d '\0' < /proc/device-tree/model)"
else
    warn "running with --force-non-pi: SPI/Waveshare steps will be skipped"
    SKIP_DISPLAY=1
fi

CASCADES_BINARY="${REPO_ROOT}/target/release/cascades"
if [[ ! -x "${CASCADES_BINARY}" ]]; then
    die "cascades binary not found at ${CASCADES_BINARY}.
     Run 'make build' (or 'cargo build --release') first."
fi
log "Binary found: ${CASCADES_BINARY}"

# ─── Step 1: system packages ──────────────────────────────────────────────

step "Installing system packages"

# Idempotent: apt-get skips already-installed packages.
APT_PKGS=(
    # Chromium runtime deps that Puppeteer's bundled Chromium needs on ARM64.
    # Without these the sidecar starts but every render fails with cryptic
    # SIGSEGV / shared-library errors.
    libnss3 libatk1.0-0 libatk-bridge2.0-0 libcups2 libdrm2 libxkbcommon0
    libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 libpango-1.0-0
    libcairo2 libasound2 libxss1 libgtk-3-0 fonts-liberation
    # Python display loop deps.
    python3 python3-pip python3-pil python3-requests
    # SPI userspace tools (raspi-config + spidev are pre-installed on
    # Raspberry Pi OS; on minimal images they aren't).
    raspi-config
    # Misc utilities the script itself uses.
    rsync curl ca-certificates git
    # Build tools — needed for `bun install` to compile native bits of
    # `sharp` if the prebuilt isn't available for our arch.
    build-essential
)
log "apt-get update…"
apt-get update -qq
log "apt-get install (${#APT_PKGS[@]} packages — first run takes a few minutes)…"
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${APT_PKGS[@]}"

# ─── Step 2: service user ─────────────────────────────────────────────────

step "Setting up service user '${SERVICE_USER}'"

if id -u "${SERVICE_USER}" >/dev/null 2>&1; then
    log "User '${SERVICE_USER}' already exists — skipping create"
else
    useradd --system --create-home --home-dir "${SERVICE_HOME}" \
            --shell /usr/sbin/nologin "${SERVICE_USER}"
    log "Created user '${SERVICE_USER}'"
fi

# Group membership for SPI / GPIO / video (Chromium occasionally wants the
# last). usermod -aG is idempotent — adding to a group already in is a no-op.
if [[ "${SKIP_DISPLAY}" -ne 1 ]]; then
    for grp in spi gpio video; do
        if getent group "$grp" >/dev/null 2>&1; then
            usermod -aG "$grp" "${SERVICE_USER}"
        fi
    done
fi

# ─── Step 3: Bun ──────────────────────────────────────────────────────────

step "Installing Bun (for the render sidecar)"

BUN_BIN="${SERVICE_HOME}/.bun/bin/bun"
if [[ -x "${BUN_BIN}" ]]; then
    log "Bun already installed: $(${BUN_BIN} --version)"
else
    log "Downloading Bun installer…"
    # The official installer respects $HOME and writes to $HOME/.bun.
    as_service_user bash -c 'curl -fsSL https://bun.sh/install | bash'
    [[ -x "${BUN_BIN}" ]] || die "Bun install failed — see output above"
    log "Installed Bun: $(${BUN_BIN} --version)"
fi

# ─── Step 4: lay out files ────────────────────────────────────────────────

step "Deploying files to ${INSTALL_DIR}"

mkdir -p \
    "${INSTALL_DIR}" \
    "${INSTALL_DIR}/data" \
    "${INSTALL_DIR}/config" \
    "${INSTALL_DIR}/config/plugins.d" \
    "${INSTALL_DIR}/templates" \
    "${INSTALL_DIR}/fonts" \
    "${INSTALL_DIR}/sidecar" \
    "${INSTALL_DIR}/display"

# Binary — install -m sets perms in one shot and is idempotent.
install -m 0755 "${CASCADES_BINARY}" "${INSTALL_DIR}/cascades"
log "Binary → ${INSTALL_DIR}/cascades"

# Templates — rsync WITHOUT --delete. The admin editor (PR #18) writes
# operator edits directly to /opt/cascades/templates/*.html.jinja, so
# `git pull && make install` must NOT clobber a saved edit just because
# the repo no longer has that file. Trade-off: a template removed from
# the repo lingers on disk until manually cleaned. Right default for an
# editor-first workflow — the admin UI is the source of truth, the repo
# is the seed.
rsync -a "${REPO_ROOT}/templates/" "${INSTALL_DIR}/templates/"

# Fonts — --delete is fine here; the bundle isn't editable from the UI
# and stale font files are pure dead weight.
rsync -a --delete "${REPO_ROOT}/fonts/" "${INSTALL_DIR}/fonts/"
log "Templates + fonts synced (templates: additive, fonts: mirror)"

# config.toml — only copied if missing; an existing one is the operator's
# customised version and we must not stomp it.
if [[ ! -f "${INSTALL_DIR}/config.toml" ]]; then
    install -m 0644 "${REPO_ROOT}/config.toml" "${INSTALL_DIR}/config.toml"
    log "config.toml → installed (you may want to edit it)"
else
    log "config.toml exists — preserved"
fi

# Plugin manifests + display.toml — overwrite. These ship with the code
# and aren't operator-edited (operator config lives in config.toml + the
# admin UI's database).
rsync -a --delete "${REPO_ROOT}/config/plugins.d/" \
                  "${INSTALL_DIR}/config/plugins.d/"
if [[ -f "${REPO_ROOT}/config/display.toml" ]]; then
    install -m 0644 "${REPO_ROOT}/config/display.toml" \
                    "${INSTALL_DIR}/config/display.toml"
fi
log "Plugin manifests + display.toml synced"

# Sidecar source — exclude node_modules (we don't push the Mac's binaries
# to the Pi; `bun install` rebuilds in step 5). Lockfile IS copied so the
# `--frozen-lockfile` install in step 5 has something to verify against.
rsync -a --delete \
    --exclude='node_modules/' \
    "${REPO_ROOT}/src/sidecar/" "${INSTALL_DIR}/sidecar/"
log "Sidecar source synced (node_modules excluded; bun.lock included)"

# Display loop script + waveshare driver placeholder.
if [[ "${SKIP_DISPLAY}" -ne 1 ]]; then
    install -m 0755 "${REPO_ROOT}/scripts/pi-display-loop.py" \
                    "${INSTALL_DIR}/display/loop.py"
    log "Display loop → ${INSTALL_DIR}/display/loop.py"
fi

# Ownership — everything under INSTALL_DIR runs as cascades:cascades.
chown -R "${SERVICE_USER}:${SERVICE_GROUP}" "${INSTALL_DIR}"

# ─── Step 5: sidecar deps ─────────────────────────────────────────────────

step "Installing sidecar dependencies (downloads ARM64 Chromium — slow first time)"

# --frozen-lockfile keeps repeated installs deterministic — refuses to
# install if bun.lock would need updating. The repo ships a checked-in
# lockfile for exactly this reason. (`install.sh` excludes the lockfile
# from rsync but `bun install` writes it back when satisfied.)
as_service_user bash -c \
    "cd '${INSTALL_DIR}/sidecar' && '${BUN_BIN}' install --frozen-lockfile"
log "Sidecar deps installed"

# ─── Step 6: SPI + Waveshare driver ───────────────────────────────────────

if [[ "${SKIP_DISPLAY}" -ne 1 ]]; then
    step "Enabling SPI bus"
    # do_spi 0 = enable. Idempotent: enabling an already-enabled bus is a no-op.
    if command -v raspi-config >/dev/null 2>&1; then
        raspi-config nonint do_spi 0
        log "SPI enabled (a reboot may be required if it wasn't already)"
    else
        warn "raspi-config not available; enable SPI manually via /boot/config.txt"
    fi

    step "Installing Waveshare 7.5\" V2 driver"
    DRIVER_DIR="${INSTALL_DIR}/display/waveshare_epd"
    # Single source-of-truth check: the panel driver itself. `__pycache__`
    # would be unreliable (only exists after the loop has run at least
    # once) and `-d "${DRIVER_DIR}"` would match an empty directory left
    # behind by an aborted clone.
    if [[ -f "${DRIVER_DIR}/epd7in5_V2.py" ]]; then
        log "Driver already installed — skipping clone"
    else
        TMP_CLONE="$(mktemp -d)"
        log "Cloning ${WAVESHARE_REPO} (shallow)…"
        git clone --depth 1 --branch "${WAVESHARE_REF}" \
                  "${WAVESHARE_REPO}" "${TMP_CLONE}" >/dev/null 2>&1 \
            || die "Waveshare driver clone failed"
        SRC="${TMP_CLONE}/RaspberryPi_JetsonNano/python/lib/waveshare_epd"
        [[ -d "${SRC}" ]] || die "Waveshare driver layout changed; expected ${SRC}"
        rsync -a "${SRC}/" "${DRIVER_DIR}/"
        chown -R "${SERVICE_USER}:${SERVICE_GROUP}" "${DRIVER_DIR}"
        rm -rf "${TMP_CLONE}"
        log "Waveshare driver → ${DRIVER_DIR}"
    fi
fi

# ─── Step 7: systemd units ────────────────────────────────────────────────

step "Installing systemd units"

UNITS_TO_INSTALL=( cascades-sidecar.service cascades.service )
[[ "${SKIP_DISPLAY}" -eq 1 ]] || UNITS_TO_INSTALL+=( cascades-display.service )

CHANGED_UNITS=0
for unit in "${UNITS_TO_INSTALL[@]}"; do
    src="${SCRIPT_DIR}/systemd/${unit}"
    dst="${SYSTEMD_DIR}/${unit}"
    if ! cmp --silent "$src" "$dst" 2>/dev/null; then
        install -m 0644 "$src" "$dst"
        log "Updated ${unit}"
        CHANGED_UNITS=1
    fi
done

if [[ "${CHANGED_UNITS}" -eq 1 ]]; then
    systemctl daemon-reload
    log "systemd reloaded"
fi

# Disable the display service if --skip-display was passed and a previous
# install enabled it — keeps re-runs convergent with the requested flags.
if [[ "${SKIP_DISPLAY}" -eq 1 ]] \
   && systemctl is-enabled cascades-display.service >/dev/null 2>&1; then
    systemctl disable --now cascades-display.service
    log "Disabled cascades-display.service (--skip-display)"
fi

for unit in "${UNITS_TO_INSTALL[@]}"; do
    systemctl enable "$unit" >/dev/null
done
log "Enabled on boot: ${UNITS_TO_INSTALL[*]}"

# ─── Step 8: restart to pick up new bits ──────────────────────────────────

step "Restarting services"

# Order matters: sidecar first, then server (which depends on it), then
# display loop (which depends on the server). systemctl restart is
# blocking, so by the time each call returns the unit is up.
systemctl restart cascades-sidecar.service
systemctl restart cascades.service
[[ "${SKIP_DISPLAY}" -eq 1 ]] || systemctl restart cascades-display.service

# Health check — `systemctl restart` blocks until the unit transitions
# but doesn't wait for it to actually stay up. A unit that ExecStart's
# successfully and crashes 500ms later will still report "started" to
# the restart call. Sleep briefly, then check is-active. Catches the
# common "missing apt dep we forgot" case before the operator has to
# go hunting in journalctl.
sleep 3
HEALTH_OK=1
HEALTH_UNITS=( cascades-sidecar cascades )
[[ "${SKIP_DISPLAY}" -eq 1 ]] || HEALTH_UNITS+=( cascades-display )
for u in "${HEALTH_UNITS[@]}"; do
    if ! systemctl is-active --quiet "$u"; then
        warn "$u is NOT active. Check: journalctl -u $u --no-pager -n 50"
        HEALTH_OK=0
    fi
done
if [[ "${HEALTH_OK}" -eq 1 ]]; then
    log "All services healthy"
fi

# ─── Step 9: post-install summary ─────────────────────────────────────────

step "Done"

cat <<EOF

Cascades is installed at ${INSTALL_DIR} and running as systemd units.

  Status:
    systemctl status cascades cascades-sidecar$( [[ ${SKIP_DISPLAY} -eq 1 ]] || printf ' cascades-display' )

  Logs:
    journalctl -u cascades -f
    journalctl -u cascades-sidecar -f
$( [[ ${SKIP_DISPLAY} -eq 1 ]] || printf '    journalctl -u cascades-display -f' )

  Web UI:
    http://$(hostname -I | awk '{print $1}'):9090/admin

  API key (after first boot — Rust server auto-generates):
    sudo cat ${INSTALL_DIR}/config/secrets.toml

To uninstall:
    sudo make uninstall          # keeps data
    sudo make uninstall PURGE=1  # removes /opt/cascades entirely
EOF
