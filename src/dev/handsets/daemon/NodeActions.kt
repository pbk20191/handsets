package dev.handsets.daemon

import android.app.UiAutomation
import android.os.Bundle
import android.view.accessibility.AccessibilityNodeInfo
import java.nio.charset.StandardCharsets
import java.util.ArrayDeque
import java.util.Deque

/**
 * Accessibility-action shortcuts. Lets RPA scripts do things like
 * node_click text="Sign in"
 * node_set_text id=com.foo:id/email text="user@example.com"
 * node_scroll class=androidx.recyclerview.widget.RecyclerView dir=forward
 * 
 * Faster and more reliable than coordinate gestures: no virtual-keyboard
 * round-trip, no race with layout animations, no chance of an overlapping
 * window swallowing the tap.
 */
internal class NodeActions(private val ua: UiAutomation) {
    // ---------- public action wrappers ----------
    fun click(selectorStr: String): ByteArray? {
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_CLICK, null, "click")
    }

    fun longClick(selectorStr: String): ByteArray? {
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_LONG_CLICK, null, "long_click")
    }

    fun focus(selectorStr: String): ByteArray? {
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_FOCUS, null, "focus")
    }

    fun setText(selectorStr: String?, text: String?): ByteArray? {
        val args = Bundle()
        args.putCharSequence(
            AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text
        )
        // Empty selector → operate on whichever EditText currently has input
        // focus. Mirrors `paste` / `submit` so callers like hs tui can update
        // a tapped-to-focus field that has no resource-id without inventing
        // a brittle selector. ACTION_SET_TEXT replaces the field's contents
        // wholesale, so this also covers the "clear before update" case.
        if (selectorStr == null || selectorStr.isEmpty()) {
            val target = findInputFocus()
            if (target == null) return err("no-focused-input")
            val ok: Boolean
            try {
                ok = target.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
            } catch (t: Throwable) {
                return err("set_text-threw:" + t.message)
            }
            return if (ok) ok("ok set_text") else err("set_text-rejected")
        }
        return perform(selectorStr, AccessibilityNodeInfo.ACTION_SET_TEXT, args, "set_text")
    }

    fun scroll(selectorStr: String, dir: String?): ByteArray? {
        val action = if ("backward".equals(dir, ignoreCase = true))
            AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD
        else
            AccessibilityNodeInfo.ACTION_SCROLL_FORWARD
        return perform(selectorStr, action, null, "scroll_" + dir)
    }

    /**
     * Insert the current system clipboard into the focused EditText (or
     * the one matched by `selectorStr`, if non-null) via
     * `AccessibilityNodeInfo.ACTION_PASTE`. The clipboard itself
     * is set with `hs clip TEXT` (wire: `clip_set`); this
     * verb is the on-device equivalent of pressing Cmd/Ctrl+V.
     */
    fun pasteAction(selectorStr: String?): ByteArray? {
        val target: AccessibilityNodeInfo?
        if (selectorStr == null || selectorStr.isEmpty()) {
            target = findInputFocus()
            if (target == null) return err("no-focused-input")
        } else {
            val sel: Selector?
            try {
                sel = Selector.Companion.parse(selectorStr)
            } catch (e: IllegalArgumentException) {
                return err("bad-selector:" + e.message)
            }
            target = find(sel)
            if (target == null) return err("not-found:" + sel)
        }
        val ok: Boolean
        try {
            ok = target.performAction(AccessibilityNodeInfo.ACTION_PASTE)
        } catch (t: Throwable) {
            return err("paste-threw:" + t.message)
        }
        return if (ok) ok("ok paste") else err("paste-rejected")
    }

    /**
     * Press the IME action button on the focused EditText (or the one
     * matched by `selectorStr`, if non-null). Routes through
     * `AccessibilityAction.ACTION_IME_ENTER`, which makes the
     * framework fire the field's configured editor action — Search /
     * Go / Send / Done / Next / Previous depending on what the app
     * declared via `android:imeOptions`.
     */
    fun imeAction(selectorStr: String?): ByteArray? {
        val target: AccessibilityNodeInfo?
        if (selectorStr == null || selectorStr.isEmpty()) {
            target = findInputFocus()
            if (target == null) return err("no-focused-input")
        } else {
            val sel: Selector?
            try {
                sel = Selector.Companion.parse(selectorStr)
            } catch (e: IllegalArgumentException) {
                return err("bad-selector:" + e.message)
            }
            target = find(sel)
            if (target == null) return err("not-found:" + sel)
        }
        val actionId: Int
        try {
            actionId = AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.getId()
        } catch (t: Throwable) {
            // ACTION_IME_ENTER is API 30+. Older devices need to send
            // a KEYCODE_ENTER via the input service instead.
            return err("ime-enter-unsupported:" + t.javaClass.getSimpleName())
        }
        val ok: Boolean
        try {
            ok = target.performAction(actionId)
        } catch (t: Throwable) {
            return err("submit-threw:" + t.message)
        }
        return if (ok) ok("ok submit") else err("submit-rejected")
    }

    /** Walk every window's tree looking for the node that holds input focus
     * (an EditText the IME is bound to). Falls back to the active window.  */
    private fun findInputFocus(): AccessibilityNodeInfo? {
        val windows = ua.getWindows()
        if (windows != null) {
            for (w in windows) {
                val root = w.getRoot()
                if (root == null) continue
                val f = root.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
                if (f != null) return f
            }
        }
        val root = ua.getRootInActiveWindow()
        return if (root == null) null else root.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
    }

    // ---------- selector-based finder (also reusable by wait_for_text) ----------
    fun parseSelector(s: String): Selector {
        return Selector.Companion.parse(s)
    }

    /** Locate the first node matching `sel`, or null.  */
    fun find(sel: Selector): AccessibilityNodeInfo? {
        val windows = ua.getWindows()
        if (windows == null || windows.isEmpty()) {
            val root = ua.getRootInActiveWindow()
            return if (root == null) null else bfs(root, sel)
        }
        for (w in windows) {
            val root = w.getRoot()
            if (root == null) continue
            val hit: AccessibilityNodeInfo? = bfs(root, sel)
            if (hit != null) return hit
        }
        return null
    }

    private fun perform(
        selectorStr: String,
        action: Int,
        args: Bundle?,
        opName: String?
    ): ByteArray? {
        val sel: Selector?
        try {
            sel = Selector.Companion.parse(selectorStr)
        } catch (e: IllegalArgumentException) {
            return err("bad-selector:" + e.message)
        }

        val n = find(sel)
        if (n == null) return err("not-found:" + sel)
        val ok: Boolean
        try {
            ok = n.performAction(action, args)
        } catch (t: Throwable) {
            return err(opName + "-threw:" + t.message)
        }
        return if (ok) ok("ok " + opName) else err(opName + "-rejected")
    }

    // ---------- selector model ----------
    /** Conjunction of attribute predicates: `text="x" id="y"`.  */
    internal class Selector {
        var textExact: String? = null
        var textSub: String? = null
        var descExact: String? = null
        var descSub: String? = null
        var idExact: String? = null
        var classExact: String? = null
        var pkgExact: String? = null

        fun matches(n: AccessibilityNodeInfo): Boolean {
            if (textExact != null) {
                val t = n.getText()
                if (t == null || !textExact.contentEquals(t)) return false
            }
            if (textSub != null) {
                val t = n.getText()
                if (t == null || !t.toString().contains(textSub!!)) return false
            }
            if (descExact != null) {
                val d = n.getContentDescription()
                if (d == null || !descExact.contentEquals(d)) return false
            }
            if (descSub != null) {
                val d = n.getContentDescription()
                if (d == null || !d.toString().contains(descSub!!)) return false
            }
            if (idExact != null) {
                val id = n.getViewIdResourceName()
                if (id == null || idExact != id) return false
            }
            if (classExact != null) {
                val c = n.getClassName()
                if (c == null) return false
                val s = c.toString()
                // Allow simple-name match: `EditText` matches
                // `android.widget.EditText`. Exact-equals path keeps the
                // unambiguous `class=android.widget.EditText` form working.
                if (classExact != s && !s.endsWith("." + classExact)) return false
            }
            if (pkgExact != null) {
                val p = n.getPackageName()
                if (p == null || !pkgExact.contentEquals(p)) return false
            }
            return true
        }

        override fun toString(): String {
            val sb = StringBuilder()
            if (textExact != null) sb.append(" text=").append(textExact)
            if (textSub != null) sb.append(" text~=").append(textSub)
            if (descExact != null) sb.append(" desc=").append(descExact)
            if (descSub != null) sb.append(" desc~=").append(descSub)
            if (idExact != null) sb.append(" id=").append(idExact)
            if (classExact != null) sb.append(" class=").append(classExact)
            if (pkgExact != null) sb.append(" pkg=").append(pkgExact)
            return if (sb.length > 0) sb.substring(1) else ""
        }

        companion object {
            fun parse(s: String): Selector {
                require(!(s == null || s.isEmpty())) { "empty" }
                val sel = Selector()
                // Split on whitespace but respect quoted values.
                val toks: MutableList<String> = tokenize(s)
                for (tok in toks) {
                    val eq = tok.indexOf('=')
                    if (eq <= 0) {
                        // Bare token = CSS-like class shorthand:
                        //   `EditText` → class=EditText (simple-name matched below).
                        sel.classExact = unquote(tok)
                        continue
                    }
                    val sub = tok.get(eq - 1) == '~'
                    val keyEnd = if (sub) eq - 1 else eq
                    val k = tok.substring(0, keyEnd)
                    val v: String = unquote(tok.substring(eq + 1))
                    when (k) {
                        "text" -> if (sub) sel.textSub = v else sel.textExact = v
                        "desc" -> if (sub) sel.descSub = v else sel.descExact = v
                        "id" -> sel.idExact = v
                        "class" -> sel.classExact = v
                        "pkg" -> sel.pkgExact = v
                        else -> throw IllegalArgumentException("unknown selector key: " + k)
                    }
                }
                return sel
            }
        }
    }

    companion object {
        private fun bfs(root: AccessibilityNodeInfo?, sel: Selector): AccessibilityNodeInfo? {
            val q: Deque<AccessibilityNodeInfo> = ArrayDeque<AccessibilityNodeInfo>()
            q.add(root)
            while (!q.isEmpty()) {
                val n = q.removeFirst()
                if (n == null) continue
                if (sel.matches(n)) return n
                val kids = n.getChildCount()
                for (i in 0..<kids) {
                    val c = n.getChild(i)
                    if (c != null) q.add(c)
                }
            }
            return null
        }

        /** Whitespace-split that keeps "quoted strings" together.  */
        private fun tokenize(s: String): MutableList<String> {
            val out: MutableList<String> = ArrayList<String>()
            val cur = StringBuilder()
            var inQuote = false
            for (i in 0..<s.length) {
                val c = s.get(i)
                if (c == '"') {
                    inQuote = !inQuote
                    cur.append(c)
                } else if (Character.isWhitespace(c) && !inQuote) {
                    if (cur.length > 0) {
                        out.add(cur.toString())
                        cur.setLength(0)
                    }
                } else {
                    cur.append(c)
                }
            }
            if (cur.length > 0) out.add(cur.toString())
            return out
        }

        private fun unquote(v: String): String {
            if (v.length >= 2 && v.get(0) == '"' && v.get(v.length - 1) == '"') {
                return v.substring(1, v.length - 1)
            }
            return v
        }

        private fun ok(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:" + tail).toByteArray(StandardCharsets.UTF_8)
        }
    }
}
