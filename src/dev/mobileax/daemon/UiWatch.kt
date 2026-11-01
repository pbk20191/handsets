package dev.mobileax.daemon

import android.os.SystemClock
import android.view.accessibility.AccessibilityEvent
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlin.time.Duration.Companion.milliseconds

/**
 * `ui_watch [min_ms=250]`: pushes the window tree (the `dump` payload)
 * whenever it changes, driven by accessibility events instead of polling.
 *
 * The first frame goes out at once. After that, an event that can change the
 * tree wakes the loop; it waits [QUICK_MS] for the burst to gather, and at
 * least `min_ms` since the last frame, then dumps. A dump equal to the last
 * one sent (bar its timestamp) is dropped, so idle screens and invisible
 * events cost nothing on the wire. Events that arrive while a dump is being
 * taken wake the loop again, so the last change is never lost. A slow reader
 * only ever gets the newest frame.
 *
 * Frames: `{"seq":N,"ev":EVENTS,"dump":{…}}`. `seq` counts frames on this
 * connection; `ev` is the daemon's event counter (as `event_seq` reports).
 * The loop runs only while a client watches; it ends when the client hangs up.
 */
internal object UiWatch {

    /** Lets a burst of events (one layout pass) arrive before dumping. */
    private const val QUICK_MS = 5L

    /** The client may ask for frames faster, but not faster than this. */
    private const val MIN_INTERVAL_FLOOR_MS = 50L

    suspend fun run(c: Conn, h: Server.Handlers, cmd: String) {
        val minMs = Server.longArg(cmd, "min_ms", 250).coerceAtLeast(MIN_INTERVAL_FLOOR_MS)
        c.closeOnEof()
        val wake = Channel<Unit>(Channel.CONFLATED)
        val consumer = UiEvents.Consumer { ev -> if (changesTree(ev.eventType)) wake.trySend(Unit) }
        h.uiEvents.subscribe(consumer)
        try {
            var seq = 0L
            var lastBody: String? = null
            var lastSent = 0L
            while (!c.closed) {
                if (seq > 0) {
                    wake.receive()
                    val wait = maxOf(QUICK_MS, lastSent + minMs - SystemClock.uptimeMillis())
                    delay(wait.milliseconds)
                }
                val dump = h.dumper.dumpAll()
                val body = withoutTimestamp(dump)
                if (body == lastBody) continue
                lastBody = body
                seq++
                lastSent = SystemClock.uptimeMillis()
                c.sendLatest(0, frame(seq, h.uiEvents.waits.seq, dump))
            }
        } finally {
            h.uiEvents.unsubscribe(consumer)
        }
    }

    private fun changesTree(type: Int): Boolean = when (type) {
        AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED,
        AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED,
        AccessibilityEvent.TYPE_WINDOWS_CHANGED,
        AccessibilityEvent.TYPE_VIEW_TEXT_CHANGED,
        AccessibilityEvent.TYPE_VIEW_SCROLLED -> true
        else -> false
    }

    /** The dump without its leading `"ts":…`, to compare two dumps. */
    private fun withoutTimestamp(dump: String): String {
        val i = dump.indexOf(",\"windows\"")
        return if (i >= 0) dump.substring(i) else dump
    }

    private fun frame(seq: Long, events: Long, dump: String): ByteArray =
        "{\"seq\":$seq,\"ev\":$events,\"dump\":$dump}".toByteArray(Charsets.UTF_8)
}
