package com.darktask.agent

import org.json.JSONObject

object Protocol {
    data class StartSession(
        val sessionId: String,
        val controllerId: String,
        val sessionToken: String,
        val sessionMode: String,
    )

    sealed class ServerMsg {
        data object HelloAck : ServerMsg()
        data object Ping : ServerMsg()
        data class Start(val session: StartSession) : ServerMsg()
        data object Unknown : ServerMsg()
    }

    sealed class Control {
        data class Move(val x: Float, val y: Float) : Control()
        data class Button(val button: String, val down: Boolean) : Control()
        data class Wheel(val delta: Int) : Control()
        data class Key(val vk: Int, val down: Boolean) : Control()
        data class Quality(val jpeg: Int, val fps: Int) : Control()
        data object Ping : Control()
        data object Ignore : Control()
    }

    fun hello(deviceId: String, deviceToken: String, hostname: String, version: String): String =
        JSONObject()
            .put("type", "hello")
            .put(
                "data",
                JSONObject()
                    .put("device_id", deviceId)
                    .put("device_token", deviceToken)
                    .put("hostname", hostname)
                    .put("agent_version", version),
            )
            .toString()

    fun heartbeat(deviceId: String, unixMs: Long, loggedIn: Boolean, idleSecs: Long?): String {
        val peek = JSONObject().put("user_logged_in", loggedIn)
        if (idleSecs != null) peek.put("idle_secs", idleSecs)
        return JSONObject()
            .put("type", "heartbeat")
            .put(
                "data",
                JSONObject()
                    .put("device_id", deviceId)
                    .put("unix_ms", unixMs)
                    .put("session_peek", peek),
            )
            .toString()
    }

    fun sessionAccepted(sessionId: String): String =
        JSONObject()
            .put("type", "session_accepted")
            .put("data", JSONObject().put("session_id", sessionId))
            .toString()

    fun sessionRejected(sessionId: String, reason: String): String =
        JSONObject()
            .put("type", "session_rejected")
            .put(
                "data",
                JSONObject()
                    .put("session_id", sessionId)
                    .put("reason", reason),
            )
            .toString()

    fun enrollBody(token: String, hostname: String, arch: String, version: String): String =
        JSONObject()
            .put("enrollment_token", token)
            .put("hostname", hostname)
            .put("platform", "android")
            .put("arch", arch)
            .put("agent_version", version)
            .toString()

    fun parseServer(text: String): ServerMsg {
        val o = JSONObject(text)
        return when (o.optString("type")) {
            "hello_ack" -> ServerMsg.HelloAck
            "ping" -> ServerMsg.Ping
            "start_session" -> {
                val d = o.getJSONObject("data")
                ServerMsg.Start(
                    StartSession(
                        sessionId = d.getString("session_id"),
                        controllerId = d.optString("controller_id"),
                        sessionToken = d.getString("session_token"),
                        sessionMode = d.optString("session_mode", "user_screen"),
                    ),
                )
            }
            else -> ServerMsg.Unknown
        }
    }

    fun parseControl(text: String): Control {
        val o = JSONObject(text)
        val d = o.optJSONObject("data") ?: JSONObject()
        return when (o.optString("type")) {
            "mouse_move" -> Control.Move(d.optDouble("x_norm").toFloat(), d.optDouble("y_norm").toFloat())
            "mouse_button" -> Control.Button(d.optString("button"), d.optBoolean("down"))
            "mouse_wheel" -> Control.Wheel(d.optInt("delta"))
            "key" -> Control.Key(d.optInt("vk"), d.optBoolean("down"))
            "set_quality" -> Control.Quality(d.optInt("jpeg_quality", StreamConfig.JPEG_QUALITY), d.optInt("max_fps", StreamConfig.DEFAULT_FPS))
            "ping" -> Control.Ping
            else -> Control.Ignore
        }
    }

    fun rpf1H264(width: Int, height: Int, keyframe: Boolean, payload: ByteArray): ByteArray {
        val out = ByteArray(16 + payload.size)
        out[0] = 'R'.code.toByte()
        out[1] = 'P'.code.toByte()
        out[2] = 'F'.code.toByte()
        out[3] = '1'.code.toByte()
        writeU32Le(out, 4, width)
        writeU32Le(out, 8, height)
        out[12] = 3
        out[13] = 3
        out[14] = if (keyframe) 1 else 0
        out[15] = 0
        System.arraycopy(payload, 0, out, 16, payload.size)
        return out
    }

    private fun writeU32Le(buf: ByteArray, offset: Int, value: Int) {
        buf[offset] = (value and 0xff).toByte()
        buf[offset + 1] = (value shr 8 and 0xff).toByte()
        buf[offset + 2] = (value shr 16 and 0xff).toByte()
        buf[offset + 3] = (value shr 24 and 0xff).toByte()
    }
}
