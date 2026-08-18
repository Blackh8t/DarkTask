package com.darktask.agent

import android.content.res.Resources
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.Image
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import okhttp3.WebSocket
import okio.ByteString.Companion.toByteString
import java.io.ByteArrayOutputStream
import java.util.concurrent.atomic.AtomicInteger
import kotlin.math.max

class CaptureSession(
    private val projection: MediaProjection,
    private val socket: WebSocket,
) : ImageReader.OnImageAvailableListener {
    private val thread = HandlerThread("darktask-capture").apply { start() }
    private val handler = Handler(thread.looper)
    private val jpegQuality = AtomicInteger(StreamConfig.JPEG_QUALITY)
    @Volatile private var running = true
    @Volatile private var lastSendNs = 0L
    @Volatile private var minFrameNs = 1_000_000_000L / StreamConfig.DEFAULT_FPS

    private var reader: ImageReader? = null
    private var display: VirtualDisplay? = null
    private val jpegOut = ByteArrayOutputStream(64 * 1024)
    private var reusable: Bitmap? = null

    fun start() {
        handler.post { openDisplay() }
    }

    fun setQuality(jpeg: Int, fps: Int) {
        jpegQuality.set(jpeg.coerceIn(20, 70))
        val clamped = fps.coerceIn(StreamConfig.MIN_FPS, StreamConfig.MAX_FPS)
        minFrameNs = 1_000_000_000L / clamped
    }

    fun stop() {
        running = false
        handler.post {
            display?.release()
            display = null
            reader?.close()
            reader = null
            reusable?.recycle()
            reusable = null
            thread.quitSafely()
        }
    }

    private fun openDisplay() {
        if (!running) return
        val metrics = Resources.getSystem().displayMetrics
        val srcW = metrics.widthPixels
        val srcH = metrics.heightPixels
        val (w, h) = scaled(srcW, srcH, StreamConfig.MAX_SIZE)
        val dpi = metrics.densityDpi

        reader?.close()
        display?.release()

        val img = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 2)
        img.setOnImageAvailableListener(this, handler)
        reader = img
        display = projection.createVirtualDisplay(
            "darktask",
            w,
            h,
            dpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            img.surface,
            null,
            handler,
        )
        Log.i(TAG, "capture ${w}x$h @ jpeg ${jpegQuality.get()} (source ${srcW}x$srcH)")
    }

    override fun onImageAvailable(reader: ImageReader) {
        val image = try {
            reader.acquireLatestImage()
        } catch (_: Exception) {
            null
        } ?: return
        try {
            if (!running) return
            val now = System.nanoTime()
            if (now - lastSendNs < minFrameNs) return
            if (socket.queueSize() > 256 * 1024) return
            val frame = encode(image) ?: return
            lastSendNs = now
            socket.send(frame.toByteString())
        } finally {
            image.close()
        }
    }

    private fun encode(image: Image): ByteArray? {
        val plane = image.planes[0]
        val buffer = plane.buffer
        val pixelStride = plane.pixelStride
        val rowStride = plane.rowStride
        val width = image.width
        val height = image.height
        var bmp = reusable
        if (bmp == null || bmp.width != width || bmp.height != height) {
            bmp?.recycle()
            bmp = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
            reusable = bmp
        }
        if (pixelStride == 4 && rowStride == width * 4) {
            buffer.rewind()
            bmp.copyPixelsFromBuffer(buffer)
        } else {
            val packed = ByteArray(width * height * 4)
            val row = ByteArray(rowStride)
            var dst = 0
            for (y in 0 until height) {
                buffer.position(y * rowStride)
                buffer.get(row, 0, rowStride)
                var src = 0
                for (x in 0 until width) {
                    packed[dst] = row[src]
                    packed[dst + 1] = row[src + 1]
                    packed[dst + 2] = row[src + 2]
                    packed[dst + 3] = row[src + 3]
                    dst += 4
                    src += pixelStride
                }
            }
            bmp.copyPixelsFromBuffer(java.nio.ByteBuffer.wrap(packed))
        }
        jpegOut.reset()
        val q = jpegQuality.get()
        if (!bmp.compress(Bitmap.CompressFormat.JPEG, q, jpegOut)) return null
        return Protocol.rpf1(width, height, q, jpegOut.toByteArray())
    }

    companion object {
        private const val TAG = "DarkTaskCapture"

        fun scaled(width: Int, height: Int, maxSize: Int): Pair<Int, Int> {
            val longest = max(width, height)
            if (longest <= maxSize) {
                return (width and 7.inv()).coerceAtLeast(8) to (height and 7.inv()).coerceAtLeast(8)
            }
            val scale = maxSize.toFloat() / longest
            val w = ((width * scale).toInt() and 7.inv()).coerceAtLeast(8)
            val h = ((height * scale).toInt() and 7.inv()).coerceAtLeast(8)
            return w to h
        }
    }
}
