#!/usr/bin/env bash
set -euo pipefail

# Auto-fetch prebuilt Pieskieo binary zip from GitHub releases.
# Usage: curl -fsSL https://raw.githubusercontent.com/DarsheeeGamer/Pieskieo/main/install/get-pieskieo.sh | bash
# Optional env:
#   PIESKIEO_VERSION   tag to install (default: latest)
#   PIESKIEO_PREFIX    install prefix (default: /usr/local)

log() { printf '%s\n' "$*" >&2; }

choose_prefix() {
  if [[ -n "${PIESKIEO_PREFIX:-}" ]]; then
    log "[installer] prefix provided via PIESKIEO_PREFIX=${PIESKIEO_PREFIX}"
    printf '%s\n' "${PIESKIEO_PREFIX}"
    return
  fi
  log "[installer] defaulting prefix to /usr/local (service install)"
  printf '%s\n' "/usr/local"
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
    log "[installer] version via PIESKIEO_VERSION=${PIESKIEO_VERSION}"
    echo "${PIESKIEO_VERSION}"
  elif [[ $# -ge 1 && -n "$1" ]]; then
    log "[installer] version via arg=$1"
    echo "$1"
  else
    if command -v curl >/dev/null 2>&1; then
      # Follow GitHub redirect header to get latest tag.
      local tag
      tag=$(curl -fsSI https://github.com/DarsheeeGamer/Pieskieo/releases/latest \
        | sed -n 's@^[Ll]ocation: .*/tag/\\(v[^/]*\\).*@\\1@p' | tail -n1)
      if [[ -n "$tag" ]]; then
        echo "$tag"
        return
      fi
    fi
    echo "v0.1.2"
  fi
}

main() {
  local platform version prefix tmp zip url bindst
  platform="$(detect_platform)"
  version="$(fetch_version "$@")"
  log "[installer] detected platform: ${platform}"
  log "[installer] using version: ${version}"
  url="https://github.com/DarsheeeGamer/Pieskieo/releases/download/${version}/pieskieo-${platform}-${version}.zip"
  log "[installer] download url: ${url}"

  tmp="$(mktemp -d)"
  zip="${tmp}/pieskieo.zip"
  log "[installer] tmp dir: ${tmp}"
  log "[installer] zip path: ${zip}"

  if command -v curl >/dev/null 2>&1; then
    log "[installer] using curl to download"
    curl -fsSL "$url" -o "$zip"
  else
    require_cmd wget
    log "[installer] using wget to download"
    wget -q "$url" -O "$zip"
  fi

  prefix="$(choose_prefix)"
  bindst="${prefix}/bin"
  log "[installer] install prefix: ${prefix}"
  log "[installer] bin dest: ${bindst}"
  mkdir -p "$bindst"

  if command -v unzip >/dev/null 2>&1; then
    log "[installer] extracting with unzip"
    unzip -qo "$zip" -d "$tmp"
  else
    require_cmd tar
    log "[installer] extracting with tar"
    (cd "$tmp" && tar -xf "$zip")
  fi

  log "[installer] installing binaries to ${bindst}"
  install -m 0755 "$tmp"/pieskieo* "$bindst"/

  log "[installer] contents installed to $bindst:"
  ls "$bindst"/pieskieo*
  log "[installer] done. Ensure $bindst is on your PATH."

  if [[ "$(uname -s)" == "Linux" ]]; then
    if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
      log "[installer] ERROR: service install requires sudo/root; rerun with sudo."
      exit 1
    fi
    log "[installer] configuring systemd service pieskieo"
    mkdir -p /var/lib/pieskieo
    cat >/etc/systemd/system/pieskieo.service <<'EOF'
[Unit]
Description=Pieskieo Database Service
After=network.target

[Service]
Type=simple
Environment=PIESKIEO_DATA=/var/lib/pieskieo
Environment=PIESKIEO_LISTEN=0.0.0.0:8000
ExecStart=/usr/local/bin/pieskieo-server --serve
Restart=on-failure
User=root
Group=root

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable pieskieo.service
    systemctl restart pieskieo.service
    echo "[installer] service pieskieo enabled and started"
  fi
}

main "$@"
