package dev.handsets.daemon

import android.os.Handler
import android.os.Looper
import android.os.MessageQueue
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineExceptionHandler
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.MainCoroutineDispatcher
import kotlinx.coroutines.launch
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.android.asCoroutineDispatcher
import kotlinx.coroutines.asExecutor
import java.io.FileDescriptor
import java.util.concurrent.Executor
import kotlin.coroutines.EmptyCoroutineContext

/**
 * Threading model of the daemon.
 *
 *  - [Main]: the process main thread running `Looper.loop()`. Every socket,
 *    pipe, listener callback, timer and coroutine lives here unless it is
 *    explicitly moved off. Code running on Main must never block.
 *  - [Work], [Blocking], [FrameCopy]: views of the kotlinx-coroutines
 *    scheduler, so every off-main task shares one worker pool
 *    (`DefaultDispatcher-worker-N`). Work is CPU work and binder calls that
 *    may take long, capped so it can't hog the pool; Blocking is calls that
 *    wait on a peer (pipe-fed binder dumps, codec and VirtualDisplay setup
 *    and teardown); FrameCopy serializes mirror pixel copies, which block on
 *    the GPU fence. Results come back to Main.
 */
object Loop {

    /** The main looper. Dispatches (posts) unless you use `Main.immediate`. */
    lateinit var Main: MainCoroutineDispatcher
        private set

    val Work: CoroutineDispatcher = Dispatchers.Default.limitedParallelism(
        minOf(4, Runtime.getRuntime().availableProcessors()).coerceAtLeast(2), "work"
    )

    val Blocking: CoroutineDispatcher = Dispatchers.IO

    val FrameCopy: CoroutineDispatcher = Dispatchers.IO.limitedParallelism(1, "frame")

    val blockingExecutor: Executor = Blocking.asExecutor()

    /**
     * Only for framework APIs that take a Handler (listener registration,
     * MediaCodec callbacks). Everything else uses [Main] / `Main.immediate`.
     */
    internal lateinit var mainHandler: Handler
        private set

    lateinit var scope: CoroutineScope
        private set

    /** Must run on the main thread after `Looper.prepareMainLooper()`. */
    fun init() {
        mainHandler = Handler(Looper.getMainLooper())
        Main = try {
            Dispatchers.Main.also { it.immediate.isDispatchNeeded(EmptyCoroutineContext) }
        } catch (t: Throwable) {
            // Factory missing from the jar: bind to the main looper directly.
            Logx.bootWarn("Dispatchers.Main unavailable; using a looper handler", t)
            mainHandler.asCoroutineDispatcher("hs-main")
        }
        scope =
            CoroutineScope(SupervisorJob() + Main.immediate + CoroutineExceptionHandler { _, t ->
                Logx.e("uncaught in coroutine", t)
            })
    }

    /** Runs [block] on the main looper on a later turn (never inline). Any thread. */
    fun post(block: () -> Unit) {
        scope.launch(Main) { block() }
    }
}

/**
 * epoll on the main looper's MessageQueue. Callbacks run on the main thread;
 * returning 0 from a callback unregisters it, otherwise the returned mask is
 * the new interest set (same contract as [MessageQueue.OnFileDescriptorEventListener]).
 */
object Io {
    const val IN = MessageQueue.OnFileDescriptorEventListener.EVENT_INPUT
    const val OUT = MessageQueue.OnFileDescriptorEventListener.EVENT_OUTPUT
    const val ERR = MessageQueue.OnFileDescriptorEventListener.EVENT_ERROR

    private val queue: MessageQueue get() = Looper.getMainLooper().queue

    fun watch(fd: FileDescriptor, events: Int, cb: (events: Int) -> Int) {
        queue.addOnFileDescriptorEventListener(fd, events) { _, ev -> cb(ev) }
    }

    /** Safe from any thread; must happen before the fd is closed. */
    fun unwatch(fd: FileDescriptor) {
        if (fd.valid()) queue.removeOnFileDescriptorEventListener(fd)
    }
}
