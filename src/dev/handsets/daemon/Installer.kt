package dev.handsets.daemon

import android.content.Context
import android.content.pm.PackageInstaller.SessionParams
import android.content.pm.PackageManager
import dev.handsets.daemon.Binders.iPackageManager
import dev.handsets.daemon.Binders.iPermissionManagerOrNull
import dev.handsets.daemon.Binders.myUserId
import java.lang.reflect.Method
import java.nio.charset.StandardCharsets

/**
 * Stream-install an APK via [PackageInstaller], the same path
 * `adb install` uses internally. We skip the /data/local/tmp staging
 * copy by writing chunked frames straight into `session.openWrite`.
 * 
 * Install result is reported back via a local [Binder] that implements
 * just the `IIntentSender.send` transaction (transaction code 1, stable
 * across all known API levels). That dodges the BroadcastReceiver dance that
 * would require a proper package name for our app_process daemon.
 */
internal class Installer(private val ctx: Context) {
    fun grantAllRequested(pkg: String) {
        try {
            val pm = ctx.getPackageManager()
            val requested = pm.getPackageInfo(pkg, PackageManager.GET_PERMISSIONS)
                .requestedPermissions
            if (requested == null) return
            val svcs = arrayOf<Any?>(
                iPermissionManagerOrNull(),
                iPackageManager(),
            )
            var svc: Any? = null
            var grant: Method? = null
            for (s in svcs) {
                if (s == null) continue
                val m: Method? = findGrant(s.javaClass)
                if (m != null) {
                    svc = s
                    grant = m
                    break
                }
            }
            if (grant == null) return
            val gp = grant.getParameterTypes()
            val userId = myUserId()
            for (perm in requested) {
                val args = arrayOfNulls<Any>(gp.size)
                args[0] = pkg
                args[1] = perm
                var uidSet = false
                for (i in 2..<gp.size) {
                    if (gp[i] == Int::class.javaPrimitiveType && !uidSet) {
                        args[i] = userId
                        uidSet = true
                    } else if (gp[i] == Int::class.javaPrimitiveType) args[i] = 0
                    else if (gp[i] == Boolean::class.javaPrimitiveType) args[i] = false
                    else args[i] = null
                }
                try {
                    grant.invoke(svc, *args)
                } catch (ignored: Throwable) { /* non-runtime perms throw — that's fine */
                }
            }
        } catch (t: Throwable) {
            System.err.println("warn: grantAllRequested failed: " + t)
        }
    }

    companion object {
        fun trySetInstallFlags(params: SessionParams?, flag: Int) {
            try {
                val m = SessionParams::class.java
                    .getDeclaredMethod("setInstallFlags", Int::class.javaPrimitiveType)
                m.setAccessible(true)
                // setInstallFlags replaces; OR in our flag with whatever was there.
                val f =
                    SessionParams::class.java.getDeclaredField("installFlags")
                f.setAccessible(true)
                val prev = f.getInt(params)
                m.invoke(params, prev or flag)
            } catch (t: Throwable) {
                // Fallback: write the field directly.
                try {
                    val f =
                        SessionParams::class.java.getDeclaredField("installFlags")
                    f.setAccessible(true)
                    f.setInt(params, f.getInt(params) or flag)
                } catch (ignored: Throwable) {
                }
            }
        }

        fun trySetRequireUserAction(params: SessionParams?, mode: Int) {
            try {
                val m = SessionParams::class.java
                    .getDeclaredMethod("setRequireUserAction", Int::class.javaPrimitiveType)
                m.invoke(params, mode)
            } catch (ignored: Throwable) {
                // pre-API-31; default behaviour is fine.
            }
        }

        private fun findGrant(cls: Class<*>): Method? {
            var picked: Method? = null
            for (m in cls.getMethods()) {
                if ("grantRuntimePermission" != m.getName()) continue
                val p = m.getParameterTypes()
                if (p.size >= 3 && p[0] == String::class.java && p[1] == String::class.java && (picked == null || p.size < picked.getParameterTypes().size)) {
                    picked = m
                }
            }
            return picked
        }

        private fun ok(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:" + tail).toByteArray(StandardCharsets.UTF_8)
        }
    }
}
