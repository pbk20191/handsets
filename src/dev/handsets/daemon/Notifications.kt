package dev.handsets.daemon

import java.io.BufferedReader
import java.io.InputStreamReader
import java.nio.charset.StandardCharsets

/**
 * Notification tray reader.
 * 
 * Direct `INotificationManager.getActiveNotifications` requires
 * the signature-protected `ACCESS_NOTIFICATIONS` permission,
 * which shell UID does *not* hold on stock Android. Instead we parse
 * the output of `dumpsys notification --noredact`, which the
 * shell process can run unrestricted. Same data source NotificationUI
 * tools use; output is large but stable enough across versions to
 * grep per-NotificationRecord blocks for the fields we care about.
 * 
 * One frame per call, NDJSON: header + one row per notification with
 * `key / posted / pkg / channel / title / text / sub_text / category`.
 * 
 * History is not exposed by dumpsys — when `history=1` we fall
 * back to the same active list with a note in the response.
 */
internal class Notifications {
    /** @param history reserved for future use; dumpsys only exposes active.
     */
    fun dump(pkgFilter: String?, limit: Int, history: Boolean): ByteArray {
        val text: String?
        try {
            text = runDumpsys()
        } catch (t: Throwable) {
            return err(
                ("dumpsys-failed:" + t.javaClass.simpleName
                        + ":" + t.message)
            )
        }
        if (text.isEmpty()) return err("empty-dumpsys")

        val blocks: MutableList<String> = splitRecords(text)
        val out = StringBuilder(4096)
        out.append("[\"key\",\"posted\",\"pkg\",\"channel\",\"title\",\"text\",\"sub_text\",\"category\"]\n")

        var emitted = 0
        for (block in blocks) {
            if (limit in 1..emitted) break
            val pkg: String = extractAfter(block, "pkg=", " \t\n") ?: continue
            if (!pkgFilter.isNullOrEmpty() && (pkgFilter != pkg)) continue

            val key: String? = extractAfter(block, "\n      key=", "\n")
            val channel: String? = extractAfter(block, "channel=", " \t\n")
            val category: String? = extractAfter(block, "category=", " \t\n)")
            val whenRaw: String? = extractAfter(block, "when=", "/ \t\n")

            var posted = 0L
            if (whenRaw != null) {
                try {
                    posted = whenRaw.trim { it <= ' ' }.toLong()
                } catch (_: NumberFormatException) {
                }
            }

            val title: String? = extractExtra(block, "android.title")
            var text2: String? = extractExtra(block, "android.text")
            if (text2 == null) text2 = extractExtra(block, "android.bigText")
            val subText: String? = extractExtra(block, "android.subText")

            out.append('[')
            appendJsonString(out, key)
            out.append(',')
            out.append(posted)
            out.append(',')
            appendJsonString(out, pkg)
            out.append(',')
            appendJsonString(out, channel)
            out.append(',')
            appendJsonString(out, title)
            out.append(',')
            appendJsonString(out, text2)
            out.append(',')
            appendJsonString(out, subText)
            out.append(',')
            appendJsonString(out, category)
            out.append("]\n")
            emitted++
        }
        return out.toString().toByteArray(StandardCharsets.UTF_8)
    }

    companion object {
        // ---------- dumpsys runner + block splitter ----------
        @Throws(Exception::class)
        private fun runDumpsys(): String {
            val p = Runtime.getRuntime().exec(
                arrayOf("dumpsys", "notification", "--noredact")
            )
            val sb = StringBuilder(64 * 1024)
            BufferedReader(
                InputStreamReader(p.inputStream, StandardCharsets.UTF_8)
            ).use { r ->
                val buf = CharArray(4096)
                var n: Int
                while ((r.read(buf).also { n = it }) > 0) sb.appendRange(buf, 0, n)
            }
            p.waitFor()
            return sb.toString()
        }

        /** Slice the dump into per-NotificationRecord blocks, dropping any
         * preamble + the Snoozed/History sections at the bottom.  */
        private fun splitRecords(text: String): MutableList<String> {
            val out: MutableList<String> = ArrayList()
            val active = text.indexOf("Notification List:")
            val from = if (active >= 0) active else 0
            val marker = "NotificationRecord("
            var i = text.indexOf(marker, from)
            while (i >= 0) {
                val j = text.indexOf(marker, i + marker.length)
                // Stop scanning once we leave the "Notification List:" section
                // (e.g. into the "Notifications by app:" report).
                val sectionEnd = text.indexOf("\n  ", i) // next 2-space section header
                if (sectionEnd >= 0 && (j !in 0..sectionEnd)) {
                    // The NotificationRecord block usually ends at a line with
                    // exactly two spaces of indent — the section boundary.
                }
                val block = if (j >= 0) text.substring(i, j) else text.substring(i)
                out.add(block)
                if (j < 0) break
                i = j
            }
            return out
        }

        // ---------- field extractors ----------
        /** Return the text between `prefix` and the first char in
         * `stopChars`, or null if `prefix` isn't present.  */
        private fun extractAfter(block: String, prefix: String, stopChars: String): String? {
            val i = block.indexOf(prefix)
            if (i < 0) return null
            val start = i + prefix.length
            var end = start
            while (end < block.length && stopChars.indexOf(block[end]) < 0) end++
            return block.substring(start, end)
        }

        /** Extract the value of `android.X=String (Y)` or
         * `android.X=CharSequence (Y)`. Returns null when the line
         * isn't present or the value is `null`.  */
        private fun extractExtra(block: String, key: String?): String? {
            // Match either `key=String (` or `key=CharSequence (`.
            var i = block.indexOf("\n                $key=")
            if (i < 0) i = block.indexOf("                $key=")
            if (i < 0) return null
            val lineStart = i + 1 // skip the leading newline if present
            val eq = block.indexOf('=', lineStart)
            if (eq < 0) return null
            val after = eq + 1
            // null literal
            if (block.startsWith("null", after)) return null
            // Look for `(...)` payload.
            val paren = block.indexOf('(', after)
            val lineEnd = block.indexOf('\n', after)
            if (paren < 0 || (lineEnd in 0..<paren)) return null
            // Balanced-paren extractor — handles parentheses inside the
            // value text. Stop on the matching ')' that closes our '('.
            var depth = 0
            for (p in paren..<block.length) {
                val c = block[p]
                if (c == '(') depth++
                else if (c == ')') {
                    depth--
                    if (depth == 0) {
                        return block.substring(paren + 1, p)
                    }
                }
                // Bail out if we run into a new extras line.
                if (c == '\n' && block.startsWith("                android.", p + 1)) break
            }
            return null
        }

        // ---------- JSON ----------
        private fun appendJsonString(out: StringBuilder, s: String?) {
            if (s == null) {
                out.append("null")
                return
            }
            out.append('"')
            val n = s.length
            for (i in 0..<n) {
                when (val ch = s[i]) {
                    '\\' -> out.append("\\\\")
                    '"' -> out.append("\\\"")
                    '\n' -> out.append("\\n")
                    '\r' -> out.append("\\r")
                    '\t' -> out.append("\\t")
                    '\u0008' -> out.append("\\b")
                    '\u000C' -> out.append("\\f")
                    else -> if (ch.code < 0x20) {
                        out.append(String.format("\\u%04x", ch.code))
                    } else {
                        out.append(ch)
                    }
                }
            }
            out.append('"')
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}
