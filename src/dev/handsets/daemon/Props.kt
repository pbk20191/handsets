package dev.handsets.daemon

import java.lang.reflect.Method
import java.nio.charset.StandardCharsets

/**
 * Direct `SystemProperties` access. Reaches the same in-process key/value
 * store that `/system/bin/getprop` mmaps — but without the per-call
 * process spawn that `adb shell getprop` pays.
 * 
 * Reads are essentially free. Writes go through `__system_property_set`,
 * which RPCs into init; only shell-grantable property prefixes succeed.
 */
internal class Props {
    private val get: Method
    private val set: Method

    init {
        val sp = Class.forName("android.os.SystemProperties")
        this.get = sp.getDeclaredMethod("get", String::class.java)
        this.set = sp.getDeclaredMethod("set", String::class.java, String::class.java)
    }

    fun doGet(key: String?): ByteArray {
        try {
            val v = get.invoke(null, key)
            return (v as String).toByteArray(StandardCharsets.UTF_8)
        } catch (t: Throwable) {
            return err("getprop-failed:" + t.message)
        }
    }

    fun doSet(key: String?, value: String?): ByteArray {
        try {
            set.invoke(null, key, value)
            return "ok".toByteArray(StandardCharsets.UTF_8)
        } catch (t: Throwable) {
            return err("setprop-failed:" + t.message)
        }
    }

    companion object {
        private fun err(tail: String?): ByteArray {
            return ("ERR:" + tail).toByteArray(StandardCharsets.UTF_8)
        }
    }
}
