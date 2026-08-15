# Remote Mobile (Android / iOS)

Shared Flutter controller application for the Remote Platform.

## Intended v0.4 role

- Android controller: device list, online state, Wake/Connect workflow, WebRTC viewer/input client.
- iOS controller: same controller functionality.
- Android endpoint (later): user-approved MediaProjection screen sharing and AccessibilityService-assisted remote gestures where policy and platform rules permit.
- iOS endpoint (limited): user-approved ReplayKit screen broadcast. Standard iOS does not expose arbitrary system-wide remote input injection comparable to Windows.

## Bootstrap locally

Flutter platform projects are intentionally generated using your installed Flutter SDK so they match the current toolchain:

```bash
cd apps/mobile_controller
flutter create --platforms=android,ios .
flutter pub get
flutter run
```

The repository already contains `pubspec.yaml` and `lib/main.dart`; keep those versions if `flutter create` asks about replacement.

## Authentication

The current UI accepts the development admin token only to exercise the API. Production mobile builds should enroll a technician identity and store its private key/certificate in Android Keystore or iOS Keychain/Secure Enclave where available. No endpoint password is required.
