package dev.mobileax.daemon

import android.content.Context
import dev.mobileax.daemon.Screenshot.CaptureArgs
import java.nio.charset.StandardCharsets

internal class Server(private val h: Handlers) {
    /** Bag of handler classes wired up in [Main]; lets the constructor
     * stay legible as we grow the command surface.  */
    /** Wired up in [Main]; `props` stays null if its init failed. */
    class Handlers {
        lateinit var sysCtx: Context
        lateinit var dumper: Dumper
        lateinit var shot: Screenshot
        lateinit var input: Input
        lateinit var installer: Installer
        lateinit var pm: Pm
        lateinit var manifest: Manifest
        lateinit var providers: Providers
        lateinit var location: Location
        lateinit var notifs: Notifications
        lateinit var clip: Clipboard
        lateinit var am: Am
        var props: Props? = null
        lateinit var settings: SettingsApi
        lateinit var wm: Wm
        lateinit var state: State
        lateinit var uiEvents: UiEvents
        lateinit var nodes: NodeActions
    }

    fun handlers(): Handlers {
        return h
    }

    /**
     * Synchronous verbs: returns the single response frame, or null when
     * `head` is not one of them (streams, transfers and waits are
     * driven by [Dispatch]). Thread-safe; callers pick the thread.
     */
    fun respond(head: String, cmd: String): ByteArray? {
        when (head) {
            "info" -> return utf8(h.shot.sourceSize().x.toString() + " " + h.shot.sourceSize().y)
            "pm_list" -> return runPmList(cmd)
            "pm_path" -> return runPmPath(cmd)
            "pm_grant", "pm_revoke" -> return runPmPerm(head, cmd)
            "am_start" -> return runAmStart(cmd)
            "am_force_stop" -> return runAmForceStop(cmd)
            "am_kill" -> return runAmKill(cmd)
            "am_broadcast" -> return runAmBroadcast(cmd)
            "getprop" -> return runGetProp(cmd)
            "setprop" -> return runSetProp(cmd)
            "settings_get" -> return runSettingsGet(cmd)
            "settings_put" -> return runSettingsPut(cmd)
            "wm_info" -> return h.wm.info()
            "wm_rotation" -> return runWmRotation(cmd)
            "state" -> return runState(cmd)
            "node_click" -> return runNodeClick(cmd, false)
            "node_long_click" -> return runNodeClick(cmd, true)
            "node_set_text" -> return runNodeSetText(cmd)
            "node_scroll" -> return runNodeScroll(cmd)
            "node_focus" -> return runNodeFocus(cmd)
            "submit" -> return runSubmit(cmd)
            "paste" -> return runPaste(cmd)
            "deeplinks" -> return runDeeplinks(cmd)
            "sms" -> return runSms(cmd)
            "calls" -> return runCalls(cmd)
            "contacts" -> return runContacts(cmd)
            "calendar" -> return runCalendar(cmd)
            "location" -> return h.location.last()
            "notifications" -> return runNotifications(cmd)
            "clip_get" -> return h.clip.get()
            "clip_set" -> return h.clip.set(afterHead(cmd))
            else -> return null
        }
    }

    /** dump / dump_active / screenshot: suspend, they wait on the dumper or the mirror. */
    suspend fun capture(head: String, cmd: String): ByteArray = try {
        when (head) {
            "dump" -> utf8(h.dumper.dumpAll())
            "dump_active" -> utf8(h.dumper.dumpActive())
            else -> h.shot.capture(parseScreenshotArgs(cmd))
        }
    } catch (t: Throwable) {
        if (t is kotlinx.coroutines.CancellationException) throw t
        if (head == "screenshot") errBytes("screenshot-failed:" + t.javaClass.getSimpleName() + ":" + t.message)
        else utf8("ERR:INTERNAL:dump-failed:" + t.javaClass.getSimpleName() + ":" + t.message)
    }

    internal class InputArgs {
        /** `wait=0`: don't wait for the window to handle a key (a live viewer). */
        var wait = true
        var x: Int = 0
        var y: Int = 0
        var x1: Int = 0
        var y1: Int = 0
        var x2: Int = 0
        var y2: Int = 0
        var dur: Int = 0
        var dy: Int = 0
        var code: Int = 0
        var keyName: String? = null
    }

