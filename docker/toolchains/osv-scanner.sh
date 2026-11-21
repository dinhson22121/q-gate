#!/usr/bin/env bash
# Installs osv-scanner (dependency vulnerability scanner), verified against the release's SHA256SUMS file.
# QGATE_OSV_SCANNER_VERSION: exact version, e.g. 2.7.1. Set to "none" to skip it.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

DEFAULT_OSV_SCANNER_VERSION="2.7.1"
RELEASES="https://github.com/google/osv-scanner/releases/download"

spec="${QGATE_OSV_SCANNER_VERSION:-$DEFAULT_OSV_SCANNER_VERSION}"
if [[ "$spec" == "none" ]]; then
  qg_log "osv-scanner skipped (QGATE_OSV_SCANNER_VERSION=none)"
  exit 0
fi
[[ "$spec" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || qg_die "QGATE_OSV_SCANNER_VERSION='$spec' must be an exact version such as $DEFAULT_OSV_SCANNER_VERSION"
qg_activate_if_installed osv-scanner "$spec" && exit 0

qg_workdir osv-scanner
binary="osv-scanner_linux_$(qg_arch_for go)"
sums="osv-scanner_SHA256SUMS"

qg_log "downloading osv-scanner $spec"
qg_fetch "$RELEASES/v$spec/$binary" "$QG_WORK_DIR/$binary"
qg_fetch "$RELEASES/v$spec/$sums" "$QG_WORK_DIR/$sums"
qg_verify_checksum sha256 "$QG_WORK_DIR/$binary" "$(qg_checksum_for "$QG_WORK_DIR/$sums" "$binary")" \
  || qg_die "osv-scanner download failed verification"

mkdir -p "$QG_WORK_DIR/staged/bin"
install -m 0755 "$QG_WORK_DIR/$binary" "$QG_WORK_DIR/staged/bin/osv-scanner"
qg_commit_install osv-scanner "$spec" "$QG_WORK_DIR/staged" "$spec"
