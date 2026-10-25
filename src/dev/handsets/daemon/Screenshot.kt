package dev.handsets.daemon

import android.app.UiAutomation
import android.content.Context
import android.content.ContextWrapper
import android.graphics.Bitmap
import android.graphics.Bitmap.CompressFormat
import android.graphics.PixelFormat
import android.graphics.Point
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.Image
import android.media.ImageReader
import android.media.ImageReader.OnImageAvailableListener
import android.view.Display
import dev.handsets.daemon.Binders.systemContext
import dev.handsets.daemon.Loop.mainHandler
import java.io.BufferedReader
import java.io.ByteArrayOutputStream
import java.io.InputStreamReader
import java.nio.charset.StandardCharsets
import java.util.concurrent.CopyOnWriteArrayList
import kotlin.concurrent.Volatile
import kotlin.math.max
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/**
 * Fast screenshot via a VirtualDisplay that mirrors the default display into
 * an ImageReader at the output resolution. Caller-controlled args:
 * size    — long-edge in pixels (default 768)
 * quality — JPEG quality 1..100 (default 80, ignored for PNG)
 * format  — "jpeg" (default), "webp", or "png"
 * max=1   — shortcut for native resolution
 * 
 * Mirror is recreated on size change (~50 ms one-time cost). Encode-only
 * changes (quality, format) are free.
 */
internal class Screenshot(private val ua: UiAutomation) {
    private val sourceSize: Point // (0,0) if lookup failed
    private val displayManager: DisplayManager? // null if framework init failed
    private var mirrorAvailable = false
    private val mirrorsMutex = Mutex()

    // Insertion-ordered cache keyed by long-edge resolution.
    // First call for a size pays ~50 ms; subsequent calls are warm.
    private val mirrors: LinkedHashMap<Int?, Mirror?> =
        object : LinkedHashMap<Int?, Mirror?>(8, 0.75f, true) {
            override fun removeEldestEntry(e: MutableMap.MutableEntry<Int?, Mirror?>): Boolean {
                if (size > MAX_CACHED_MIRRORS) {
                    e.value?.let { m -> Loop.scope.launch { m.close() } }
                    return true
                }
                return false
            }
        }

    /** Package-private: H264Streamer reuses these to build its own
     * MediaCodec-backed VirtualDisplay without re-running framework init.  */
    fun displayManager(): DisplayManager = displayManager!!

    fun sourceSize(): Point {
        return sourceSize
    }

    init {
        this.sourceSize = lookupSourceSize()
        var dm: DisplayManager? = null
        try {
            dm = initDisplayManager()
        } catch (t: Throwable) {
            System.err.println("screenshot: DisplayManager init failed: " + t)
        }
        this.displayManager = dm
    }

    /** Creates the default 768 px mirror; call once after construction. */
    suspend fun probeMirror() {
        val dm = displayManager
        var ok = false
        if (dm != null) {
            try {
                val m: Mirror = Mirror.create(dm, sourceSize, 768)
                mirrorsMutex.withLock { mirrors.put(768, m) }
                ok = true
                println(
                    ("screenshot: mirror " + m.outW + "x" + m.outH
                            + " of display " + sourceSize.x + "x" + sourceSize.y)
                )
            } catch (t: Throwable) {
                System.err.println("screenshot: mirror probe failed: " + t)
            }
        }
        this.mirrorAvailable = ok
    }

    suspend fun capture(args: CaptureArgs): ByteArray? {
        val longEdge = resolveLongEdge(args)
        val quality: Int = clampQuality(args.quality)
        val png: Boolean = isPng(args.format)

        var result: ByteArray
        if (mirrorAvailable) {
            try {
                result = mirrorCapture(longEdge, quality, args.format)
            } catch (t: Throwable) {
                System.err.println("screenshot: mirror.capture failed, falling back: " + t)
                result = withContext(Loop.Work) { fallbackCapture(longEdge, quality, args.format) }
            }
        } else {
            result = withContext(Loop.Work) { fallbackCapture(longEdge, quality, args.format) }
        }

        // Optional FLAG_SECURE detection. It shells out to dumpsys, so keep
        // the hot screenshot path pure unless the caller explicitly asks.
        val threshold = if (png) 32 * 1024 else 12 * 1024
        if (args.secureCheck && result.size < threshold) {
            val secureWin: String? = withContext(Loop.Work) { findSecureWindow() }
            if (secureWin != null) {
                return ("ERR:secure-window:" + secureWin)
                    .toByteArray(StandardCharsets.UTF_8)
            }
        }
        return result
    }