    // ---------- adb-like command handlers ----------
    private fun runPmList(cmd: String): ByteArray {
        val thirdParty = cmd.contains(" 3") || cmd.contains("third=1")
        val systemOnly = cmd.contains(" s") || cmd.contains("system=1")
        return h.pm.list(thirdParty, systemOnly)
    }

    private fun runPmPath(cmd: String): ByteArray {
        val pkg: String = positional(cmd) ?: return errBytes("pm_path-needs-pkg")
        return h.pm.path(pkg)
    }

    private fun runPmPerm(head: String?, cmd: String): ByteArray {
        val parts = positionalPair(cmd) ?: return errBytes("$head-needs-pkg-and-perm")
        if ("pm_grant" == head) return h.pm.grant(parts[0], parts[1])
        return h.pm.revoke(parts[0], parts[1])
    }

    private fun runAmStart(cmd: String): ByteArray {
        val a: AmArgs = parseAmArgs(cmd)
        if (a.component == null) return errBytes("am_start-needs-n=pkg/.Class")
        return h.am.start(a.component, a.action, a.data, a.flags)
    }

    private fun runAmForceStop(cmd: String): ByteArray {
        val pkg: String = positional(cmd) ?: return errBytes("am_force_stop-needs-pkg")
        return h.am.forceStop(pkg)
    }

    private fun runAmKill(cmd: String): ByteArray {
        val pkg: String = positional(cmd) ?: return errBytes("am_kill-needs-pkg")
        return h.am.kill(pkg)
    }

    private fun runAmBroadcast(cmd: String): ByteArray {
        val a: AmArgs = parseAmArgs(cmd)
        return h.am.broadcast(a.action, a.component, a.data)
    }

    /**
     * `swipe_dir <left></left>|right|up|down> [dur=N]` — synthesises an 80%-of-screen
     * swipe in the named direction starting from the screen centre. Screen
     * dimensions come from the existing Screenshot mirror so we don't
     * round-trip another binder call per swipe.
     */
    // ---------- arg parsers for the new commands ----------
    internal class FileArgs {
        var path: String? = null
        var mode: Int = 0
        var size: Long = 0
        var reinstall: Boolean = false
        var grant: Boolean = false
    }

    private class AmArgs {
        var component: String? = null
        var action: String? = null
        var data: String? = null
        var flags: Int = 0
    }

    // ---------- second-wave command handlers ----------
    private fun runGetProp(cmd: String): ByteArray {
        val key: String = positional(cmd) ?: return errBytes("getprop-needs-key")
        return h.props!!.doGet(key)
    }

    private fun runSetProp(cmd: String): ByteArray {
        val pair = positionalPair(cmd) ?: return errBytes("setprop-needs-key-and-value")
        return h.props!!.doSet(pair[0], pair[1])
    }

    private fun runSettingsGet(cmd: String): ByteArray {
        val parts = positionalPair(cmd) ?: return errBytes("settings_get-needs-namespace-and-key")
        return h.settings.get(parts[0], parts[1])
    }

    private fun runSettingsPut(cmd: String): ByteArray {
        // Need namespace + key + value (3 positional tokens).
        val tail: String =
            afterHead(cmd) ?: return errBytes("settings_put-needs-namespace-key-value")
        val parts = tail.split("\\s+".toRegex(), limit = 3).toTypedArray()
        if (parts.size < 3) return errBytes("settings_put-needs-namespace-key-value")
        return h.settings.put(parts[0], parts[1], parts[2])
    }

    private fun runWmRotation(cmd: String): ByteArray {
        val v: String = positional(cmd) ?: return errBytes("wm_rotation-needs-value")
        return try {
            h.wm.setRotation(v.toInt())
        } catch (_: NumberFormatException) {
            errBytes("bad-rotation:$v")
        }
    }

    // ---------- node_* handlers ----------
    private fun runNodeClick(cmd: String, longClick: Boolean): ByteArray? {
        val sel: String = afterHead(cmd)
            ?: return errBytes((if (longClick) "node_long_click" else "node_click") + "-needs-selector")
        return if (longClick) h.nodes.longClick(sel) else h.nodes.click(sel)
    }

