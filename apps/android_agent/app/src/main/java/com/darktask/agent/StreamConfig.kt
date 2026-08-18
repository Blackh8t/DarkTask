package com.darktask.agent

/**
 * Encoder profile aligned with DarkTask Windows agent and scrcpy's no-audio
 * lightweight defaults from https://github.com/nustato/scrcpy-remote-android
 *
 * scrcpy-server cannot run inside a normal APK (it needs the ADB/shell UID).
 * This agent uses the same bitrate/size/fps intent via MediaProjection + JPEG.
 */
object StreamConfig {
    const val MAX_SIZE = 1280
    const val DEFAULT_FPS = 12
    const val MAX_FPS = 15
    const val MIN_FPS = 8
    const val JPEG_QUALITY = 40
    const val HEARTBEAT_SECS = 60L
    const val RECONNECT_SECS = 5L
    const val VERSION = BuildConfig.VERSION_NAME
}
