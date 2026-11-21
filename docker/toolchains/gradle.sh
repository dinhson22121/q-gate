#!/usr/bin/env bash
# Installs Gradle (optional; most projects use ./gradlew instead), verified against the published .sha256.
# QGATE_GRADLE_VERSION: exact version, e.g. 8.14.3. Empty skips Gradle.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

GRADLE_DIST="https://services.gradle.org/distributions"

spec="${QGATE_GRADLE_VERSION:-}"
if [[ -z "$spec" ]]; then
  exit 0
fi
[[ "$spec" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?(-rc-[0-9]+)?$ ]] || qg_die "QGATE_GRADLE_VERSION='$spec' must be a version such as 8.14.3"
qg_activate_if_installed gradle "$spec" && exit 0

qg_workdir gradle
archive="gradle-$spec-bin.zip"

qg_log "downloading Gradle $spec"
qg_fetch "$GRADLE_DIST/$archive" "$QG_WORK_DIR/$archive"
qg_fetch "$GRADLE_DIST/$archive.sha256" "$QG_WORK_DIR/$archive.sha256"
qg_verify_checksum sha256 "$QG_WORK_DIR/$archive" "$(qg_first_token "$QG_WORK_DIR/$archive.sha256")" \
  || qg_die "Gradle download failed verification"

unzip -q "$QG_WORK_DIR/$archive" -d "$QG_WORK_DIR/unzipped"
mv "$QG_WORK_DIR/unzipped/gradle-$spec" "$QG_WORK_DIR/staged"
qg_commit_install gradle "$spec" "$QG_WORK_DIR/staged" "$spec"
