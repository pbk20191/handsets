package dev.mobileax.daemon

import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.time.Duration.Companion.milliseconds

/**
 * Event-driven refresh of [State]'s device snapshot: a short debounce after
 * [mark], plus a heartbeat when nothing has happened for a while (battery
 * level/charging have no event hook). Computed on Work, published on main.
 */
internal class StateRefresher(private val state: State, private val debounceMs: Long, private val heartbeatMs: Long) {

    /** Latest published snapshot; see [State.snapshots]. */
    val snapshots = MutableStateFlow<ByteArray?>(null)

    /** One pending "something changed"; extra marks collapse into it. */
    private val marks = Channel<Unit>(Channel.CONFLATED)
    private var loop: Job? = null

    /** Main thread only. */
    fun start() {
        if (loop == null) loop = Loop.scope.launch { run() }
    }

    /** Any thread. Something changed: refresh soon. */
    fun mark() {
        marks.trySend(Unit)
    }

    /**
     * Waits for a mark (or the heartbeat), refreshes [debounceMs] after the
     * first mark of a burst, then repeats. Marks during the debounce fold
     * into that refresh, so a steady event stream can't starve subscribers;
     * a mark during the refresh itself triggers one more.
     */
    private suspend fun run() {
        while (true) {
            val marked = withTimeoutOrNull(heartbeatMs.milliseconds) { marks.receive() } != null
            if (marked) {
                delay(debounceMs.milliseconds)
                marks.tryReceive()
            }
            try {
                val snap = withContext(Loop.Work) { state.computeDevice() }
                state.publish(snap)
            } catch (t: Throwable) {
                if (t is kotlinx.coroutines.CancellationException) throw t
                Logx.w("device snapshot failed", t)
            }
        }
    }
}
