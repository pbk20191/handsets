package dev.handsets.daemon

import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.CoroutineExceptionHandler
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import java.io.EOFException
import java.io.FileDescriptor
import java.io.IOException
import kotlin.coroutines.resume

/**
 * One client socket on the main looper. Non-blocking; every method must be
 * called on [Loop.Main].
 *
 * Wire framing is `uint32 BE length + payload` in both directions; [send]
 * adds the prefix, [sendRaw] doesn't.
 *
 * Output is queued and drained on EVENT_OUTPUT. [send] suspends once more
 * than [OUT_LIMIT] bytes are queued (backpressure). [sendLatest] is for
 * frames where only the newest matters: a frame still waiting in its slot is
 * replaced instead of queued.
 */
class Conn(private val fd: FileDescriptor) {

    val job: Job = SupervisorJob(Loop.scope.coroutineContext[Job])
    val scope = CoroutineScope(Loop.Main + job + CoroutineExceptionHandler { _, t ->
        // A stray failure in a connection's coroutine must never reach the
        // looper's uncaught handler (that would kill app_process).
        if (t !is IOException) System.err.println("hsd: connection coroutine failed: $t")
    })

    private var inBuf = ByteArray(8192)
    private var inStart = 0
    private var inEnd = 0

    private val outQueue = ArrayDeque<ByteArray>()
    private var outHead = 0          // bytes of outQueue.first() already written
    private var outBytes = 0L
    private val latest = HashMap<Int, ByteArray>()

    private var readWaiter: CancellableContinuation<Unit>? = null
    private val writeWaiters = ArrayList<CancellableContinuation<Unit>>()
    private var watching = 0
    private var inCallback = false
    var closed = false
        private set

    init {
        job.invokeOnCompletion { Loop.mainHandler.post { close() } }
    }

    // ── reading ─────────────────────────────────────────────────────────

    suspend fun readInt(): Int {
        ensure(4)
        val b = inBuf
        val i = inStart
        inStart += 4
        return ((b[i].toInt() and 0xff) shl 24) or ((b[i + 1].toInt() and 0xff) shl 16) or
            ((b[i + 2].toInt() and 0xff) shl 8) or (b[i + 3].toInt() and 0xff)
    }

    suspend fun readFully(n: Int): ByteArray {
        ensure(n)
        val out = inBuf.copyOfRange(inStart, inStart + n)
        inStart += n
        return out
    }

    /** Next bytes available (up to [max]); null on EOF. */
    suspend fun readChunk(max: Int): ByteArray? {
        if (inEnd == inStart) {
            inStart = 0; inEnd = 0
            if (!fill()) return null
        }
        val n = minOf(max, inEnd - inStart)
        val out = inBuf.copyOfRange(inStart, inStart + n)
        inStart += n
        return out
    }

    /** Reads and discards input until EOF, then closes. For one-way streams. */
    fun closeOnEof() {
        scope.launch {
            try { while (readChunk(64 * 1024) != null) { /* discard */ } } catch (_: IOException) {}
            close()
        }
    }

    /**
     * Suspends until the peer hangs up, without consuming input: if the peer
     * sends bytes instead (a pipelined command), this just never returns.
     */
    suspend fun awaitHangup() {
        val one = ByteArray(1)
        while (true) {
            if (closed) return
            if (inEnd > inStart) kotlinx.coroutines.awaitCancellation()
            try {
                val n = Os.recvfrom(fd, one, 0, 1, OsConstants.MSG_PEEK, null)
                if (n == 0) return
                kotlinx.coroutines.awaitCancellation()
            } catch (e: ErrnoException) {
                if (e.errno != OsConstants.EAGAIN) return
            }
            suspendCancellableCoroutine<Unit> { c ->
                readWaiter = c
                updateInterest()
                c.invokeOnCancellation { if (readWaiter === c) { readWaiter = null; updateInterest() } }
            }
        }
    }

    private suspend fun ensure(n: Int) {
        while (inEnd - inStart < n) {
            if (inBuf.size - inStart < n) {
                val nb = if (inBuf.size >= n) inBuf else ByteArray(maxOf(n, inBuf.size * 2))
                System.arraycopy(inBuf, inStart, nb, 0, inEnd - inStart)
                inEnd -= inStart; inStart = 0; inBuf = nb
            }
            if (!fill()) throw EOFException()
        }
    }

    /** Reads at least one byte into inBuf; false on EOF. */
    private suspend fun fill(): Boolean {
        while (true) {
            if (closed) throw EOFException()
            if (inEnd == inBuf.size) {
                if (inStart > 0) {
                    System.arraycopy(inBuf, inStart, inBuf, 0, inEnd - inStart)
                    inEnd -= inStart; inStart = 0
                } else inBuf = inBuf.copyOf(inBuf.size * 2)
            }
            try {
                val r = Os.read(fd, inBuf, inEnd, inBuf.size - inEnd)
                if (r == 0) return false
                inEnd += r
                return true
            } catch (e: ErrnoException) {
                if (e.errno != OsConstants.EAGAIN) return false
            }
            suspendCancellableCoroutine<Unit> { c ->
                readWaiter = c
                updateInterest()
                c.invokeOnCancellation { if (readWaiter === c) readWaiter = null }
            }
        }
    }

