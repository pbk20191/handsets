package dev.handsets.daemon

import android.app.UiAutomation
import android.os.Looper
import android.view.accessibility.AccessibilityEvent
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.coroutines.resume

/**
 * Single owner of `UiAutomation.setOnAccessibilityEventListener` (it only
 * takes one listener) fanning events out to [State] and [waits]. Events are
 * handled on the main looper.
 */
class UiEvents(private val ua: UiAutomation) {

    fun interface Consumer {
        fun onEvent(ev: AccessibilityEvent)
    }

    val waits = Waits()
    private val consumers = ArrayList<Consumer>()
    private var installed = false

    init {
        Loop.post { install() }
    }

    /** Main thread only. */
    fun subscribe(c: Consumer) {
        consumers.add(c)
    }

    private fun install() {
        if (installed) return
        try {
            ua.serviceInfo?.let { info ->
                info.eventTypes = info.eventTypes or AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED or
                    AccessibilityEvent.TYPE_WINDOWS_CHANGED or AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED
                ua.serviceInfo = info
            }
            ua.setOnAccessibilityEventListener { ev ->
                if (Looper.myLooper() == Looper.getMainLooper()) dispatch(ev)
                else {
                    val copy = AccessibilityEvent.obtain(ev)
                    Loop.post { dispatch(copy) }
                }
            }
            installed = true
        } catch (t: Throwable) {
            Logx.w("UiEvents install failed", t)
        }
    }

    private fun dispatch(ev: AccessibilityEvent) {
        waits.onEvent()
        for (c in consumers) {
            try { c.onEvent(ev) } catch (_: Throwable) {}
        }
    }
}

/**
 * "Wait until N ms of quiet" and "wait until this predicate holds, rechecking
 * on every accessibility event". Main thread only; nothing blocks.
 */
class Waits {

    private var lastEventTs = System.currentTimeMillis()
    private var eventSeq = 0L
    private val waiters = ArrayList<CancellableContinuation<Unit>>()

    fun onEvent() = touch()

    /** Marks "an action just happened" so idle is measured from now. */
    fun touch() {
        lastEventTs = System.currentTimeMillis()
        eventSeq++
        if (waiters.isEmpty()) return
        val ws = ArrayList(waiters)
        waiters.clear()
        for (w in ws) if (w.isActive) w.resume(Unit)
    }

    /** @return ms the event stream has been idle, or -1 on timeout. */
    suspend fun awaitIdle(idleMs: Long, timeoutMs: Long): Long {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (true) {
            val now = System.currentTimeMillis()
            val since = now - lastEventTs
            if (since >= idleMs) return since
            if (now >= deadline) return -1
            withTimeoutOrNull(minOf(idleMs - since, deadline - now)) { nextEvent() }
        }
    }

    /** @return ms until [check] first held, or -1 on timeout. */
    suspend fun awaitPredicate(timeoutMs: Long, check: suspend () -> Boolean): Long {
        val start = System.currentTimeMillis()
        val deadline = start + timeoutMs
        var seen = eventSeq
        if (safeCheck(check)) return 0
        while (true) {
            val now = System.currentTimeMillis()
            if (now >= deadline) return -1
            // A check may suspend (node lookup on Work): if events arrived
            // meanwhile, recheck at once instead of waiting for the next one.
            if (eventSeq == seen) withTimeoutOrNull(deadline - now) { nextEvent() }
            seen = eventSeq
            if (safeCheck(check)) return System.currentTimeMillis() - start
        }
    }

    private suspend fun safeCheck(check: suspend () -> Boolean) = try { check() } catch (_: Throwable) { false }

    private suspend fun nextEvent() = suspendCancellableCoroutine<Unit> { c ->
        waiters.add(c)
        c.invokeOnCancellation { waiters.remove(c) }
    }
}
