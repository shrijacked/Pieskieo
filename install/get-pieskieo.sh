#!/usr/bin/env bash
set -euo pipefail

# Auto-fetch prebuilt Pieskieo binary zip from GitHub releases.
# Usage: curl -fsSL https://raw.githubusercontent.com/DarsheeeGamer/Pieskieo/main/install/get-pieskieo.sh | bash
# Optional env:
#   PIESKIEO_VERSION   tag to install (e.g., v0.1.2). If unset, uses latest release.
#   PIESKIEO_PREFIX    install prefix (default: /usr/local if writable, else ~/.local)

choose_prefix() {
  if [[ -n "${PIESKIEO_PREFIX:-}" ]]; then
    echo "${PIESKIEO_PREFIX}"
    return
  fi
  if [[ -w /usr/local/bin ]]; then
    echo "/usr/local"
  else
    echo "${HOME}/.local"
  fi
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || { echo "Missing dependency: $1" >&2; exit 1; }
}

detect_platform() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux) os="linux" ;;
    Darwin) os="macos" ;;
    *) echo "Unsupported OS: $os" >&2; exit 1 ;;
  esac
  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="arm64" ;;
    *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
  esac
  echo "${os}-${arch}"
}

fetch_version() {
  if [[ -n "${PIESKIEO_VERSION:-}" ]]; then
    echo "${PIESKIEO_VERSION}"
    return
  fi
  require_cmd curl
  curl -fsSL https://api.github.com/repos/DarsheeeGamer/Pieskieo/releases/latest \
    | sed -n 's/ *"tag_name": *"\\(v[^"]*\\)".*/\\1/p' | head -n1
}

main() {
  local platform version prefix tmp zip url bindst
  platform="$(detect_platform)"
  version="$(fetch_version)"
  if [[ -z "$version" ]]; then
    echo "Could not determine latest release version." >&2
    exit 1
  fi
  url="https://github.com/DarsheeeGamer/Pieskieo/releases/download/${version}/pieskieo-${platform}-${version}.zip"
  echo "Downloading ${url}"

  tmp="$(mktemp -d)"
  zip="${tmp}/pieskieo.zip"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$zip"
  else
    require_cmd wget
    wget -q "$url" -O "$zip"
  fi

  prefix="$(choose_prefix)"
  bindst="${prefix}/bin"
  mkdir -p "$bindst"

  if command -v unzip >/dev/null 2>&1; then
    unzip -qo "$zip" -d "$tmp"
  else
    require_cmd tar
    (cd "$tmp" && tar -xf "$zip")
  fi

  install -m 0755 "$tmp"/pieskieo* "$bindst"/

  echo "Installed to $bindst:"
  ls "$bindst"/pieskieo*
  echo "Ensure $bindst is on your PATH."
}

main "$@"
