#!/usr/bin/env bash
# Container entrypoint (run under tini): prepare volumes, install the toolchains chosen in qgate.env,
# then exec the command (default: qgate serve). `qgate runner` (the isolated runner container) skips all
# of that: it has no /data or /config and mounts /toolchains read-only (qgate installs them).
set -euo pipefail

TOOLCHAIN_SCRIPTS=/opt/qgate/toolchains
CONFIG_TEMPLATE=/opt/qgate/qgate.toml.example
DATA_DIR="${QGATE_DATA_DIR:-/data}"
CONFIG_DIR="${QGATE_CONFIG_DIR:-/config}"
TOOLCHAINS_DIR="${QGATE_TOOLCHAINS_DIR:-/toolchains}"

log() { printf '[entrypoint] %s\n' "$*" >&2; }

permission_hint() {
  log "WARNING: $1 is not writable by uid $(id -u):$(id -g)."
  log "  The volume was probably created for another QGATE_UID/QGATE_GID. Either set them back, or fix"
  log "  ownership once: docker compose run --rm --user root qgate chown -R $(id -u):$(id -g) $1"
}

prepare_data_dir() {
  if mkdir -p "$DATA_DIR/cache" "$DATA_DIR/logs" 2>/dev/null && [[ -w "$DATA_DIR" ]]; then
    return
  fi
  permission_hint "$DATA_DIR"
}

# First start: give the user an editable server config next to docker-compose.yml.
seed_config() {
  local target="$CONFIG_DIR/qgate.toml"
  [[ -e "$target" || ! -f "$CONFIG_TEMPLATE" ]] && return
  if [[ -w "$CONFIG_DIR" ]] && cp "$CONFIG_TEMPLATE" "$target" 2>/dev/null; then
    log "created $target from the template (edit it and restart to change server settings)"
  else
    log "no $target; using built-in defaults"
  fi
}

install_toolchains() {
  if [[ "${QGATE_TOOLCHAINS_SKIP_INSTALL:-false}" == "true" ]]; then
    log "toolchain installation skipped (QGATE_TOOLCHAINS_SKIP_INSTALL=true)"
    return
  fi
  if ! mkdir -p "$TOOLCHAINS_DIR" 2>/dev/null || [[ ! -w "$TOOLCHAINS_DIR" ]]; then
    permission_hint "$TOOLCHAINS_DIR"
    return
  fi
  # Never block startup: a toolchain that cannot be installed only disables the checks that need it.
  bash "$TOOLCHAIN_SCRIPTS/install.sh" || log "WARNING: toolchain installation reported an error; continuing"
}

# The runner only needs its cache home; toolchains appear in the shared volume as qgate installs them.
prepare_runner() {
  if ! mkdir -p "${HOME:?}" 2>/dev/null || [[ ! -w "$HOME" ]]; then
    permission_hint "$HOME"
  fi
}

if [[ "${1:-}" == "qgate" && "${2:-}" == "runner" ]]; then
  prepare_runner
  log "starting: $*"
  exec "$@"
fi

prepare_data_dir
seed_config
install_toolchains
# shellcheck source=docker/toolchains/env.sh
. "$TOOLCHAIN_SCRIPTS/env.sh"

if [[ $# -eq 0 ]]; then
  set -- qgate serve
fi
log "starting: $*"
exec "$@"
