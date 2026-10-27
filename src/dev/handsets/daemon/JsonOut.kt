package dev.handsets.daemon

class JsonOut(private val sb: StringBuilder) {
    fun beginObj(): JsonOut {
        sb.append('{')
        return this
    }

    fun endObj(): JsonOut {
        trimComma()
        sb.append('}').append(',')
        return this
    }

    fun beginArr(): JsonOut {
        sb.append('[')
        return this
    }

    fun endArr(): JsonOut {
        trimComma()
        sb.append(']').append(',')
        return this
    }

    fun key(k: String?): JsonOut {
        sb.append('"').append(k).append('"').append(':')
        return this
    }

    fun str(k: String?, v: CharSequence?): JsonOut {
        if (v.isNullOrEmpty()) return this
        key(k)
        escape(v)
        sb.append(',')
        return this
    }

    fun num(k: String?, v: Long): JsonOut {
        key(k).append(v.toString()).append(',')
        return this
    }

    fun bool(k: String?, v: Boolean): JsonOut {
        if (!v) return this
        key(k).append("true").append(',')
        return this
    }

    fun rect(k: String?, l: Int, t: Int, r: Int, b: Int): JsonOut {
        key(k).append('[')
            .append(l).append(',')
            .append(t).append(',')
            .append(r).append(',')
            .append(b).append("],")
        return this
    }

    fun append(s: String?): StringBuilder {
        return sb.append(s)
    }

    fun append(c: Char): StringBuilder {
        return sb.append(c)
    }

    fun append(v: Int): StringBuilder {
        return sb.append(v)
    }

    fun trimComma() {
        val n = sb.length
        if (n > 0 && sb[n - 1] == ',') sb.setLength(n - 1)
    }

    fun finish(): String {
        trimComma()
        return sb.toString()
    }

    private fun escape(v: CharSequence) {
        sb.append('"')
        val n = v.length
        for (i in 0..<n) {
            val c = v[i]
            when (c) {
                '"' -> sb.append("\\\"")
                '\\' -> sb.append("\\\\")
                '\n' -> sb.append("\\n")
                '\r' -> sb.append("\\r")
                '\t' -> sb.append("\\t")
                '\u0008' -> sb.append("\\b")
                '\u000C' -> sb.append("\\f")
                else -> if (c.code < 0x20) {
                    sb.append("\\u").append(String.format("%04x", c.code))
                } else {
                    sb.append(c)
                }
            }
        }
        sb.append('"')
    }
}
