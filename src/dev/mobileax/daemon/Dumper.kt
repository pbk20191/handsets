package dev.mobileax.daemon

import android.app.UiAutomation
import android.graphics.Rect
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

class Dumper(private val ua: UiAutomation) {
    private val buf = StringBuilder(64 * 1024)

    /** One dump at a time: they share [buf]. Waiters suspend, they don't block a thread. */
    private val mutex = Mutex()

    suspend fun dumpAll(): String = mutex.withLock { withContext(Loop.Work) { dumpAllLocked() } }

    private fun dumpAllLocked(): String {
        buf.setLength(0)
        val out = JsonOut(buf)
        out.beginObj()
        out.num("ts", System.currentTimeMillis())

        out.key("windows").beginArr()
        val windows = ua.getWindows()
        if (windows != null) {
            for (w in windows) {
                writeWindow(w, out)
            }
        }
        out.endArr()
        out.endObj()
        return out.finish()
    }

    suspend fun dumpActive(): String = mutex.withLock { withContext(Loop.Work) { dumpActiveLocked() } }

    private fun dumpActiveLocked(): String {
        buf.setLength(0)
        val out = JsonOut(buf)
        out.beginObj()
        out.num("ts", System.currentTimeMillis())

        val root = ua.rootInActiveWindow
        out.key("root")
        Traverse.writeNode(root, out)

        out.endObj()
        return out.finish()
    }

    private fun writeWindow(w: AccessibilityWindowInfo?, out: JsonOut) {
        if (w == null) return
        out.beginObj()
        out.num("id", w.id.toLong())
        out.num("type", w.type.toLong())
        out.bool("active", w.isActive)
        out.bool("focused", w.isFocused)
        out.num("layer", w.layer.toLong())

        val r = Rect()
        w.getBoundsInScreen(r)
        out.rect("bounds", r.left, r.top, r.right, r.bottom)

        var root: AccessibilityNodeInfo? = null
        try {
            root = w.root
        } catch (_: Throwable) {
            // some windows aren't accessible to the shell uid
        }
        out.key("root")
        Traverse.writeNode(root, out)

        out.endObj()
    }
}
