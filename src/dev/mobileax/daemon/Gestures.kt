package dev.mobileax.daemon

import android.graphics.Rect
import android.os.SystemClock
import android.view.MotionEvent
import android.view.accessibility.AccessibilityNodeInfo
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import java.nio.charset.StandardCharsets
import kotlin.math.roundToInt
import kotlin.time.Duration.Companion.milliseconds

/**
 * Input verbs, `wait_for_*` and the tap-then-settle composites. Runs on the
 * main looper; gesture timing uses delay(), and each injectInputEvent (a
 * synchronous binder call) hops to Work.
 */
internal class Gestures(private val h: Server.Handlers) {

    private val waits get() = h.uiEvents.waits

    suspend fun handle(head: String, cmd: String): ByteArray? = when (head) {
        "tap", "swipe", "down", "move", "up", "scroll", "key", "text" -> input(head, cmd)
        "swipe_dir" -> swipeDir(cmd)
        "wait_for_idle" -> waitForIdle(cmd)
        "event_seq" -> utf8("seq=${waits.seq}")
        "settle" -> settle(cmd)
        "settle_dump" -> settleDump(cmd)
        "wait_for_text" -> waitForText(cmd)
        "wait_for_activity" -> waitForActivity(cmd)
        "tap_and_dump" -> tapAnd(cmd, dump = true)
        "tap_and_settle" -> tapAnd(cmd, dump = false)
        else -> null
    }

    // ── gestures ────────────────────────────────────────────────────────

    private suspend fun motion(down: Long, at: Long, action: Int, x: Int, y: Int) =
        withContext(Loop.Work) { h.input.motion(down, at, action, x, y) }

    /** Down, one 60 Hz frame, up — reads as a real tap to gesture detectors. */
    suspend fun tap(x: Int, y: Int) {
        val t = SystemClock.uptimeMillis()
        motion(t, t, MotionEvent.ACTION_DOWN, x, y)
        delay(16.milliseconds)
        motion(t, SystemClock.uptimeMillis(), MotionEvent.ACTION_UP, x, y)
    }

    /**
     * One MOVE per vsync, stamped with the frame time, positioned by elapsed
     * time; the last MOVE lands on the endpoint and UP shares its time.
     */
    suspend fun swipe(x1: Int, y1: Int, x2: Int, y2: Int, durMs0: Int) {
        val durMs = if (durMs0 <= 0) 500 else durMs0
        val t0 = SystemClock.uptimeMillis()
        motion(t0, t0, MotionEvent.ACTION_DOWN, x1, y1)
        var at: Long
        do {
            at = maxOf(Frames.next(), t0 + 1)
            val f = minOf(1f, (at - t0).toFloat() / durMs)
            motion(t0, at, MotionEvent.ACTION_MOVE, (x1 + (x2 - x1) * f).roundToInt(),
                (y1 + (y2 - y1) * f).roundToInt()
            )
        } while (at - t0 < durMs)
        motion(t0, at, MotionEvent.ACTION_UP, x2, y2)
    }

    private suspend fun input(head: String, cmd: String): ByteArray = try {
        if (head == "text") {
            val body = if (cmd.length > 5) cmd.substring(5) else ""
            withContext(Loop.Work) { h.input.text(body) }
            OK
        } else {
            val a = Server.parseInputArgs(cmd)
            when (head) {
                "tap" -> {
                    // Off-screen taps dispatch into nothing yet would report ok.
                    val sz = h.shot.sourceSize()
                    if (sz.x > 0 && sz.y > 0 && (a.x < 0 || a.x >= sz.x || a.y < 0 || a.y >= sz.y)) {
                        Server.errBytes("OFF_SCREEN:x=${a.x} y=${a.y} display=${sz.x}x${sz.y}")
                    } else { tap(a.x, a.y); OK }
                }
                "swipe" -> { swipe(a.x1, a.y1, a.x2, a.y2, if (a.dur > 0) a.dur else 300); OK }
                "down" -> { h.input.pointerDown(a.x, a.y); OK }
                "move" -> { h.input.pointerMove(a.x, a.y); OK }
                "up" -> { h.input.pointerUp(a.x, a.y); OK }
                "scroll" -> {
                    // Positive dy → swipe up → content scrolls up.
                    val dy = if (a.dy == 0) 1 else a.dy
                    swipe(a.x, a.y, a.x, a.y - dy, 120); OK
                }
                "key" -> when {
                    a.code != 0 -> { withContext(Loop.Work) { h.input.key(a.code) }; OK }
                    a.keyName != null -> { withContext(Loop.Work) { h.input.keyByName(a.keyName) }; OK }
                    else -> Server.errBytes("key-needs-name-or-code")
                }
                else -> Server.errBytes("unknown-input:$head")
            }
        }
    } catch (t: Throwable) {
        if (t is kotlinx.coroutines.CancellationException) throw t
        Server.errBytes("$head-failed:${t.javaClass.simpleName}:${t.message}")
    }

