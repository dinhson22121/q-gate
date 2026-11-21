#!/usr/bin/env bash
# Installs govulncheck (Go's call-graph aware vulnerability scanner) with the installed Go toolchain.
# The module is verified by Go itself (go.sum database, sum.golang.org); the build is pinned to the version.
# QGATE_GOVULNCHECK_VERSION: exact version, e.g. v1.10.0. Set to "none" to skip it.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

DEFAULT_GOVULNCHECK_VERSION="v1.10.0"

spec="${QGATE_GOVULNCHECK_VERSION:-$DEFAULT_GOVULNCHECK_VERSION}"
if [[ "$spec" == "none" ]]; then
  qg_log "govulncheck skipped (QGATE_GOVULNCHECK_VERSION=none)"
  exit 0
fi
[[ "$spec" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || qg_die "QGATE_GOVULNCHECK_VERSION='$spec' must be an exact version such as $DEFAULT_GOVULNCHECK_VERSION"
qg_activate_if_installed govulncheck "$spec" && exit 0

go_bin="$(qg_tool_dir go)/current/bin/go"
[[ -x "$go_bin" ]] || qg_die "govulncheck needs the go toolchain (not installed)"

qg_workdir govulncheck
mkdir -p "$QG_WORK_DIR/staged/bin"
qg_log "building govulncheck $spec"
# -modcacherw keeps the scratch module cache removable by the EXIT trap.
GOBIN="$QG_WORK_DIR/staged/bin" GOPATH="$QG_WORK_DIR/gopath" GOCACHE="$QG_WORK_DIR/gocache" \
  GOFLAGS="-modcacherw" GOTOOLCHAIN=local \
  "$go_bin" install "golang.org/x/vuln/cmd/govulncheck@$spec"
qg_commit_install govulncheck "$spec" "$QG_WORK_DIR/staged" "$spec"
