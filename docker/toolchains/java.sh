#!/usr/bin/env bash
# Installs an Eclipse Temurin JDK via the Adoptium API v3, verified against package.checksum (SHA-256).
# QGATE_JAVA_VERSION: Temurin feature release (major), e.g. 21. The newest build of that major is used.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

ADOPTIUM_API="https://api.adoptium.net/v3/assets/latest"

spec="${QGATE_JAVA_VERSION:-21}"
[[ "$spec" =~ ^[0-9]{1,3}$ ]] || qg_die "QGATE_JAVA_VERSION='$spec' must be a Java major version such as 21"
qg_activate_if_installed java "$spec" && exit 0

qg_workdir java
arch="$(qg_arch_for adoptium)"
query="architecture=$arch&image_type=jdk&os=linux&vendor=eclipse"

asset="$(qg_fetch_text "$ADOPTIUM_API/$spec/hotspot?$query" | jq -r '
  first // empty | "\(.binary.package.link) \(.binary.package.checksum) \(.binary.package.name) \(.release_name)"')"
[[ -n "$asset" ]] || qg_die "Adoptium has no Temurin $spec JDK for linux/$arch"
read -r link checksum name release <<<"$asset"
[[ "$link" == https://* ]] || qg_die "unexpected Temurin download link '$link'"
qg_require_spec "Temurin package" "$name"

qg_log "downloading Temurin $release for linux/$arch"
qg_fetch "$link" "$QG_WORK_DIR/$name"
qg_verify_checksum sha256 "$QG_WORK_DIR/$name" "$checksum" || qg_die "Temurin download failed verification"

qg_extract_tar "$QG_WORK_DIR/$name" "$QG_WORK_DIR/staged"
qg_commit_install java "$spec" "$QG_WORK_DIR/staged" "$release"
