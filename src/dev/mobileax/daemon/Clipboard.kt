package dev.mobileax.daemon

import android.content.ClipData
import dev.mobileax.daemon.Binders.asInterface
import dev.mobileax.daemon.Binders.service
import java.lang.reflect.Method
import java.nio.charset.StandardCharsets

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
    private val svc: Any by lazy {
        asInterface($$"android.content.IClipboard$Stub", service("clipboard"))!!
    }

    /** Longest (most recent) getPrimaryClip overload. */
    private val getClip: Method? by lazy {
        svc.javaClass.getMethods()
            .filter { it.name == "getPrimaryClip" && it.returnType == ClipData::class.java }
            .maxByOrNull { it.getParameterTypes().size }
    }

    /** Longest (most recent) setPrimaryClip overload. */
    private val setClip: Method? by lazy {
        svc.javaClass.getMethods()
            .filter { m ->
                val p = m.getParameterTypes()
                m.name == "setPrimaryClip" && p.isNotEmpty() && p[0] == ClipData::class.java
            }
            .maxByOrNull { it.getParameterTypes().size }
    }

    // ---------- read ----------
    fun get(): ByteArray {
        try {
            val service = svc
            val m = getClip ?: return err("get-method-not-found")
            val cd = m.invoke(service, *buildArgs(m, null)) as? ClipData? ?: return ByteArray(0)
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
            val cd = ClipData.newPlainText("mobax", text)
            val service = svc
            val m = setClip ?: return err("set-method-not-found")
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
            val service = svc
            val m = getClip ?: return null
            val cd = m.invoke(service, *buildArgs(m, null)) as ClipData?
            if (cd == null || cd.itemCount == 0) return ""
            val cs = cd.getItemAt(0).text
            return cs?.toString() ?: ""
        } catch (_: Throwable) {
            return null
        }
    }

    // ---------- reflection helpers ----------
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
                when (c) {
                    String::class.java -> {
                        // First String → calling pkg; any later String → attributionTag (null is fine).
                        args[i] = if (stringSlotConsumed) null else CALLER
                        stringSlotConsumed = true
                    }
                    Int::class.javaPrimitiveType -> {
                        args[i] = 0 // userId / deviceId — 0 = current / default
                    }
                    Boolean::class.javaPrimitiveType -> {
                        args[i] = false // e.g. autoSelectionAllowed flag added in newer APIs
                    }
                    Long::class.javaPrimitiveType -> {
                        args[i] = 0L
                    }
                    else -> {
                        args[i] = null
                    }
                }
                i++
            }
            return args
        }

        // ---------- ClipData → bytes ----------
        private fun clipDataToBytes(cd: ClipData): ByteArray {
            if (cd.itemCount == 0) return ByteArray(0)
            val item = cd.getItemAt(0)
            val cs = item.text
            if (cs != null) return cs.toString().toByteArray(StandardCharsets.UTF_8)
            // No text item — emit a short note describing what's there.
            val sb = StringBuilder()
            if (item.uri != null) sb.append("uri:").append(item.uri)
            else if (item.getIntent() != null) sb.append("intent:").append(item.getIntent())
            else sb.append("")
            return sb.toString().toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}
