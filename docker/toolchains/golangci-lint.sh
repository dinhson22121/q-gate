#!/usr/bin/env bash
# Installs golangci-lint (used only for its depguard import-boundary linter), verified against the checksums
# file of the same GitHub release.
# QGATE_GOLANGCI_LINT_VERSION: exact version, e.g. 2.15.0. Set to "none" to skip it.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

DEFAULT_GOLANGCI_LINT_VERSION="2.15.0"
RELEASES="https://github.com/golangci/golangci-lint/releases/download"

spec="${QGATE_GOLANGCI_LINT_VERSION:-$DEFAULT_GOLANGCI_LINT_VERSION}"
if [[ "$spec" == "none" ]]; then
  qg_log "golangci-lint skipped (QGATE_GOLANGCI_LINT_VERSION=none)"
  exit 0
fi
[[ "$spec" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || qg_die "QGATE_GOLANGCI_LINT_VERSION='$spec' must be an exact version such as $DEFAULT_GOLANGCI_LINT_VERSION"
qg_activate_if_installed golangci-lint "$spec" && exit 0

qg_workdir golangci-lint
archive="golangci-lint-$spec-linux-$(qg_arch_for go).tar.gz"
sums="golangci-lint-$spec-checksums.txt"

qg_log "downloading golangci-lint $spec"
qg_fetch "$RELEASES/v$spec/$archive" "$QG_WORK_DIR/$archive"
qg_fetch "$RELEASES/v$spec/$sums" "$QG_WORK_DIR/$sums"
qg_verify_checksum sha256 "$QG_WORK_DIR/$archive" "$(qg_checksum_for "$QG_WORK_DIR/$sums" "$archive")" \
  || qg_die "golangci-lint download failed verification"

qg_extract_tar "$QG_WORK_DIR/$archive" "$QG_WORK_DIR/unpacked"
mkdir -p "$QG_WORK_DIR/staged/bin"
install -m 0755 "$QG_WORK_DIR/unpacked/golangci-lint" "$QG_WORK_DIR/staged/bin/golangci-lint"
qg_commit_install golangci-lint "$spec" "$QG_WORK_DIR/staged" "$spec"