    private suspend fun mirrorCapture(longEdge: Int, quality: Int, format: String?): ByteArray =
        mirrorAt(longEdge).encodeLatest(format, quality)

    private suspend fun mirrorAt(longEdge: Int): Mirror = mirrorsMutex.withLock {
        mirrors[longEdge] ?: Mirror.create(displayManager!!, sourceSize, longEdge).also { mirrors[longEdge] = it }
    }

    private fun fallbackCapture(longEdge: Int, quality: Int, format: String?): ByteArray {
        var full: Bitmap? = null
        var scaled: Bitmap? = null
        try {
            full = ua.takeScreenshot()
            if (full == null) return "ERR:screenshot-null".toByteArray()
            val currentLong = max(full.getWidth(), full.getHeight())
            val s = longEdge.toFloat() / currentLong
            if (s < 1f) {
                val tw = max(1, Math.round(full.getWidth() * s))
                val th = max(1, Math.round(full.getHeight() * s))
                scaled = Bitmap.createScaledBitmap(full, tw, th, true)
            }
            val toEncode = if (scaled != null) scaled else full
            return encode(toEncode, format, quality)
        } catch (t: Throwable) {
            return (("ERR:screenshot-failed:" + t.javaClass.getSimpleName()
                    + ":" + t.message)).toByteArray()
        } finally {
            if (scaled != null) scaled.recycle()
            if (full != null) full.recycle()
        }
    }

    private fun resolveLongEdge(args: CaptureArgs): Int {
        if (args.max) return max(sourceSize.x, sourceSize.y)
        var v = if (args.longEdge > 0) args.longEdge else 768
        if (sourceSize.x > 0) {
            val srcLong = max(sourceSize.x, sourceSize.y)
            if (v > srcLong) v = srcLong
        }
        return v
    }

    // ---- value object ----
    class CaptureArgs {
        var longEdge: Int = 0 // 0 means "default" (768)
        var quality: Int = JPEG_DEFAULT_Q
        var format: String = "jpeg"
        var max: Boolean = false
        var fps: Int = 0 // 0 = uncapped (stream command only)
        var bitrateKbps: Int = 0 // 0 = default (6000 for stream_h264)
        var tile: Int = 0 // 0 = default (128 for stream_tilejpeg)
        var gopSec: Int = 0 // 0 = default (1 for stream_h264)
        var secureCheck: Boolean = false
    }

    // ---- streaming hook ----
    /** Notified on the mirror's listener thread whenever a fresh frame has
     * landed in `cached`. Implementations MUST be non-blocking.  */
    fun interface FrameListener {
        fun onFrame()
    }

    /**
     * Returns the (possibly-newly-created) mirror for the resolution implied
     * by `args`. Used by the stream command to subscribe to frame events
     * on the same cached mirror the per-call screenshot path uses.
     */
    suspend fun mirrorFor(args: CaptureArgs): Mirror {
        check(mirrorAvailable) { "mirror unavailable" }
        return mirrorAt(resolveLongEdge(args))
    }

