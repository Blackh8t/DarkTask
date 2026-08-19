package com.darktask.agent

/**
 * Encoder profile aligned with scrcpy's no-audio lightweight defaults
 * from https://github.com/nustato/scrcpy-remote-android
 *
 * Android uses MediaCodec H.264 (hardware). Windows agent uses OpenH264 at 800px default.
 */
object StreamConfig {
    const val MAX_SIZE = 1280
    const val DEFAULT_FPS = 12
    const val MAX_FPS = 15
    const val MIN_FPS = 8
    const val VIDEO_BITRATE = 1_000_000
    const val JPEG_QUALITY = 40
    const val HEARTBEAT_SECS = 60L
    const val RECONNECT_SECS = 5L
    const val VERSION = BuildConfig.VERSION_NAME

    fun bitrateForQuality(jpeg: Int): Int =
        jpeg.coerceIn(20, 70) * 25_000
}
