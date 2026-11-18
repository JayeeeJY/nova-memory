#!/bin/bash
# Build a fresh candidate. Never kill an application or an existing runtime service.
# Run this entrypoint directly. The managed .codex environment directory is read-only;
# no Codex Run-button configuration is created or modified by this script.
set -euo pipefail

repo="$(cd -- "$(dirname -- "$0")/.." && pwd -P)"
runtime=""
verify=0
no_build=0
app_args=()

usage() {
  printf '%s\n' 'Usage: script/build_and_run.sh [--verify] [--profile-dir ABS] [--port N] [--runtime-bundle ABS] [--no-build]'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --verify) verify=1; shift ;;
    --no-build) no_build=1; shift ;;
    --profile-dir)
      [[ $# -ge 2 && "$2" == /* ]] || { usage >&2; exit 2; }
      app_args+=(--profile-dir "$2"); shift 2 ;;
    --port)
      [[ $# -ge 2 && "$2" =~ ^[0-9]+$ ]] || { usage >&2; exit 2; }
      [[ ${#2} -le 5 ]] || { usage >&2; exit 2; }
      port=$((10#$2))
      [[ "$port" -ge 1024 && "$port" -le 65535 ]] || { usage >&2; exit 2; }
      app_args+=(--port "$port"); shift 2 ;;
    --runtime-bundle)
      [[ $# -ge 2 && "$2" == /* ]] || { usage >&2; exit 2; }
      runtime="$2"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

[[ "$(/usr/bin/uname -s)" == Darwin ]] || { printf '%s\n' 'The desktop app requires macOS.' >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { printf '%s\n' 'This development packaging script requires Python 3; the packaged Nova Memory runtime does not.' >&2; exit 1; }
mkdir -p -- "$repo/.artifacts"
candidate="$(mktemp -d "$repo/.artifacts/desktop-candidate.XXXXXXXX")"
app="$candidate/Nova Memory.app"
if [[ -z "$runtime" ]]; then
  runtime="$candidate/Local Runtime"
  runtime_args=(--output "$runtime")
  if [[ "$no_build" -eq 1 ]]; then runtime_args+=(--no-build); fi
  python3 "$repo/scripts/package-local.py" "${runtime_args[@]}"
fi
mkdir -p -- "$candidate/unsigned"
unsigned_app="$candidate/unsigned/Nova Memory.app"
package_args=(--output "$unsigned_app" --runtime-bundle "$runtime")
if [[ "$no_build" -eq 1 ]]; then package_args+=(--no-build); fi
python3 "$repo/scripts/package-desktop.py" "${package_args[@]}"
python3 "$repo/scripts/sign-desktop-local.py" --source "$unsigned_app" --output "$app"

# LaunchServices needs a real APPL bundle for a SwiftUI/AppKit GUI.
if [[ ${#app_args[@]} -gt 0 ]]; then
  /usr/bin/open -n "$app" --args "${app_args[@]}"
else
  /usr/bin/open -n "$app"
fi
# --verify checks the sealed app and sidecar; it does not claim a visible window.
# LaunchServices accepts the launch request above; no global process scan is used.
if [[ "$verify" -eq 1 ]]; then
  python3 "$repo/scripts/sign-desktop-local.py" --verify "$app"
fi
