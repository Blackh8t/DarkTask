# rust.privateserver.im VPS profile

## DNS
Point `rust.privateserver.im` A/AAAA at the VPS. TCP 80/443 must reach Caddy for public TLS issuance/renewal.

## Build server from source

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
cargo build --release -p remote-server
```

## Install

```bash
sudo bash infra/vps-install.sh
```

## Validate

```bash
curl https://rust.privateserver.im/health
sudo systemctl status remote-platform caddy coturn --no-pager
sudo journalctl -u remote-platform -f
```

## Ports
- 80/tcp: ACME redirect/challenge through Caddy
- 443/tcp: HTTPS/WSS control plane
- 3478/udp,tcp: TURN/STUN
- 5349/tcp,udp: TURN TLS listener (certificate still needs to be configured)
- 49160-49260/udp: TURN relay allocations

## Important
Caddy and coturn cannot both bind TCP/443 on the same public IP. For maximum restrictive-firewall coverage, add a second relay IP/host later for TURN/TLS :443.
