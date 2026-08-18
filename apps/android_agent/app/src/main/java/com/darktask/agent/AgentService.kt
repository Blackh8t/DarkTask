package com.darktask.agent

import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.app.KeyguardManager
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

class AgentService : Service() {
    private val prefs by lazy { Prefs(this) }
    private val http = OkHttpClient.Builder()
        .pingInterval(20, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.SECONDS)
        .connectTimeout(15, TimeUnit.SECONDS)
        .retryOnConnectionFailure(true)
        .build()

    private var control: WebSocket? = null
    private var sessionSocket: WebSocket? = null
    private var capture: CaptureSession? = null
    private var projection: MediaProjection? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private val running = AtomicBoolean(false)
    @Volatile private var online = false
    private var heartbeat: Runnable? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        instance = this
        startAsForeground(false)
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "darktask:agent").apply {
            setReferenceCounted(false)
            acquire()
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_PROJECTION -> {
                val code = intent.getIntExtra(EXTRA_RESULT_CODE, 0)
                val data = if (Build.VERSION.SDK_INT >= 33) {
                    intent.getParcelableExtra(EXTRA_RESULT_DATA, Intent::class.java)
                } else {
                    @Suppress("DEPRECATION")
                    intent.getParcelableExtra(EXTRA_RESULT_DATA)
                }
                if (code != 0 && data != null) {
                    attachProjection(code, data)
                }
            }
        }
        if (prefs.enrolled && running.compareAndSet(false, true)) {
            connectControl()
        }
        broadcastStatus()
        return START_STICKY
    }

    override fun onDestroy() {
        running.set(false)
        endSession()
        control?.close(1000, "stop")
        control = null
        try {
            projection?.stop()
        } catch (_: Exception) {
        }
        projection = null
        wakeLock?.let { if (it.isHeld) it.release() }
        if (instance === this) instance = null
        broadcastStatus()
        super.onDestroy()
    }

    private fun attachProjection(code: Int, data: Intent) {
        val mgr = getSystemService(MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        try {
            projection?.stop()
        } catch (_: Exception) {
        }
        startAsForeground(true)
        projection = mgr.getMediaProjection(code, data)
        projection?.registerCallback(object : MediaProjection.Callback() {
            override fun onStop() {
                endSession()
                projection = null
                broadcastStatus()
            }
        }, null)
        broadcastStatus()
    }

    private fun connectControl() {
        control?.cancel()
        control = null
        online = false
        val url = wsUrl(prefs.server, "/ws/agent")
        val req = Request.Builder().url(url).build()
        control = http.newWebSocket(req, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                val hostname = android.os.Build.MODEL
                webSocket.send(
                    Protocol.hello(
                        prefs.deviceId,
                        prefs.deviceToken,
                        hostname,
                        StreamConfig.VERSION,
                    ),
                )
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                when (val msg = Protocol.parseServer(text)) {
                    Protocol.ServerMsg.HelloAck -> {
                        online = true
                        broadcastStatus()
                        scheduleHeartbeat(webSocket)
                    }
                    Protocol.ServerMsg.Ping -> {}
                    is Protocol.ServerMsg.Start -> handleStart(webSocket, msg.session)
                    Protocol.ServerMsg.Unknown -> {}
                }
            }

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                webSocket.close(code, reason)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                online = false
                broadcastStatus()
                reconnect()
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                Log.w(TAG, "control socket failed: ${t.message}")
                online = false
                broadcastStatus()
                reconnect()
            }
        })
    }

    private fun scheduleHeartbeat(ws: WebSocket) {
        heartbeat?.let { handler.removeCallbacks(it) }
        val task = object : Runnable {
            override fun run() {
                if (!running.get() || ws !== control) return
                val km = getSystemService(KEYGUARD_SERVICE) as KeyguardManager
                val loggedIn = !km.isKeyguardLocked
                val idle = InputAccessibilityService.idleSecs()
                ws.send(Protocol.heartbeat(prefs.deviceId, System.currentTimeMillis(), loggedIn, idle))
                handler.postDelayed(this, StreamConfig.HEARTBEAT_SECS * 1000L)
            }
        }
        heartbeat = task
        handler.postDelayed(task, StreamConfig.HEARTBEAT_SECS * 1000L)
    }

    private fun reconnect() {
        if (!running.get()) return
        handler.postDelayed({
            if (running.get() && (control == null || !online)) {
                connectControl()
            }
        }, StreamConfig.RECONNECT_SECS * 1000L)
    }

    private fun handleStart(controlWs: WebSocket, session: Protocol.StartSession) {
        if (session.sessionMode == "admin_workspace") {
            controlWs.send(
                Protocol.sessionRejected(
                    session.sessionId,
                    "admin workspace is not supported on Android; use user_screen",
                ),
            )
            return
        }
        val proj = projection
        if (proj == null) {
            controlWs.send(
                Protocol.sessionRejected(
                    session.sessionId,
                    "screen capture not granted; open DarkTask and tap Grant screen capture",
                ),
            )
            return
        }
        endSession()
        val url = wsUrl(
            prefs.server,
            "/ws/session/${session.sessionId}?role=agent&token=${session.sessionToken}",
        )
        sessionSocket = http.newWebSocket(
            Request.Builder().url(url).build(),
            object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    inSession = true
                    capture = CaptureSession(proj, webSocket).also { it.start() }
                    controlWs.send(Protocol.sessionAccepted(session.sessionId))
                    startAsForeground(true, inSession = true)
                    broadcastStatus()
                }

                override fun onMessage(webSocket: WebSocket, text: String) {
                    applyControl(Protocol.parseControl(text))
                }

                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                    endSession()
                }

                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                    Log.w(TAG, "session failed: ${t.message}")
                    endSession()
                }
            },
        )
    }

    private fun applyControl(msg: Protocol.Control) {
        when (msg) {
            is Protocol.Control.Move -> {
                pointerX = msg.x
                pointerY = msg.y
            }
            is Protocol.Control.Button -> {
                val input = InputAccessibilityService.instance
                when (msg.button) {
                    "left" -> {
                        if (msg.down) {
                            downX = pointerX
                            downY = pointerY
                        } else {
                            val dx = pointerX - downX
                            val dy = pointerY - downY
                            val moved = dx * dx + dy * dy > 0.0004f
                            if (input == null) return
                            if (moved) input.swipe(downX, downY, pointerX, pointerY, 80)
                            else input.tap(pointerX, pointerY)
                        }
                    }
                    "right" -> if (msg.down) input?.global(android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_BACK)
                    "middle" -> if (msg.down) input?.global(android.accessibilityservice.AccessibilityService.GLOBAL_ACTION_HOME)
                }
            }
            is Protocol.Control.Wheel -> {
                val input = InputAccessibilityService.instance ?: return
                val dy = if (msg.delta > 0) -0.18f else 0.18f
                input.swipe(pointerX, pointerY, pointerX, (pointerY + dy).coerceIn(0f, 1f), 90)
            }
            is Protocol.Control.Key -> AndroidKeys.apply(msg.vk, msg.down)
            is Protocol.Control.Quality -> capture?.setQuality(msg.jpeg, msg.fps)
            Protocol.Control.Ping, Protocol.Control.Ignore -> {}
        }
    }

    private fun endSession() {
        inSession = false
        capture?.stop()
        capture = null
        sessionSocket?.close(1000, "end")
        sessionSocket = null
        startAsForeground(projection != null)
        broadcastStatus()
    }

    private fun startAsForeground(hasProjection: Boolean, inSession: Boolean = false) {
        val launch = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val text = if (inSession) getString(R.string.notif_session) else getString(R.string.notif_text)
        val notif = Notification.Builder(this, DarkTaskApp.CHANNEL_ID)
            .setContentTitle(getString(R.string.notif_title))
            .setContentText(text)
            .setSmallIcon(R.drawable.ic_stat)
            .setContentIntent(launch)
            .setOngoing(true)
            .build()
        val type = when {
            Build.VERSION.SDK_INT < 29 -> 0
            hasProjection -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
            else -> ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
        }
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(DarkTaskApp.NOTIF_ID, notif, type)
        } else {
            startForeground(DarkTaskApp.NOTIF_ID, notif)
        }
    }

    fun status(): Status = Status(
        running = running.get(),
        online = online,
        enrolled = prefs.enrolled,
        hasProjection = projection != null,
        hasInput = InputAccessibilityService.available(),
        inSession = inSession,
        deviceId = prefs.deviceId,
    )

    private fun broadcastStatus() {
        sendBroadcast(Intent(ACTION_STATUS).setPackage(packageName))
    }

    data class Status(
        val running: Boolean,
        val online: Boolean,
        val enrolled: Boolean,
        val hasProjection: Boolean,
        val hasInput: Boolean,
        val inSession: Boolean,
        val deviceId: String,
    )

    companion object {
        private const val TAG = "DarkTaskAgent"
        const val ACTION_STOP = "com.darktask.agent.STOP"
        const val ACTION_PROJECTION = "com.darktask.agent.PROJECTION"
        const val ACTION_STATUS = "com.darktask.agent.STATUS"
        const val EXTRA_RESULT_CODE = "result_code"
        const val EXTRA_RESULT_DATA = "result_data"

        @Volatile
        var instance: AgentService? = null
            private set

        private val handler = android.os.Handler(android.os.Looper.getMainLooper())
        private var pointerX = 0.5f
        private var pointerY = 0.5f
        private var downX = 0.5f
        private var downY = 0.5f

        fun wsUrl(server: String, path: String): String {
            val base = server.trim().trimEnd('/')
                .replace("https://", "wss://")
                .replace("http://", "ws://")
            return base + path
        }
    }
}
