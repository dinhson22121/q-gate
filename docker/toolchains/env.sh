# shellcheck shell=bash
# Sourced by the entrypoint after installation. Static locations (PATH entries, GOPATH, caches, ...) are
# set as image ENV in the Dockerfile so `docker compose exec` sees them too; this file only fixes up
# what depends on the container's runtime state.

_qg_toolchains="${QGATE_TOOLCHAINS_DIR:-/toolchains}"

# JAVA_HOME / MAVEN_HOME pointing at a missing directory breaks tools such as ./gradlew.
if [ ! -x "$_qg_toolchains/java/current/bin/java" ]; then
  unset JAVA_HOME
fi
if [ ! -d "$_qg_toolchains/maven/current" ]; then
  unset MAVEN_HOME
fi

# Tools write caches and config under $HOME; the root filesystem may be read-only and the uid may have
# no passwd entry, so fall back to the data volume.
if [ -z "${HOME:-}" ] || [ "$HOME" = "/" ] || ! { mkdir -p "$HOME" && [ -w "$HOME" ]; } 2>/dev/null; then
  export HOME="${QGATE_DATA_DIR:-/data}/home"
  mkdir -p "$HOME" 2>/dev/null || true
fi

unset _qg_toolchains
