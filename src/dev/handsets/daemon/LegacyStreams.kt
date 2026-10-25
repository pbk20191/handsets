package dev.handsets.daemon

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.runBlocking
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.concurrent.Executors

/**
 * Temporary bridge for handlers that still do blocking stream I/O. They run
 * on [LegacyIo.dispatcher] threads and reach the [Conn] through these
 * streams, which hop to the main looper for every read and flush.
 */
internal object LegacyIo {
    val dispatcher = Executors.newCachedThreadPool { r ->
        Thread(r, "hs-legacy").apply { isDaemon = true }
    }.asCoroutineDispatcher()

    fun out(c: Conn): DataOutputStream = DataOutputStream(BufferedOutputStream(ConnOutputStream(c), 64 * 1024))
    fun inp(c: Conn): DataInputStream = DataInputStream(ConnInputStream(c))

    fun <T> onMain(block: suspend () -> T): T = try {
        runBlocking(Loop.Main) { block() }
    } catch (e: CancellationException) {
        throw IOException("connection closed", e)
    }
}

private class ConnOutputStream(private val c: Conn) : OutputStream() {
    override fun write(b: Int) = write(byteArrayOf(b.toByte()), 0, 1)
    override fun write(b: ByteArray, off: Int, len: Int) {
        if (len == 0) return
        val copy = b.copyOfRange(off, off + len)
        LegacyIo.onMain { c.sendRaw(copy) }
    }
}

private class ConnInputStream(private val c: Conn) : InputStream() {
    override fun read(): Int {
        val b = ByteArray(1)
        return if (read(b, 0, 1) < 0) -1 else b[0].toInt() and 0xff
    }
    override fun read(b: ByteArray, off: Int, len: Int): Int {
        if (len == 0) return 0
        val chunk = LegacyIo.onMain { c.readChunk(len) } ?: return -1
        System.arraycopy(chunk, 0, b, off, chunk.size)
        return chunk.size
    }
}
