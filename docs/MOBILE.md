# Mobile support matrix

| Capability | Android controller | iOS controller | Android endpoint | iOS endpoint |
|---|---:|---:|---:|---:|
| Device list/status | Yes | Yes | Yes | Yes |
| Wake & Connect | Yes | Yes | N/A | N/A |
| WebRTC remote viewer | Target | Target | Target | Target screen broadcast |
| Touch -> remote mouse | Target | Target | N/A | N/A |
| Remote keyboard to Windows | Target | Target | N/A | N/A |
| Full device screen sharing | N/A | N/A | User-approved MediaProjection | User-approved ReplayKit |
| Arbitrary system remote control | N/A | N/A | Constrained; AccessibilityService/user grants | No general public API |
| Certificate technician identity | Target | Target | Device identity | Device identity |

## Design rule

Mobile controller functionality is first-class. Mobile endpoint control remains explicitly constrained by each OS security model; no attempt is made to bypass platform consent or security controls.
