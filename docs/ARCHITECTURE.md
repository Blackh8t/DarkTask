# Architecture — v0.3

## Design rules

- Windows first.
- Native binaries.
- Endpoint has almost no configuration UI.
- Server is control plane; P2P is data plane whenever possible.
- TURN is fallback, not default.
- Server must survive process/host restarts without losing enrollment state.
- Enrollment and technician authorization are separate trust domains.

## Components

### remote-agent

Runs on the managed Windows endpoint. v0.3 currently handles enrollment, persistent device identity, presence, heartbeat, and session-request acceptance. The next layer adds capture/input/WebRTC.

### remote-controller

Technician-side CLI during the transport bring-up. It lists devices and requests sessions. A small native GUI replaces the CLI once the media path is functioning.

### remote-server

Native Linux Rust daemon. Responsibilities:

- device enrollment
- device credential verification
- presence
- session authorization/routing
- audit state
- signaling coordination

It is **not** intended to proxy every video frame.

### coturn

Independent TURN/STUN service. Used only when ICE cannot establish an acceptable direct path.

## Linux runtime

```text
systemd
  └── remote-server
        ├── HTTP/WSS :8787 on localhost
        └── SQLite /var/lib/remote-platform/remote.db

Caddy/nginx
  └── HTTPS/WSS :443

coturn
  ├── STUN/TURN UDP
  └── TURN TLS fallback
```

## Endpoint session path

```text
Controller             Server                  Agent
    |                     |                      |
    | session request     |                      |
    |-------------------->| StartSession         |
    |                     |--------------------->|
    |                     |                      |
    |<====== SDP / ICE signaling via server ====>|
    |                                            |
    |<========== WebRTC direct media/data ======>|
    |                                            |
    |<============ TURN only if needed =========>|
```
