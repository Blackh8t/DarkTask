package com.darktask.agent

import android.content.res.Resources
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.projection.MediaProjection
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.view.Surface
import okhttp3.WebSocket
import okio.ByteString.Companion.toByteString
import kotlin.math.max

class CaptureSession(
    private val projection: MediaProjection,
    private val socket: WebSocket,
) {
    private val thread = HandlerThread("darktask-h264").apply { start() }
    private val handler = Handler(thread.looper)
    @Volatile private var running = true
    @Volatile private var bitrate = StreamConfig.VIDEO_BITRATE
    private var width = 0
    private var height = 0
    private var codec: MediaCodec? = null
    private var inputSurface: Surface? = null
    private var display: VirtualDisplay? = null
    private var csd: ByteArray? = null

    fun start() {
        handler.post { openEncoder() }
    }

    fun setQuality(jpeg: Int, fps: Int) {
        bitrate = StreamConfig.bitrateForQuality(jpeg)
        val params = Bundle()
        params.putInt(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, bitrate)
        try {
            codec?.setParameters(params)
        } catch (_: Exception) {
        }
        val _fps = fps.coerceIn(StreamConfig.MIN_FPS, StreamConfig.MAX_FPS)
        Log.i(TAG, "quality jpeg=$jpeg -> ${bitrate}bps fps hint=$_fps")
    }

    fun stop() {
        running = false
        handler.post {
            try {
                display?.release()
            } catch (_: Exception) {
            }
            display = null
            try {
                codec?.stop()
            } catch (_: Exception) {
            }
            try {
                codec?.release()
            } catch (_: Exception) {
            }
            codec = null
            try {
                inputSurface?.release()
            } catch (_: Exception) {
            }
            inputSurface = null
            thread.quitSafely()
        }
    }

    private fun openEncoder() {
        if (!running) return
        val metrics = Resources.getSystem().displayMetrics
        val (w, h) = scaled(metrics.widthPixels, metrics.heightPixels, StreamConfig.MAX_SIZE)
        width = w
        height = h
        val fps = StreamConfig.DEFAULT_FPS

        val encoder = try {
            configure(videoFormat(w, h, fps, profile = true))
        } catch (e: Exception) {
            Log.w(TAG, "baseline profile rejected, retrying: ${e.message}")
            configure(videoFormat(w, h, fps, profile = false))
        }
        codec = encoder
        val surface = encoder.createInputSurface()
        inputSurface = surface
        encoder.start()

        display = projection.createVirtualDisplay(
            "darktask",
            w,
            h,
            metrics.densityDpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            surface,
            null,
            handler,
        )
        requestKey()
        Log.i(TAG, "capture ${w}x$h h264 ${bitrate}bps @ ${fps}fps (source ${metrics.widthPixels}x${metrics.heightPixels})")
    }

    private fun configure(format: MediaFormat): MediaCodec {
        val encoder = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        try {
        encoder.setCallback(object : MediaCodec.Callback() {
            override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {}

            override fun onOutputBufferAvailable(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
                if (!running) {
                    try {
                        codec.releaseOutputBuffer(index, false)
                    } catch (_: Exception) {
                    }
                    return
                }
                try {
                    val buf = codec.getOutputBuffer(index)
                    if (buf != null && info.size > 0) {
                        val bytes = ByteArray(info.size)
                        buf.position(info.offset)
                        buf.limit(info.offset + info.size)
                        buf.get(bytes)
                        val config = info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0
                        val key = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
                        if (config) {
                            csd = bytes
                        } else if (socket.queueSize() < 512 * 1024) {
                            val payload = if (key) prefixCsd(bytes) else bytes
                            socket.send(
                                Protocol.rpf1H264(width, height, key, payload).toByteString(),
                            )
                        }
                    }
                } finally {
                    try {
                        codec.releaseOutputBuffer(index, false)
                    } catch (_: Exception) {
                    }
                }
            }

            override fun onError(codec: MediaCodec, e: MediaCodec.CodecException) {
                Log.w(TAG, "encoder error: ${e.message}")
            }

            override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {
                val sps = copyCsd(format, "csd-0")
                val pps = copyCsd(format, "csd-1")
                if (sps != null) {
                    csd = if (pps != null) sps + pps else sps
                }
            }
        }, handler)
        encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            return encoder
        } catch (e: Exception) {
            encoder.release()
            throw e
        }
    }

    private fun videoFormat(w: Int, h: Int, fps: Int, profile: Boolean): MediaFormat {
        val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, w, h)
        format.setInteger(MediaFormat.KEY_BIT_RATE, bitrate)
        format.setInteger(MediaFormat.KEY_FRAME_RATE, fps)
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
        format.setInteger(MediaFormat.KEY_PRIORITY, 0)
        if (Build.VERSION.SDK_INT >= 29) {
            format.setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
        }
        if (Build.VERSION.SDK_INT >= 30) {
            format.setInteger(MediaFormat.KEY_LATENCY, 1)
        }
        if (profile) {
            format.setInteger(
                MediaFormat.KEY_PROFILE,
                MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline,
            )
        }
        return format
    }

    private fun requestKey() {
        val params = Bundle()
        params.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)
        try {
            codec?.setParameters(params)
        } catch (_: Exception) {
        }
    }

    private fun prefixCsd(frame: ByteArray): ByteArray {
        val head = csd ?: return frame
        if (hasSps(frame)) return frame
        val out = ByteArray(head.size + frame.size)
        System.arraycopy(head, 0, out, 0, head.size)
        System.arraycopy(frame, 0, out, head.size, frame.size)
        return out
    }

    companion object {
        private const val TAG = "DarkTaskCapture"

        fun scaled(width: Int, height: Int, maxSize: Int): Pair<Int, Int> {
            val longest = max(width, height)
            val scale = if (longest > maxSize) maxSize.toFloat() / longest else 1f
            val w = ((width * scale).toInt() and 15.inv()).coerceAtLeast(16)
            val h = ((height * scale).toInt() and 15.inv()).coerceAtLeast(16)
            return w to h
        }

        private fun copyCsd(format: MediaFormat, key: String): ByteArray? {
            val buf = format.getByteBuffer(key) ?: return null
            val dup = buf.duplicate()
            val bytes = ByteArray(dup.remaining())
            dup.get(bytes)
            return bytes
        }

        private fun hasSps(buf: ByteArray): Boolean {
            var i = 0
            while (i + 4 < buf.size) {
                val sc = when {
                    buf[i] == 0.toByte() && buf[i + 1] == 0.toByte() && buf[i + 2] == 0.toByte() && buf[i + 3] == 1.toByte() -> 4
                    buf[i] == 0.toByte() && buf[i + 1] == 0.toByte() && buf[i + 2] == 1.toByte() -> 3
                    else -> 0
                }
                if (sc == 0) {
                    i++
                    continue
                }
                if (i + sc < buf.size && (buf[i + sc].toInt() and 0x1F) == 7) return true
                i += sc + 1
            }
            return false
        }
    }
}
