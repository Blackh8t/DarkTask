# DarkTask Android endpoint

Enrolled Android device agent. Speaks the same control-plane protocol as the Windows agent. Android video is hardware H.264 (no audio); Windows remains JPEG. The admin portal and `remote-controller` decode both.

Encoder profile (no audio, bandwidth over fidelity), taken from [scrcpy-remote-android](https://github.com/nustato/scrcpy-remote-android) + DarkTask defaults:

| Setting | Value |
|---|---|
| Audio | off |
| Max size | 1280 (longest edge, multiple of 16) |
| Video | H.264 Baseline, 1 Mbps |
| FPS | 12 (cap 15) |

`scrcpy-server` itself cannot run inside a normal APK (it requires the ADB/shell UID for SurfaceControl + INJECT_EVENTS). This app uses the platform-legal equivalents:

- **Capture:** user-approved MediaProjection (no microphone), hardware H.264 @ 1 Mbps
- **Input:** user-approved AccessibilityService gestures (Back/Home/Recents + tap/swipe)

## Build

Requires JDK 17+ (`JAVA_HOME`) and Android SDK (`ANDROID_HOME` or `local.properties`).

```powershell
cd apps\android_agent
$env:JAVA_HOME = "C:\Program Files\Eclipse Adoptium\jdk-21.0.7.6-hotspot"
$env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\sdk"
.\gradlew.bat assembleRelease
```

APK:

```
app\build\outputs\apk\release\app-release-unsigned.apk
```

Debug (sideload):

```
.\gradlew.bat assembleDebug
adb install -r app\build\outputs\apk\debug\app-debug.apk
```

## Enroll

1. Install the APK.
2. Paste the server URL and enrollment token from the admin portal.
3. Tap **Enroll and start**.
4. Allow screen capture.
5. Enable **DarkTask** under Accessibility (for remote taps).

After reboot, the agent comes back online for presence. Screen capture must be granted again before a session can start — Android does not let MediaProjection survive reboot.

## Security

- Device token is stored in app-private SharedPreferences.
- Enrollment token is only used for `/api/v1/enroll`.
- Accessibility is configured with `canRetrieveWindowContent=false`; it injects gestures only.
- No audio is captured or transmitted.
