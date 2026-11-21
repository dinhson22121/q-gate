#!/usr/bin/env bash
# Installs Rust with rustup-init (verified against its published .sha256); rustup then verifies every
# component it downloads. Toolchains live side by side in RUSTUP_HOME; `rustup default` switches them.
# QGATE_RUST_VERSION: stable (default), beta, nightly or an exact version such as 1.90.0.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

RUSTUP_DIST="https://static.rust-lang.org/rustup/dist"

spec="${QGATE_RUST_VERSION:-stable}"
qg_require_spec QGATE_RUST_VERSION "$spec"

rust_dir="$(qg_tool_dir rust)"
export RUSTUP_HOME="$rust_dir/rustup" CARGO_HOME="$rust_dir/cargo"
marker="$rust_dir/.installed-$spec"
rustup="$CARGO_HOME/bin/rustup"

if [[ -f "$marker" && -x "$rustup" ]]; then
  "$rustup" default "$spec" >/dev/null 2>&1
  qg_log "rust $(cat "$marker") already installed (spec '$spec')"
  exit 0
fi

qg_workdir rust
if [[ ! -x "$rustup" ]]; then
  triple="$(qg_arch_for triple)"
  qg_log "downloading rustup-init for $triple"
  qg_fetch "$RUSTUP_DIST/$triple/rustup-init" "$QG_WORK_DIR/rustup-init"
  qg_fetch "$RUSTUP_DIST/$triple/rustup-init.sha256" "$QG_WORK_DIR/rustup-init.sha256"
  qg_verify_checksum sha256 "$QG_WORK_DIR/rustup-init" "$(qg_first_token "$QG_WORK_DIR/rustup-init.sha256")" \
    || qg_die "rustup-init download failed verification"
  chmod +x "$QG_WORK_DIR/rustup-init"
  "$QG_WORK_DIR/rustup-init" -y --no-modify-path --profile minimal --default-toolchain none
fi

qg_log "installing Rust toolchain '$spec' (minimal profile + clippy, rustfmt)"
"$rustup" toolchain install "$spec" --profile minimal --component clippy --component rustfmt
"$rustup" default "$spec"
"$CARGO_HOME/bin/rustc" --version | awk '{ print $2 }' >"$marker"
qg_log "rust $(cat "$marker") installed ($rust_dir)"
