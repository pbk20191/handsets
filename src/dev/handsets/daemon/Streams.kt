package dev.handsets.daemon

import android.graphics.Point
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Bundle
import android.os.SystemClock
import android.view.Surface
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import java.nio.ByteBuffer

/**
 * Screen streams. Each owns its connection until the client leaves; frames
 * are encoded on Work and written from the main looper.
 */
internal object Streams {

    private const val IDLE_INTERVAL_MS = 500L

    private class Encoded(val seq: Long, val bytes: ByteArray)

    /** Last JPEG per (mirror, quality); main thread only. */
    private val jpegCache = HashMap<Pair<Screenshot.Mirror, Int>, Encoded>()

    /** Latest frame as JPEG, encoded once per frame for every stream on the same mirror and quality. */
    private suspend fun sharedJpeg(mirror: Screenshot.Mirror, quality: Int): ByteArray? {
        val key = mirror to quality
        val seq = mirror.frameSeq
        jpegCache[key]?.let { if (it.seq == seq) return it.bytes }
        val jpeg = withContext(Loop.Work) {
            try { mirror.encodeLatest("jpeg", quality) } catch (_: IllegalStateException) { null }
        } ?: return null
        jpegCache[key] = Encoded(seq, jpeg)
        return jpeg
    }

    /** `stream`: one JPEG per mirror frame (or per fps tick); re-sends on idle. */
    suspend fun jpeg(c: Conn, mirror: Screenshot.Mirror, quality: Int, fps: Int) {
        val wake = Channel<Unit>(Channel.CONFLATED)
        val l = Screenshot.FrameListener { wake.trySend(Unit) }
        mirror.subscribe(l)
        try {
            suspend fun sendOne() {
                // Skip encoding while the previous frame is still on its way.
                if (c.pendingBytes > 0) return
                c.sendLatest(0, sharedJpeg(mirror, quality) ?: return)
            }
            val intervalMs = if (fps > 0) 1000L / fps else 0L
            sendOne()
            var next = SystemClock.uptimeMillis() + intervalMs
            while (true) {
                // Bounded wait: the periodic re-send doubles as a keep-alive.
                withTimeoutOrNull(IDLE_INTERVAL_MS) { wake.receive() }
                if (intervalMs > 0) {
                    // fps cap: send on the first vsync at or after the slot.
                    var now = SystemClock.uptimeMillis()
                    while (now < next) now = Frames.next()
                    wake.tryReceive()
                    next = now + intervalMs
                }
                sendOne()
            }
        } finally {
            mirror.unsubscribe(l)
            if (!mirror.hasSubscribers()) jpegCache.keys.removeAll { it.first === mirror }
        }
    }

    /** `stream_tilejpeg`: keyframe then deltas; lossless, so every packet is sent. */
    suspend fun tiles(c: Conn, mirror: Screenshot.Mirror, quality: Int, tile: Int) {
        val ts = TileStreamer(mirror, quality, tile)
        val wake = Channel<Unit>(Channel.CONFLATED)
        val l = Screenshot.FrameListener { wake.trySend(Unit) }
        mirror.subscribe(l)
        try {
            var first = true
            while (true) {
                if (!first) withTimeoutOrNull(TileStreamer.IDLE_INTERVAL_MS) { wake.receive() }
                val force = first
                val body = withContext(Loop.Work) { ts.nextPacket(force) }
                first = false
                if (body != null) c.send(body)
            }
        } catch (t: Throwable) {
            if (t is kotlinx.coroutines.CancellationException || t is java.io.IOException) throw t
            t.printStackTrace()   // old TileStreamer logged and ended the stream without a frame
        } finally {
            mirror.unsubscribe(l)
        }
    }

    // ── H.264 ───────────────────────────────────────────────────────────

    private const val MIME_AVC = "video/avc"
    private const val COLOR_FORMAT_SURFACE = 0x7F000789
    private const val VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR = 16
    /** Stock software AVC encoders cap at 2048 on both axes. */
    private const val ENCODER_MAX_DIM = 2048

    private data class H264Key(val w: Int, val h: Int, val fps: Int, val kbps: Int, val gopSec: Int)

    /** Live encoders by parameters; main thread only. */
    private val hubs = HashMap<H264Key, H264Hub>()

    /** `keyframe`: IDR from every encoder; answers the number of stream connections. */
    fun requestKeyframeAll(): Int {
        for (hub in hubs.values) hub.requestKeyframe()
        return hubs.values.sumOf { it.subs.size }
    }

