#!/usr/bin/env bash
# Installs every toolchain listed in QGATE_TOOLCHAINS that is not in /toolchains yet, then prints a summary.
# A failing toolchain is logged and skipped; the exit code stays 0 so qgate still starts.
# --strict: exit 1 if any toolchain fails (used when baking toolchains into an image).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=docker/toolchains/lib.sh
. "$SCRIPT_DIR/lib.sh"

ALLOWED_TOOLCHAINS="go java node python rust quality"
DEFAULT_TOOLCHAINS="go,java,quality"
LOCK_TIMEOUT_SECS="${QGATE_TOOLCHAINS_LOCK_TIMEOUT:-1800}"

strict=false
if [[ "${1:-}" == "--strict" ]]; then
  strict=true
fi

# Installer scripts per toolchain, in order.
scripts_for() {
  case "$1" in
    java) echo "java maven gradle" ;;
    # Go: govulncheck and golangci-lint (depguard) are built for / run against Go code; they need the go toolchain.
    go) echo "go govulncheck golangci-lint" ;;
    # Language-independent quality gate tools (dependency vulnerabilities, secret leaks).
    quality) echo "gitleaks osv-scanner" ;;
    *) echo "$1" ;;
  esac
}

# Lower-cased, de-duplicated, validated list from the comma-separated QGATE_TOOLCHAINS.
requested_toolchains() {
  local raw="${QGATE_TOOLCHAINS-$DEFAULT_TOOLCHAINS}" item seen=" "
  raw="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | tr ',;' '  ')"
  for item in $raw; do
    [[ "$item" == "none" ]] && continue
    if [[ " $ALLOWED_TOOLCHAINS " != *" $item "* ]]; then
      qg_warn "ignoring unknown toolchain '$item' (allowed: ${ALLOWED_TOOLCHAINS// /, })"
      continue
    fi
    [[ "$seen" == *" $item "* ]] && continue
    seen="$seen$item "
    printf '%s\n' "$item"
  done
}

fail_or_continue() {
  qg_warn "$1"
  if [[ "$strict" == true ]]; then
    exit 1
  fi
  exit 0
}

mapfile -t toolchains < <(requested_toolchains)
if [[ ${#toolchains[@]} -eq 0 ]]; then
  qg_log "no toolchains requested (QGATE_TOOLCHAINS='${QGATE_TOOLCHAINS-}')"
  exit 0
fi

mkdir -p "$QGATE_TOOLCHAINS_DIR" 2>/dev/null || true
if [[ ! -w "$QGATE_TOOLCHAINS_DIR" ]]; then
  fail_or_continue "$QGATE_TOOLCHAINS_DIR is not writable by uid $(id -u); toolchains not installed (see README, 'Toolchains')"
fi

exec 9>"$QGATE_TOOLCHAINS_DIR/.lock"
if ! flock -n 9; then
  qg_log "another container is installing toolchains; waiting up to ${LOCK_TIMEOUT_SECS}s"
  flock -w "$LOCK_TIMEOUT_SECS" 9 || fail_or_continue "timed out waiting for $QGATE_TOOLCHAINS_DIR/.lock"
fi
# Leftovers of an interrupted run (we hold the lock, so nobody else is using them).
rm -rf "${QGATE_TOOLCHAINS_DIR:?}/.tmp"

qg_log "requested: ${toolchains[*]} (arch $(qg_arch))"
failed=()
for toolchain in "${toolchains[@]}"; do
  for script in $(scripts_for "$toolchain"); do
    started=$SECONDS
    if bash "$SCRIPT_DIR/$script.sh"; then
      elapsed=$((SECONDS - started))
      if [[ $elapsed -gt 1 ]]; then
        qg_log "$script done in ${elapsed}s"
      fi
    else
      failed+=("$script")
      qg_warn "$script FAILED - skipped; qgate will start without it. Check network access / versions in qgate.env."
    fi
  done
done

bash "$SCRIPT_DIR/list.sh" >&2
if [[ ${#failed[@]} -gt 0 ]]; then
  qg_warn "failed: ${failed[*]}"
  if [[ "$strict" == true ]]; then
    exit 1
  fi
fi
exit 0
