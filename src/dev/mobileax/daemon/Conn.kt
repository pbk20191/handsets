package dev.mobileax.daemon

import android.net.LocalSocket
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
import java.nio.ByteBuffer
import kotlin.coroutines.resume

/**
 * One client socket on the main looper. Non-blocking; every method must be
 * called on [Loop.Main].
 *
 * Wire framing is `uint32 BE length + payload` in both directions; [send]
 * adds the prefix, [sendRaw] doesn't. Both queue the caller's array as is
 * (no copy), so it must not be changed afterwards.
 *
 * Output is queued and drained on EVENT_OUTPUT. [send] suspends once more
 * than [OUT_LIMIT] bytes are queued (backpressure). [sendLatest] is for
 * frames where only the newest matters: a frame still waiting in its slot is
 * replaced instead of queued.
 *
 * Owns [socket]: LocalSocket closes its fd when finalized, so the reference
 * is held until [close] for the whole session.
 */
class Conn(private val socket: LocalSocket) {

    private val fd: FileDescriptor = socket.fileDescriptor

    val job: Job = SupervisorJob(Loop.scope.coroutineContext[Job])
    val scope = CoroutineScope(Loop.Main.immediate + job + CoroutineExceptionHandler { _, t ->
        // A stray failure in a connection's coroutine must never reach the
        // looper's uncaught handler (that would kill app_process).
        if (t !is IOException) Logx.w("connection coroutine failed", t)
    })

    private var inBuf = ByteArray(8192)
    private var inStart = 0
    private var inEnd = 0

    /** Unsent output, oldest first; each buffer's position is how far it got. */
    private val outQueue = ArrayDeque<ByteBuffer>()
    private var outBytes = 0L
    private val latest = HashMap<Int, ByteArray>()

    private var readWaiter: CancellableContinuation<Unit>? = null
    private val writeWaiters = ArrayList<CancellableContinuation<Unit>>()
    private var watching = 0
    private var inCallback = false
    var closed = false
        private set

    init {
        job.invokeOnCompletion { Loop.post { close() } }
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
        if (n > inBuf.size) return readLarge(n)
        ensure(n)
        val out = inBuf.copyOfRange(inStart, inStart + n)
        inStart += n
        shrinkIfIdle()
        return out
    }

    /**
     * A frame bigger than the buffer: read the rest of it straight into its
     * own array instead of growing inBuf for it and then copying it out.
     */
    private suspend fun readLarge(n: Int): ByteArray {
        val out = ByteArray(n)
        var got = minOf(n, inEnd - inStart)
        System.arraycopy(inBuf, inStart, out, 0, got)
        inStart += got
        while (got < n) {
            val r = readSome(out, got, n - got)
            if (r == 0) throw EOFException()
            got += r
        }
        return out
    }

    /** Consumes [n] bytes without keeping them. */
    suspend fun skip(n: Int) {
        var left = n
        while (left > 0) {
            if (inEnd == inStart) {
                inStart = 0; inEnd = 0
                if (!fill()) throw EOFException()
            }
            val k = minOf(left, inEnd - inStart)
            inStart += k
            left -= k
        }
    }

    /** Drop a buffer grown for one large chunk once it has been consumed. */
    private fun shrinkIfIdle() {
        if (inStart == inEnd && inBuf.size > MAX_IDLE_BUF) {
            inBuf = ByteArray(8192); inStart = 0; inEnd = 0
        }
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
            suspendCancellableCoroutine { c ->
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
        if (closed) throw EOFException()
        if (inEnd == inBuf.size) {
            if (inStart > 0) {
                System.arraycopy(inBuf, inStart, inBuf, 0, inEnd - inStart)
                inEnd -= inStart; inStart = 0
            } else inBuf = inBuf.copyOf(inBuf.size * 2)
        }
        val r = readSome(inBuf, inEnd, inBuf.size - inEnd)
        inEnd += r
        return r > 0
    }

    /** Reads at least one byte into [dst], waiting for input; 0 on EOF or a socket error. */
    private suspend fun readSome(dst: ByteArray, off: Int, len: Int): Int {
        while (true) {
            if (closed) throw EOFException()
            try {
                return Os.read(fd, dst, off, len)
            } catch (e: ErrnoException) {
                if (e.errno != OsConstants.EAGAIN) return 0
            }
            suspendCancellableCoroutine { c ->
                readWaiter = c
                updateInterest()
                c.invokeOnCancellation { if (readWaiter === c) readWaiter = null }
            }
        }
    }

    // ── writing ─────────────────────────────────────────────────────────

    /** Sends `payload[off until off + len]` as one frame. */
    suspend fun send(payload: ByteArray, off: Int = 0, len: Int = payload.size - off) {
        enqueue(header(len))
        enqueue(payload, off, len)
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

    private fun enqueue(b: ByteArray, off: Int = 0, len: Int = b.size - off) {
        if (closed) throw EOFException()
        if (len == 0) return
        // A fresh wrapper per enqueue: shared arrays (END, PONG…) never share a position.
        outQueue.addLast(ByteBuffer.wrap(b, off, len))
        outBytes += len
    }

    private suspend fun awaitBelow(limit: Long) {
        while (outBytes > limit || (limit == 0L && latest.isNotEmpty())) {
            if (closed) throw EOFException()
            suspendCancellableCoroutine { c ->
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
                Os.sendto(fd, b, MSG_NOSIGNAL, null, 0)   // advances b's position
            } catch (e: ErrnoException) {
                if (e.errno == OsConstants.EAGAIN) break
                close(); return
            } catch (_: Exception) {
                close(); return
            }
            outBytes -= n
            if (!b.hasRemaining()) outQueue.removeFirst()
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
        try { socket.close() } catch (_: Throwable) {}
        outQueue.clear(); latest.clear(); outBytes = 0
        readWaiter?.let { readWaiter = null; it.cancel(EOFException()) }
        for (w in ArrayList(writeWaiters)) w.cancel(EOFException())
        writeWaiters.clear()
        job.cancel()
    }

    companion object {
        const val OUT_LIMIT = 4L * 1024 * 1024
        private const val MAX_IDLE_BUF = 64 * 1024
        private const val MSG_NOSIGNAL = 0x4000   // linux; not exposed by OsConstants
    }
}