    /**
     * `stream_h264`. Connections asking for the same output size, fps, bitrate
     * and GOP share one encoder + VirtualDisplay; each access unit is copied
     * once and the same bytes go to every subscriber. The hub keeps SPS/PPS
     * and the frames since the last IDR, so a joiner (or a subscriber that
     * fell a full queue behind) restarts from that IDR at once, even on a
     * static screen where the encoder emits nothing new. The shared encoder
     * never waits for one slow client.
     */
    suspend fun h264(
        c: Conn, dm: DisplayManager, sourceSize: Point,
        targetLongEdge: Int, bitrateKbps0: Int, fps0: Int, gopSec0: Int,
    ) {
        val srcW = sourceSize.x
        val srcH = sourceSize.y
        if (srcW <= 0 || srcH <= 0) return c.send(err("no-source-size"))
        var scale = minOf(1f, targetLongEdge.toFloat() / maxOf(srcW, srcH))
        scale = minOf(scale, minOf(ENCODER_MAX_DIM.toFloat() / srcW, ENCODER_MAX_DIM.toFloat() / srcH))
        val key = H264Key(
            minOf(ENCODER_MAX_DIM, roundEven(maxOf(2, Math.round(srcW * scale)))),
            minOf(ENCODER_MAX_DIM, roundEven(maxOf(2, Math.round(srcH * scale)))),
            if (fps0 > 0) fps0 else 60,
            if (bitrateKbps0 > 0) bitrateKbps0 else 6000,
            if (gopSec0 > 0) gopSec0 else 1,
        )
        val sub = H264Sub(gopMax(key) + 56)
        var hub = hubs[key]
        if (hub == null) {
            hub = H264Hub(key, dm)
            hubs[key] = hub
            hub.subs.add(sub)
            try {
                // Codec + VirtualDisplay setup are synchronous binder calls.
                withContext(Loop.Blocking) { hub.start() }
                if (hub.released) hub.stopNow()
            } catch (t: Throwable) {
                if (t is kotlinx.coroutines.CancellationException) { hub.release(); throw t }
                hub.fail(t)
                hub.release()
            }
        } else {
            hub.join(sub)
        }
        try {
            for (frame in sub.ch) c.send(frame)
            sub.error?.let { c.send(it) }
        } finally {
            hub.leave(sub)
        }
    }

    private class H264Sub(queue: Int) {
        val ch = Channel<ByteArray>(queue)
        /** Skipping until the next IDR (no usable GOP when it joined or lagged). */
        var waitingIdr = false
        var error: ByteArray? = null
    }

    private class H264Hub(val key: H264Key, val dm: DisplayManager) {
        val subs = LinkedHashSet<H264Sub>()
        private var csd: ByteArray? = null
        /** The last IDR and every frame after it; empty when unknown or too long. */
        private val gop = ArrayList<ByteArray>()
        private var encoder: MediaCodec? = null
        private var surface: Surface? = null
        private var vd: VirtualDisplay? = null
        var released = false
            private set

        /** Release resources from start() that raced with an early release(). */
        fun stopNow() { released = false; release() }

