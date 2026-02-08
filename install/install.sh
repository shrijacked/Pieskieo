#!/usr/bin/env bash
set -euo pipefail

# Pieskieo installer (Linux/macOS)
# Downloads prebuilt release, installs into /usr/local/bin (or $PIESKIEO_PREFIX), optional systemd service on Linux when PIESKIEO_SERVICE=1.
# Usage: curl -fsSL https://raw.githubusercontent.com/DarsheeeGamer/Pieskieo/main/install/install.sh | bash

PREFIX=${PIESKIEO_PREFIX:-/usr/local}
SERVICE=${PIESKIEO_SERVICE:-1}
VERSION=${PIESKIEO_VERSION:-}

log(){ printf '%s\n' "$*" >&2; }

platform() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Linux) os=linux;;
    Darwin) os=macos;;
    *) log "unsupported OS $os"; exit 1;;
  esac
  case "$arch" in
    x86_64|amd64) arch=x86_64;;
    arm64|aarch64) arch=arm64;;
    *) log "unsupported arch $arch"; exit 1;;
  esac
  echo "${os}-${arch}"
}

latest_version() {
  local tag
  tag=$(curl -fsSI https://github.com/DarsheeeGamer/Pieskieo/releases/latest | sed -n 's@^[Ll]ocation: .*/tag/\(v[^/]*\).*@\1@p' | tail -n1)
  echo "${tag:-v1.0.0}"
}

main(){
  local plat ver url tmp zip bindst
  plat=$(platform)
  ver=${VERSION:-$(latest_version)}
  url="https://github.com/DarsheeeGamer/Pieskieo/releases/download/${ver}/pieskieo-${plat}-${ver}.zip"
  log "platform=${plat} version=${ver}"
  log "downloading ${url}"
  tmp=$(mktemp -d)
  zip="$tmp/pieskieo.zip"
  curl -fsSL "$url" -o "$zip"
  bindst="$PREFIX/bin"
  mkdir -p "$bindst"
  unzip -qo "$zip" -d "$tmp"
  install -m 0755 "$tmp"/pieskieo* "$bindst/"
  log "installed to $bindst"
  if [[ "$SERVICE" = "1" && "$(uname -s)" = "Linux" ]]; then
    if [[ $EUID -ne 0 ]]; then log "PIESKIEO_SERVICE=1 requires sudo"; exit 1; fi
    cat >/etc/systemd/system/pieskieo.service <<EOF
[Unit]
Description=Pieskieo Database Service
After=network.target

[Service]
Type=simple
Environment=PIESKIEO_DATA=/var/lib/pieskieo
Environment=PIESKIEO_LISTEN=0.0.0.0:8000
ExecStart=${bindst}/pieskieo-server --serve
Restart=on-failure
User=root
Group=root

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable pieskieo.service
    systemctl restart pieskieo.service
    log "systemd service pieskieo started"
  fi
  log "done. ensure $bindst on PATH"
}

main "$@"
