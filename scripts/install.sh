#!/usr/bin/env bash
#
# Cascades server installer — fully idempotent.
#
# Cascades is the *server* half of the dashboard system: it serves rendered
# PNGs at /image.png (and /api/image/{id}) plus the admin UI at /admin.
# Clients — typically a Raspberry Pi running a thin display loop like
# `skagit-flats` — fetch the PNG and push it to a panel. This installer
# does NOT install anything panel-related; that's the client's job and
# lives in the client's own repo.
#
# Re-running this script after a code update should be safe and should
# converge the host to the desired state without clobbering user data
# (SQLite database, secrets.toml, custom config).
#
# What it does, in order:
#   1.  Sanity-checks: root, Linux host (or --force-non-pi to skip the
#       "looks like a Raspberry Pi" check), pre-built Rust binary present.
#   2.  apt-installs system packages — almost entirely Chromium runtime libs
#       so Puppeteer's bundled browser doesn't crash with cryptic SIGSEGV.
#   3.  Installs Bun for the cascades user if missing.
#   4.  Creates the `cascades` system user (no panel-related groups).
#   5.  Lays out /opt/cascades/{cascades,templates/,fonts/,config/,sidecar/,data/}.
#       - Templates: rsync WITHOUT --delete so admin-UI saves (PR #18) survive
#         `git pull && make install`.
#       - Fonts + plugins: rsync WITH --delete (not editable from the UI).
#       - config/secrets.toml: PRESERVED if present (auto-generated on first server boot).
#       - data/: PRESERVED (SQLite lives there).
#       - config.toml: PRESERVED if present (operator's customised version).
#   6.  `bun install --frozen-lockfile` in /opt/cascades/sidecar
#       (downloads Chromium for the host arch; ~150MB the first time).
#   7.  Installs two systemd units (cascades-sidecar, cascades), daemon-reloads,
#       enables them on boot.
#   8.  Restarts services in dependency order, then health-checks `is-active`
#       so a missing-apt-dep failure surfaces immediately.
#
# Re-run any time. The only state mutation that isn't strictly idempotent is
# the systemctl restart at the end — that's what you want, otherwise an
# install of new code wouldn't take effect until the next reboot.
#
# Usage:
#   sudo ./scripts/install.sh
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

# Arg parsing
FORCE_NON_PI=0
for arg in "$@"; do
    case "$arg" in
        --force-non-pi)  FORCE_NON_PI=1 ;;
        --help|-h)
            sed -n '3,45p' "$0"
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
     Pass --force-non-pi to install on a generic Linux host (e.g. NUC, NAS, VM)."
    fi
    log "Detected: $(tr -d '\0' < /proc/device-tree/model)"
else
    warn "running with --force-non-pi: skipping the Pi-model check"
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
    # Chromium runtime deps that Puppeteer's bundled Chromium needs.
    # Without these the sidecar starts but every render fails with cryptic
    # SIGSEGV / shared-library errors. Bookworm and Trixie both ship the
    # un-suffixed names below as installable (Trixie also offers `*t64`
    # transitional packages but doesn't require them).
    libnss3 libatk1.0-0 libatk-bridge2.0-0 libcups2 libdrm2 libxkbcommon0
    libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 libpango-1.0-0
    libcairo2 libasound2 libxss1 libgtk-3-0 fonts-liberation
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

# No special group memberships needed. Cascades is a server process — it
# doesn't talk to SPI, GPIO, or video hardware. The thin display client
# (e.g. skagit-flats) handles all of that on its own service account.

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
    "${INSTALL_DIR}/sidecar"

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

# ─── Step 6: systemd units ────────────────────────────────────────────────

step "Installing systemd units"

# Two units: sidecar (Bun + Puppeteer) and the Rust server. The server
# Requires= the sidecar so they restart and stop together.
#
# Convergence: if a previous install (PR #19) had `cascades-display.service`
# enabled — that unit is gone now — disable + remove the leftover.
if systemctl list-unit-files cascades-display.service --no-legend 2>/dev/null \
       | grep -q cascades-display.service; then
    systemctl disable --now cascades-display.service 2>/dev/null || true
    rm -f "${SYSTEMD_DIR}/cascades-display.service"
    log "Removed legacy cascades-display.service from a previous install"
fi

UNITS_TO_INSTALL=( cascades-sidecar.service cascades.service )

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

for unit in "${UNITS_TO_INSTALL[@]}"; do
    systemctl enable "$unit" >/dev/null
done
log "Enabled on boot: ${UNITS_TO_INSTALL[*]}"

# ─── Step 7: restart to pick up new bits ──────────────────────────────────

step "Restarting services"

# Order matters: sidecar first, then server (which Requires= it).
# systemctl restart is blocking, so by the time each call returns the
# unit is up.
systemctl restart cascades-sidecar.service
systemctl restart cascades.service

# Health check — `systemctl restart` blocks until the unit transitions
# but doesn't wait for it to actually stay up. A unit that ExecStart's
# successfully and crashes 500ms later will still report "started" to
# the restart call. Sleep briefly, then check is-active. Catches the
# common "missing apt dep we forgot" case before the operator has to
# go hunting in journalctl.
sleep 3
HEALTH_OK=1
for u in cascades-sidecar cascades; do
    if ! systemctl is-active --quiet "$u"; then
        warn "$u is NOT active. Check: journalctl -u $u --no-pager -n 50"
        HEALTH_OK=0
    fi
done
if [[ "${HEALTH_OK}" -eq 1 ]]; then
    log "All services healthy"
fi

# ─── Step 8: post-install summary ─────────────────────────────────────────

step "Done"

cat <<EOF

Cascades is installed at ${INSTALL_DIR} and running as systemd units.

  Status:
    systemctl status cascades cascades-sidecar

  Logs:
    journalctl -u cascades -f
    journalctl -u cascades-sidecar -f

  Web UI:
    http://$(hostname -I | awk '{print $1}'):9090/admin

  API key (after first boot — Rust server auto-generates):
    sudo cat ${INSTALL_DIR}/config/secrets.toml

  Image endpoints for the thin display client to fetch:
    http://localhost:9090/image.png                  (legacy alias, no auth)
    http://localhost:9090/api/image/default          (requires Bearer <api_key>)

To uninstall:
    sudo make uninstall          # keeps data
    sudo make uninstall PURGE=1  # removes /opt/cascades entirely
EOF
