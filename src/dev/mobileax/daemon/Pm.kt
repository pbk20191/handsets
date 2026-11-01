package dev.mobileax.daemon

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import dev.mobileax.daemon.Binders.iPackageManager
import dev.mobileax.daemon.Binders.iPermissionManagerOrNull
import dev.mobileax.daemon.Binders.myUserId
import java.lang.reflect.InvocationTargetException
import java.lang.reflect.Method
import java.nio.charset.StandardCharsets

/**
 * Direct-Binder pm subcommands. Mirrors the surface a developer reaches for
 * after `adb shell pm …` — same calls, no per-command JVM cold-start.
 */
internal class Pm(private val ctx: Context) {
    /** Args: optional flags "3" (third-party only) or "s" (system only).
     * Output columns: package \t label \t sourceDir. Label is the
     * user-visible app name from the manifest; blank for packages whose
     * manifest label is just the package name (overlays, RROs, providers).  */
    fun list(thirdPartyOnly: Boolean, systemOnly: Boolean): ByteArray {
        val pm = ctx.packageManager
        val pkgs = pm.getInstalledPackages(0)
        val sb = StringBuilder(8 * 1024)
        for (p in pkgs) {
            val ai = p.applicationInfo ?: continue
            val system = (ai.flags and ApplicationInfo.FLAG_SYSTEM) != 0
            if (thirdPartyOnly && system) continue
            if (systemOnly && !system) continue
            var label = ""
            try {
                val l = pm.getApplicationLabel(ai)
                val s = l.toString()
                if (s != p.packageName) {
                    label = s.replace('\t', ' ').replace('\n', ' ')
                }
            } catch (_: Throwable) {
            }
            sb.append(p.packageName)
                .append('\t')
                .append(label)
                .append('\t')
                .append(ai.sourceDir ?: "")
                .append('\n')
        }
        return sb.toString().toByteArray(StandardCharsets.UTF_8)
    }

    fun path(pkg: String): ByteArray {
        try {
            val ai = ctx.packageManager.getApplicationInfo(pkg, 0)
            val src = ai.sourceDir ?: return err("no-sourceDir:$pkg")
            return src.toByteArray(StandardCharsets.UTF_8)
        } catch (_: PackageManager.NameNotFoundException) {
            return err("not-found:$pkg")
        }
    }

    fun grant(pkg: String?, perm: String?): ByteArray {
        return changeRuntimePerm(pkg, perm,  /* grant */true)
    }

    fun revoke(pkg: String?, perm: String?): ByteArray {
        return changeRuntimePerm(pkg, perm,  /* grant */false)
    }

    private fun changeRuntimePerm(pkg: String?, perm: String?, grant: Boolean): ByteArray {
        val methodName = if (grant) "grantRuntimePermission" else "revokeRuntimePermission"
        // Permission management moved off IPackageManager onto IPermissionManager
        // around API 30. Try the dedicated service first.
        val candidates = arrayOf(
            iPermissionManagerOrNull(),
            iPackageManager(),
        )
        var lastCause: Throwable? = null
        for (svc in candidates) {
            if (svc == null) continue
            val picked: Method = findRuntimePermMethod(svc.javaClass, methodName) ?: continue
            lastCause = try {
                picked.invoke(svc, *buildRuntimePermArgs(picked, pkg, perm))
                return ok("ok $methodName $pkg $perm")
            } catch (ite: InvocationTargetException) {
                ite.cause ?: ite
            } catch (t: Throwable) {
                t
            }
        }
        if (lastCause != null) {
            return err(
                ((if (grant) "grant" else "revoke") + "-failed:"
                        + lastCause.javaClass.getSimpleName() + ":" + lastCause.message)
            )
        }
        return err("$methodName-not-exposed")
    }

    companion object {
        private fun findRuntimePermMethod(cls: Class<*>, name: String): Method? {
            var picked: Method? = null
            for (m in cls.methods) {
                if (name != m.name) continue
                val p = m.parameterTypes
                if (p.size < 3) continue
                if (p[0] != String::class.java || p[1] != String::class.java) continue
                if (picked == null || p.size < picked.parameterTypes.size) {
                    picked = m
                }
            }
            return picked
        }

        private fun buildRuntimePermArgs(m: Method, pkg: String?, perm: String?): Array<Any?> {
            val p = m.parameterTypes
            val userId = myUserId()
            val args = arrayOfNulls<Any>(p.size)
            args[0] = pkg
            args[1] = perm
            var userIdAssigned = false
            for (i in 2..<p.size) {
                if (p[i] == Int::class.javaPrimitiveType && !userIdAssigned) {
                    args[i] = userId
                    userIdAssigned = true
                } else if (p[i] == Int::class.javaPrimitiveType) {
                    args[i] = 0
                } else if (p[i] == Boolean::class.javaPrimitiveType) {
                    args[i] = false
                } else {
                    args[i] = null
                }
            }
            return args
        }

        private fun ok(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}
