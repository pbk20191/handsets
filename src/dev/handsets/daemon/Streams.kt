package dev.handsets.daemon

import android.graphics.Point
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Bundle
import android.view.Surface
import kotlinx.coroutines.CompletableDeferred
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

    /** `stream`: one JPEG per mirror frame (or per fps tick); re-sends on idle. */
    suspend fun jpeg(c: Conn, mirror: Screenshot.Mirror, quality: Int, fps: Int) {
        val wake = Channel<Unit>(Channel.CONFLATED)
        val l = Screenshot.FrameListener { wake.trySend(Unit) }
        mirror.subscribe(l)
        try {
            suspend fun sendOne() {
                // Skip encoding while the previous frame is still on its way.
                if (c.pendingBytes > 0) return
                val jpeg = withContext(Loop.Work) {
                    try { mirror.encodeLatest("jpeg", quality) } catch (_: IllegalStateException) { null }
                } ?: return
                c.sendLatest(0, jpeg)
            }
            val intervalMs = if (fps > 0) 1000L / fps else 0L
            sendOne()
            var next = System.currentTimeMillis() + intervalMs
            while (true) {
                if (intervalMs > 0) {
                    val wait = next - System.currentTimeMillis()
                    if (wait > 0) withTimeoutOrNull(wait) { wake.receive() }
                    wake.tryReceive()
                    next = System.currentTimeMillis() + intervalMs
                } else {
                    // Bounded wait: the periodic re-send doubles as a keep-alive.
                    withTimeoutOrNull(IDLE_INTERVAL_MS) { wake.receive() }
                }
                sendOne()
            }
        } finally {
            mirror.unsubscribe(l)
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

    /** Running encoders, for the out-of-band `keyframe` verb. Main thread only. */
    private val active = LinkedHashSet<MediaCodec>()

    fun requestKeyframeAll(): Int {
        for (enc in active) {
            try {
                enc.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
            } catch (_: Throwable) {
                // Transient encoder state; the next request will do.
            }
        }
        return active.size
    }

    /**
     * `stream_h264`: a VirtualDisplay renders into the encoder's input
     * surface; access units go out one frame each, in order, never dropped.
     * An output buffer is released only after its frame is queued on the
     * socket, so a slow client stalls just this encoder.
     */
    suspend fun h264(
        c: Conn, dm: DisplayManager, sourceSize: Point,
        targetLongEdge: Int, bitrateKbps0: Int, fps0: Int, gopSec0: Int,
    ) {
        val bitrateKbps = if (bitrateKbps0 > 0) bitrateKbps0 else 6000
        val fps = if (fps0 > 0) fps0 else 60
        val gopSec = if (gopSec0 > 0) gopSec0 else 1
        val srcW = sourceSize.x
        val srcH = sourceSize.y
        if (srcW <= 0 || srcH <= 0) return c.send(err("no-source-size"))
        var scale = minOf(1f, targetLongEdge.toFloat() / maxOf(srcW, srcH))
        scale = minOf(scale, minOf(ENCODER_MAX_DIM.toFloat() / srcW, ENCODER_MAX_DIM.toFloat() / srcH))
        val outW = minOf(ENCODER_MAX_DIM, roundEven(maxOf(2, Math.round(srcW * scale))))
        val outH = minOf(ENCODER_MAX_DIM, roundEven(maxOf(2, Math.round(srcH * scale))))

        // (payload, output buffer index or -1 for codec config) in encoder order.
        val frames = Channel<Pair<ByteArray, Int>>(Channel.UNLIMITED)
        val failed = CompletableDeferred<Throwable>()
        var encoder: MediaCodec? = null
        var surface: Surface? = null
        var vd: VirtualDisplay? = null
        try {
            val fmt = MediaFormat.createVideoFormat(MIME_AVC, outW, outH).apply {
                setInteger(MediaFormat.KEY_COLOR_FORMAT, COLOR_FORMAT_SURFACE)
                setInteger(MediaFormat.KEY_BIT_RATE, bitrateKbps * 1000)
                setInteger(MediaFormat.KEY_FRAME_RATE, fps)
                setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, gopSec)
                // VBR: the emulator's Codec2 encoder wedges on CBR + low-latency
                // flags. Prepend-SPS/PPS keys make it reject configure(); each
                // session emits codec config before its first IDR anyway.
                try { setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR) } catch (_: Throwable) {}
                // No B-frames: decode order == display order.
                try { setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0) } catch (_: Throwable) {}
            }
            val enc = withContext(Loop.Work) { MediaCodec.createEncoderByType(MIME_AVC) }
            encoder = enc
            enc.setCallback(object : MediaCodec.Callback() {
                override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {}

                override fun onOutputBufferAvailable(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
                    val buf = try { codec.getOutputBuffer(index) } catch (t: Throwable) { null }
                    if (buf == null || info.size <= 0) {
                        try { codec.releaseOutputBuffer(index, false) } catch (_: Throwable) {}
                        return
                    }
                    buf.position(info.offset)
                    buf.limit(info.offset + info.size)
                    val payload = ByteArray(info.size)
                    buf.get(payload)
                    frames.trySend(payload to index)
                }

                // Some encoders deliver SPS/PPS here rather than as a CODEC_CONFIG buffer.
                override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {
                    val csd = concat(format.getByteBuffer("csd-0"), format.getByteBuffer("csd-1"))
                    if (csd.isNotEmpty()) frames.trySend(csd to -1)
                }

                override fun onError(codec: MediaCodec, e: MediaCodec.CodecException) {
                    failed.complete(e)
                }
            }, Loop.mainHandler)
            withContext(Loop.Work) {
                enc.configure(fmt, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            }
            surface = enc.createInputSurface()
            enc.start()
            active.add(enc)

            vd = dm.createVirtualDisplay("hs-h264-${outW}x$outH", outW, outH, 320, surface, VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR)
                ?: return c.send(err("createVirtualDisplay-null"))
            println("h264: started ${outW}x$outH @ $bitrateKbps kbps  fps=$fps")

            while (true) {
                val (payload, index) = kotlinx.coroutines.selects.select<Pair<ByteArray, Int>?> {
                    frames.onReceive { it }
                    failed.onAwait { throw it }
                } ?: break
                c.send(payload)
                if (index >= 0) try { enc.releaseOutputBuffer(index, false) } catch (_: Throwable) {}
            }
        } catch (t: Throwable) {
            if (t is kotlinx.coroutines.CancellationException || t is java.io.IOException) throw t
            try { c.send(err("h264-failed:${t.javaClass.simpleName}:${t.message}")) } catch (_: Throwable) {}
            t.printStackTrace()
        } finally {
            frames.close()
            encoder?.let { active.remove(it) }
            try { vd?.release() } catch (_: Throwable) {}
            encoder?.let { e ->
                try { e.stop() } catch (_: Throwable) {}
                try { e.release() } catch (_: Throwable) {}
            }
            try { surface?.release() } catch (_: Throwable) {}
        }
    }

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
