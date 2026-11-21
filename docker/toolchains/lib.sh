#!/usr/bin/env bash
# Shared helpers for the toolchain installers. Sourced, never executed directly.
#
# Layout under $QGATE_TOOLCHAINS_DIR (default /toolchains):
#   <tool>/<spec>/        one directory per requested version spec (e.g. go/1.24, java/21)
#   <tool>/<spec>/.qgate-version   the exact version that was resolved and installed
#   <tool>/current        symlink to the active <spec> directory
#   .tmp/                 scratch space; same filesystem so the final `mv` is atomic
#   .lock                 flock target so two containers never install concurrently

QGATE_TOOLCHAINS_DIR="${QGATE_TOOLCHAINS_DIR:-/toolchains}"
QG_CURL_CONNECT_TIMEOUT=20
QG_CURL_MAX_TIME=1800
QG_CURL_RETRIES=3
QG_VERSION_FILE=".qgate-version"

qg_log() { printf '[toolchains] %s\n' "$*" >&2; }
qg_warn() { printf '[toolchains] WARNING: %s\n' "$*" >&2; }
qg_die() {
  printf '[toolchains] ERROR: %s\n' "$*" >&2
  exit 1
}

# Normalised CPU architecture: amd64 or arm64.
qg_arch() {
  local machine="${QG_UNAME_M:-$(uname -m)}"
  case "$machine" in
    x86_64 | amd64) echo amd64 ;;
    aarch64 | arm64) echo arm64 ;;
    *) qg_die "unsupported CPU architecture '$machine' (only amd64 and arm64 are supported)" ;;
  esac
}

# Architecture name in a vendor's vocabulary: qg_arch_for <go|adoptium|node|triple>.
qg_arch_for() {
  local arch
  arch="$(qg_arch)"
  case "$1:$arch" in
    go:amd64) echo amd64 ;;
    go:arm64) echo arm64 ;;
    adoptium:amd64 | node:amd64) echo x64 ;;
    adoptium:arm64) echo aarch64 ;;
    node:arm64) echo arm64 ;;
    triple:amd64) echo x86_64-unknown-linux-gnu ;;
    triple:arm64) echo aarch64-unknown-linux-gnu ;;
    *) qg_die "unknown architecture vocabulary '$1'" ;;
  esac
}

# Version specs end up in URLs and paths: allow only a conservative character set.
qg_require_spec() {
  local name="$1" value="$2"
  if [[ ! "$value" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ ]]; then
    qg_die "$name='$value' is not a valid version (letters, digits, '.', '_' and '-' only)"
  fi
}

# HTTPS-only download with retries; redirects must stay on HTTPS too.
qg_fetch() {
  local url="$1" dest="$2"
  [[ "$url" == https://* ]] || qg_die "refusing non-HTTPS URL: $url"
  curl --proto '=https' --proto-redir '=https' --tlsv1.2 --fail --silent --show-error --location \
    --retry "$QG_CURL_RETRIES" --retry-delay 2 --connect-timeout "$QG_CURL_CONNECT_TIMEOUT" \
    --max-time "$QG_CURL_MAX_TIME" --output "$dest" "$url"
}

# Prints a URL's body (small metadata documents only).
qg_fetch_text() {
  local url="$1" tmp
  tmp="$(mktemp "${QG_WORK_DIR:-/tmp}/fetch.XXXXXX")"
  if ! qg_fetch "$url" "$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  cat "$tmp"
  rm -f "$tmp"
}

# qg_verify_checksum <sha256|sha512> <file> <expected-hex>; fails loudly on mismatch or malformed input.
qg_verify_checksum() {
  local algo="$1" file="$2" expected="$3" length actual
  case "$algo" in
    sha256) length=64 ;;
    sha512) length=128 ;;
    *) qg_die "unsupported checksum algorithm '$algo'" ;;
  esac
  expected="$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')"
  if [[ ! "$expected" =~ ^[0-9a-f]{$length}$ ]]; then
    qg_warn "no valid $algo checksum published for $(basename "$file") (got '${expected:0:20}...')"
    return 1
  fi
  actual="$("${algo}sum" "$file" | cut -d' ' -f1)"
  if [[ "$actual" != "$expected" ]]; then
    qg_warn "$algo MISMATCH for $(basename "$file"): expected $expected, got $actual"
    return 1
  fi
  return 0
}

# First whitespace-separated token of a checksum file (`<hex>  <name>` or bare `<hex>`).
qg_first_token() { awk 'NR==1 { print $1 }' "$1"; }

# Checksum of <filename> from a `<hex>  <name>` sums file (a leading '*' on the name marks binary mode); empty if absent.
qg_checksum_for() {
  local file="$1" name="$2"
  awk -v n="$name" '{ f = $2; sub(/^\*/, "", f); if (f == n) { print $1; exit } }' "$file"
}

# Scratch directory for one installer run; removed on exit.
qg_workdir() {
  mkdir -p "$QGATE_TOOLCHAINS_DIR/.tmp"
  QG_WORK_DIR="$(mktemp -d "$QGATE_TOOLCHAINS_DIR/.tmp/$1.XXXXXX")"
  export QG_WORK_DIR
  trap 'rm -rf "$QG_WORK_DIR"' EXIT
}

qg_tool_dir() { echo "$QGATE_TOOLCHAINS_DIR/$1"; }

# True when <tool>/<spec> is fully installed (the version file is written last).
qg_is_installed() { [[ -f "$(qg_tool_dir "$1")/$2/$QG_VERSION_FILE" ]]; }

qg_installed_version() { cat "$(qg_tool_dir "$1")/$2/$QG_VERSION_FILE" 2>/dev/null || true; }

# Atomically repoint <tool>/current at <spec>.
qg_activate() {
  local tool="$1" spec="$2" dir
  dir="$(qg_tool_dir "$tool")"
  ln -sfn "$spec" "$dir/.current.new"
  mv -Tf "$dir/.current.new" "$dir/current"
}

# Moves a staged directory into <tool>/<spec>, records the version and activates it.
qg_commit_install() {
  local tool="$1" spec="$2" staged="$3" version="$4" dir
  dir="$(qg_tool_dir "$tool")"
  mkdir -p "$dir"
  printf '%s\n' "$version" >"$staged/$QG_VERSION_FILE"
  rm -rf "${dir:?}/$spec"
  mv "$staged" "$dir/$spec"
  qg_activate "$tool" "$spec"
  qg_log "$tool $version installed ($dir/$spec)"
}

# Fast path shared by installers: already installed -> just (re)activate, return 0.
qg_activate_if_installed() {
  local tool="$1" spec="$2"
  qg_is_installed "$tool" "$spec" || return 1
  qg_activate "$tool" "$spec"
  qg_log "$tool $(qg_installed_version "$tool" "$spec") already installed (spec '$spec')"
}

# Extracts a tarball into a fresh directory, dropping the archive's top-level folder.
qg_extract_tar() {
  local archive="$1" dest="$2"
  mkdir -p "$dest"
  tar -xf "$archive" -C "$dest" --strip-components=1 --no-same-owner
}
