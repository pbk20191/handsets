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

    /** Main thread only. */
    fun start() = schedule(heartbeatMs)

    /** Main thread only. */
    fun mark() = schedule(debounceMs)

    private fun schedule(afterMs: Long) {
        pending?.cancel()
        pending = Loop.scope.launch {
            delay(afterMs)
            try {
                val snap = withContext(Loop.Work) { state.computeDevice() }
                state.publish(snap)
            } catch (t: Throwable) {
                if (t is kotlinx.coroutines.CancellationException) throw t
            }
            schedule(heartbeatMs)
        }
    }
}
