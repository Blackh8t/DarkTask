package com.darktask.agent

import android.Manifest
import android.app.Activity
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import android.widget.Toast
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class MainActivity : Activity() {
    private val prefs by lazy { Prefs(this) }
    private val io = Executors.newSingleThreadExecutor()
    private val http = OkHttpClient.Builder()
        .connectTimeout(12, TimeUnit.SECONDS)
        .readTimeout(12, TimeUnit.SECONDS)
        .build()

    private lateinit var server: EditText
    private lateinit var token: EditText
    private lateinit var statusLine: TextView
    private lateinit var detailLine: TextView

    private val statusReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            render()
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        server = findViewById(R.id.server)
        token = findViewById(R.id.token)
        statusLine = findViewById(R.id.statusLine)
        detailLine = findViewById(R.id.detailLine)

        server.setText(prefs.server.ifEmpty { "https://rust.privateserver.im" })
        token.setText(prefs.enrollToken)

        findViewById<Button>(R.id.enroll).setOnClickListener { enrollAndStart() }
        findViewById<Button>(R.id.grantScreen).setOnClickListener { requestProjection() }
        findViewById<Button>(R.id.grantInput).setOnClickListener {
            startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))
        }
        findViewById<Button>(R.id.stop).setOnClickListener {
            startService(Intent(this, AgentService::class.java).setAction(AgentService.ACTION_STOP))
            stopService(Intent(this, AgentService::class.java))
            render()
        }

        if (Build.VERSION.SDK_INT >= 33) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        if (prefs.enrolled) startAgent()
    }

    override fun onResume() {
        super.onResume()
        val filter = IntentFilter(AgentService.ACTION_STATUS)
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(statusReceiver, filter, RECEIVER_NOT_EXPORTED)
        } else {
            registerReceiver(statusReceiver, filter)
        }
        render()
    }

    override fun onPause() {
        super.onPause()
        runCatching { unregisterReceiver(statusReceiver) }
    }

    private fun enrollAndStart() {
        prefs.server = server.text.toString()
        prefs.enrollToken = token.text.toString()
        if (prefs.server.isEmpty() || prefs.enrollToken.isEmpty()) {
            toast("Server and enrollment token are required")
            return
        }
        statusLine.text = "Enrolling…"
        io.execute {
            try {
                if (!prefs.enrolled) doEnroll()
                runOnUiThread {
                    startAgent()
                    requestProjection()
                }
            } catch (e: Exception) {
                runOnUiThread {
                    statusLine.text = "Enrollment failed"
                    detailLine.text = e.message ?: "unknown error"
                }
            }
        }
    }

    private fun doEnroll() {
        val body = Protocol.enrollBody(
            prefs.enrollToken,
            android.os.Build.MODEL,
            android.os.Build.SUPPORTED_ABIS.firstOrNull() ?: "unknown",
            StreamConfig.VERSION,
        )
        val req = Request.Builder()
            .url(prefs.server + "/api/v1/enroll")
            .post(body.toRequestBody(JSON))
            .build()
        http.newCall(req).execute().use { resp ->
            val text = resp.body?.string().orEmpty()
            if (!resp.isSuccessful) {
                throw IllegalStateException("Server returned ${resp.code}")
            }
            val json = JSONObject(text)
            prefs.deviceId = json.getString("device_id")
            prefs.deviceToken = json.getString("device_token")
        }
    }

    private fun startAgent() {
        val intent = Intent(this, AgentService::class.java)
        if (Build.VERSION.SDK_INT >= 26) startForegroundService(intent) else startService(intent)
        render()
    }

    private fun requestProjection() {
        val mgr = getSystemService(MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        startActivityForResult(mgr.createScreenCaptureIntent(), REQ_CAPTURE)
    }

    @Deprecated("Activity result")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_CAPTURE) return
        if (resultCode != RESULT_OK || data == null) {
            toast("Screen capture is required before a remote session can start")
            return
        }
        val intent = Intent(this, AgentService::class.java)
            .setAction(AgentService.ACTION_PROJECTION)
            .putExtra(AgentService.EXTRA_RESULT_CODE, resultCode)
            .putExtra(AgentService.EXTRA_RESULT_DATA, data)
        if (Build.VERSION.SDK_INT >= 26) startForegroundService(intent) else startService(intent)
    }

    private fun render() {
        val s = AgentService.instance?.status()
        val enrolled = prefs.enrolled
        when {
            s?.inSession == true -> {
                statusLine.text = "In session"
                statusLine.setTextColor(getColor(R.color.green))
            }
            s?.online == true -> {
                statusLine.text = "Online"
                statusLine.setTextColor(getColor(R.color.green))
            }
            s?.running == true -> {
                statusLine.text = "Connecting…"
                statusLine.setTextColor(getColor(R.color.blue))
            }
            enrolled -> {
                statusLine.text = "Enrolled · stopped"
                statusLine.setTextColor(getColor(R.color.muted))
            }
            else -> {
                statusLine.text = "Not enrolled"
                statusLine.setTextColor(getColor(R.color.text))
            }
        }
        val capture = if (s?.hasProjection == true) "Screen capture on" else "Screen capture off"
        val input = if (InputAccessibilityService.available()) "Remote input on" else "Remote input off"
        val id = prefs.deviceId.ifEmpty { "—" }
        detailLine.text = "$capture · $input\nDevice $id"
    }

    private fun toast(msg: String) {
        Toast.makeText(this, msg, Toast.LENGTH_LONG).show()
    }

    companion object {
        private val JSON = "application/json; charset=utf-8".toMediaType()
        private const val REQ_CAPTURE = 91
    }
}