    private suspend fun swipeDir(cmd: String): ByteArray {
        val dir = Server.positional(cmd) ?: return Server.errBytes("swipe_dir-needs-direction")
        // 500 ms over 60% of the screen reads as a deliberate drag; dur=N for flings.
        val dur = Server.longArg(cmd, "dur", 500).toInt()
        val sz = h.shot.sourceSize()
        val w = sz.x
        val ht = sz.y
        if (w <= 0 || ht <= 0) return Server.errBytes("swipe_dir-no-display-size")
        val hi = 8
        val lo = 2
        // Horizontal swipes drift 12% vertically so list rows don't see a click.
        val drift = ht * 12 / 100
        val d = dir.lowercase()
        val (x1, y1, x2, y2) = when (d) {
            "left" -> listOf(w * hi / 10, ht / 2 - drift / 2, w * lo / 10, ht / 2 + drift / 2)
            "right" -> listOf(w * lo / 10, ht / 2 + drift / 2, w * hi / 10, ht / 2 - drift / 2)
            "up" -> listOf(w / 2, ht * hi / 10, w / 2, ht * lo / 10)
            "down" -> listOf(w / 2, ht * lo / 10, w / 2, ht * hi / 10)
            else -> return Server.errBytes("swipe_dir-bad-direction:$dir")
        }
        return try {
            swipe(x1, y1, x2, y2, dur)
            utf8("ok dir=$d x1=$x1 y1=$y1 x2=$x2 y2=$y2 dur=$dur")
        } catch (t: Throwable) {
            if (t is kotlinx.coroutines.CancellationException) throw t
            Server.errBytes("swipe-failed:${t.javaClass.simpleName}:${t.message}")
        }
    }

    // ── waits ───────────────────────────────────────────────────────────

    private suspend fun waitForIdle(cmd: String): ByteArray {
        val idle = Server.longArg(cmd, "idle_ms", Server.DEFAULT_IDLE_MS)
        val timeout = Server.longArg(cmd, "timeout_ms", Server.DEFAULT_TIMEOUT_MS)
        val start = System.currentTimeMillis()
        val quiet = waits.awaitIdle(idle, timeout)
        if (quiet < 0) return Server.errBytes("TIMEOUT:wait_for_idle:idle_ms=$idle timeout_ms=$timeout")
        // elapsed is the time spent waiting, as for the other waits; idle is
        // how long the screen had been quiet (often far longer than idle_ms).
        return utf8("ok elapsed=${System.currentTimeMillis() - start} idle=$quiet")
    }

    /** `settle since=SEQ`: see [Waits.awaitSettle]. */
    private suspend fun settle(cmd: String): ByteArray {
        val since = Server.longArg(cmd, "since", -1)
        if (since < 0) return Server.errBytes("BAD_ARG:settle-needs-since")
        val first = Server.longArg(cmd, "first_ms", 200)
        val idle = Server.longArg(cmd, "idle_ms", 120)
        val timeout = Server.longArg(cmd, "timeout_ms", 1500)
        val r = waits.awaitSettle(since, first, idle, timeout)
            ?: return Server.errBytes("TIMEOUT:settle:idle_ms=$idle timeout_ms=$timeout")
        return utf8("ok elapsed=${r.first} changed=${if (r.second) 1 else 0}")
    }

    /**
     * `settle_dump since=SEQ [first_ms] [idle_ms] [timeout_ms]`: settle as
     * `settle` does, then answer with the screen once it holds still, in one
     * round trip: `ok elapsed=N changed=0|1 moving=0|1` (moving: it never went
     * quiet), a newline, and the `dump` payload.
     *
     * Quiet is not still. A window sliding in sends no events while it moves,
     * and its nodes report where they were mid-slide; so while an app window
     * reaches past the screen's edge, wait for the next event (the slide's
     * end). Then two dumps [STILL_GAP_MS] apart must agree.
     */
    private suspend fun settleDump(cmd: String): ByteArray {
        val since = Server.longArg(cmd, "since", -1)
        if (since < 0) return Server.errBytes("BAD_ARG:settle_dump-needs-since")
        val first = Server.longArg(cmd, "first_ms", 200)
        val idle = Server.longArg(cmd, "idle_ms", 120)
        val timeout = Server.longArg(cmd, "timeout_ms", 1500)
        val start = System.currentTimeMillis()
        val left = { timeout - (System.currentTimeMillis() - start) }
        val r = waits.awaitSettle(since, first, idle, timeout)
        val changed = r?.second ?: true
        var dump = h.dumper.dumpAll()
        if (changed) {
            while (left() > 0) {
                if (h.dumper.sliding()) {
                    waits.awaitEvent(minOf(SLIDE_MS, left()))
                    dump = h.dumper.dumpAll()
                    continue
                }
                delay(STILL_GAP_MS.milliseconds)
                val again = h.dumper.dumpAll()
                val still = sansTimestamp(again) == sansTimestamp(dump)
                dump = again
                if (still) break
            }
        }
        val head = "ok elapsed=${System.currentTimeMillis() - start} changed=${if (changed) 1 else 0} moving=${if (r == null) 1 else 0}"
        return utf8("$head\n$dump")
    }