    private fun runNodeSetText(cmd: String): ByteArray? {
        val tail: String =
            afterHead(cmd) ?: return errBytes("node_set_text-needs-selector-and-value")
        // Pull value= out, treat the rest as the selector.
        val value: String =
            extractKey(tail, "value") ?: return errBytes("node_set_text-needs-value=")
        // Empty selector is intentional: NodeActions.setText handles it by
        // targeting whichever EditText currently has input focus. Kept
        // explicit here so callers don't need a sentinel value — they just
        // omit the selector and `node_set_text value="..."` writes to the
        // focused field, which is what mobax tui needs after a tap-to-focus.
        return h.nodes.setText(removeKey(tail, "value").trim { it <= ' ' }, value)
    }

    private fun runNodeScroll(cmd: String): ByteArray? {
        val tail: String = afterHead(cmd) ?: return errBytes("node_scroll-needs-selector-and-dir")
        var dir: String? = extractKey(tail, "dir")
        if (dir == null) dir = "forward"
        val sel: String = removeKey(tail, "dir")
        if (sel.isEmpty()) return errBytes("node_scroll-needs-selector")
        return h.nodes.scroll(sel, dir)
    }

    private fun runNodeFocus(cmd: String): ByteArray? {
        val sel: String = afterHead(cmd) ?: return errBytes("node_focus-needs-selector")
        return h.nodes.focus(sel)
    }

    private fun runSubmit(cmd: String): ByteArray? {
        // `submit` with no args targets the focused EditText. A trailing
        // selector overrides that — `submit text~=Email`.
        return h.nodes.imeAction(afterHead(cmd))
    }

    private fun runPaste(cmd: String): ByteArray? {
        // `paste` with no args inserts the system clipboard into the
        // focused EditText. Trailing selector overrides — `paste id=...`.
        return h.nodes.pasteAction(afterHead(cmd))
    }

    private fun runDeeplinks(cmd: String): ByteArray {
        val pkg: String? = positional(cmd)
        return h.manifest.deeplinks(pkg)
    }

    // ---------- user-data ContentProvider verbs ----------
    private fun runSms(cmd: String): ByteArray {
        val kind: String? = strArgOr(cmd, "type", "inbox")
        val limit = longArg(cmd, "limit", 50).toInt()
        return h.providers.sms(kind, limit)
    }

    private fun runCalls(cmd: String): ByteArray {
        val kind: String? = strArgOr(cmd, "type", "all")
        val limit = longArg(cmd, "limit", 50).toInt()
        return h.providers.calls(kind, limit)
    }

    private fun runContacts(cmd: String): ByteArray {
        val limit = longArg(cmd, "limit", 50).toInt()
        return h.providers.contacts(limit)
    }

    private fun runCalendar(cmd: String): ByteArray {
        val now = System.currentTimeMillis()
        val from: Long = longArg(cmd, "from", now)
        val to: Long = longArg(cmd, "to", now + 7L * 24 * 60 * 60 * 1000)
        val limit = longArg(cmd, "limit", 50).toInt()
        return h.providers.calendar(from, to, limit)
    }

    private fun runNotifications(cmd: String): ByteArray {
        val pkg: String? = strArg(cmd, "pkg")
        val limit = longArg(cmd, "limit", 50).toInt()
        val history = longArg(cmd, "history", 0) != 0L
        return h.notifs.dump(pkg, limit, history)
    }

    /**
     * Streaming subscription to State.device() snapshots. Server pushes one
     * frame whenever the cache is recomputed (driven by the existing a11y +
     * display listeners). The client side hands these straight to stdout /
     * its local mirror — no polling, no per-read binder hops.
     * 
     * The connection thread blocks on the queue here. The State refresher
     * thread re-fires every HEARTBEAT_MS even if nothing changed, so a
     * write failure surfaces a dead client within that window.
     */
    private fun runState(cmd: String): ByteArray {
        val field: String = positional(cmd) ?: return errBytes("state-needs-field")
        return when (field) {
            "interactive" -> h.state.interactive()
            "battery_level" -> h.state.batteryLevel()
            "battery_charging" -> h.state.batteryCharging()
            "top" -> h.state.topActivity()
            "procs" -> h.state.procs()
            "device" -> h.state.device()
            "device_fresh" -> h.state.deviceFresh()
            else -> errBytes("unknown-state-field:$field")
        }
    }