    // ── writing ─────────────────────────────────────────────────────────

    suspend fun send(payload: ByteArray) {
        enqueue(header(payload.size))
        enqueue(payload)
        drain()
        awaitBelow(OUT_LIMIT)
    }

    suspend fun sendRaw(bytes: ByteArray) {
        enqueue(bytes)
        drain()
        awaitBelow(OUT_LIMIT)
    }

    fun sendLatest(slot: Int, payload: ByteArray) {
        if (closed) return
        if (outQueue.isEmpty() && latest.isEmpty()) {
            enqueue(header(payload.size)); enqueue(payload)
            drain()
        } else {
            latest[slot] = payload
        }
    }

    /** Bytes still waiting to be written (a pending latest-slot frame counts as 1). */
    val pendingBytes: Long get() = outBytes + (if (latest.isEmpty()) 0 else 1)

    /** Suspends until everything queued is written. */
    suspend fun flush() = awaitBelow(0)

    private fun header(n: Int) = byteArrayOf((n ushr 24).toByte(), (n ushr 16).toByte(), (n ushr 8).toByte(), n.toByte())

    private fun enqueue(b: ByteArray) {
        if (closed) throw EOFException()
        if (b.isEmpty()) return
        outQueue.addLast(b)
        outBytes += b.size
    }

    private suspend fun awaitBelow(limit: Long) {
        while (outBytes > limit || (limit == 0L && latest.isNotEmpty())) {
            if (closed) throw EOFException()
            suspendCancellableCoroutine<Unit> { c ->
                writeWaiters.add(c)
                c.invokeOnCancellation { writeWaiters.remove(c) }
            }
        }
        if (closed) throw EOFException()
    }

    private fun drain() {
        while (!closed) {
            if (outQueue.isEmpty()) {
                if (latest.isEmpty()) break
                val it = latest.entries.iterator()
                val e = it.next(); it.remove()
                enqueue(header(e.value.size)); enqueue(e.value)
            }
            val b = outQueue.first()
            val n = try {
                Os.sendto(fd, b, outHead, b.size - outHead, MSG_NOSIGNAL, null, 0)
            } catch (e: ErrnoException) {
                if (e.errno == OsConstants.EAGAIN) break
                close(); return
            } catch (e: Exception) {
                close(); return
            }
            outHead += n
            outBytes -= n
            if (outHead == b.size) { outQueue.removeFirst(); outHead = 0 }
        }
        wakeWriters()
        updateInterest()
    }

    private fun wakeWriters() {
        if (writeWaiters.isEmpty()) return
        val ws = ArrayList(writeWaiters)
        writeWaiters.clear()
        for (w in ws) if (w.isActive) w.resume(Unit)
    }

    // ── event plumbing ──────────────────────────────────────────────────

    private fun wanted(): Int {
        if (closed) return 0
        var m = 0
        if (readWaiter != null) m = m or Io.IN
        if (outQueue.isNotEmpty() || latest.isNotEmpty()) m = m or Io.OUT
        return m
    }

    private fun updateInterest() {
        if (inCallback) return       // onEvents returns the new mask itself
        val m = wanted()
        if (m == watching) return
        watching = m
        if (m == 0) Io.unwatch(fd) else Io.watch(fd, m or Io.ERR, ::onEvents)
    }

    private fun onEvents(ev: Int): Int {
        if (ev and Io.ERR != 0) { Io.unwatch(fd); watching = 0; close(); return 0 }
        inCallback = true
        try {
            if (ev and Io.OUT != 0) drain()
            if (ev and Io.IN != 0) {
                val w = readWaiter
                readWaiter = null
                w?.resume(Unit)
            }
        } finally {
            inCallback = false
        }
        if (closed) return 0
        watching = wanted()
        return if (watching == 0) 0 else watching or Io.ERR
    }

    fun close() {
        if (closed) return
        closed = true
        if (watching != 0) { Io.unwatch(fd); watching = 0 }
        try { Os.close(fd) } catch (_: Throwable) {}
        outQueue.clear(); latest.clear(); outBytes = 0
        readWaiter?.let { readWaiter = null; it.cancel(EOFException()) }
        for (w in ArrayList(writeWaiters)) w.cancel(EOFException())
        writeWaiters.clear()
        job.cancel()
    }

    companion object {
        const val OUT_LIMIT = 4L * 1024 * 1024
        private const val MSG_NOSIGNAL = 0x4000   // linux; not exposed by OsConstants
    }
}