    private fun sansTimestamp(dump: String): String {
        val i = dump.indexOf(",\"windows\"")
        return if (i >= 0) dump.substring(i) else dump
    }

    private suspend fun waitForText(cmd: String): ByteArray {
        val text = Server.strArg(cmd, "text")
        if (text.isNullOrEmpty()) return Server.errBytes("wait_for_text-needs-text")
        val mode = Server.strArgOr(cmd, "match", "sub")
        val timeout = Server.longArg(cmd, "timeout_ms", Server.DEFAULT_TIMEOUT_MS)
        val scope = Server.strArg(cmd, "scope") ?: "all"
        val sel = Server.textSelector(text, mode)
        var found: AccessibilityNodeInfo? = null
        val elapsed = waits.awaitPredicate(timeout) {
            found = withContext(Loop.Work) { h.nodes.find(sel, scope) }
            found != null
        }
        if (elapsed < 0) return Server.errBytes("TIMEOUT:wait_for_text:$text")
        val r = Rect()
        found!!.getBoundsInScreen(r)
        return utf8("ok x=${r.left} y=${r.top} w=${r.width()} h=${r.height()} elapsed=$elapsed")
    }

    private suspend fun waitForActivity(cmd: String): ByteArray {
        val component = Server.strArg(cmd, "n") ?: return Server.errBytes("wait_for_activity-needs-n")
        val timeout = Server.longArg(cmd, "timeout_ms", Server.DEFAULT_TIMEOUT_MS)
        // Exact match, or same package: Android often resolves an alias to a
        // different concrete activity (.Settings → .homepage.SettingsHomepageActivity).
        val slash = component.indexOf('/')
        val pkgPrefix = if (slash >= 0) component.substring(0, slash + 1) else "$component/"
        val elapsed = waits.awaitPredicate(timeout) {
            val top = withContext(Loop.Work) { h.state.topActivity() }
            if (top.isEmpty() || (top.size >= 4 && String(top, 0, 4, StandardCharsets.UTF_8) == "ERR:")) false
            else String(top, StandardCharsets.UTF_8).let { it == component || it.startsWith(pkgPrefix) }
        }
        if (elapsed < 0) return Server.errBytes("TIMEOUT:wait_for_activity:$component")
        return utf8("ok elapsed=$elapsed")
    }

    private suspend fun tapAnd(cmd: String, dump: Boolean): ByteArray {
        val name = if (dump) "tap_and_dump" else "tap_and_settle"
        val x = Server.longArg(cmd, "x", -1).toInt()
        val y = Server.longArg(cmd, "y", -1).toInt()
        if (x < 0 || y < 0) return Server.errBytes("$name-needs-x-y")
        val idle = Server.longArg(cmd, "idle_ms", Server.DEFAULT_IDLE_MS)
        val timeout = Server.longArg(cmd, "timeout_ms", Server.DEFAULT_TAP_TIMEOUT_MS)
        waits.touch()   // measure idle from after the tap
        val start = System.currentTimeMillis()
        tap(x, y)
        val quiet = waits.awaitIdle(idle, timeout)
        if (!dump) {
            return if (quiet < 0) Server.errBytes("tap-then-timeout")
            else utf8("ok elapsed=${System.currentTimeMillis() - start} idle=$quiet")
        }
        if (quiet < 0) return Server.errBytes("TIMEOUT:tap_and_dump")
        return try {
            utf8(h.dumper.dumpActive())
        } catch (t: Throwable) {
            if (t is kotlinx.coroutines.CancellationException) throw t
            Server.errBytes("INTERNAL:dump-failed:${t.message}")
        }
    }

    private companion object {
        /** Longest wait for the event that ends a window slide. */
        const val SLIDE_MS = 700L

        /** Two dumps this far apart that agree mean the screen holds still. */
        const val STILL_GAP_MS = 40L

        val OK = "ok".toByteArray(StandardCharsets.UTF_8)
        fun utf8(s: String) = s.toByteArray(StandardCharsets.UTF_8)
    }
}
