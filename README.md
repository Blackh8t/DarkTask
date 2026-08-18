# Remote Platform v0.4

A deliberately small Windows-first managed remote-control platform with a native Rust Linux server.

## What v0.3 does

v0.3 is the first **end-to-end remote-control prototype** in this repository:

- persistent device enrollment
- Windows agent presence/heartbeat
- Linux native management/signalling daemon
- SQLite device/session persistence
- controller device listing
- authenticated session creation
- per-session random bearer token
- live Windows desktop capture
- compressed binary desktop frames
- controller viewer window
- remote mouse movement/buttons/wheel
- remote keyboard events for common Windows keys
- systemd service deployment
- Caddy TLS/WSS reverse-proxy template
- coturn template retained for the WebRTC transport upgrade
- GitHub Actions builds for Linux server + Windows binaries

## Important transport note

v0.3 intentionally uses a **binary WebSocket relay through `remote-server`** for the first working remote desktop. This makes the complete capture/view/input path testable before adding WebRTC complexity.

The production transport target remains:

1. direct WebRTC/ICE P2P
2. STUN hole punching
3. TURN/UDP relay fallback
4. TURN/TLS 443 fallback

The capture/input/session layers are separated from transport so the relay can be replaced without redesigning enrollment or control logic.

## Current frame path

```text
Windows agent
  GDI desktop capture (BGRA8)
       |
       v
  zstd level 1
       |
       v
 WSS binary frame
       |
       v
 Linux remote-server
 authenticated session relay
       |
       v
 remote-controller
       |
       v
 minifb viewer
```

Control path:

```text
controller mouse/keyboard
       |
       v
ControlMessage JSON
       |
       v
server session relay
       |
       v
Windows agent
       |
       +-- SetCursorPos
       +-- mouse_event
       `-- keybd_event
```

For v0.4 the GDI + zstd frame stage should be replaced by DXGI Desktop Duplication + hardware H.264, and the session relay should become WebRTC signaling only.

## Workspace

```text
apps/
  agent/            Windows endpoint agent
  android_agent/    Android endpoint APK (MediaProjection, no audio)
  controller/       technician CLI + remote viewer
  server/           Linux management/signalling/session relay daemon
  mobile_controller Flutter technician scaffold (Android/iOS viewer)
crates/
  protocol/    shared wire types and frame format
infra/
  systemd/
  caddy/
  coturn/
.github/workflows/build.yml
```

## 1. Build the server

On Linux with Rust installed:

```bash
cargo build --release -p remote-server
```

For production, prefer the GitHub Actions artifact and copy only the compiled binary to the VPS.

## 2. Install the Linux daemon

```bash
sudo ./infra/systemd/install-server.sh ./target/release/remote-server
sudo nano /etc/remote-platform/server.env
```

Generate independent secrets:

```bash
openssl rand -hex 32
openssl rand -hex 32
```

Example `/etc/remote-platform/server.env`:

```ini
REMOTE_BIND=127.0.0.1:8787
REMOTE_DB=/var/lib/remote-platform/remote.db
REMOTE_ADMIN_TOKEN=REPLACE_WITH_ADMIN_SECRET
REMOTE_ENROLL_TOKEN=REPLACE_WITH_ENROLLMENT_SECRET
RUST_LOG=remote_server=info,tower_http=info
```

Then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now remote-platform
sudo systemctl status remote-platform
journalctl -u remote-platform -f
```

Health check:

```bash
curl http://127.0.0.1:8787/health
```

## 3. TLS/WSS

Run the Rust daemon on localhost and expose it through Caddy/Nginx on HTTPS/WSS. See:

```text
infra/caddy/Caddyfile.example
```

Do not expose the unencrypted session websocket to the public Internet.

## 4. Build Windows binaries

On Windows:

```powershell
cargo build --release -p remote-agent -p remote-controller
```

Artifacts:

```text
target\release\remote-agent.exe
target\release\remote-controller.exe
```

## 5. Enroll/run an agent

```powershell
.\remote-agent.exe `
  --server https://remote.example.com `
  --enroll YOUR_ENROLLMENT_SECRET
```

Its device identity is stored under:

```text
%PROGRAMDATA%\RemotePlatform\identity.json
```

For this prototype, run the agent in the interactive Windows session so it can capture that desktop and inject input.

## 6. List devices

```powershell
$env:REMOTE_ADMIN_TOKEN='YOUR_ADMIN_SECRET'
.\remote-controller.exe --server https://remote.example.com devices
```

## 7. Connect

```powershell
.\remote-controller.exe `
  --server https://remote.example.com `
  connect DEVICE-UUID
```

A viewer window opens after the first frame arrives.

- mouse movement/buttons are forwarded
- wheel is forwarded
- common keyboard keys are forwarded
- `Esc` closes the local viewer

## Security properties in v0.3

- enrollment secret and admin secret are separate
- enrolled device tokens are persisted as SHA-256 hashes server-side
- each remote session gets a random 256-bit-style token derived from two UUIDv4 values
- the session token is only returned to the authorized controller and delivered to the already-authenticated agent
- session websocket peers must present that token
- server binds localhost by default
- systemd service uses `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome=true`, and a dedicated service account
- TLS/WSS is expected at the reverse proxy

### Not yet production-hardening complete

v0.3 is a functional engineering prototype, not yet a production unattended-access release. Before customer deployment add:

- device public-key identity rather than bearer-only device authentication
- controller user accounts/OIDC and RBAC
- explicit device authorization policy
- session token expiry and single-use semantics
- endpoint consent/policy indicators where required
- Windows service + per-session user helper architecture
- secure-desktop/UAC handling
- signed binaries and signed updater
- rate limiting and abuse controls
- WebRTC E2E media/data path so normal desktop data bypasses the management server
- session disconnect/timeout cleanup
- multi-monitor enumeration
- clipboard

## v0.4 performance target

```text
Capture:     DXGI Desktop Duplication
Encode:      hardware H.264 (Media Foundation / vendor acceleration)
Transport:   WebRTC
Direct:      ICE/STUN
Fallback:    TURN UDP -> TURN TLS 443
Input:       reliable/fast DataChannels
Server:      signaling/auth only for P2P sessions
```

This is the point where bandwidth and latency become comparable to a purpose-built RustDesk-style product rather than a proof-of-control relay.


## v0.4 mobile controller

A shared Flutter controller scaffold now lives in `apps/mobile_controller`. It targets Android and iOS and is designed to use the same device registry, technician certificate identity, WebRTC signalling, Wake-on-LAN actions and User Screen/Admin Workspace choices as the desktop controller. See `docs/MOBILE.md`.
