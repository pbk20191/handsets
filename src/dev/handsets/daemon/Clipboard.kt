package dev.handsets.daemon

import android.content.ClipData
import dev.handsets.daemon.Binders.asInterface
import dev.handsets.daemon.Binders.service
import java.lang.reflect.Method
import java.nio.charset.StandardCharsets
import kotlin.concurrent.Volatile

/**
 * Clipboard read/write/watch via the `IClipboard` binder.
 * 
 * AOSP's ClipboardService unconditionally allows shell UID:
 * `if (callingUid == Process.SHELL_UID) return true;`
 * so we don't need foreground-window tricks or runtime grants — just
 * reach the binder and call getPrimaryClip / setPrimaryClip directly.
 * 
 * Watch mode polls every `interval_ms` (default 500); listener
 * callback wiring would need a custom Binder stub for
 * IOnPrimaryClipChangedListener, which is more code than it's worth
 * given how cheap a clip-read is.
 */
internal class Clipboard {
    @Volatile
    private var svc: Any? = null

    @Volatile
    private var getClip: Method? = null

    @Volatile
    private var setClip: Method? = null

    @Synchronized
    private fun svc(): Any {
        if (svc == null) {
            svc = asInterface(
                "android.content.IClipboard\$Stub",
                service("clipboard")
            )
        }
        return svc!!
    }

    // ---------- read ----------
    fun get(): ByteArray? {
        try {
            val service = svc()
            val m = getOrFindGet(service.javaClass)
            if (m == null) return err("get-method-not-found")
            val cd = m.invoke(service, *buildArgs(m, null)) as ClipData?
            if (cd == null) return ByteArray(0)
            return clipDataToBytes(cd)
        } catch (t: Throwable) {
            val c: Throwable = (if (t.cause != null) t.cause else t)!!
            return err("get-failed:" + c.javaClass.getSimpleName() + ":" + c.message)
        }
    }

    // ---------- write ----------
    fun set(text: String?): ByteArray {
        var text = text
        if (text == null) text = ""
        try {
            val cd = ClipData.newPlainText("hs", text)
            val service = svc()
            val m = getOrFindSet(service.javaClass)
            if (m == null) return err("set-method-not-found")
            m.invoke(service, *buildArgs(m, cd))
            return "ok".toByteArray(StandardCharsets.UTF_8)
        } catch (t: Throwable) {
            val c: Throwable = (if (t.cause != null) t.cause else t)!!
            return err("set-failed:" + c.javaClass.getSimpleName() + ":" + c.message)
        }
    }

    // ---------- watch (polled streaming) ----------
    /** Polls the clipboard at `intervalMs` cadence. Each time the
     * text changes, writes one length-prefixed frame with the new
     * content. Terminates on IO error (client gone) with a 0-length
     * frame so the host sees a clean end.  */
    fun currentText(): String? {
        try {
            val service = svc()
            val m = getOrFindGet(service.javaClass)
            if (m == null) return null
            val cd = m.invoke(service, *buildArgs(m, null)) as ClipData?
            if (cd == null || cd.getItemCount() == 0) return ""
            val cs = cd.getItemAt(0).getText()
            return if (cs == null) "" else cs.toString()
        } catch (ignored: Throwable) {
            return null
        }
    }

    // ---------- reflection helpers ----------
    private fun getOrFindGet(cls: Class<*>): Method? {
        val cached = getClip
        if (cached != null) return cached
        synchronized(this) {
            if (getClip != null) return getClip
            for (m in cls.getMethods()) {
                if ("getPrimaryClip" != m.getName()) continue
                if (m.getReturnType() != ClipData::class.java) continue
                // Prefer the longest (most-recent) overload.
                if (getClip == null
                    || m.getParameterTypes().size > getClip!!.getParameterTypes().size
                ) {
                    getClip = m
                }
            }
            return getClip
        }
    }

    private fun getOrFindSet(cls: Class<*>): Method? {
        val cached = setClip
        if (cached != null) return cached
        synchronized(this) {
            if (setClip != null) return setClip
            for (m in cls.getMethods()) {
                if ("setPrimaryClip" != m.getName()) continue
                val p = m.getParameterTypes()
                if (p.size < 1 || p[0] != ClipData::class.java) continue
                if (setClip == null
                    || p.size > setClip!!.getParameterTypes().size
                ) {
                    setClip = m
                }
            }
            return setClip
        }
    }

    companion object {
        private const val CALLER = "com.android.shell"

        /** Build the arg array for getPrimaryClip / setPrimaryClip. The
         * signatures across API levels share the same shape:
         * `[ClipData?] String pkg [, String attributionTag] [, int userId] [, int deviceId]`  */
        private fun buildArgs(m: Method, clipDataOrNull: ClipData?): Array<Any?> {
            val pt = m.getParameterTypes()
            val args = arrayOfNulls<Any>(pt.size)
            var i = 0
            if (clipDataOrNull != null && pt[0] == ClipData::class.java) {
                args[i++] = clipDataOrNull
            }
            var stringSlotConsumed = false
            while (i < pt.size) {
                val c = pt[i]
                if (c == String::class.java) {
                    // First String → calling pkg; any later String → attributionTag (null is fine).
                    args[i] = if (stringSlotConsumed) null else CALLER
                    stringSlotConsumed = true
                } else if (c == Int::class.javaPrimitiveType) {
                    args[i] = 0 // userId / deviceId — 0 = current / default
                } else if (c == Boolean::class.javaPrimitiveType) {
                    args[i] = false // e.g. autoSelectionAllowed flag added in newer APIs
                } else if (c == Long::class.javaPrimitiveType) {
                    args[i] = 0L
                } else {
                    args[i] = null
                }
                i++
            }
            return args
        }

        // ---------- ClipData → bytes ----------
        private fun clipDataToBytes(cd: ClipData): ByteArray {
            if (cd.getItemCount() == 0) return ByteArray(0)
            val item = cd.getItemAt(0)
            val cs = item.getText()
            if (cs != null) return cs.toString().toByteArray(StandardCharsets.UTF_8)
            // No text item — emit a short note describing what's there.
            val sb = StringBuilder()
            if (item.getUri() != null) sb.append("uri:").append(item.getUri())
            else if (item.getIntent() != null) sb.append("intent:").append(item.getIntent())
            else sb.append("")
            return sb.toString().toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:" + tail).toByteArray(StandardCharsets.UTF_8)
        }
    }
}
