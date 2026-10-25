package dev.handsets.daemon

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.asExecutor
import java.util.concurrent.ExecutorService
import java.util.concurrent.SynchronousQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.Executors

/**
 * Executors besides [Loop.Work], for work that would otherwise sit on the
 * Work pool for long stretches and delay taps and dumps queued behind it.
 */
object Pools {

    /** Mirror pixel copies (one per frame, blocks on the GPU fence). */
    val frames: ExecutorService = Executors.newSingleThreadExecutor { r ->
        Thread(r, "hs-frames").apply { isDaemon = true }
    }

    /**
     * Calls that block for as long as a peer takes: binder dumps into a pipe
     * a slow client drains, codec and VirtualDisplay setup/teardown. Threads
     * exist only while such a call runs.
     */
    val blockingExecutor = Dispatchers.IO.asExecutor()

    val blocking = Dispatchers.IO
}
