package dev.handsets.daemon

import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Event-driven refresh of [State]'s device snapshot. [mark] (main thread)
 * restarts a short debounce; a heartbeat also refreshes when nothing has
 * happened for a while (battery level/charging have no event hook). The
 * snapshot is computed on Work and published on main.
 */
internal class StateRefresher(private val state: State, private val debounceMs: Long, private val heartbeatMs: Long) {

    private var pending: Job? = null
    private var debouncing = false
    private var computing = false
    private var dirtyAgain = false

    /** Main thread only. */
    fun start() = schedule(heartbeatMs, debounce = false)

    /**
     * Main thread only. Refreshes [debounceMs] after the first event of a
     * burst; later events in the burst don't push it back (so a steady event
     * stream can't starve subscribers), and events during a refresh queue one more.
     */
    fun mark() {
        when {
            debouncing -> {}
            computing -> dirtyAgain = true
            else -> schedule(debounceMs, debounce = true)
        }
    }

    private fun schedule(afterMs: Long, debounce: Boolean) {
        pending?.cancel()
        debouncing = debounce
        pending = Loop.scope.launch {
            delay(afterMs)
            debouncing = false
            computing = true
            try {
                val snap = withContext(Loop.Work) { state.computeDevice() }
                state.publish(snap)
            } catch (t: Throwable) {
                if (t is kotlinx.coroutines.CancellationException) throw t
            } finally {
                computing = false
            }
            if (dirtyAgain) {
                dirtyAgain = false
                schedule(debounceMs, debounce = true)
            } else {
                schedule(heartbeatMs, debounce = false)
            }
        }
    }
}
