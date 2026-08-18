# Deploy DarkTask Linux server to VPS (requires SSH key on root@62.72.31.30).
param(
    [string]$HostAlias = "darktask-vps",
    [string]$Binary = "dist/remote-server-linux-x86_64/remote-server"
)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$bin = Join-Path $root $Binary
if (-not (Test-Path $bin)) {
    throw "Server binary not found: $bin`nRun: gh run download -D dist (after CI build)"
}

Write-Host "Uploading $bin -> ${HostAlias}:/usr/local/bin/remote-server"
scp $bin "${HostAlias}:/usr/local/bin/remote-server.new"
ssh $HostAlias @'
set -euo pipefail
install -o root -g root -m 0755 /usr/local/bin/remote-server.new /usr/local/bin/remote-server
rm -f /usr/local/bin/remote-server.new
if systemctl is-active --quiet remote-platform 2>/dev/null; then
  systemctl restart remote-platform
  systemctl --no-pager --full status remote-platform | head -n 12
elif pgrep -x remote-server >/dev/null; then
  pkill -x remote-server || true
  sleep 1
  nohup /usr/local/bin/remote-server >/var/log/remote-platform.log 2>&1 &
  echo "Restarted standalone remote-server"
else
  echo "No service found — start with: systemctl enable --now remote-platform"
fi
curl -fsS http://127.0.0.1:8789/health || curl -fsS http://127.0.0.1:8787/health || true
'@

Write-Host "Done. Verify: curl http://62.72.31.30:8789/health"
