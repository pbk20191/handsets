package dev.handsets.daemon

import android.graphics.Rect
import android.os.Build
import android.view.accessibility.AccessibilityNodeInfo

object Traverse {
    private val PREFETCH = resolvePrefetchFlags()
    private val TMP_RECT = Rect()

    /** A node whose `children` array is open; [next] is the next child index to write. */
    private class Open(val node: AccessibilityNodeInfo, val count: Int, var next: Int = 0)

    /**
     * Writes [root] and its subtree depth-first. An explicit stack instead of
     * recursion, so a deep hierarchy can't overflow the thread stack.
     */
    fun writeNode(root: AccessibilityNodeInfo?, out: JsonOut) {
        val open = ArrayDeque<Open>()
        var n: AccessibilityNodeInfo? = root
        while (true) {
            if (n == null) {
                out.append("null,")
            } else {
                writeFields(n, out)
                val cc = n.getChildCount()
                if (cc > 0) {
                    out.key("children").beginArr()
                    open.addLast(Open(n, cc))
                } else {
                    out.endObj()
                }
            }
            // Next child of the innermost open node, closing finished ones.
            var advanced = false
            while (open.isNotEmpty()) {
                val top = open.last()
                if (top.next < top.count) {
                    n = getChildSafe(top.node, top.next++)
                    advanced = true
                    break
                }
                open.removeLast()
                out.endArr()
                out.endObj()
            }
            if (!advanced) return
        }
    }

    /** Opens the node's object and writes its own fields (not children). */
    private fun writeFields(n: AccessibilityNodeInfo, out: JsonOut) {
        out.beginObj()

        val cls = n.getClassName()
        if (cls != null) out.str("cls", cls)

        val pkg = n.getPackageName()
        if (pkg != null) out.str("pkg", pkg)

        val rid = n.getViewIdResourceName()
        if (rid != null && !rid.isEmpty()) out.str("rid", rid)

        val text = n.getText()
        if (text != null && text.length > 0) out.str("text", text)

        val desc = n.getContentDescription()
        if (desc != null && desc.length > 0) out.str("desc", desc)

        if (Build.VERSION.SDK_INT >= 26) {
            val hint = n.getHintText()
            if (hint != null && hint.length > 0) out.str("hint", hint)
        }

        val b = TMP_RECT
        n.getBoundsInScreen(b)
        out.rect("bounds", b.left, b.top, b.right, b.bottom)

        val flags = StringBuilder(8)
        if (n.isClickable()) flags.append('c')
        if (n.isLongClickable()) flags.append('L')
        if (n.isScrollable()) flags.append('s')
        if (n.isCheckable()) flags.append('k')
        if (n.isChecked()) flags.append('K')
        if (n.isFocusable()) flags.append('f')
        if (n.isFocused()) flags.append('F')
        if (n.isEnabled()) flags.append('e')
        if (n.isSelected()) flags.append('S')
        if (n.isPassword()) flags.append('p')
        if (n.isVisibleToUser()) flags.append('v')
        if (flags.length > 0) out.str("flags", flags)
    }

    private fun getChildSafe(n: AccessibilityNodeInfo, i: Int): AccessibilityNodeInfo? {
        try {
            if (Build.VERSION.SDK_INT >= 33 && PREFETCH != -1) {
                return n.getChild(i, PREFETCH)
            }
            return n.getChild(i)
        } catch (t: Throwable) {
            return null
        }
    }

    private fun resolvePrefetchFlags(): Int {
        try {
            // FLAG_PREFETCH_DESCENDANTS_HYBRID was added in API 33.
            return AccessibilityNodeInfo::class.java
                .getField("FLAG_PREFETCH_DESCENDANTS_HYBRID")
                .getInt(null)
        } catch (t: Throwable) {
            // Older constant (API 16+); takes effect via internal client.
            try {
                return AccessibilityNodeInfo::class.java
                    .getField("FLAG_PREFETCH_DESCENDANTS")
                    .getInt(null)
            } catch (t2: Throwable) {
                return -1
            }
        }
    }
}
