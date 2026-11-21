#!/usr/bin/env bash
# Installs Node.js from nodejs.org, verified against the release's SHASUMS256.txt.
# QGATE_NODE_VERSION: major ("24", newest release of that line) or exact ("24.11.1").
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

NODE_DIST="https://nodejs.org/dist"

spec="${QGATE_NODE_VERSION:-24}"
[[ "$spec" =~ ^[0-9]+(\.[0-9]+\.[0-9]+)?$ ]] || qg_die "QGATE_NODE_VERSION='$spec' must be a major (24) or exact (24.11.1) version"
qg_activate_if_installed node "$spec" && exit 0

qg_workdir node
arch="$(qg_arch_for node)"

if [[ "$spec" == *.* ]]; then
  version="v$spec"
else
  # index.json is sorted newest first.
  version="$(qg_fetch_text "$NODE_DIST/index.json" \
    | jq -r --arg major "v$spec." '[ .[] | select(.version | startswith($major)) ] | first // empty | .version')"
  [[ -n "$version" ]] || qg_die "no Node.js release found for major $spec"
fi

archive="node-$version-linux-$arch.tar.xz"
qg_log "downloading Node.js $version for linux/$arch"
qg_fetch "$NODE_DIST/$version/$archive" "$QG_WORK_DIR/$archive"
qg_fetch "$NODE_DIST/$version/SHASUMS256.txt" "$QG_WORK_DIR/SHASUMS256.txt"
expected="$(awk -v name="$archive" '$2 == name { print $1 }' "$QG_WORK_DIR/SHASUMS256.txt")"
qg_verify_checksum sha256 "$QG_WORK_DIR/$archive" "$expected" || qg_die "Node.js download failed verification"

qg_extract_tar "$QG_WORK_DIR/$archive" "$QG_WORK_DIR/staged"
qg_commit_install node "$spec" "$QG_WORK_DIR/staged" "${version#v}"
