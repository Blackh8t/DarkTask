package com.darktask.agent

import android.content.Context
import android.content.SharedPreferences

class Prefs(context: Context) {
    private val p: SharedPreferences =
        context.applicationContext.getSharedPreferences("darktask", Context.MODE_PRIVATE)

    var server: String
        get() = p.getString("server", "") ?: ""
        set(v) { p.edit().putString("server", v.trim().trimEnd('/')).apply() }

    var enrollToken: String
        get() = p.getString("enroll", "") ?: ""
        set(v) { p.edit().putString("enroll", v.trim()).apply() }

    var deviceId: String
        get() = p.getString("device_id", "") ?: ""
        set(v) { p.edit().putString("device_id", v).apply() }

    var deviceToken: String
        get() = p.getString("device_token", "") ?: ""
        set(v) { p.edit().putString("device_token", v).apply() }

    val enrolled: Boolean
        get() = deviceId.isNotEmpty() && deviceToken.isNotEmpty()

    fun clearIdentity() {
        p.edit().remove("device_id").remove("device_token").apply()
    }
}
