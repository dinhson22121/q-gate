#!/usr/bin/env bash
# Installs gitleaks (secret scanner), verified against the checksums file of the same GitHub release.
# QGATE_GITLEAKS_VERSION: exact version, e.g. 8.30.1. Set to "none" to skip it.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

DEFAULT_GITLEAKS_VERSION="8.30.1"
RELEASES="https://github.com/gitleaks/gitleaks/releases/download"

spec="${QGATE_GITLEAKS_VERSION:-$DEFAULT_GITLEAKS_VERSION}"
if [[ "$spec" == "none" ]]; then
  qg_log "gitleaks skipped (QGATE_GITLEAKS_VERSION=none)"
  exit 0
fi
[[ "$spec" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || qg_die "QGATE_GITLEAKS_VERSION='$spec' must be an exact version such as $DEFAULT_GITLEAKS_VERSION"
qg_activate_if_installed gitleaks "$spec" && exit 0

qg_workdir gitleaks
archive="gitleaks_${spec}_linux_$(qg_arch_for node).tar.gz"
sums="gitleaks_${spec}_checksums.txt"

qg_log "downloading gitleaks $spec"
qg_fetch "$RELEASES/v$spec/$archive" "$QG_WORK_DIR/$archive"
qg_fetch "$RELEASES/v$spec/$sums" "$QG_WORK_DIR/$sums"
qg_verify_checksum sha256 "$QG_WORK_DIR/$archive" "$(qg_checksum_for "$QG_WORK_DIR/$sums" "$archive")" \
  || qg_die "gitleaks download failed verification"

mkdir -p "$QG_WORK_DIR/staged/bin"
tar -xzf "$QG_WORK_DIR/$archive" -C "$QG_WORK_DIR/staged/bin" --no-same-owner gitleaks
qg_commit_install gitleaks "$spec" "$QG_WORK_DIR/staged" "$spec"
