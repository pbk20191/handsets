package dev.handsets.daemon

import android.content.ComponentName
import android.content.Context
import android.content.pm.PackageManager
import android.content.res.AssetManager
import android.content.res.XmlResourceParser
import java.nio.charset.StandardCharsets

/**
 * Walks an installed APK's binary AndroidManifest.xml to enumerate every
 * declared deeplink URI template — intent-filters with action.VIEW and
 * one or more `<data>` children. We go straight to the manifest
 * rather than [PackageManager.queryIntentActivities] because the
 * binder surface drops attributes (pathPattern, port, mimeType) we
 * need to reconstruct the URI shape the app declared.
 */
internal class Manifest(private val ctx: Context) {
    /** Emit one row per (intent-filter, &lt;data&gt;) pair with action.VIEW.  */
    fun deeplinks(pkg: String?): ByteArray {
        if (pkg == null || pkg.isEmpty()) return err("deeplinks-needs-pkg")

        val apkPath: String?
        try {
            val pi = ctx.getPackageManager().getPackageInfo(pkg, 0)
            val ai = pi.applicationInfo
            apkPath = if (ai == null) null else ai.sourceDir
        } catch (t: Throwable) {
            return err("pkg-not-found:" + pkg)
        }
        if (apkPath == null) return err("apk-path-missing:" + pkg)

        val parser: XmlResourceParser
        try {
            // AssetManager() ctor and addAssetPath are @hide; reach them
            // through reflection. openXmlResourceParser then returns a
            // parser positioned at the start of the binary XML stream.
            val am = AssetManager::class.java.getConstructor().newInstance()
            AssetManager::class.java.getMethod("addAssetPath", String::class.java)
                .invoke(am, apkPath)
            parser = AssetManager::class.java
                .getMethod(
                    "openXmlResourceParser",
                    Int::class.javaPrimitiveType,
                    String::class.java
                )
                .invoke(am, 0, "AndroidManifest.xml") as XmlResourceParser
        } catch (t: Throwable) {
            return err("manifest-open-failed:" + t.message)
        }

        val out = StringBuilder(2048)
        try {
            walk(parser, pkg, out)
        } catch (t: Throwable) {
            return err("manifest-parse-failed:" + t.message)
        } finally {
            parser.close()
        }

        if (out.length == 0) {
            return ("no deeplinks declared in " + pkg)
                .toByteArray(StandardCharsets.UTF_8)
        }
        return out.toString().toByteArray(StandardCharsets.UTF_8)
    }

    companion object {
        private const val ANDROID = "http://schemas.android.com/apk/res/android"

        @Throws(Exception::class)
        private fun walk(p: XmlResourceParser, pkg: String?, out: StringBuilder) {
            var pkgAttr = pkg
            var currentComp: String? = null
            var inFilter = false
            var filterHasView = false
            val cats: MutableList<String?> = ArrayList<String?>()
            val dataLines: MutableList<String> = ArrayList<String>()

            var ev: Int
            while ((p.next().also { ev = it }) != XmlResourceParser.END_DOCUMENT) {
                if (ev == XmlResourceParser.START_TAG) {
                    val name = p.getName()
                    if ("manifest" == name) {
                        val pa = p.getAttributeValue(null, "package")
                        if (pa != null && !pa.isEmpty()) pkgAttr = pa
                    } else if ("activity" == name || "activity-alias" == name) {
                        currentComp = Companion.resolveComponent(
                            pkgAttr!!,
                            p.getAttributeValue(ANDROID, "name")
                        )
                    } else if ("intent-filter" == name && currentComp != null) {
                        inFilter = true
                        filterHasView = false
                        cats.clear()
                        dataLines.clear()
                    } else if (inFilter && "action" == name) {
                        if ("android.intent.action.VIEW" == p.getAttributeValue(ANDROID, "name")) {
                            filterHasView = true
                        }
                    } else if (inFilter && "category" == name) {
                        var c = p.getAttributeValue(ANDROID, "name")
                        if (c != null) {
                            if (c.startsWith("android.intent.category.")) {
                                c = c.substring("android.intent.category.".length)
                            }
                            cats.add(c)
                        }
                    } else if (inFilter && "data" == name) {
                        val line: String = describeData(p)
                        if (!line.isEmpty()) dataLines.add(line)
                    }
                } else if (ev == XmlResourceParser.END_TAG) {
                    val name = p.getName()
                    if ("intent-filter" == name && inFilter) {
                        if (filterHasView && !dataLines.isEmpty()) {
                            val catStr = if (cats.isEmpty()) "-" else cats.joinToString(",")
                            for (d in dataLines) {
                                out.append(pad(d, 60)).append("  ")
                                    .append(pad(catStr, 22)).append("  ")
                                    .append(currentComp).append('\n')
                            }
                        }
                        inFilter = false
                    } else if ("activity" == name || "activity-alias" == name) {
                        currentComp = null
                    }
                }
            }
        }

        /** Build a `scheme://host[:port][path]` template from one &lt;data&gt; tag.  */
        private fun describeData(p: XmlResourceParser): kotlin.String {
            val scheme = p.getAttributeValue(ANDROID, "scheme")
            val host = p.getAttributeValue(ANDROID, "host")
            val port = p.getAttributeValue(ANDROID, "port")
            val path = p.getAttributeValue(ANDROID, "path")
            val prefix = p.getAttributeValue(ANDROID, "pathPrefix")
            val pattern = p.getAttributeValue(ANDROID, "pathPattern")
            val mime = p.getAttributeValue(ANDROID, "mimeType")

            val sb = StringBuilder()
            if (scheme != null) sb.append(scheme).append("://")
            if (host != null) sb.append(host)
            if (port != null) sb.append(':').append(port)
            if (path != null) sb.append(path)
            else if (prefix != null) sb.append(prefix).append('*')
            else if (pattern != null) sb.append(pattern)
            if (mime != null) {
                if (sb.length > 0) sb.append("  ")
                sb.append("mime=").append(mime)
            }
            return sb.toString()
        }

        /** Resolve a possibly-relative activity name into pkg/.Class form.  */
        private fun resolveComponent(
            pkg: kotlin.String,
            activityName: kotlin.String?
        ): kotlin.String {
            if (activityName == null || activityName.isEmpty()) return pkg + "/?"
            val full: kotlin.String?
            if (activityName.startsWith(".")) full = pkg + activityName
            else if (!activityName.contains(".")) full = pkg + "." + activityName
            else full = activityName
            return ComponentName(pkg, full).flattenToShortString()
        }

        private fun pad(s: kotlin.String, w: Int): kotlin.String {
            if (s.length >= w) return s
            val b = StringBuilder(s)
            while (b.length < w) b.append(' ')
            return b.toString()
        }

        private fun err(tail: kotlin.String?): ByteArray {
            return ("ERR:" + tail).toByteArray(StandardCharsets.UTF_8)
        }
    }
}
