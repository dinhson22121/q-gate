#!/usr/bin/env bash
# Installs Apache Maven (needs the java toolchain), verified against the Apache-published .sha512.
# QGATE_MAVEN_VERSION: exact version, e.g. 3.9.11. Set to "none" to skip Maven.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

DEFAULT_MAVEN_VERSION="3.9.11"
APACHE_MIRRORS=("https://downloads.apache.org/maven" "https://archive.apache.org/dist/maven")

spec="${QGATE_MAVEN_VERSION:-$DEFAULT_MAVEN_VERSION}"
if [[ "$spec" == "none" ]]; then
  qg_log "maven skipped (QGATE_MAVEN_VERSION=none)"
  exit 0
fi
[[ "$spec" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || qg_die "QGATE_MAVEN_VERSION='$spec' must be an exact version such as $DEFAULT_MAVEN_VERSION"
qg_activate_if_installed maven "$spec" && exit 0

qg_workdir maven
archive="apache-maven-$spec-bin.tar.gz"
path="maven-${spec%%.*}/$spec/binaries/$archive"

# Current releases live on downloads.apache.org, older ones only on the archive.
downloaded=""
for mirror in "${APACHE_MIRRORS[@]}"; do
  if qg_fetch "$mirror/$path" "$QG_WORK_DIR/$archive" 2>/dev/null \
    && qg_fetch "$mirror/$path.sha512" "$QG_WORK_DIR/$archive.sha512"; then
    downloaded="$mirror"
    break
  fi
done
[[ -n "$downloaded" ]] || qg_die "Maven $spec could not be downloaded from any Apache mirror"
qg_log "downloaded Maven $spec from $downloaded"
qg_verify_checksum sha512 "$QG_WORK_DIR/$archive" "$(qg_first_token "$QG_WORK_DIR/$archive.sha512")" \
  || qg_die "Maven download failed verification"

qg_extract_tar "$QG_WORK_DIR/$archive" "$QG_WORK_DIR/staged"
qg_commit_install maven "$spec" "$QG_WORK_DIR/staged" "$spec"
