package dev.handsets.daemon

import android.view.Choreographer
import kotlinx.coroutines.delay
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume

/**
 * Vsync ticks on the main looper. Falls back to a 16 ms delay if the
 * Choreographer can't be created (no display connection).
 */
internal object Frames {

    private val choreographer: Choreographer? by lazy {
        try { Choreographer.getInstance() } catch (t: Throwable) {
            System.err.println("hsd: Choreographer unavailable ($t); pacing with delay(16)")
            null
        }
    }

    /** Suspends until the next vsync; returns its frame time in uptime milliseconds. */
    suspend fun next(): Long {
        val ch = choreographer ?: run {
            delay(16)
            return android.os.SystemClock.uptimeMillis()
        }
        return suspendCancellableCoroutine { k ->
            val cb = Choreographer.FrameCallback { nanos -> if (k.isActive) k.resume(nanos / 1_000_000) }
            ch.postFrameCallback(cb)
            k.invokeOnCancellation { Loop.mainHandler.post { ch.removeFrameCallback(cb) } }
        }
    }
}
