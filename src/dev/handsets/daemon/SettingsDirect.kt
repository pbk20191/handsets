package dev.handsets.daemon

import android.os.Binder
import android.os.Bundle
import android.os.IBinder
import android.os.Process
import dev.handsets.daemon.Binders.iActivityManager
import dev.handsets.daemon.Binders.myUserId
import java.lang.reflect.Constructor
import java.lang.reflect.Method
import java.nio.charset.StandardCharsets
import java.util.Locale
import kotlin.concurrent.Volatile

/**
 * Direct settings provider access — skips both the `adb shell` hop
 * AND our own `/system/bin/settings` exec fallback. Acquires the
 * `settings` provider via `IActivityManager.getContentProviderExternal`,
 * caches the `IContentProvider`, then issues `provider.call(...)`
 * to `GET_/PUT_<ns>`.
 * 
 * Expected wire cost: sub-millisecond once warm.
 * 
 * Falls back through [SettingsApi.execFallback] for any namespace where
 * the reflective binding can't be made.
 */
internal class SettingsDirect {
    private val token: IBinder = Binder()

    @Volatile
    private var provider: Any? = null // IContentProvider

    @Volatile
    private var callMethod: Method? = null

    @Volatile
    private var callPrefix: Array<Any?> = emptyArray() // constant args before (method, arg, extras)

    @Volatile
    private var authoritySlot = -1

    @Volatile
    private var failed = false // sticky: stop retrying if init keeps failing

    /** Get the value for a setting, or null on lookup failure (caller falls back).  */
    fun tryGet(ns: String, key: String?): String? {
        if (failed) return null
        try {
            val prov = provider()
            if (prov == null) return null
            val n = callMethod!!.getParameterTypes().size
            val extras = Bundle()
            extras.putInt("_user", myUserId())
            val args = buildCallArgs(n, "GET_" + ns.lowercase(Locale.getDefault()), key, extras)
            val res = callMethod!!.invoke(prov, *args) as Bundle?
            return if (res == null) null else res.getString("value")
        } catch (t: Throwable) {
            return null
        }
    }

    /** Try to write a setting; return true on success.  */
    fun tryPut(ns: String, key: String?, value: String?): Boolean {
        if (failed) return false
        try {
            val prov = provider()
            if (prov == null) return false
            val n = callMethod!!.getParameterTypes().size
            val extras = Bundle()
            extras.putString("value", value)
            extras.putInt("_user", myUserId())
            val args = buildCallArgs(n, "PUT_" + ns.lowercase(Locale.getDefault()), key, extras)
            callMethod!!.invoke(prov, *args)
            return true
        } catch (t: Throwable) {
            return false
        }
    }

    // ---------- lazy init ----------
    private fun provider(): Any? {
        val p = provider
        if (p != null) return p
        synchronized(this) {
            if (provider != null) return provider
            try {
                val iam = iActivityManager()
                val ext: Method? = findExternalGetter(iam!!.javaClass)
                if (ext == null) {
                    failed = true
                    return null
                }
                val userId = myUserId()
                val holder = ext.invoke(iam, "settings", userId, token, "hs")
                if (holder == null) {
                    failed = true
                    return null
                }
                val providerField = holder.javaClass.getField("provider")
                val prov = providerField.get(holder)
                if (prov == null) {
                    failed = true
                    return null
                }
                val call = findCallMethod(prov.javaClass)
                if (call == null) {
                    failed = true
                    return null
                }
                val paramTypes = call.getParameterTypes()
                callPrefix = buildPrefix(paramTypes)
                callMethod = call
                provider = prov
                return prov
            } catch (t: Throwable) {
                failed = true
                return null
            }
        }
    }

    private fun findCallMethod(cls: Class<*>): Method? {
        // Trailing 3 params are always (String method, String arg, Bundle extras);
        // preceding params shift between API levels (callingPkg, featureId,
        // authority, AttributionSource, …). Pick the call() with that suffix.
        var best: Method? = null
        for (m in cls.getMethods()) {
            if ("call" != m.getName()) continue
            if (m.getReturnType() != Bundle::class.java) continue
            val p = m.getParameterTypes()
            if (p.size < 3) continue
            if (p[p.size - 1] != Bundle::class.java) continue
            if (p[p.size - 2] != String::class.java) continue
            if (p[p.size - 3] != String::class.java) continue
            if (best == null || p.size > best.getParameterTypes().size) best = m
        }
        return best
    }

    private fun buildPrefix(paramTypes: Array<Class<*>?>): Array<Any?> {
        val prefixLen = paramTypes.size - 3
        val pre = arrayOfNulls<Any>(prefixLen)
        var attributionSource: Any? = null
        var seenAttribution = false
        for (i in 0..<prefixLen) {
            val c: Class<*> = paramTypes[i]!!
            if (c.getName().endsWith("AttributionSource") && !seenAttribution) {
                if (attributionSource == null) attributionSource = makeAttributionSource()
                pre[i] = attributionSource
                seenAttribution = true
            } else if (c == String::class.java) {
                // Last leftover String slot is the authority; mark it so we
                // know to set it at call time.
                pre[i] = null // filled at call time if authoritySlot points here
                authoritySlot = i
            } else {
                pre[i] = null
            }
        }
        // First String slot of an older signature is callingPkg, not authority.
        // (legacy 4-arg: callingPkg, method, arg, extras — prefixLen=1, slot=0)
        // (5-arg: callingPkg, authority, method, arg, extras — prefixLen=2, slot=1)
        // (5-arg with AttributionSource: AS, authority, method, arg, extras — prefixLen=2, slot=1)
        // For the legacy 4-arg shape there's no authority slot; just callingPkg.
        if (prefixLen == 1 && pre[0] == null) {
            pre[0] = "com.android.shell"
            authoritySlot = -1
        } else if (prefixLen == 2 && !seenAttribution && pre[0] == null) {
            // (callingPkg, authority, ...)
            pre[0] = "com.android.shell"
            authoritySlot = 1
        }
        return pre
    }

    private fun buildCallArgs(
        paramCount: Int,
        method: String?,
        arg: String?,
        extras: Bundle?
    ): Array<Any?> {
        val out = arrayOfNulls<Any>(paramCount)
        System.arraycopy(callPrefix, 0, out, 0, callPrefix.size)
        out[paramCount - 3] = method
        out[paramCount - 2] = arg
        out[paramCount - 1] = extras
        if (authoritySlot >= 0) out[authoritySlot] = "settings"
        return out
    }

    companion object {
        private fun findExternalGetter(iamCls: Class<*>): Method? {
            // Stable shape: (String name, int userId, IBinder token, String tag).
            for (m in iamCls.getMethods()) {
                if ("getContentProviderExternal" != m.getName()) continue
                val p = m.getParameterTypes()
                if (p.size == 4 && p[0] == String::class.java && p[1] == Int::class.javaPrimitiveType && IBinder::class.java.isAssignableFrom(
                        p[2]
                    ) && p[3] == String::class.java
                ) {
                    return m
                }
            }
            return null
        }

        private fun makeAttributionSource(): Any? {
            try {
                val b = Class.forName("android.content.AttributionSource\$Builder")
                val ctor: Constructor<*> = b.getConstructor(Int::class.javaPrimitiveType)
                val builder: Any = ctor.newInstance(Process.myUid())
                val setPkg = b.getMethod("setPackageName", String::class.java)
                setPkg.invoke(builder, "com.android.shell")
                return b.getMethod("build").invoke(builder)
            } catch (t: Throwable) {
                return null
            }
        }

        fun bytes(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }
    }
}
