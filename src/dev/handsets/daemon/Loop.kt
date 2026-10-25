package dev.handsets.daemon

import android.os.Handler
import android.os.Looper
import android.os.MessageQueue
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineExceptionHandler
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.android.asCoroutineDispatcher
import kotlinx.coroutines.asCoroutineDispatcher
import java.io.FileDescriptor
import kotlin.coroutines.EmptyCoroutineContext
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.ThreadFactory
import java.util.concurrent.atomic.AtomicInteger

/**
 * Threading model of the daemon.
 *
 *  - [Main]: the process main thread running `Looper.loop()`. Every socket,
 *    pipe, listener callback, timer and coroutine lives here unless it is
 *    explicitly moved off. Code running on Main must never block.
 *  - [Work]: a small fixed pool for CPU work (encoding, buffer copies, tree
 *    dumps) and binder calls that may take long. Results come back to Main.
 */
object Loop {

    lateinit var Main: CoroutineDispatcher
        private set

    /** The Work pool as a plain Executor, for Java callers. */
    val workExecutor: ExecutorService = Executors.newFixedThreadPool(
        minOf(4, Runtime.getRuntime().availableProcessors()).coerceAtLeast(2),
        daemonThreads("hs-work"),
    )

    val Work: CoroutineDispatcher = workExecutor.asCoroutineDispatcher()

    lateinit var mainHandler: Handler
        private set

    lateinit var scope: CoroutineScope
        private set

    /** Must run on the main thread after `Looper.prepareMainLooper()`. */
    fun init() {
        mainHandler = Handler(Looper.getMainLooper())
        Main = try {
            Dispatchers.Main.immediate.also { it.isDispatchNeeded(EmptyCoroutineContext) }
        } catch (t: Throwable) {
            // Factory missing from the jar: bind to the main looper directly.
            System.err.println("hsd: Dispatchers.Main unavailable ($t); using a looper handler")
            mainHandler.asCoroutineDispatcher("hs-main").immediate
        }
        scope = CoroutineScope(SupervisorJob() + Main + CoroutineExceptionHandler { _, t ->
            System.err.println("hsd: uncaught in coroutine: $t")
            t.printStackTrace()
        })
    }

    private fun daemonThreads(prefix: String): ThreadFactory {
        val n = AtomicInteger()
        return ThreadFactory { r ->
            Thread(r, "$prefix-${n.incrementAndGet()}").apply { isDaemon = true }
        }
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
