package dev.handsets.daemon

import android.app.UiAutomation
import android.graphics.Rect
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo

class Dumper(private val ua: UiAutomation) {
    private val buf = StringBuilder(64 * 1024)

    @Synchronized
    fun dumpAll(): String {
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

    @Synchronized
    fun dumpActive(): String {
        buf.setLength(0)
        val out = JsonOut(buf)
        out.beginObj()
        out.num("ts", System.currentTimeMillis())

        val root = ua.getRootInActiveWindow()
        out.key("root")
        Traverse.writeNode(root, out)

        out.endObj()
        return out.finish()
    }

    private fun writeWindow(w: AccessibilityWindowInfo?, out: JsonOut) {
        if (w == null) return
        out.beginObj()
        out.num("id", w.getId().toLong())
        out.num("type", w.getType().toLong())
        out.bool("active", w.isActive())
        out.bool("focused", w.isFocused())
        out.num("layer", w.getLayer().toLong())

        val r = Rect()
        w.getBoundsInScreen(r)
        out.rect("bounds", r.left, r.top, r.right, r.bottom)

        var root: AccessibilityNodeInfo? = null
        try {
            root = w.getRoot()
        } catch (t: Throwable) {
            // some windows aren't accessible to the shell uid
        }
        out.key("root")
        Traverse.writeNode(root, out)

        out.endObj()
    }
}