    companion object {
        const val MAX_CMD: Int = 256

        /** Parse k=v tokens plus a single positional name (for `key BACK`).  */
        fun parseInputArgs(cmd: String): InputArgs {
            val a = InputArgs()
            val sp = cmd.indexOf(' ')
            if (sp < 0) return a
            val tail = cmd.substring(sp + 1).trim { it <= ' ' }
            if (tail.isEmpty()) return a
            for (tok in tail.split("\\s+".toRegex()).dropLastWhile { it.isEmpty() }
                .toTypedArray()) {
                val eq = tok.indexOf('=')
                if (eq < 0) {
                    // positional — treat as key name for `key BACK` style
                    if (a.keyName == null) a.keyName = tok
                    continue
                }
                val k = tok.substring(0, eq)
                val v = tok.substring(eq + 1)
                try {
                    when (k) {
                        "x" -> a.x = v.toInt()
                        "y" -> a.y = v.toInt()
                        "x1" -> a.x1 = v.toInt()
                        "y1" -> a.y1 = v.toInt()
                        "x2" -> a.x2 = v.toInt()
                        "y2" -> a.y2 = v.toInt()
                        "dur" -> a.dur = v.toInt()
                        "dy" -> a.dy = v.toInt()
                        "code" -> a.code = v.toInt()
                        "wait" -> a.wait = v != "0"
                        else -> {}
                    }
                } catch (_: NumberFormatException) {
                }
            }
            return a
        }

        fun clampQuality(q: Int): Int {
            if (q <= 0) return 80
            if (q > 100) return 100
            return q
        }

        fun parseScreenshotArgs(cmd: String): CaptureArgs {
            val a = CaptureArgs()
            // cmd = "screenshot[ k=v[ k=v...]]"
            val sp = cmd.indexOf(' ')
            if (sp < 0) return a
            val tail = cmd.substring(sp + 1).trim { it <= ' ' }
            if (tail.isEmpty()) return a
            for (tok in tail.split("\\s+".toRegex()).dropLastWhile { it.isEmpty() }
                .toTypedArray()) {
                val eq = tok.indexOf('=')
                if (eq <= 0) continue
                val k = tok.substring(0, eq)
                val v = tok.substring(eq + 1)
                try {
                    when (k) {
                        "size" -> a.longEdge = v.toInt()
                        "q", "quality" -> a.quality = v.toInt()
                        "fmt", "format" -> a.format = v
                        "max" -> a.max = "0" != v && !"false".equals(v, ignoreCase = true)
                        "fps" -> a.fps = v.toInt()
                        "bitrate", "kbps" -> a.bitrateKbps = v.toInt()
                        "tile" -> a.tile = v.toInt()
                        "gop" -> a.gopSec = v.toInt()
                        "secure_check", "secure" -> a.secureCheck =
                            "0" != v && !"false".equals(v, ignoreCase = true)

                        else -> {}
                    }
                } catch (_: NumberFormatException) {
                }
            }
            return a
        }

        fun utf8(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }

        fun errBytes(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }

        fun parseFileArgs(cmd: String): FileArgs {
            val a = FileArgs()
            val sp = cmd.indexOf(' ')
            if (sp < 0) return a
            val tail = cmd.substring(sp + 1).trim { it <= ' ' }
            if (tail.isEmpty()) return a
            for (tok in tail.split("\\s+".toRegex()).dropLastWhile { it.isEmpty() }
                .toTypedArray()) {
                val eq = tok.indexOf('=')
                if (eq <= 0) continue
                val k = tok.substring(0, eq)
                val v = tok.substring(eq + 1)
                try {
                    when (k) {
                        "path" -> a.path = v
                        "mode" -> a.mode = v.toInt(8)
                        "size" -> a.size = v.toLong()
                        "reinstall" -> a.reinstall = "0" != v
                        "grant" -> a.grant = "0" != v
                        else -> {}
                    }
                } catch (_: NumberFormatException) {
                }
            }
            return a
        }

        private fun parseAmArgs(cmd: String): AmArgs {
            val a = AmArgs()
            val sp = cmd.indexOf(' ')
            if (sp < 0) return a
            val tail = cmd.substring(sp + 1).trim { it <= ' ' }
            if (tail.isEmpty()) return a
            for (tok in tail.split("\\s+".toRegex()).dropLastWhile { it.isEmpty() }
                .toTypedArray()) {
                val eq = tok.indexOf('=')
                if (eq <= 0) continue
                val k = tok.substring(0, eq)
                val v = tok.substring(eq + 1)
                try {
                    when (k) {
                        "n" -> a.component = v
                        "a" -> a.action = v
                        "d" -> a.data = v
                        "f" -> a.flags = Integer.decode(v)
                        else -> {}
                    }
                } catch (_: NumberFormatException) {
                }
            }
            return a
        }

        /** Return the first whitespace-separated token after the command head.  */
        fun positional(cmd: String): String? {
            val sp = cmd.indexOf(' ')
            if (sp < 0) return null
            val tail = cmd.substring(sp + 1).trim { it <= ' ' }
            if (tail.isEmpty()) return null
            val sp2 = tail.indexOf(' ')
            return if (sp2 < 0) tail else tail.substring(0, sp2)
        }

        // ---------- wait-for / composite handlers ----------
        const val DEFAULT_IDLE_MS: Long = 200
        const val DEFAULT_TIMEOUT_MS: Long = 5000
        const val DEFAULT_TAP_TIMEOUT_MS: Long = 2000

        // ---------- key=value arg helpers ----------
        fun textSelector(text: String?, mode: String?): NodeActions.Selector {
            val s = NodeActions.Selector()
            if ("exact".equals(mode, ignoreCase = true)) s.textExact = text
            else s.textSub = text
            return s
        }

        /** Parse `key=value` from a whitespace-tokenised string.  */
        fun strArg(cmd: String, key: String?): String? {
            return strArgOr(cmd, key, null)
        }

        fun strArgOr(cmd: String, key: String?, dflt: String?): String? {
            val sp = cmd.indexOf(' ')
            if (sp < 0) return dflt
            val tail = cmd.substring(sp + 1)
            val pat = "$key="
            var i = 0
            while ((tail.indexOf(pat, i).also { i = it }) >= 0) {
                if (i == 0 || Character.isWhitespace(tail[i - 1])) {
                    val start = i + pat.length
                    if (start < tail.length && tail[start] == '"') {
                        val end = tail.indexOf('"', start + 1)
                        if (end > 0) return tail.substring(start + 1, end)
                        return dflt
                    }
                    var end = start
                    while (end < tail.length && !Character.isWhitespace(tail[end])) end++
                    return tail.substring(start, end)
                }
                i += pat.length
            }
            return dflt
        }

        fun longArg(cmd: String, key: String?, dflt: Long): Long {
            val v: String = strArg(cmd, key) ?: return dflt
            return try {
                v.toLong()
            } catch (_: NumberFormatException) {
                dflt
            }
        }

        /** Same as strArg but works on an arbitrary token string (no command head).  */
        private fun extractKey(tail: String, key: String?): String? {
            val fakeCmd = "_ $tail"
            return strArg(fakeCmd, key)
        }

        /** Strip a `key="..."` or `key=value` token from `tail`.  */
        private fun removeKey(tail: String, key: String?): String {
            val out = StringBuilder(tail.length)
            var i = 0
            var inQuote = false
            val tok = StringBuilder()
            while (i < tail.length) {
                val c = tail[i]
                if (c == '"') inQuote = !inQuote
                if (Character.isWhitespace(c) && !inQuote) {
                    val t = tok.toString()
                    if (t.isNotEmpty()) {
                        if (!t.startsWith("$key=")) {
                            if (out.isNotEmpty()) out.append(' ')
                            out.append(t)
                        }
                        tok.setLength(0)
                    }
                } else {
                    tok.append(c)
                }
                i++
            }
            val t = tok.toString()
            if (t.isNotEmpty() && !t.startsWith("$key=")) {
                if (out.isNotEmpty()) out.append(' ')
                out.append(t)
            }
            return out.toString()
        }

        /** Return everything after the command head (the first space-separated token), or null.  */
        fun afterHead(cmd: String): String? {
            val sp = cmd.indexOf(' ')
            if (sp < 0) return null
            val tail = cmd.substring(sp + 1).trim { it <= ' ' }
            return tail.ifEmpty { null }
        }

        private fun positionalPair(cmd: String): Array<String>? {
            val sp = cmd.indexOf(' ')
            if (sp < 0) return null
            val tail = cmd.substring(sp + 1).trim { it <= ' ' }
            if (tail.isEmpty()) return null
            val parts = tail.split("\\s+".toRegex(), limit = 2).toTypedArray()
            if (parts.size < 2) return null
            return parts
        }
    }
}
