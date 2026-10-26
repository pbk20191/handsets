package dev.handsets.daemon

import android.content.ContentResolver
import android.content.Context
import android.database.Cursor
import android.net.Uri
import android.os.Binder
import android.os.Bundle
import android.os.IBinder
import android.os.Process
import dev.handsets.daemon.Binders.iActivityManager
import dev.handsets.daemon.Binders.myUserId
import java.lang.reflect.Constructor
import java.lang.reflect.Method
import java.nio.charset.StandardCharsets
import kotlin.concurrent.Volatile

/**
 * Read-only views over the user-data ContentProviders: SMS, call log,
 * contacts, calendar.
 * 
 * Goes straight through `IActivityManager.getContentProviderExternal`
 * (same path as [SettingsDirect]) instead of
 * [ContentResolver.query]. The ContentResolver path needs an
 * ActivityThread Application binding which our `app_process`
 * daemon doesn't have — Android 14+ rejects every query with
 * SecurityException: Unable to find app for caller …
 * The external path takes a caller package name string directly and
 * just needs the UID's permission bits, which shell UID has by default
 * for all four providers on AOSP / stock Pixel.
 * 
 * Output is NDJSON: first line is the column-name array, subsequent
 * lines are row-value arrays.
 */
internal class Providers(sysCtx: Context?) {
    private val token: IBinder = Binder()
    private val providerCache = java.util.concurrent.ConcurrentHashMap<String, Any>()

    @Volatile
    private var queryMethod: Method? = null

    @Volatile
    private var queryPrefix: Array<Any?> = emptyArray() // constant prefix args

    @Volatile
    private var uriSlot = 0 // index of the Uri arg

    @Volatile
    private var projSlot = 0 // index of the String[] proj arg

    @Volatile
    private var bundleSlot = 0 // index of the Bundle queryArgs arg

    @Volatile
    private var signalSlot = 0 // index of ICancellationSignal (may be left null)

    @Volatile
    private var queryWired = false

    // ---------- public verb entry points ----------
    fun sms(kind: String?, limit: Int): ByteArray {
        val path: String?
        when (if (kind == null) "inbox" else kind) {
            "inbox" -> path = "/inbox"
            "sent" -> path = "/sent"
            "all" -> path = ""
            else -> return err("bad-type:" + kind)
        }
        return query(
            "sms",
            Uri.parse("content://sms" + path),
            arrayOf<String>("_id", "address", "body", "date", "type", "read", "thread_id"),
            null, null, "date DESC", limit
        )
    }

    fun calls(kind: String?, limit: Int): ByteArray {
        var sel: String? = null
        var args: Array<String>? = null
        when (if (kind == null) "all" else kind) {
            "all" -> {}
            "in" -> {
                sel = "type=?"
                args = arrayOf<String>("1")
            }

            "out" -> {
                sel = "type=?"
                args = arrayOf<String>("2")
            }

            "missed" -> {
                sel = "type=?"
                args = arrayOf<String>("3")
            }

            else -> return err("bad-type:" + kind)
        }
        return query(
            "call_log",
            Uri.parse("content://call_log/calls"),
            arrayOf<String>("_id", "number", "name", "date", "duration", "type"),
            sel, args, "date DESC", limit
        )
    }

    fun contacts(limit: Int): ByteArray {
        // /data/phones is the pre-joined view of every phone-number data
        // row with its parent contact name — one row per phone number
        // (contacts without phones don't appear). For agent / automation
        // use this is the table you actually want; the bare
        // /contacts table only has names and photos.
        //
        // Column names: raw data1/data2/data3 are the canonical storage
        // names and accepted on every Android version. `number` / `type`
        // / `label` are aliases that some OEM builds reject; we re-label
        // them at projection time using SQL `AS`. Phone.NUMBER = data1,
        // Phone.TYPE = data2, Phone.LABEL = data3.
        return query(
            "com.android.contacts",
            Uri.parse("content://com.android.contacts/data/phones"),
            arrayOf<String>(
                "contact_id",
                "display_name",
                "data1",  // Phone.NUMBER
                "data2",  // Phone.TYPE (1=home, 2=mobile, 3=work, …)
                "data3",  // Phone.LABEL (custom label when type=0)
                "starred",
            ),
            null, null, "display_name COLLATE LOCALIZED", limit
        )
    }

