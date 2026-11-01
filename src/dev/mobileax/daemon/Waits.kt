package dev.mobileax.daemon

import android.app.UiAutomation
import android.os.Looper
import android.view.accessibility.AccessibilityEvent
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.coroutines.resume
import kotlin.time.Duration.Companion.milliseconds

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
                    @Suppress("DEPRECATION") // the copy constructor needs API 30
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

    /**
     * @return ms the event stream has been quiet once it has been for
     * [idleMs] (not the time spent waiting: that can be 0), or -1 on timeout.
     */
    suspend fun awaitIdle(idleMs: Long, timeoutMs: Long): Long {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (true) {
            val now = System.currentTimeMillis()
            val since = now - lastEventTs
            if (since >= idleMs) return since
            if (now >= deadline) return -1
            withTimeoutOrNull(minOf(idleMs - since, deadline - now).milliseconds) { nextEvent() }
        }
    }

    /** Accessibility events (and [touch]es) seen so far; a mark for [awaitSettle]. */
    val seq: Long get() = eventSeq

    /**
     * Settle after an action taken once [sinceSeq] was read: wait up to
     * [firstMs] for the first event after it (an action whose effect has not
     * reached the tree yet looks quiet), then for [idleMs] of quiet.
     *
     * @return ms waited and whether any event arrived, or null on timeout.
     */
    suspend fun awaitSettle(sinceSeq: Long, firstMs: Long, idleMs: Long, timeoutMs: Long): Pair<Long, Boolean>? {
        val start = System.currentTimeMillis()
        val firstDeadline = start + minOf(firstMs, timeoutMs)
        while (eventSeq == sinceSeq) {
            val now = System.currentTimeMillis()
            if (now >= firstDeadline) break
            withTimeoutOrNull((firstDeadline - now).milliseconds) { nextEvent() }
        }
        val changed = eventSeq != sinceSeq
        if (!changed) return (System.currentTimeMillis() - start) to false
        val left = timeoutMs - (System.currentTimeMillis() - start)
        if (left <= 0 || awaitIdle(idleMs, left) < 0) return null
        return (System.currentTimeMillis() - start) to true
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
            if (eventSeq == seen) withTimeoutOrNull((deadline - now).milliseconds) { nextEvent() }
            seen = eventSeq
            if (safeCheck(check)) return System.currentTimeMillis() - start
        }
    }

    private suspend fun safeCheck(check: suspend () -> Boolean) = try { check() } catch (_: Throwable) { false }

    private suspend fun nextEvent() = suspendCancellableCoroutine { c ->
        waiters.add(c)
        c.invokeOnCancellation { waiters.remove(c) }
    }
}