    // ---- mirror pipeline ----
    internal class Mirror private constructor(
        val reader: ImageReader,
        val vd: VirtualDisplay,
        @JvmField val outW: Int,
        @JvmField val outH: Int
    ) {
        /** Guards `cached` (and its stride) for readers and the frame swap. */
        private val frameMutex = Mutex()

        /** Serializes pixel copies into `scratch` against each other and close(). */
        private val absorbMutex = Mutex()

        // COW so fanout doesn't need the lock and listeners can come and go safely.
        private val subscribers = CopyOnWriteArrayList<FrameListener>()

        fun subscribe(l: FrameListener?) {
            subscribers.add(l)
        }

        fun unsubscribe(l: FrameListener?) {
            subscribers.remove(l)
        }

        fun hasSubscribers(): Boolean {
            return !subscribers.isEmpty()
        }

        /** Runs [block] with the latest frame (null before the first) and its stride width in pixels. */
        suspend fun <T> withFrame(block: (Bitmap?, Int) -> T): T =
            frameMutex.withLock { block(cached, cachedRowPix) }

        // The listener thread does the expensive GPU-fence-blocking
        // copyPixelsFromBuffer into `scratch` *without* holding the lock, then
        // briefly takes the lock just to swap scratch <-> cached. Capture
        // threads only ever read `cached` under the lock and never wait on
        // GPU work.
        /** Bumped each time `cached` is replaced; lets streams share one encode per frame.  */
        @Volatile
        var frameSeq: Long = 0
        private var cached: Bitmap? = null // most recent stable frame at stride width
        private var cachedRowPix = 0
        private var scratch: Bitmap? = null // owned by listener thread; swapped in & out
        private var scratchRowPix = 0

        suspend fun encodeLatest(format: String?, quality: Int): ByteArray = frameMutex.withLock {
            withContext(Loop.Work) {
                val c = checkNotNull(cached) { "no frame yet" }
                val crop = cachedRowPix != outW
                val toEncode = if (crop) Bitmap.createBitmap(c, 0, 0, outW, outH) else c
                try {
                    val baos = ByteArrayOutputStream(64 * 1024)
                    val cf: CompressFormat = compressFormat(format)
                    toEncode.compress(cf, if (cf == CompressFormat.PNG) 100 else quality, baos)
                    baos.toByteArray()
                } finally {
                    if (crop) toEncode.recycle()
                }
            }
        }

        // Main thread only: one absorb in flight; frames arriving meanwhile
        // collapse into a single re-acquire of the latest image.
        private var absorbing = false
        private var absorbPending = false

        fun onImageAvailable() {
            if (absorbing) {
                absorbPending = true
                return
            }
            val img: Image = try {
                reader.acquireLatestImage()
            } catch (t: Throwable) {
                null
            } ?: return
            absorbing = true
            // FrameCopy: ~16 ms GPU-fence copies per frame must not queue
            // ahead of taps and dumps on the Work pool.
            Loop.scope.launch(Loop.FrameCopy) {
                try {
                    absorb(img)
                } catch (ignored: Throwable) {
                } finally {
                    img.close()
                }
                withContext(Loop.Main) {
                    absorbing = false
                    if (absorbPending) {
                        absorbPending = false
                        onImageAvailable()
                    }
                }
            }
        }

        private suspend fun absorb(img: Image) = absorbMutex.withLock {
            val p = img.getPlanes()[0]
            val rowPix = p.getRowStride() / max(1, p.getPixelStride())
            val h = img.getHeight()

            // Resize the private scratch, then do the GPU-fence-blocking copy
            // (~16 ms on a vsync-locked producer) without holding frameMutex.
            val sc = scratch?.takeIf { scratchRowPix == rowPix && it.getHeight() == h }
                ?: Bitmap.createBitmap(rowPix, h, Bitmap.Config.ARGB_8888).also {
                    scratch?.recycle()
                    scratch = it
                    scratchRowPix = rowPix
                }
            sc.copyPixelsFromBuffer(p.getBuffer())

            frameMutex.withLock {
                val oldCached = cached
                val oldRowPix = cachedRowPix
                cached = scratch
                cachedRowPix = scratchRowPix
                scratch = oldCached // reuse previous cached as next scratch
                scratchRowPix = oldRowPix
                frameSeq++
            }

            // Listeners must be non-blocking.
            for (l in subscribers) {
                try {
                    l.onFrame()
                } catch (ignored: Throwable) {
                }
            }
        }

        suspend fun close() {
            try {
                reader.setOnImageAvailableListener(null, null)
            } catch (ignored: Throwable) {
            }
            try {
                vd.release()
            } catch (ignored: Throwable) {
            }
            try {
                reader.close()
            } catch (ignored: Throwable) {
            }
            absorbMutex.withLock {
                frameMutex.withLock {
                    cached?.recycle()
                    cached = null
                    scratch?.recycle()
                    scratch = null
                }
            }
        }

        companion object {
            suspend fun create(dm: DisplayManager, sourceSize: Point, targetLongEdge: Int): Mirror {
                var srcW = sourceSize.x
                var srcH = sourceSize.y
                if (srcW <= 0 || srcH <= 0) {
                    val d = dm.getDisplay(Display.DEFAULT_DISPLAY)
                    val p = Point()
                    d.getRealSize(p)
                    srcW = p.x
                    srcH = p.y
                }

                val longEdge = max(srcW, srcH)
                var scale = targetLongEdge.toFloat() / longEdge
                if (scale > 1f) scale = 1f
                val outW = max(1, Math.round(srcW * scale))
                val outH = max(1, Math.round(srcH * scale))

                val reader = ImageReader.newInstance(outW, outH, PixelFormat.RGBA_8888, 3)

                var flags: Int
                try {
                    flags =
                        DisplayManager::class.java.getDeclaredField("VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR")
                            .getInt(null)
                } catch (t: Throwable) {
                    flags = 16
                }

                // Frame-available callbacks arrive on the main looper; the pixel
                // copy (blocks on the GPU fence) runs on the Work pool.
                val handler = mainHandler

                // Build the mirror shell so we can register the listener before
                // the VirtualDisplay starts producing frames.
                val holder = arrayOfNulls<Mirror>(1)
                reader.setOnImageAvailableListener(object : OnImageAvailableListener {
                    override fun onImageAvailable(r: ImageReader?) {
                        val mr = holder[0]
                        if (mr != null) mr.onImageAvailable()
                    }
                }, handler)

                val vd = withContext(Loop.Blocking) {
                    dm.createVirtualDisplay(
                        "hs-mirror-" + outW + "x" + outH, outW, outH, 320,
                        reader.getSurface(), flags
                    )
                }
                if (vd == null) {
                    reader.close()
                    throw RuntimeException("createVirtualDisplay returned null")
                }

                val m = Mirror(reader, vd, outW, outH)
                holder[0] = m

                // Wait up to 1500 ms for the first frame so the very first capture
                // has data. On an idle source the compositor may delay the initial
                // mirror frame; the listener still wakes us as soon as it arrives.
                val deadline = System.nanoTime() + 1500000000L
                while (System.nanoTime() < deadline) {
                    if (m.withFrame { bmp, _ -> bmp != null }) return m
                    // Also try a direct pull — sometimes the listener doesn't fire
                    // for the very first frame but the queue has it.
                    val img = reader.acquireLatestImage()
                    if (img != null) {
                        try {
                            m.absorb(img)
                        } finally {
                            img.close()
                        }
                        return m
                    }
                    delay(15)
                }
                return m
            }
        }
    }

