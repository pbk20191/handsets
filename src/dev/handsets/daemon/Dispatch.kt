package dev.handsets.daemon

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.launch
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
                    "state_watch" -> { stateWatch(c, s.handlers().state); return }
                    "stream", "stream_h264", "stream_tilejpeg", "clip_watch", "monitor" -> {
                        c.closeOnEof()
                        legacyOut(c) { out ->
                            when (head) {
                                "stream" -> s.runStream(cmd, out)
                                "stream_h264" -> s.runH264Stream(cmd, out)
                                "stream_tilejpeg" -> s.runTileStream(cmd, out)
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
                    in GESTURES -> c.send(gestures(s).handle(head, cmd)!!)
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

    private var gestures: Gestures? = null
    private fun gestures(s: Server) = gestures ?: Gestures(s.handlers()).also { gestures = it }

    /** Pushes every fresh device snapshot; only the newest unsent one is kept. */
    private suspend fun stateWatch(c: Conn, state: State) {
        c.closeOnEof()
        val l = State.SnapshotListener { json -> c.scope.launch { c.sendLatest(0, json) } }
        try {
            withContext(Loop.Work) { state.subscribe(l) }
            awaitCancellation()
        } finally {
            state.unsubscribe(l)
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
        val GESTURES = setOf("tap", "swipe", "down", "move", "up", "scroll", "key", "text", "swipe_dir",
            "wait_for_idle", "wait_for_text", "wait_for_activity", "tap_and_dump", "tap_and_settle")
        /** Cheap, bounded calls answered straight from the looper. */
        val ON_MAIN = setOf("info", "getprop", "wm_info")
    }
}
