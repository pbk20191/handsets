package dev.handsets.daemon

import android.graphics.Bitmap
import java.io.ByteArrayOutputStream
import java.io.DataOutputStream
import java.io.IOException
import kotlin.math.min

/**
 * Tile-based JPEG-diff streamer.
 * 
 * Wire packet (length-prefixed in the existing TCP framing):
 * u8 type
 * 0x00 (DELTA)    : u16 num_tiles, repeated: (u16 idx, u16 w, u16 h, u32 jlen, bytes)
 * 0x01 (KEYFRAME) : u32 jlen, bytes  (full-frame JPEG)
 * 
 * Picks keyframe when ≥ 50% of tiles changed in one frame, otherwise emits
 * a delta containing only the changed tiles. Idle keep-alive (every
 * [.IDLE_INTERVAL_MS]) sends a 3-byte empty delta so the viewer
 * can detect disconnects without burning bandwidth on a static screen.
 */
internal class TileStreamer(private val mirror: Screenshot.Mirror, quality: Int, tileEdge: Int) {
    private val quality: Int
    private val tileEdge: Int

    // Per-stream state, lazily sized in initIfNeeded().
    private var srcW = -1
    private var srcH = -1
    private var tilesX = 0
    private var tilesY = 0
    private var totalTiles = 0
    private lateinit var prevHashes: IntArray
    private var tilePix: IntArray? = null
    private lateinit var changedIdx: IntArray
    private lateinit var changedW: IntArray
    private lateinit var changedH: IntArray

    private val packetBuf = ByteArrayOutputStream(64 * 1024)
    private val jpegScratch = ByteArrayOutputStream(32 * 1024)

    /** Next wire packet (Work thread). `forceKeyframe` for the first one.  */
    fun nextPacket(forceKeyframe: Boolean): ByteArray {
        synchronized(mirror.lockObject()) {
            return buildPacketUnderLock(forceKeyframe)
        }
    }

    /** Must be called while holding `mirror.lockObject()`.  */
    private fun buildPacketUnderLock(forceKeyframe: Boolean): ByteArray {
        val bmp = mirror.currentCached()
        if (bmp == null) {
            // No frame yet — send an empty delta so the viewer at least sees
            // liveness.
            return EMPTY_DELTA
        }
        val dispW = mirror.outW
        val dispH = mirror.outH
        initIfNeeded(dispW, dispH)

        // Hash every tile; collect changed.
        var changed = 0
        for (row in 0..<tilesY) {
            val ty = row * tileEdge
            val th = min(tileEdge, dispH - ty)
            for (col in 0..<tilesX) {
                val tx = col * tileEdge
                val tw = min(tileEdge, dispW - tx)
                bmp.getPixels(tilePix!!, 0, tw, tx, ty, tw, th)
                val hh: Int = Companion.arrayHash(tilePix!!, tw * th)
                val idx = row * tilesX + col
                if (forceKeyframe || hh != prevHashes[idx]) {
                    prevHashes[idx] = hh
                    changedIdx[changed] = idx
                    changedW[changed] = tw
                    changedH[changed] = th
                    changed++
                }
            }
        }

        val keyframe = forceKeyframe || (changed * 2 >= totalTiles)
        packetBuf.reset()
        val dos = DataOutputStream(packetBuf)
        try {
            if (keyframe) {
                writeKeyframe(dos, bmp, dispW, dispH)
            } else {
                writeDelta(dos, bmp, changed)
            }
        } catch (ioe: IOException) {
            // Buffer writes don't really throw IOException, but the contract
            // forces us to handle it.
            return EMPTY_DELTA
        }
        return packetBuf.toByteArray()
    }

    @Throws(IOException::class)
    private fun writeKeyframe(dos: DataOutputStream, bmp: Bitmap, dispW: Int, dispH: Int) {
        dos.writeByte(TYPE_KEYFRAME.toInt())
        jpegScratch.reset()
        val toEncode: Bitmap?
        var recycle = false
        if (bmp.getWidth() == dispW) {
            toEncode = bmp
        } else {
            // Cached has row-stride padding past the logical display width;
            // crop to the visible area before encoding.
            toEncode = Bitmap.createBitmap(bmp, 0, 0, dispW, dispH)
            recycle = true
        }
        toEncode.compress(Bitmap.CompressFormat.JPEG, quality, jpegScratch)
        if (recycle) toEncode.recycle()
        val jpeg = jpegScratch.toByteArray()
        dos.writeInt(jpeg.size)
        dos.write(jpeg)
    }

    @Throws(IOException::class)
    private fun writeDelta(dos: DataOutputStream, bmp: Bitmap, changed: Int) {
        dos.writeByte(TYPE_DELTA.toInt())
        dos.writeShort(changed)
        for (i in 0..<changed) {
            val idx = changedIdx[i]
            val tw = changedW[i]
            val th = changedH[i]
            val tx = (idx % tilesX) * tileEdge
            val ty = (idx / tilesX) * tileEdge
            val tileBmp = Bitmap.createBitmap(bmp, tx, ty, tw, th)
            jpegScratch.reset()
            tileBmp.compress(Bitmap.CompressFormat.JPEG, quality, jpegScratch)
            tileBmp.recycle()
            val jbytes = jpegScratch.toByteArray()
            dos.writeShort(idx)
            dos.writeShort(tw)
            dos.writeShort(th)
            dos.writeInt(jbytes.size)
            dos.write(jbytes)
        }
    }

    private fun initIfNeeded(w: Int, h: Int) {
        if (w == srcW && h == srcH && tilePix != null) return
        srcW = w
        srcH = h
        tilesX = (w + tileEdge - 1) / tileEdge
        tilesY = (h + tileEdge - 1) / tileEdge
        totalTiles = tilesX * tilesY
        prevHashes = IntArray(totalTiles)
        changedIdx = IntArray(totalTiles)
        changedW = IntArray(totalTiles)
        changedH = IntArray(totalTiles)
        tilePix = IntArray(tileEdge * tileEdge)
        println(
            ("tilejpeg: stream " + w + "x" + h + " tile=" + tileEdge
                    + " (" + tilesX + "x" + tilesY + " = " + totalTiles + " tiles), q=" + quality)
        )
    }

    init {
        this.quality = if (quality > 0 && quality <= 100) quality else 80
        this.tileEdge = if (tileEdge > 0) tileEdge else DEFAULT_TILE
    }

    companion object {
        const val IDLE_INTERVAL_MS: Long = 500L
        private const val DEFAULT_TILE = 128
        private const val TYPE_DELTA: Byte = 0
        private const val TYPE_KEYFRAME: Byte = 1

        private fun arrayHash(a: IntArray, count: Int): Int {
            var h = 1
            for (i in 0..<count) h = 31 * h + a[i]
            return h
        }

        /** Pre-built 3-byte empty-delta packet (type=0, num_tiles=0).  */
        private val EMPTY_DELTA = byteArrayOf(TYPE_DELTA, 0, 0)
    }
}
