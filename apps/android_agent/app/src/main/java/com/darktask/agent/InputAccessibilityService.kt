package com.darktask.agent

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.graphics.Path
import android.os.SystemClock
import android.view.accessibility.AccessibilityEvent
import java.util.concurrent.atomic.AtomicLong

class InputAccessibilityService : AccessibilityService() {
    override fun onServiceConnected() {
        instance = this
        lastInputElapsed.set(SystemClock.elapsedRealtime())
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        lastInputElapsed.set(SystemClock.elapsedRealtime())
    }

    override fun onInterrupt() {}

    override fun onDestroy() {
        if (instance === this) instance = null
        super.onDestroy()
    }

    fun tap(nx: Float, ny: Float) {
        val (x, y) = point(nx, ny)
        val path = Path().apply { moveTo(x, y) }
        dispatch(GestureDescription.StrokeDescription(path, 0, 40))
    }

    fun swipe(x1: Float, y1: Float, x2: Float, y2: Float, durationMs: Long) {
        val a = point(x1, y1)
        val b = point(x2, y2)
        val path = Path().apply {
            moveTo(a.first, a.second)
            lineTo(b.first, b.second)
        }
        dispatch(GestureDescription.StrokeDescription(path, 0, durationMs.coerceIn(40, 400)))
    }

    fun global(action: Int): Boolean = performGlobalAction(action)

    private fun point(nx: Float, ny: Float): Pair<Float, Float> {
        val dm = resources.displayMetrics
        val x = (nx.coerceIn(0f, 1f) * (dm.widthPixels - 1)).coerceAtLeast(0f)
        val y = (ny.coerceIn(0f, 1f) * (dm.heightPixels - 1)).coerceAtLeast(0f)
        return x to y
    }

    private fun dispatch(stroke: GestureDescription.StrokeDescription) {
        val gesture = GestureDescription.Builder().addStroke(stroke).build()
        dispatchGesture(gesture, null, null)
    }

    companion object {
        @Volatile
        var instance: InputAccessibilityService? = null
            private set

        private val lastInputElapsed = AtomicLong(0)

        fun available(): Boolean = instance != null

        fun idleSecs(): Long? {
            val last = lastInputElapsed.get()
            if (last == 0L) return null
            return ((SystemClock.elapsedRealtime() - last) / 1000L).coerceAtLeast(0)
        }
    }
}

object AndroidKeys {
    fun apply(vk: Int, down: Boolean): Boolean {
        if (!down) return true
        val svc = InputAccessibilityService.instance ?: return false
        return when (vk) {
            8, 27 -> svc.global(AccessibilityService.GLOBAL_ACTION_BACK)
            36, 91 -> svc.global(AccessibilityService.GLOBAL_ACTION_HOME)
            9 -> svc.global(AccessibilityService.GLOBAL_ACTION_RECENTS)
            else -> false
        }
    }
}
