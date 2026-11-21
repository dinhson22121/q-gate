#!/usr/bin/env bash
# Installs uv (verified against its release .sha256), then a uv-managed CPython (uv verifies the
# interpreter download against hashes embedded in the uv binary).
# QGATE_PYTHON_VERSION: e.g. 3.13 or 3.13.5.  QGATE_UV_VERSION: "latest" (at first install) or e.g. 0.9.5.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

UV_RELEASES="https://github.com/astral-sh/uv/releases"

spec="${QGATE_PYTHON_VERSION:-3.13}"
uv_spec="${QGATE_UV_VERSION:-latest}"
[[ "$spec" =~ ^3\.[0-9]+(\.[0-9]+)?$ ]] || qg_die "QGATE_PYTHON_VERSION='$spec' must look like 3.13 or 3.13.5"
qg_require_spec QGATE_UV_VERSION "$uv_spec"

qg_workdir python
export UV_PYTHON_INSTALL_DIR="$QGATE_TOOLCHAINS_DIR/python/.uv-pythons"
export UV_PYTHON_BIN_DIR="$QGATE_TOOLCHAINS_DIR/python/.uv-bin"
export UV_PYTHON_PREFERENCE=only-managed
# We create our own bin/ links below; this silences uv's "not on your PATH" hint.
export UV_PYTHON_INSTALL_BIN=0
export UV_CACHE_DIR="$QG_WORK_DIR/uv-cache"

install_uv() {
  local triple archive base
  triple="$(qg_arch_for triple)"
  archive="uv-$triple.tar.gz"
  if [[ "$uv_spec" == "latest" ]]; then
    base="$UV_RELEASES/latest/download"
  else
    base="$UV_RELEASES/download/$uv_spec"
  fi
  qg_log "downloading uv $uv_spec for $triple"
  qg_fetch "$base/$archive" "$QG_WORK_DIR/$archive"
  qg_fetch "$base/$archive.sha256" "$QG_WORK_DIR/$archive.sha256"
  qg_verify_checksum sha256 "$QG_WORK_DIR/$archive" "$(qg_first_token "$QG_WORK_DIR/$archive.sha256")" \
    || qg_die "uv download failed verification"
  qg_extract_tar "$QG_WORK_DIR/$archive" "$QG_WORK_DIR/uv-staged"
  qg_commit_install uv "$uv_spec" "$QG_WORK_DIR/uv-staged" \
    "$("$QG_WORK_DIR/uv-staged/uv" --version | awk '{ print $2 }')"
}

qg_activate_if_installed uv "$uv_spec" || install_uv
uv="$(qg_tool_dir uv)/current/uv"

qg_activate_if_installed python "$spec" && exit 0

qg_log "installing CPython $spec with uv"
"$uv" python install "$spec"
interpreter="$("$uv" python find "$spec")"
[[ -x "$interpreter" ]] || qg_die "uv did not report an interpreter for Python $spec"

mkdir -p "$QG_WORK_DIR/staged/bin"
ln -s "$interpreter" "$QG_WORK_DIR/staged/bin/python"
ln -s "$interpreter" "$QG_WORK_DIR/staged/bin/python3"
for name in pip pip3; do
  if [[ -e "$(dirname "$interpreter")/$name" ]]; then
    ln -s "$(dirname "$interpreter")/$name" "$QG_WORK_DIR/staged/bin/$name"
  fi
done
qg_commit_install python "$spec" "$QG_WORK_DIR/staged" "$("$interpreter" -c 'import platform; print(platform.python_version())')"
