package dev.handsets.daemon

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.runBlocking
import java.io.BufferedOutputStream
import java.io.DataOutputStream
import java.io.IOException
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
