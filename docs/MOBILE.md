# Mobile support matrix

| Capability | Android controller | iOS controller | Android endpoint | iOS endpoint |
|---|---:|---:|---:|---:|
| Device list/status | Yes | Yes | Yes | Yes |
| Wake & Connect | Yes | Yes | N/A | N/A |
| WebRTC remote viewer | Target | Target | Target | Target screen broadcast |
| Touch -> remote mouse | Target | Target | N/A | N/A |
| Remote keyboard to Windows | Target | Target | N/A | N/A |
| Full device screen sharing | N/A | N/A | Yes — user-approved MediaProjection, no audio, JPEG 40 @ 12fps / 1280px | User-approved ReplayKit |
| Arbitrary system remote control | N/A | N/A | Constrained; AccessibilityService gestures (tap/swipe/Back/Home) | No general public API |
| Certificate technician identity | Target | Target | Device identity | Device identity |

## Design rule

Mobile controller functionality is first-class. Mobile endpoint control remains explicitly constrained by each OS security model; no attempt is made to bypass platform consent or security controls.

The Android endpoint APK lives in `apps/android_agent`. It enrolls through the same `/api/v1/enroll` + `/ws/agent` path as Windows. Capture is user-approved MediaProjection (no audio). Remote taps use AccessibilityService.

