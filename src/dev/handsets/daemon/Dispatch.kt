package dev.handsets.daemon

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.withContext
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.IOException
import java.nio.charset.StandardCharsets
import kotlin.system.exitProcess

/**
 * Per-connection command loop. Same order and framing as the old
 * thread-per-connection server: read a length-prefixed command, answer it,
 * repeat. Stream verbs own the socket until the client goes away.
 */
class Dispatch(private val ready: CompletableDeferred<Server>) {

    suspend fun session(c: Conn) {
        try {
            while (true) {
                val len = c.readInt()
                if (len <= 0 || len > Server.MAX_CMD) {
                    c.send(Server.errBytes("BAD_ARG:bad-length:$len"))
                    c.flush()
                    return
                }
                // UTF-8 so `clip_set <CJK>` / `text <CJK>` round-trip.
                val cmd = String(c.readFully(len), StandardCharsets.UTF_8).trim()
                val sp = cmd.indexOf(' ')
                val head = if (sp >= 0) cmd.substring(0, sp) else cmd
                when (head) {
                    "ping" -> { c.send(PONG); continue }
                    "quit" -> { c.send(BYE); c.flush(); exitProcess(0) }
                }
                val s = ready.await()
                when (head) {
                    "keyframe" -> {
                        H264Streamer.requestKeyframeAll()
                        c.send(Server.utf8("ok n=" + H264Streamer.activeCount()))
                    }
                    "stream", "stream_h264", "stream_tilejpeg", "state_watch", "clip_watch", "monitor" -> {
                        c.closeOnEof()
                        legacyOut(c) { out ->
                            when (head) {
                                "stream" -> s.runStream(cmd, out)
                                "stream_h264" -> s.runH264Stream(cmd, out)
                                "stream_tilejpeg" -> s.runTileStream(cmd, out)
                                "state_watch" -> s.runStateWatch(out)
                                "clip_watch" -> s.runClipWatch(cmd, out)
                                else -> s.runMonitor(out)
                            }
                        }
                        return
                    }
                    "pull" -> legacyOut(c) { s.runPull(cmd, it) }
                    "dumpsys" -> legacyOut(c) { s.runDumpsys(cmd, it) }
                    "logcat" -> legacyOut(c) { s.runLogcat(cmd, it) }
                    "shell" -> legacyOut(c) { s.runShell(cmd, it) }
                    "push" -> c.send(legacyIn(c) { s.runPush(cmd, it) })
                    "install" -> c.send(legacyIn(c) { s.runInstall(cmd, it) })
                    "install_multi" -> c.send(legacyIn(c) { s.runInstallMulti(cmd, it) })
                    in BLOCKING_WAITS -> c.send(withContext(LegacyIo.dispatcher) { s.respond(head, cmd) })
                    in ON_MAIN -> c.send(s.respond(head, cmd) ?: unknown(head))
                    else -> c.send(withContext(Loop.Work) { s.respond(head, cmd) } ?: unknown(head))
                }
            }
        } catch (_: IOException) {
            // client went away
        } catch (t: Throwable) {
            if (t !is kotlinx.coroutines.CancellationException) t.printStackTrace()
        } finally {
            c.close()
        }
    }

    private fun unknown(head: String) = Server.errBytes("UNKNOWN_CMD:$head")

    private suspend fun legacyOut(c: Conn, block: (DataOutputStream) -> Unit) {
        withContext(LegacyIo.dispatcher) {
            val out = LegacyIo.out(c)
            block(out)
            out.flush()
        }
    }

    private suspend fun legacyIn(c: Conn, block: (DataInputStream) -> ByteArray): ByteArray =
        withContext(LegacyIo.dispatcher) { block(LegacyIo.inp(c)) }

    private companion object {
        val PONG = "pong".toByteArray()
        val BYE = "bye".toByteArray()
        val BLOCKING_WAITS = setOf("wait_for_idle", "wait_for_text", "wait_for_activity", "tap_and_dump", "tap_and_settle")
        /** Cheap, bounded calls answered straight from the looper. */
        val ON_MAIN = setOf("info", "getprop", "wm_info")
    }
}
