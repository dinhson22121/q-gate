#!/usr/bin/env bash
# Installs Go from go.dev, verified against the SHA-256 published in https://go.dev/dl/?mode=json.
# QGATE_GO_VERSION: "latest" (newest stable at first install), a minor ("1.24") or exact ("1.24.3") version.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

GO_DL_INDEX="https://go.dev/dl/?mode=json"
GO_DL_BASE="https://dl.google.com/go"

spec="${QGATE_GO_VERSION:-latest}"
qg_require_spec QGATE_GO_VERSION "$spec"
qg_activate_if_installed go "$spec" && exit 0

qg_workdir go
arch="$(qg_arch_for go)"

# Picks "<filename> <sha256> <version>" for the newest stable release matching the spec.
select_release() {
  jq -r --arg spec "$spec" --arg arch "$arch" '
    [ .[] | select(.stable)
      | select($spec == "latest" or .version == "go\($spec)" or (.version | startswith("go\($spec).")))
      | .files[] | select(.os == "linux" and .arch == $arch and .kind == "archive") ]
    | first // empty | "\(.filename) \(.sha256) \(.version)"'
}

release="$(qg_fetch_text "$GO_DL_INDEX" | select_release)"
if [[ -z "$release" ]]; then
  # Older releases are only listed in the full index.
  release="$(qg_fetch_text "$GO_DL_INDEX&include=all" | select_release)"
fi
[[ -n "$release" ]] || qg_die "no Go release matches '$spec' for linux/$arch"
read -r filename sha256 version <<<"$release"
qg_require_spec "Go archive" "$filename"

qg_log "downloading Go ${version#go} for linux/$arch"
qg_fetch "$GO_DL_BASE/$filename" "$QG_WORK_DIR/$filename"
qg_verify_checksum sha256 "$QG_WORK_DIR/$filename" "$sha256" || qg_die "Go download failed verification"

qg_extract_tar "$QG_WORK_DIR/$filename" "$QG_WORK_DIR/staged"
qg_commit_install go "$spec" "$QG_WORK_DIR/staged" "${version#go}"
