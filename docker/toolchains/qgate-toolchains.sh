#!/usr/bin/env bash
# qgate-toolchains status   show installed toolchains
# qgate-toolchains install  install what QGATE_TOOLCHAINS asks for (same as on container start)
set -euo pipefail
SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"

case "${1:-status}" in
  status | list) exec bash "$SCRIPT_DIR/list.sh" ;;
  install) exec bash "$SCRIPT_DIR/install.sh" "${@:2}" ;;
  -h | --help | help)
    sed -n '2,3s/^# //p' "$0"
    ;;
  *)
    printf 'unknown command: %s\n' "$1" >&2
    sed -n '2,3s/^# //p' "$0" >&2
    exit 2
    ;;
esac
