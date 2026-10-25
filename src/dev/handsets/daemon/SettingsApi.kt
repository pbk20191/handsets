package dev.handsets.daemon

import android.content.ContentResolver
import android.content.Context
import android.provider.Settings
import java.io.BufferedReader
import java.io.IOException
import java.io.InputStreamReader
import java.nio.charset.StandardCharsets
import java.util.Locale

/**
 * settings get/put — direct ContentResolver against the Settings provider.
 * Skips both the `adb shell` hop and the `/system/bin/settings`
 * process spawn.
 * 
 * Shell UID has WRITE_SECURE_SETTINGS (granted by adbd), so all three
 * namespaces (system / secure / global) are writable from here, same as
 * `adb shell settings put …`.
 */
internal class SettingsApi(ctx: Context) {
    private val cr: ContentResolver?
    private val direct = SettingsDirect()

    init {
        this.cr = ctx.getContentResolver()
    }

    fun get(namespace: String, key: String?): ByteArray {
        // Fast path: direct IContentProvider.call via getContentProviderExternal.
        var v = direct.tryGet(namespace, key)
        if (v != null) return v.toByteArray(StandardCharsets.UTF_8)
        // Second try: public ContentResolver helpers. Will usually fail from
        // app_process (no IApplicationThread registration) but is harmless to try.
        try {
            when (namespace.lowercase(Locale.getDefault())) {
                "system" -> v = Settings.System.getString(cr, key)
                "secure" -> v = Settings.Secure.getString(cr, key)
                "global" -> v = Settings.Global.getString(cr, key)
                else -> return err("bad-namespace:" + namespace)
            }
            if (v != null) return v.toByteArray(StandardCharsets.UTF_8)
        } catch (ignored: Throwable) {
        }
        // Last resort: spawn the settings binary.
        return execFallback("get", namespace, key, null)
    }

    fun put(namespace: String, key: String?, value: String?): ByteArray {
        if (direct.tryPut(namespace, key, value)) {
            return "ok".toByteArray(StandardCharsets.UTF_8)
        }
        try {
            val ok: Boolean
            when (namespace.lowercase(Locale.getDefault())) {
                "system" -> ok = Settings.System.putString(cr, key, value)
                "secure" -> ok = Settings.Secure.putString(cr, key, value)
                "global" -> ok = Settings.Global.putString(cr, key, value)
                else -> return err("bad-namespace:" + namespace)
            }
            if (ok) return "ok".toByteArray(StandardCharsets.UTF_8)
        } catch (ignored: Throwable) {
        }
        return execFallback("put", namespace, key, value)
    }

    private fun execFallback(op: String?, ns: String?, key: String?, value: String?): ByteArray {
        try {
            val pb: ProcessBuilder?
            if ("get" == op) {
                pb = ProcessBuilder("/system/bin/settings", "get", ns, key)
            } else {
                pb = ProcessBuilder(
                    "/system/bin/settings", "put", ns, key,
                    if (value == null) "" else value
                )
            }
            pb.redirectErrorStream(true)
            val p = pb.start()
            val sb = StringBuilder()
            BufferedReader(InputStreamReader(p.getInputStream())).use { r ->
                var line: String?
                while ((r.readLine().also { line = it }) != null) sb.append(line).append('\n')
            }
            val code = p.waitFor()
            val out = sb.toString().trim { it <= ' ' }
            if (code != 0) return err("settings-exit-" + code + (if (out.isEmpty()) "" else ":" + out))
            return (if ("get" == op)
                (if (out.isEmpty()) "null" else out)
            else
                "ok").toByteArray(StandardCharsets.UTF_8)
        } catch (e: IOException) {
            return err("settings-fallback-failed:" + e.message)
        } catch (e: InterruptedException) {
            return err("settings-fallback-failed:" + e.message)
        }
    }

    companion object {
        private fun err(tail: String?): ByteArray {
            return ("ERR:" + tail).toByteArray(StandardCharsets.UTF_8)
        }
    }
}