        fun start() {
            val fmt = MediaFormat.createVideoFormat(MIME_AVC, key.w, key.h).apply {
                setInteger(MediaFormat.KEY_COLOR_FORMAT, COLOR_FORMAT_SURFACE)
                setInteger(MediaFormat.KEY_BIT_RATE, key.kbps * 1000)
                setInteger(MediaFormat.KEY_FRAME_RATE, key.fps)
                setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, key.gopSec)
                // VBR: the emulator's Codec2 encoder wedges on CBR + low-latency
                // flags. Prepend-SPS/PPS keys make it reject configure(); codec
                // config is emitted before the first IDR and cached for joiners.
                try { setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR) } catch (_: Throwable) {}
                // No B-frames: decode order == display order.
                try { setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0) } catch (_: Throwable) {}
            }
            val enc = MediaCodec.createEncoderByType(MIME_AVC)
            encoder = enc
            enc.setCallback(object : MediaCodec.Callback() {
                override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {}

                override fun onOutputBufferAvailable(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
                    if (released) return
                    var payload: ByteArray? = null
                    try {
                        if (released) return
                        val buf = codec.getOutputBuffer(index)
                        if (buf != null && info.size > 0) {
                            buf.position(info.offset)
                            buf.limit(info.offset + info.size)
                            payload = ByteArray(info.size).also { buf.get(it) }
                        }
                    } catch (_: Throwable) {
                        payload = null
                    } finally {
                        try { codec.releaseOutputBuffer(index, false) } catch (_: Throwable) {}
                    }
                    if (payload == null) return
                    if (released) return
                    if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) config(payload)
                    else frame(payload, info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0)
                }

                // Some encoders deliver SPS/PPS here rather than as a CODEC_CONFIG buffer.
                override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {
                    val b = concat(format.getByteBuffer("csd-0"), format.getByteBuffer("csd-1"))
                    if (b.isNotEmpty() && !released) config(b)
                }

                override fun onError(codec: MediaCodec, e: MediaCodec.CodecException) {
                    fail(e)
                    release()
                }
            }, Loop.mainHandler)
            enc.configure(fmt, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            val sf = enc.createInputSurface()
            surface = sf
            enc.start()
            vd = dm.createVirtualDisplay("hs-h264-${key.w}x${key.h}", key.w, key.h, 320, sf, VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR)
                ?: throw VdNull()
            println("h264: started ${key.w}x${key.h} @ ${key.kbps} kbps  fps=${key.fps}")
        }

        private fun config(b: ByteArray) {
            csd = b
            for (s in subs.toList()) s.ch.trySend(b)
        }

        private fun frame(b: ByteArray, idr: Boolean) {
            if (idr) gop.clear()
            if (idr || gop.isNotEmpty()) {
                if (gop.size < gopMax(key)) gop.add(b) else { gop.clear(); requestKeyframe() }
            }
            // Snapshot: a send can resume a subscriber inline that then leaves.
            for (s in subs.toList()) {
                if (s.waitingIdr) {
                    if (!idr) continue
                    s.waitingIdr = false
                }
                if (!s.ch.trySend(b).isSuccess) catchUp(s)
            }
        }

        /** Restart [s] from the cached SPS/PPS + GOP (drops whatever it had queued). */
        private fun catchUp(s: H264Sub) {
            while (s.ch.tryReceive().isSuccess) { /* drop the backlog */ }
            csd?.let { s.ch.trySend(it) }
            if (gop.isNotEmpty()) {
                for (f in gop) s.ch.trySend(f)
                s.waitingIdr = false
            } else {
                s.waitingIdr = true
                requestKeyframe()
            }
        }

        fun join(s: H264Sub) {
            subs.add(s)
            catchUp(s)
        }

        fun leave(s: H264Sub) {
            subs.remove(s)
            s.ch.close()
            if (subs.isEmpty()) release()
        }

        fun fail(t: Throwable) {
            val e = if (t is VdNull) err("createVirtualDisplay-null")
            else err("h264-failed:${t.javaClass.simpleName}:${t.message}").also { t.printStackTrace() }
            for (s in subs.toList()) { s.error = e; s.ch.close() }
        }

        fun requestKeyframe() {
            try {
                encoder?.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
            } catch (_: Throwable) {
                // Transient encoder state; the next request will do.
            }
        }

        fun release() {
            if (released) return
            released = true
            if (hubs[key] === this) hubs.remove(key)
            for (s in subs.toList()) s.ch.close()
            val v = vd
            val enc = encoder
            val sf = surface
            // Codec/display teardown is synchronous binder work: keep it off main.
            Loop.blockingExecutor.execute {
                try { v?.release() } catch (_: Throwable) {}
                enc?.let { e ->
                    try { e.stop() } catch (_: Throwable) {}
                    try { e.release() } catch (_: Throwable) {}
                }
                try { sf?.release() } catch (_: Throwable) {}
            }
        }
    }

    /** Frames one GOP can hold (fps × GOP seconds, a little slack), bounded for memory. */
    private fun gopMax(k: H264Key) = (k.fps * k.gopSec + 8).coerceIn(30, 600)

    private class VdNull : RuntimeException("createVirtualDisplay-null")

    private fun concat(a: ByteBuffer?, b: ByteBuffer?): ByteArray {
        val out = ByteArray((a?.remaining() ?: 0) + (b?.remaining() ?: 0))
        var p = 0
        if (a != null) { val n = a.remaining(); a.get(out, p, n); p += n }
        b?.get(out, p, b.remaining())
        return out
    }

    private fun roundEven(v: Int) = (v + 1) and 1.inv()

    private fun err(tail: String) = "ERR:$tail".toByteArray()
}
