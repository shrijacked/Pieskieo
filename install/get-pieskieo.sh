#!/usr/bin/env bash
set -euo pipefail

# Auto-fetch prebuilt Pieskieo binary zip from GitHub releases.
# Usage: curl -fsSL https://raw.githubusercontent.com/DarsheeeGamer/Pieskieo/main/install/get-pieskieo.sh | bash
# Optional env:
#   PIESKIEO_VERSION   tag to install (default: latest)
#   PIESKIEO_PREFIX    install prefix (default: /usr/local if writable or when service enabled, else ~/.local)
#   PIESKIEO_SERVICE   set to "1" to install/run as systemd service (Linux, requires sudo)

choose_prefix() {
  if [[ -n "${PIESKIEO_PREFIX:-}" ]]; then
    echo "[installer] prefix provided via PIESKIEO_PREFIX=${PIESKIEO_PREFIX}"
    echo "${PIESKIEO_PREFIX}"
    return
  fi
  if [[ -w /usr/local/bin || "${PIESKIEO_SERVICE:-0}" = "1" ]]; then
    echo "[installer] /usr/local/bin writable; using /usr/local"
    echo "/usr/local"
  else
    echo "[installer] /usr/local/bin not writable; falling back to ~/.local"
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
    echo "[installer] version via PIESKIEO_VERSION=${PIESKIEO_VERSION}"
    echo "${PIESKIEO_VERSION}"
  elif [[ $# -ge 1 && -n "$1" ]]; then
    echo "[installer] version via arg=$1"
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
  echo "[installer] detected platform: ${platform}"
  echo "[installer] using version: ${version}"
  url="https://github.com/DarsheeeGamer/Pieskieo/releases/download/${version}/pieskieo-${platform}-${version}.zip"
  echo "[installer] download url: ${url}"

  tmp="$(mktemp -d)"
  zip="${tmp}/pieskieo.zip"
  echo "[installer] tmp dir: ${tmp}"
  echo "[installer] zip path: ${zip}"

  if command -v curl >/dev/null 2>&1; then
    echo "[installer] using curl to download"
    curl -fsSL "$url" -o "$zip"
  else
    require_cmd wget
    echo "[installer] using wget to download"
    wget -q "$url" -O "$zip"
  fi

  prefix="$(choose_prefix)"
  bindst="${prefix}/bin"
  echo "[installer] install prefix: ${prefix}"
  echo "[installer] bin dest: ${bindst}"
  mkdir -p "$bindst"

  if command -v unzip >/dev/null 2>&1; then
    echo "[installer] extracting with unzip"
    unzip -qo "$zip" -d "$tmp"
  else
    require_cmd tar
    echo "[installer] extracting with tar"
    (cd "$tmp" && tar -xf "$zip")
  fi

  echo "[installer] installing binaries to ${bindst}"
  install -m 0755 "$tmp"/pieskieo* "$bindst"/

  echo "[installer] contents installed to $bindst:"
  ls "$bindst"/pieskieo*
  echo "[installer] done. Ensure $bindst is on your PATH."

  if [[ "$(uname -s)" == "Linux" && "${PIESKIEO_SERVICE:-0}" = "1" ]]; then
    if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
      echo "[installer] ERROR: PIESKIEO_SERVICE=1 requires sudo/root; rerun with sudo." >&2
      exit 1
    fi
    echo "[installer] configuring systemd service pieskieo"
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
