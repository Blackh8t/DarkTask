#!/usr/bin/env bash
set -euo pipefail

DOMAIN="rust.privateserver.im"
APP_USER="remote-platform"
APP_DIR="/opt/remote-platform"
STATE_DIR="/var/lib/remote-platform"
CONF_DIR="/etc/remote-platform"

if [[ $EUID -ne 0 ]]; then
  echo "Run as root: sudo bash infra/vps-install.sh" >&2
  exit 1
fi

apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y \
  build-essential pkg-config libssl-dev sqlite3 curl ca-certificates \
  caddy coturn ufw

if ! id -u "$APP_USER" >/dev/null 2>&1; then
  useradd --system --home "$STATE_DIR" --shell /usr/sbin/nologin "$APP_USER"
fi

install -d -o "$APP_USER" -g "$APP_USER" -m 0750 "$STATE_DIR"
install -d -o root -g "$APP_USER" -m 0750 "$CONF_DIR"
install -d -o root -g root -m 0755 "$APP_DIR"

# Rust is only required on the VPS when building from source.
if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust/Cargo not found. Install rustup for the deployment account, build the server, then rerun this script."
  echo "Official installer: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
fi

if [[ -f target/release/remote-server ]]; then
  install -o root -g root -m 0755 target/release/remote-server /usr/local/bin/remote-server
fi

if [[ ! -f "$CONF_DIR/server.env" ]]; then
  ADMIN_SECRET="$(openssl rand -hex 32)"
  ENROLL_SECRET="$(openssl rand -hex 32)"
  cat > "$CONF_DIR/server.env" <<ENV
REMOTE_BIND=127.0.0.1:8787
REMOTE_DB=$STATE_DIR/remote.db
REMOTE_ADMIN_TOKEN=$ADMIN_SECRET
REMOTE_ENROLL_TOKEN=$ENROLL_SECRET
RUST_LOG=remote_server=info,tower_http=info
ENV
  chown root:"$APP_USER" "$CONF_DIR/server.env"
  chmod 0640 "$CONF_DIR/server.env"
fi

cat > /etc/systemd/system/remote-platform.service <<'UNIT'
[Unit]
Description=Remote Platform Rust Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=remote-platform
Group=remote-platform
EnvironmentFile=/etc/remote-platform/server.env
ExecStart=/usr/local/bin/remote-server
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/remote-platform

[Install]
WantedBy=multi-user.target
UNIT

install -m 0644 infra/caddy/Caddyfile /etc/caddy/Caddyfile

TURN_SECRET="$(openssl rand -hex 32)"
sed "s/CHANGE_ME_TURN_SECRET/$TURN_SECRET/" infra/coturn/turnserver.conf > /etc/turnserver.conf
chmod 0640 /etc/turnserver.conf

# Debian/Ubuntu coturn package startup flag when present.
if [[ -f /etc/default/coturn ]]; then
  if grep -q '^#\?TURNSERVER_ENABLED=' /etc/default/coturn; then
    sed -i 's/^#\?TURNSERVER_ENABLED=.*/TURNSERVER_ENABLED=1/' /etc/default/coturn
  else
    echo 'TURNSERVER_ENABLED=1' >> /etc/default/coturn
  fi
fi

ufw allow 22/tcp || true
ufw allow 80/tcp
ufw allow 443/tcp
ufw allow 3478/tcp
ufw allow 3478/udp
ufw allow 5349/tcp
ufw allow 5349/udp
ufw allow 49160:49260/udp

systemctl daemon-reload
if [[ -x /usr/local/bin/remote-server ]]; then
  systemctl enable --now remote-platform
fi
systemctl enable --now caddy
systemctl enable --now coturn || systemctl restart coturn || true

echo
echo "Deployment profile installed for $DOMAIN"
echo "Check DNS first, then:"
echo "  curl https://$DOMAIN/health"
echo "  systemctl status remote-platform caddy coturn --no-pager"
echo "  journalctl -u remote-platform -f"
echo
echo "Server secrets: $CONF_DIR/server.env"
echo "TURN secret: /etc/turnserver.conf"
