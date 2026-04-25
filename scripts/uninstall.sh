#!/usr/bin/env bash
#
# Cascades uninstaller — symmetric with install.sh.
#
# Default: stops + disables services, removes binaries + systemd units,
# but PRESERVES /opt/cascades/data (SQLite) and /opt/cascades/config
# (operator's config.toml + secrets.toml). Re-running install.sh after
# this returns you to a working state with all your layouts/sources intact.
#
# Pass --purge to also remove /opt/cascades and the cascades user — for
# the case where you're decommissioning the host entirely.

set -euo pipefail

INSTALL_DIR="/opt/cascades"
SERVICE_USER="cascades"
SYSTEMD_DIR="/etc/systemd/system"

PURGE=0
for arg in "$@"; do
    case "$arg" in
        --purge) PURGE=1 ;;
        --help|-h)
            sed -n '3,16p' "$0"
            exit 0
            ;;
        *) echo "unknown option: $arg" >&2; exit 1 ;;
    esac
done

[[ $EUID -eq 0 ]] || { echo "must run as root" >&2; exit 1; }

log() { printf '\e[1;36m[uninstall]\e[0m %s\n' "$*"; }

# Stop + disable in reverse-dependency order. Don't fail on
# "unit not loaded" — that's the expected state on a partial install.
for unit in cascades-display.service cascades.service cascades-sidecar.service; do
    if systemctl list-unit-files "$unit" --no-legend 2>/dev/null | grep -q "$unit"; then
        systemctl disable --now "$unit" 2>/dev/null || true
        log "stopped + disabled $unit"
    fi
done

# Remove unit files, then daemon-reload to clear them from systemd's cache.
for unit in cascades-display.service cascades.service cascades-sidecar.service; do
    if [[ -f "${SYSTEMD_DIR}/${unit}" ]]; then
        rm -f "${SYSTEMD_DIR}/${unit}"
        log "removed ${SYSTEMD_DIR}/${unit}"
    fi
done
systemctl daemon-reload

if [[ "${PURGE}" -eq 1 ]]; then
    log "PURGE: removing ${INSTALL_DIR} and user '${SERVICE_USER}'"
    rm -rf "${INSTALL_DIR}"
    if id -u "${SERVICE_USER}" >/dev/null 2>&1; then
        userdel -r "${SERVICE_USER}" 2>/dev/null || \
            userdel "${SERVICE_USER}" 2>/dev/null || \
            log "could not remove user '${SERVICE_USER}' (still in use?)"
    fi
    log "Purged"
else
    # Preserve data + config. Remove only the binary, sidecar/, display/,
    # templates/, fonts/ — everything that install.sh would re-create.
    for sub in cascades sidecar display templates fonts; do
        rm -rf "${INSTALL_DIR}/${sub:?}"
    done
    log "Removed binary + sidecar + templates + fonts"
    log "Preserved: ${INSTALL_DIR}/data, ${INSTALL_DIR}/config"
    log "(use --purge to wipe everything)"
fi
