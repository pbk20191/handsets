package dev.mobileax.daemon

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
                val cc = n.childCount
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

        val cls = n.className
        if (cls != null) out.str("cls", cls)

        val pkg = n.packageName
        if (pkg != null) out.str("pkg", pkg)

        val rid = n.viewIdResourceName
        if (rid != null && rid.isNotEmpty()) out.str("rid", rid)

        val text = n.getText()
        if (text != null && text.isNotEmpty()) out.str("text", text)

        val desc = n.contentDescription
        if (desc != null && desc.isNotEmpty()) out.str("desc", desc)

        val hint = n.hintText
        if (hint != null && hint.isNotEmpty()) out.str("hint", hint)

        val b = TMP_RECT
        n.getBoundsInScreen(b)
        out.rect("bounds", b.left, b.top, b.right, b.bottom)

        val flags = StringBuilder(8)
        if (n.isClickable) flags.append('c')
        if (n.isLongClickable) flags.append('L')
        if (n.isScrollable) flags.append('s')
        if (n.isCheckable) flags.append('k')
        @Suppress("DEPRECATION") // getChecked() is API 36; min API is 28
        if (n.isChecked) flags.append('K')
        if (n.isFocusable) flags.append('f')
        if (n.isFocused) flags.append('F')
        if (n.isEnabled) flags.append('e')
        if (n.isSelected) flags.append('S')
        if (n.isPassword) flags.append('p')
        if (n.isVisibleToUser) flags.append('v')
        if (n.isEditable) flags.append('E')
        if (n.isHeading) flags.append('H')
        if (flags.isNotEmpty()) out.str("flags", flags)

        // What toolkits such as Compose say beyond the class name: the role
        // ("Switch"), its state ("On"), lists and ranges, and the actions a
        // node takes. Role and state come in the device's language.
        n.extras?.getCharSequence(ROLE_DESCRIPTION)?.let { if (it.isNotEmpty()) out.str("role", it) }
        if (Build.VERSION.SDK_INT >= 30) n.stateDescription?.let { if (it.isNotEmpty()) out.str("state", it) }
        n.error?.let { if (it.isNotEmpty()) out.str("error", it) }
        n.rangeInfo?.let { out.str("range", "${fmt(it.current)} ${fmt(it.min)}..${fmt(it.max)}") }
        n.collectionInfo?.let { out.str("collection", "${it.rowCount}x${it.columnCount}") }
        n.collectionItemInfo?.let { out.str("item", "${it.rowIndex},${it.columnIndex}") }
        val actions = actionsOf(n)
        if (actions.isNotEmpty()) out.str("actions", actions)
    }

    /** androidx's key for a role description; Compose sets it for Role.Switch, Role.Tab, … */
    private const val ROLE_DESCRIPTION = "AccessibilityNodeInfo.roleDescription"

    private fun fmt(f: Float): String = if (f == f.toLong().toFloat()) f.toLong().toString() else f.toString()

    /**
     * The actions worth knowing about, comma-separated. Click, long click and
     * checking are already flags; focus, selection and text-granularity
     * moves are on nearly every node and say nothing.
     */
    private val NOTABLE: Map<Int, String> = buildMap {
        put(AccessibilityNodeInfo.ACTION_SET_TEXT, "set_text")
        put(AccessibilityNodeInfo.ACTION_SCROLL_FORWARD, "scroll_forward")
        put(AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD, "scroll_backward")
        put(AccessibilityNodeInfo.ACTION_EXPAND, "expand")
        put(AccessibilityNodeInfo.ACTION_COLLAPSE, "collapse")
        put(AccessibilityNodeInfo.ACTION_DISMISS, "dismiss")
        val a = AccessibilityNodeInfo.AccessibilityAction::class.java
        listOf(
            "ACTION_SCROLL_UP" to "scroll_up", "ACTION_SCROLL_DOWN" to "scroll_down",
            "ACTION_SCROLL_LEFT" to "scroll_left", "ACTION_SCROLL_RIGHT" to "scroll_right",
            "ACTION_SCROLL_TO_POSITION" to "scroll_to_position", "ACTION_SET_PROGRESS" to "set_progress",
            "ACTION_PAGE_UP" to "page_up", "ACTION_PAGE_DOWN" to "page_down",
            "ACTION_IME_ENTER" to "ime_enter",
        ).forEach { (field, name) ->
            // Constants added after API 28 are looked up, not linked.
            runCatching { (a.getField(field).get(null) as AccessibilityNodeInfo.AccessibilityAction).id }
                .onSuccess { put(it, name) }
        }
    }

    /** Standard action ids; anything else with a label is the app's own custom action. */
    private val STANDARD: Set<Int> = runCatching {
        AccessibilityNodeInfo.AccessibilityAction::class.java.fields
            .filter { it.type == AccessibilityNodeInfo.AccessibilityAction::class.java }
            .map { (it.get(null) as AccessibilityNodeInfo.AccessibilityAction).id }
            .toSet()
    }.getOrDefault(emptySet())

    private fun actionsOf(n: AccessibilityNodeInfo): String {
        val list = n.actionList
        if (list.isEmpty()) return ""
        val sb = StringBuilder()
        for (a in list) {
            val name = NOTABLE[a.id]
                ?: a.label?.takeIf { it.isNotEmpty() && a.id !in STANDARD }?.let { "custom:" + it.toString().replace(',', ' ') }
                ?: continue
            if (sb.isNotEmpty()) sb.append(',')
            sb.append(name)
        }
        return sb.toString()
    }

    private fun getChildSafe(n: AccessibilityNodeInfo, i: Int): AccessibilityNodeInfo? {
        try {
            if (Build.VERSION.SDK_INT >= 33 && PREFETCH != -1) {
                return n.getChild(i, PREFETCH)
            }
            return n.getChild(i)
        } catch (_: Throwable) {
            return null
        }
    }

    private fun resolvePrefetchFlags(): Int {
        try {
            // FLAG_PREFETCH_DESCENDANTS_HYBRID was added in API 33.
            return AccessibilityNodeInfo::class.java
                .getField("FLAG_PREFETCH_DESCENDANTS_HYBRID")
                .getInt(null)
        } catch (_: Throwable) {
            // Older constant (API 16+); takes effect via internal client.
            try {
                return AccessibilityNodeInfo::class.java
                    .getField("FLAG_PREFETCH_DESCENDANTS")
                    .getInt(null)
            } catch (_: Throwable) {
                return -1
            }
        }
    }
}
