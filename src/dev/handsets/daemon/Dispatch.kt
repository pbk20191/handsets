package dev.handsets.daemon

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.IOException
import java.nio.charset.StandardCharsets
import kotlin.system.exitProcess

/**
 * Per-connection command loop. Same order and framing as the old
 * thread-per-connection server: read a length-prefixed command, answer it,
 * repeat. Stream verbs own the socket until the client goes away.
 */
internal class Dispatch(private val ready: CompletableDeferred<Server>) {

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
                val cmd = String(c.readFully(len), StandardCharsets.UTF_8).trim { it <= ' ' }   // Java trim(): ASCII controls + space only
                val sp = cmd.indexOf(' ')
                val head = if (sp >= 0) cmd.substring(0, sp) else cmd
                when (head) {
                    "ping" -> { c.send(PONG); continue }
                    "quit" -> { c.send(BYE); c.flush(); exitProcess(0) }
                }
                val s = ready.await()
                when (head) {
                    "keyframe" -> c.send(Server.utf8("ok n=" + Streams.requestKeyframeAll()))
                    "state_watch" -> { stateWatch(c, s.handlers().state); return }
                    "clip_watch" -> {
                        c.closeOnEof()
                        procs(s).clipWatch(c, Server.longArg(cmd, "interval_ms", 500))
                        return
                    }
                    "stream", "stream_h264", "stream_tilejpeg" -> {
                        c.closeOnEof()
                        screenStream(c, s.handlers(), head, cmd)
                        return
                    }
                    "pull" -> transfers(s).pull(c, Server.parseFileArgs(cmd).path)
                    "dumpsys" -> procs(s).dumpsys(c, Server.afterHead(cmd))
                    "logcat" -> procs(s).logcat(c, Server.afterHead(cmd))
                    "shell" -> procs(s).shell(c, Server.afterHead(cmd))
                    "monitor" -> procs(s).monitor(c)
                    "push" -> c.send(Server.parseFileArgs(cmd).let { a ->
                        val path = a.path
                        if (path == null) Server.errBytes("push-needs-path")
                        else transfers(s).push(c, path, a.mode, a.size)
                    })
                    "install" -> c.send(Server.parseFileArgs(cmd).let { a ->
                        transfers(s).install(c, a.size, a.reinstall, a.grant)
                    })
                    "install_multi" -> c.send(installMulti(c, s, cmd))
                    "pm_uninstall" -> c.send(Server.positional(cmd)?.let { transfers(s).uninstall(it) }
                        ?: Server.errBytes("pm_uninstall-needs-pkg"))
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

    private var procs: ProcStream? = null
    private fun procs(s: Server) = procs ?: ProcStream(s.handlers().clip).also { procs = it }

    private var transfers: Transfers? = null
    private fun transfers(s: Server) = transfers
        ?: Transfers(s.handlers().sysCtx, s.handlers().installer).also { transfers = it }

    /** install_multi sizes=N0,N1,… [reinstall=1] [grant=1] */
    private suspend fun installMulti(c: Conn, s: Server, cmd: String): ByteArray {
        val base = Server.parseFileArgs(cmd)
        var sizes: LongArray? = null
        for (tok in cmd.split(Regex("""\s+"""))) {
            if (tok.startsWith("sizes=")) {
                sizes = try {
                    tok.substring("sizes=".length).split(",").map { it.toLong() }.toLongArray()
                } catch (e: NumberFormatException) {
                    return Server.errBytes("install_multi-bad-sizes")
                }
            }
        }
        if (sizes == null || sizes.isEmpty()) return Server.errBytes("install_multi-needs-sizes")
        return transfers(s).installMulti(c, sizes, base.reinstall, base.grant)
    }

    private var gestures: Gestures? = null
    private fun gestures(s: Server) = gestures ?: Gestures(s.handlers()).also { gestures = it }

    /** Pushes every fresh device snapshot; only the newest unsent one is kept. */
    private suspend fun stateWatch(c: Conn, state: State) {
        c.closeOnEof()
        withContext(Loop.Work) { state.snapshots() }
            .filterNotNull()
            .collect { c.sendLatest(0, it) }
    }

    private fun unknown(head: String) = Server.errBytes("UNKNOWN_CMD:$head")

    /** stream / stream_tilejpeg / stream_h264; all default to native resolution. */
    private suspend fun screenStream(c: Conn, h: Server.Handlers, head: String, cmd: String) {
        val prefix = when (head) { "stream" -> "stream-failed:"; "stream_tilejpeg" -> "tilejpeg-failed:"; else -> "h264-failed:" }
        try {
            val args = Server.parseScreenshotArgs(cmd)
            if (args.longEdge == 0 && !args.max) args.max = true
            when (head) {
                "stream" -> {
                    val mirror = withContext(Loop.Work) { h.shot.mirrorFor(args) }
                    Streams.jpeg(c, mirror, Server.clampQuality(args.quality), args.fps)
                }
                "stream_tilejpeg" -> {
                    val mirror = withContext(Loop.Work) { h.shot.mirrorFor(args) }
                    Streams.tiles(c, mirror, Server.clampQuality(args.quality), if (args.tile > 0) args.tile else 128)
                }
                else -> {
                    val src = h.shot.sourceSize()
                    val srcLong = maxOf(src.x, src.y)
                    val target = if (args.max) srcLong else if (args.longEdge > 0) minOf(args.longEdge, srcLong) else 768
                    Streams.h264(c, h.shot.displayManager(), src, target, args.bitrateKbps, args.fps, args.gopSec)
                }
            }
        } catch (t: Throwable) {
            if (t is kotlinx.coroutines.CancellationException || t is IOException) throw t
            c.send(Server.errBytes(prefix + t.javaClass.simpleName + ":" + t.message))
        }
    }

    private companion object {
        val PONG = "pong".toByteArray()
        val BYE = "bye".toByteArray()
        val GESTURES = setOf("tap", "swipe", "down", "move", "up", "scroll", "key", "text", "swipe_dir",
            "wait_for_idle", "wait_for_text", "wait_for_activity", "tap_and_dump", "tap_and_settle")
        /** Cheap, bounded calls answered straight from the looper. */
        val ON_MAIN = setOf("info", "getprop")
    }
}
