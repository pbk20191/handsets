package dev.handsets.daemon

import java.io.BufferedReader
import java.io.InputStreamReader
import java.nio.charset.StandardCharsets
import java.util.regex.Matcher
import java.util.regex.Pattern

/**
 * Read-only view of the LocationManager's last-known fixes.
 * 
 * Strategy: spawn `dumpsys location` and scan for the
 * `Location[...]` substring that android.location.Location#toString
 * emits. That toString format is stable across AOSP versions:
 * Location[<provider> <lat>,<lon>[ hAcc=N][ et=…][ alt=N][ vel=N][ bear=N] … mock?]
 * 
 * Reaching ILocationManager.getLastLocation directly would skip the
 * dumpsys process spawn, but the AIDL signature changes meaningfully
 * across API levels (LocationRequest, attribution tag, LastLocationRequest)
 * and dumpsys already contains exactly the data we want. One ~30 ms shell
 * invocation per call is fine for this verb.
 * 
 * Output is NDJSON — same shape as [Providers]: first line is the
 * column-name array, then one row per provider.
</lon></lat></provider> */
internal class Location {
    fun last(): ByteArray {
        val p: Process?
        try {
            p = ProcessBuilder("/system/bin/dumpsys", "location")
                .redirectErrorStream(true).start()
        } catch (e: Exception) {
            return err("dumpsys-spawn:" + e.message)
        }

        // Preserve first-seen order; the freshest "last location" block
        // tends to appear before per-provider history.
        val fixes: MutableMap<String?, DoubleArray?> = LinkedHashMap()
        val mocks: MutableMap<String?, Boolean?> = LinkedHashMap()
        try {
            BufferedReader(
                InputStreamReader(p.inputStream, StandardCharsets.UTF_8)
            ).use { br ->
                for (ln in br.lineSequence()) {
                    val m: Matcher = LOC.matcher(ln)
                    while (m.find()) {
                        val prov = m.group(1)!!
                        if (fixes.containsKey(prov)) continue
                        val lat = m.group(2)!!.toDouble()
                        val lon = m.group(3)!!.toDouble()
                        val tail = m.group(4) ?: ""
                        fixes[prov] = doubleArrayOf(
                            lat, lon,
                            kv(tail, "hAcc"),
                            kv(tail, "alt"),
                            kv(tail, "vel"),
                            kv(tail, "bear"),
                        )
                        mocks[prov] = tail.contains(" mock")
                    }
                }
            }
        } catch (e: Exception) {
            return err("dumpsys-read:" + e.message)
        } finally {
            try {
                p.destroy()
            } catch (ignored: Throwable) {
            }
        }

        val out = StringBuilder(256)
        out.append("[\"provider\",\"lat\",\"lon\",\"accuracy_m\",")
            .append("\"altitude_m\",\"speed_mps\",\"bearing_deg\",\"mock\"]\n")
        for (e in fixes.entries) {
            val v: DoubleArray = e.value!!
            out.append('[').append('"').append(e.key).append("\",")
            out.append(v[0]).append(',').append(v[1]).append(',')
            appendNum(out, v[2])
            out.append(',')
            appendNum(out, v[3])
            out.append(',')
            appendNum(out, v[4])
            out.append(',')
            appendNum(out, v[5])
            out.append(',')
            out.append(if (mocks[e.key] == true) "true" else "false")
            out.append("]\n")
        }
        return out.toString().toByteArray(StandardCharsets.UTF_8)
    }

    companion object {
        // Captures provider name, lat, lon, and the rest-of-bracket tail
        // (key=value attributes + optional " mock" sentinel).
        private val LOC: Pattern = Pattern.compile(
            ("Location\\[\\s*([A-Za-z_][A-Za-z_0-9]*)\\s+"
                    + "(-?\\d+(?:\\.\\d+)?),(-?\\d+(?:\\.\\d+)?)"
                    + "([^\\]]*)\\]")
        )

        private val KV: Pattern = Pattern.compile(
            "([A-Za-z]+)=(-?\\d+(?:\\.\\d+)?)"
        )

        private fun kv(tail: String, key: String?): Double {
            val m: Matcher = KV.matcher(tail)
            while (m.find()) {
                if (m.group(1) == key) {
                    try {
                        return m.group(2)!!.toDouble()
                    } catch (ignored: NumberFormatException) {
                        return Double.NaN
                    }
                }
            }
            return Double.NaN
        }

        private fun appendNum(out: StringBuilder, v: Double) {
            if (v.isNaN()) out.append("null")
            else out.append(v)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:" + tail).toByteArray(StandardCharsets.UTF_8)
        }
    }
}
