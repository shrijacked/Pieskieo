  #!/usr/bin/env bash
  set -euo pipefail


  command -v curl >/dev/null 2>&1 || { log "curl required"; exit 1; }

  platform() {
    local os arch
    arch=$(uname -m)
    case "$os" in
      darwin) os=macos ;;
      *) log "unsupported OS: $os"; exit 1 ;;
    case "$arch" in
      x86_64|amd64) arch=x86_64 ;;
      arm64|aarch64) arch=arm64 ;;
      *) log "unsupported arch: $arch"; exit 1 ;;
    echo "${os}-${arch}"
  }

  latest_version() {
    local tag
    if tag=$(curl -fsSI https://github.com/DarsheeeGamer/Pieskieo/releases/latest \
        | sed -n 's/.*\/tag\/\([v0-9.]*\).*/\1/p' | tr -d '\r'); then
    fi
    echo "v2.0.0"
  }

  PREFIX=${PIESKIEO_PREFIX:-/usr/local}
  SERVICE=${PIESKIEO_SERVICE:-1}

  plat=$(platform)
  ver=${VERSION:-$(latest_version)}
  log "platform=$plat version=$ver"

  url="https://github.com/DarsheeeGamer/Pieskieo/releases/download/${ver}/pieskieo-${plat}-${ver}.zip"
  log "url=$url"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  zip="$tmp/pieskieo.zip"

  log "downloading"
  curl -fL "$url" -o "$zip"

  log "extracting"
  unzip -qo "$zip" -d "$tmp"

  bindst="$PREFIX/bin"
  log "installing binaries to $bindst"
  mkdir -p "$bindst"
  find "$tmp" -maxdepth 1 -type f -name "pieskieo*" -exec install -m 0755 {} "$bindst/" \;

  if [[ "$SERVICE" == "1" && $(uname -s | tr '[:upper:]' '[:lower:]') == linux ]]; then
    log "configuring systemd service"
    mkdir -p /var/lib/pieskieo
    cat >/etc/systemd/system/pieskieo.service <<'UNIT'
  [Unit]
  Description=Pieskieo database server
  After=network.target

  [Service]
  Type=simple
  User=root
  ExecStart=/usr/local/bin/pieskieo-server --serve --data-dir /var/lib/pieskieo --listen 0.0.0.0:8000
  Restart=on-failure
  LimitNOFILE=1048576

  [Install]
  WantedBy=multi-user.target
  UNIT
    systemctl daemon-reload
    systemctl enable --now pieskieo || true
    log "service pieskieo enabled"
  fi

  log "done. ensure $bindst is on your PATH"
