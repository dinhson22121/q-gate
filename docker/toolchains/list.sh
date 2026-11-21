#!/usr/bin/env bash
# Prints the installed toolchains: active version first, other installed versions after it.
set -euo pipefail
# shellcheck source=docker/toolchains/lib.sh
. "$(dirname "$0")/lib.sh"

TOOLS="go java maven gradle node uv python gitleaks osv-scanner govulncheck golangci-lint"

printf 'Toolchains in %s:\n' "$QGATE_TOOLCHAINS_DIR"
found=false
for tool in $TOOLS; do
  dir="$(qg_tool_dir "$tool")"
  [[ -L "$dir/current" ]] || continue
  active="$(readlink "$dir/current")"
  others=()
  for path in "$dir"/*/; do
    spec="$(basename "$path")"
    if [[ "$spec" != "$active" && "$spec" != current && -f "$path/$QG_VERSION_FILE" ]]; then
      others+=("$(cat "$path/$QG_VERSION_FILE")")
    fi
  done
  line="$(printf '  %-7s %s' "$tool" "$(qg_installed_version "$tool" "$active")")"
  if [[ ${#others[@]} -gt 0 ]]; then
    line="$line  (also installed: ${others[*]})"
  fi
  printf '%s\n' "$line"
  found=true
done

rust_dir="$(qg_tool_dir rust)"
if [[ -x "$rust_dir/cargo/bin/rustc" ]]; then
  active="$(RUSTUP_HOME="$rust_dir/rustup" CARGO_HOME="$rust_dir/cargo" "$rust_dir/cargo/bin/rustc" --version 2>/dev/null \
    | awk '{ print $2 }' || true)"
  printf '  %-7s %s\n' rust "${active:-installed (no default toolchain)}"
  found=true
fi

if [[ "$found" == false ]]; then
  printf '  (none installed)\n'
fi