    fun calendar(fromMs: Long, toMs: Long, limit: Int): ByteArray {
        return query(
            "com.android.calendar",
            Uri.parse(
                ("content://com.android.calendar/instances/when/"
                        + fromMs + "/" + toMs)
            ),
            arrayOf<String>(
                "event_id", "title", "begin", "end",
                "allDay", "eventLocation", "calendar_displayName"
            ),
            null, null, "begin ASC", limit
        )
    }

    // ---------- core query ----------
    private fun query(
        authority: String?, uri: Uri?, proj: Array<String>?,
        selection: String?, selectionArgs: Array<String>?,
        sort: String?, limit: Int
    ): ByteArray {
        val provider = acquire(authority)
        if (provider == null) return err("provider-acquire-failed:" + authority)

        val m: Method?
        try {
            m = ensureQueryMethod(provider.javaClass)
        } catch (t: Throwable) {
            return err("query-method:" + t.message)
        }
        if (m == null) return err("query-method-not-found")

        val qa = Bundle()
        if (selection != null) {
            qa.putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection)
            if (selectionArgs != null) {
                qa.putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, selectionArgs)
            }
        }
        if (sort != null) qa.putString(ContentResolver.QUERY_ARG_SQL_SORT_ORDER, sort)
        if (limit > 0) qa.putInt(ContentResolver.QUERY_ARG_LIMIT, limit)

        var c: Cursor? = null
        try {
            val args = buildQueryArgs(uri, proj, qa)
            c = m.invoke(provider, *args) as Cursor?
            if (c == null) return err("provider-null:" + authority)
            return encodeNdjson(c, limit)
        } catch (t: Throwable) {
            // Unwrap InvocationTargetException to surface the real cause.
            val cause: Throwable = (if (t.cause != null) t.cause else t)!!
            return err(
                ("query-failed:" + authority + ":"
                        + cause.javaClass.getSimpleName() + ":" + cause.message)
            )
        } finally {
            if (c != null) try {
                c.close()
            } catch (ignored: Throwable) {
            }
        }
    }

    /** Acquire an IContentProvider for `authority` via
     * IActivityManager.getContentProviderExternal — the same path
     * SettingsDirect uses. Cached per-authority for life of the
     * daemon (the binder is stable).  */
    private fun acquire(authority: String?): Any? {
        if (authority == null) return null
        providerCache[authority]?.let { return it }
        // Two callers racing on a cold authority both look it up; the first stored wins.
        try {
            val iam = iActivityManager()
            val ext: Method = findExternalGetter(iam!!.javaClass) ?: return null
            val holder = ext.invoke(iam, authority, myUserId(), token, "hs") ?: return null
            val prov = holder.javaClass.getField("provider").get(holder) ?: return null
            return providerCache.putIfAbsent(authority, prov) ?: prov
        } catch (t: Throwable) {
            Logx.w("providers: acquire $authority failed", t)
            return null
        }
    }

    /** Resolve and cache the right IContentProvider.query overload.
     * We want the one whose trailing params are `(Uri, String[],  Bundle, ICancellationSignal)` and return Cursor; the prefix
     * contains caller identity (String callingPkg + featureId, or an
     * AttributionSource on API 31+).  */
    private fun ensureQueryMethod(providerCls: Class<*>): Method? {
        if (queryWired) return queryMethod
        // No lock: racing callers compute the same overload; queryWired is published last.
        var best: Method? = null
        for (m in providerCls.getMethods()) {
            if ("query" != m.getName()) continue
            if (m.getReturnType() != Cursor::class.java) continue
            val p = m.getParameterTypes()
            if (p.size < 4) continue
            val n = p.size
            if (p[n - 4] != Uri::class.java) continue
            if (p[n - 3] != Array<String>::class.java) continue
            if (p[n - 2] != Bundle::class.java) continue
            if ("android.os.ICancellationSignal" != p[n - 1].getName()) continue
            if (best == null || p.size < best.getParameterTypes().size) {
                // Prefer the *shortest* prefix — fewer mystery params.
                best = m
            }
        }
        if (best != null) {
            val pt = best.getParameterTypes()
            val n = pt.size
            uriSlot = n - 4
            projSlot = n - 3
            bundleSlot = n - 2
            signalSlot = n - 1
            queryPrefix = buildQueryPrefix(pt, n - 4)
            queryMethod = best
        }
        queryWired = true
        return queryMethod
    }

    private fun buildQueryArgs(uri: Uri?, proj: Array<String>?, qa: Bundle?): Array<Any?> {
        val n = queryPrefix.size + 4
        val args = arrayOfNulls<Any>(n)
        System.arraycopy(queryPrefix, 0, args, 0, queryPrefix.size)
        args[uriSlot] = uri
        args[projSlot] = proj
        args[bundleSlot] = qa
        args[signalSlot] = null
        return args
    }

    companion object {
        private const val CALLER = "com.android.shell"

        private fun findExternalGetter(iamCls: Class<*>): Method? {
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

        private fun buildQueryPrefix(pt: Array<Class<*>?>, prefixLen: Int): Array<Any?> {
            val out = arrayOfNulls<Any>(prefixLen)
            var attribution: Any? = null
            for (i in 0..<prefixLen) {
                val c: Class<*> = pt[i]!!
                if (c.getName().endsWith("AttributionSource")) {
                    if (attribution == null) attribution = buildShellAttribution()
                    out[i] = attribution
                } else if (c == String::class.java) {
                    // First leftover String slot is callingPkg.
                    out[i] = if (out[i] == null) CALLER else out[i]
                } else {
                    out[i] = null
                }
            }
            // Legacy (callingPkg, featureId, ...) — featureId may be the
            // second String. Leaving it null is fine on all known overloads.
            return out
        }

        private fun buildShellAttribution(): Any? {
            try {
                val b = Class.forName("android.content.AttributionSource\$Builder")
                val ctor: Constructor<*> = b.getConstructor(Int::class.javaPrimitiveType)
                val builder: Any = ctor.newInstance(Process.myUid())
                b.getMethod("setPackageName", String::class.java).invoke(builder, CALLER)
                return b.getMethod("build").invoke(builder)
            } catch (t: Throwable) {
                return null
            }
        }

        // ---------- Cursor → NDJSON ----------
        private fun encodeNdjson(c: Cursor, limit: Int): ByteArray {
            val cols = c.getColumnNames()
            val out = StringBuilder(4096)
            out.append('[')
            for (i in cols.indices) {
                if (i > 0) out.append(',')
                appendJsonString(out, cols[i])
            }
            out.append("]\n")

            var rows = 0
            while (c.moveToNext()) {
                if (limit > 0 && rows >= limit) break
                out.append('[')
                for (i in cols.indices) {
                    if (i > 0) out.append(',')
                    appendCell(out, c, i)
                }
                out.append("]\n")
                rows++
            }
            return out.toString().toByteArray(StandardCharsets.UTF_8)
        }

        private fun appendCell(out: StringBuilder, c: Cursor, i: Int) {
            if (c.isNull(i)) {
                out.append("null")
                return
            }
            val t = c.getType(i)
            when (t) {
                Cursor.FIELD_TYPE_INTEGER -> {
                    out.append(c.getLong(i))
                    return
                }

                Cursor.FIELD_TYPE_FLOAT -> {
                    out.append(c.getDouble(i))
                    return
                }

                Cursor.FIELD_TYPE_STRING -> {
                    appendJsonString(out, c.getString(i))
                    return
                }

                Cursor.FIELD_TYPE_BLOB -> {
                    out.append("null")
                    return
                }

                else -> out.append("null")
            }
        }

        private fun appendJsonString(out: StringBuilder, s: String?) {
            if (s == null) {
                out.append("null")
                return
            }
            out.append('"')
            val n = s.length
            for (i in 0..<n) {
                val ch = s.get(i)
                when (ch) {
                    '\\' -> out.append("\\\\")
                    '"' -> out.append("\\\"")
                    '\n' -> out.append("\\n")
                    '\r' -> out.append("\\r")
                    '\t' -> out.append("\\t")
                    '\u0008' -> out.append("\\b")
                    '\u000C' -> out.append("\\f")
                    else -> if (ch.code < 0x20) {
                        out.append(String.format("\\u%04x", ch.code))
                    } else {
                        out.append(ch)
                    }
                }
            }
            out.append('"')
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:" + tail).toByteArray(StandardCharsets.UTF_8)
        }
    }
}