    companion object {
        private const val JPEG_DEFAULT_Q = 80

        private const val MAX_CACHED_MIRRORS = 4

        @Throws(Exception::class)
        private fun initDisplayManager(): DisplayManager? {
//        if (android.os.Looper.getMainLooper() == null) {
//            android.os.Looper.prepareMainLooper();
//        }
//        Class<?> atCls = Class.forName("android.app.ActivityThread");
//        Object atInst = atCls.getMethod("systemMain").invoke(null);
            val sysCtx = systemContext()
            // Android 14+ rejects createVirtualDisplay when the Context's
            // op-package name doesn't map to the calling UID. The system
            // Context's package is "android" but we run as shell UID (2000),
            // so the check fails with:
            //   SecurityException: packageName must match the calling uid
            // Three-tier strategy:
            //  1. createPackageContext("com.android.shell", 0) — usually
            //     returns a context whose getOpPackageName() is the shell
            //     package, which matches shell UID.
            //  2. If that didn't change getOpPackageName(), wrap the result
            //     to force the override (DisplayManagerGlobal reads
            //     context.getOpPackageName() to populate the system request).
            //  3. If createPackageContext threw, wrap the system context.
            var base = sysCtx
            var chosen: String?
            try {
                base = sysCtx!!.createPackageContext("com.android.shell", 0)
                chosen = "com.android.shell (pkg-ctx)"
            } catch (t: Throwable) {
                System.err.println("screenshot: createPackageContext(com.android.shell) failed: " + t)
                chosen = "com.android.shell (wrapped sysCtx)"
            }
            if ("com.android.shell" != base!!.getOpPackageName()) {
                val inner = base
                base = object : ContextWrapper(inner) {
                    override fun getPackageName(): String {
                        return "com.android.shell"
                    }

                    override fun getOpPackageName(): String {
                        return "com.android.shell"
                    }
                }
                chosen += " + wrapper"
            }
            System.err.println(
                ("screenshot: display ctx = " + chosen
                        + ", op=" + base.getOpPackageName())
            )
            return base.getSystemService(Context.DISPLAY_SERVICE) as DisplayManager?
        }

        /** Walk `dumpsys window windows` for the first visible window
         * with FLAG_SECURE in its `fl=` line. Returns the window's
         * short name (e.g. `com.bank.app/.LoginActivity`) or null.  */
        private fun findSecureWindow(): String? {
            try {
                val p = Runtime.getRuntime().exec(
                    arrayOf<String>("dumpsys", "window", "windows")
                )
                BufferedReader(
                    InputStreamReader(
                        p.getInputStream(),
                        StandardCharsets.UTF_8
                    )
                ).use { r ->
                    var currentWin: String? = null
                    var line: String?
                    while ((r.readLine().also { line = it }) != null) {
                        val t = line!!.trim { it <= ' ' }
                        if (t.startsWith("Window #")) {
                            val curly = t.indexOf('{')
                            val closeBrace = t.indexOf('}', curly)
                            if (curly >= 0 && closeBrace > curly) {
                                val inside = t.substring(curly + 1, closeBrace)
                                val sp = inside.lastIndexOf(' ')
                                currentWin = if (sp >= 0)
                                    inside.substring(sp + 1)
                                else
                                    inside
                            }
                        } else if (t.startsWith("fl=") && containsToken(t, "SECURE")) {
                            return if (currentWin != null) currentWin else "unknown"
                        }
                    }
                }
                p.waitFor()
            } catch (ignored: Throwable) {
            }
            return null
        }

        /** Whole-token match against a space-separated flags line.  */
        private fun containsToken(flagsLine: String, token: String): Boolean {
            for (tok in flagsLine.substring(3).split(" ".toRegex()).dropLastWhile { it.isEmpty() }
                .toTypedArray()) {
                if (token == tok) return true
            }
            return false
        }

        private fun encode(bmp: Bitmap, format: String?, quality: Int): ByteArray {
            val baos = ByteArrayOutputStream(64 * 1024)
            val cf: CompressFormat = compressFormat(format)
            bmp.compress(cf, if (cf == CompressFormat.PNG) 100 else quality, baos)
            return baos.toByteArray()
        }

        private fun isPng(format: String?): Boolean {
            return "png".equals(format, ignoreCase = true)
        }

        private fun compressFormat(format: String?): CompressFormat {
            if (isPng(format)) return CompressFormat.PNG
            if ("webp".equals(format, ignoreCase = true)) {
                try {
                    return CompressFormat.valueOf("WEBP_LOSSY")
                } catch (ignored: IllegalArgumentException) {
                    return CompressFormat.valueOf("WEBP")
                }
            }
            return CompressFormat.JPEG
        }

        private fun clampQuality(q: Int): Int {
            if (q <= 0) return JPEG_DEFAULT_Q
            if (q > 100) return 100
            return q
        }

        private fun lookupSourceSize(): Point {
            try {
                val dmg = Class.forName("android.hardware.display.DisplayManagerGlobal")
                val inst = dmg.getMethod("getInstance").invoke(null)
                val d = dmg.getMethod("getRealDisplay", Int::class.javaPrimitiveType)
                    .invoke(inst, Display.DEFAULT_DISPLAY) as Display
                val p = Point()
                d.getRealSize(p)
                return p
            } catch (t: Throwable) {
                return Point(0, 0)
            }
        }
    }
}
