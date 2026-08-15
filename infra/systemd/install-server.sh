#!/usr/bin/env bash
set -euo pipefail
if [[ ${EUID} -ne 0 ]]; then echo "Run as root" >&2; exit 1; fi
BIN=${1:-./target/release/remote-server}
id remote-platform >/dev/null 2>&1 || useradd --system --home /var/lib/remote-platform --shell /usr/sbin/nologin remote-platform
install -d -o remote-platform -g remote-platform -m 0750 /var/lib/remote-platform
install -d -o root -g remote-platform -m 0750 /etc/remote-platform
install -m 0755 "$BIN" /usr/local/bin/remote-server
install -m 0644 infra/systemd/remote-platform.service /etc/systemd/system/remote-platform.service
if [[ ! -f /etc/remote-platform/server.env ]]; then
  install -m 0640 -o root -g remote-platform infra/systemd/server.env.example /etc/remote-platform/server.env
  echo "Created /etc/remote-platform/server.env - EDIT SECRETS BEFORE STARTING."
fi
systemctl daemon-reload
echo "Then: nano /etc/remote-platform/server.env && systemctl enable --now remote-platform"
