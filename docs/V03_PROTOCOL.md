# v0.3 Session Protocol

## Session establishment

1. Controller authenticates to `POST /api/v1/devices/{device_id}/session` using the admin bearer token.
2. Server creates `session_id` and random `session_token`.
3. Server sends `StartSession` to the authenticated device control websocket.
4. Agent connects to `/ws/session/{session_id}?role=agent&token=...`.
5. Controller connects to `/ws/session/{session_id}?role=controller&token=...`.
6. Server relays messages only between those two session peers.

## Desktop frame

Binary websocket message:

```text
0       4      RPF1
4       4      width, little-endian u32
8       4      height, little-endian u32
12      1      pixel format: 1 = BGRA8, 2 = JPEG, 3 = H264
13      1      compression: 1 = zstd, 2 = jpeg, 3 = h264
14      1      jpeg quality, or h264 keyframe (1 = IDR+SPS/PPS, 0 = P)
15      1      reserved
16      ...    payload
```

Windows agents send JPEG. Android agents send Annex-B H.264 (no audio).

## Control messages

Text websocket messages containing tagged JSON:

- mouse_move
- mouse_button
- mouse_wheel
- key
- set_quality
- ping

Coordinates are normalized `0.0..1.0`, making control independent of the controller window size.
