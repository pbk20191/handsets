package dev.mobileax.daemon

import android.app.ActivityManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ResolveInfo
import android.net.Uri
import dev.mobileax.daemon.Binders.iActivityManager
import dev.mobileax.daemon.Binders.iActivityTaskManagerOrNull
import dev.mobileax.daemon.Binders.myUserId
import java.lang.reflect.InvocationTargetException
import java.lang.reflect.Method
import java.nio.charset.StandardCharsets

/**
 * Direct-Binder am subcommands.
 * 
 * `start` and `broadcast` go through the system Context (these wrap the same
 * Binder calls `am` ultimately makes). `force_stop` calls IActivityManager
 * directly via reflection because the public Java API doesn't expose it.
 */
internal class Am(private val ctx: Context) {
    /**
     * @param component  "pkg/.RelativeClass" or "pkg/fully.Qualified" (required)
     * @param action     intent action, may be null
     * @param data       intent data URI, may be null
     * @param flags      intent flags, OR'd with FLAG_ACTIVITY_NEW_TASK
     */
    fun start(component: String?, action: String?, data: String?, flags: Int): ByteArray {
        try {
            var cn: ComponentName? = parseComponent(component)
            if (cn == null && component != null && component.indexOf('/') < 0) {
                // Bare package name — resolve the launcher activity the same
                // way `monkey -p pkg 1` and Launcher.startActivity do.
                val launch = ctx.packageManager.getLaunchIntentForPackage(component)
                if (launch != null) cn = launch.component
                if (cn == null) {
                    // No package matched. Try the user-visible label as the
                    // last fallback so `mobax open "Chrome"` works alongside
                    // `mobax open com.android.chrome`.
                    val hits = findByLabel(component)
                    if (hits.size == 1) {
                        val a = hits[0]!!.activityInfo
                        cn = ComponentName(a.packageName, a.name)
                    } else if (hits.size > 1) {
                        val pkgs = StringBuilder()
                        for (i in hits.indices) {
                            if (i > 0) pkgs.append(',')
                            pkgs.append(hits[i]!!.activityInfo.packageName)
                        }
                        return err(
                            ("AMBIGUOUS_LABEL:" + component + ":" + pkgs
                                    + " — use the package id (e.g. "
                                    + hits[0]!!.activityInfo.packageName + ")")
                        )
                    }
                }
            }
            if (cn == null) return err("bad-component:$component")
            val intent = Intent()
            intent.setComponent(cn)
            if (!action.isNullOrEmpty()) intent.setAction(action)
            if (!data.isNullOrEmpty()) intent.setData(Uri.parse(data))
            intent.addFlags(flags or Intent.FLAG_ACTIVITY_NEW_TASK)
            // Use IActivityTaskManager.startActivityAsUser (Q+) or
            // IActivityManager.startActivityAsUser (pre-Q). The shell UID is
            // recognised by both as a legitimate caller when callingPackage is
            // "com.android.shell".
            val result = startActivityViaBinder(intent)
            return ok("ok started=" + cn.flattenToShortString() + " result=" + result)
        } catch (ite: InvocationTargetException) {
            val cause: Throwable = (ite.cause ?: ite)
            return err("start-failed:" + cause.javaClass.getSimpleName() + ":" + cause.message)
        } catch (t: Throwable) {
            return err("start-failed:" + t.javaClass.getSimpleName() + ":" + t.message)
        }
    }

    /**
     * Reflectively call `startActivityAsUser` on whichever Binder
     * interface exposes it for our SDK level. We don't pin a specific
     * signature: we walk the methods named "startActivityAsUser" and pick
     * the int-returning overload whose 4th parameter is `Intent` —
     * that shape has been stable since API 28.
     */
    @Throws(Exception::class)
    private fun startActivityViaBinder(intent: Intent?): Int {
        // Try IActivityTaskManager first (Q+), then IActivityManager.
        val candidates = arrayOf(
            iActivityTaskManagerOrNull(),
            iActivityManager(),
        )
        var last: Throwable? = null
        for (svc in candidates) {
            if (svc == null) continue
            val m: Method = findStartActivityAsUser(svc.javaClass) ?: continue
            try {
                val args: Array<Any?> = buildStartArgs(m, intent)
                val ret = m.invoke(svc, *args)
                return ret as? Int ?: 0
            } catch (ite: InvocationTargetException) {
                last = ite.cause ?: ite
            }
        }
        if (last is Exception) throw last
        throw IllegalStateException("no startActivityAsUser binding found")
    }

    fun forceStop(pkg: String?): ByteArray {
        try {
            val iam = iActivityManager()
            // Newer signature: forceStopPackage(String, int) — the int is userId on every
            // API level we ship to. (Q+ stays at this 2-arg shape.)
            val m = iam!!.javaClass.getMethod(
                "forceStopPackage",
                String::class.java,
                Int::class.javaPrimitiveType
            )
            val userId = myUserId()
            m.invoke(iam, pkg, userId)
            return ok("ok force-stop=$pkg")
        } catch (ite: InvocationTargetException) {
            val cause: Throwable = (ite.cause ?: ite)
            return err("force-stop-failed:" + cause.javaClass.getSimpleName() + ":" + cause.message)
        } catch (t: Throwable) {
            return err("force-stop-threw:" + t.javaClass.getSimpleName() + ":" + t.message)
        }
    }

    fun kill(pkg: String?): ByteArray {
        try {
            val am = ctx.getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager
            am.killBackgroundProcesses(pkg)
            return ok("ok kill=$pkg")
        } catch (t: Throwable) {
            return err("kill-failed:" + t.javaClass.getSimpleName() + ":" + t.message)
        }
    }

    fun broadcast(action: String?, component: String?, data: String?): ByteArray {
        try {
            if (action.isNullOrEmpty() && component.isNullOrEmpty()) {
                return err("broadcast-needs-action-or-component")
            }
            val intent = Intent()
            if (!action.isNullOrEmpty()) intent.setAction(action)
            if (!data.isNullOrEmpty()) intent.setData(Uri.parse(data))
            if (!component.isNullOrEmpty()) {
                val cn: ComponentName =
                    parseComponent(component) ?: return err("bad-component:$component")
                intent.setComponent(cn)
            }
            ctx.sendBroadcast(intent)
            return ok("ok broadcast=" + (action ?: component))
        } catch (t: Throwable) {
            return err("broadcast-failed:" + t.javaClass.getSimpleName() + ":" + t.message)
        }
    }

    /** Launcher activities (ACTION_MAIN + CATEGORY_LAUNCHER) whose app label
     * matches `label` case-insensitively. Restricted to launcher
     * activities so a label collision with a non-app package — system
     * providers, overlays — doesn't poison the resolution.  */
    private fun findByLabel(label: String?): MutableList<ResolveInfo?> {
        val out: MutableList<ResolveInfo?> = ArrayList()
        if (label.isNullOrEmpty()) return out
        val pm = ctx.packageManager
        val probe = Intent(Intent.ACTION_MAIN)
        probe.addCategory(Intent.CATEGORY_LAUNCHER)
        val launchers = pm.queryIntentActivities(probe, 0)
        for (ri in launchers) {
            try {
                val l = ri.loadLabel(pm)
                if (label.equals(l.toString(), ignoreCase = true)) {
                    out.add(ri)
                }
            } catch (_: Throwable) {
            }
        }
        return out
    }

    companion object {
        private fun findStartActivityAsUser(cls: Class<*>): Method? {
            var best: Method? = null
            var bestParams = -1
            for (m in cls.getMethods()) {
                if ("startActivityAsUser" != m.name) continue
                if (m.returnType != Int::class.javaPrimitiveType) continue
                val p = m.getParameterTypes()
                // Canonical shape: (IApplicationThread, String, [String featureId,] Intent, ...
                // int userId). The 4th param is Intent on Q+; the 3rd on older surfaces.
                var intentIdx = -1
                for (i in p.indices) {
                    if (p[i] == Intent::class.java) {
                        intentIdx = i
                        break
                    }
                }
                if (intentIdx !in 2..3) continue
                if (p[p.size - 1] != Int::class.javaPrimitiveType) continue
                if (p.size > bestParams) {
                    best = m
                    bestParams = p.size
                }
            }
            return best
        }

        private fun buildStartArgs(m: Method, intent: Intent?): Array<Any?> {
            val p = m.getParameterTypes()
            val args: MutableList<Any?> = ArrayList(p.size)
            var intentIdx = -1
            for (i in p.indices) {
                if (p[i] == Intent::class.java) {
                    intentIdx = i
                    break
                }
            }
            val userId = myUserId()
            for (i in p.indices) {
                if (i == intentIdx) {
                    args.add(intent)
                } else if (i == p.size - 1 && p[i] == Int::class.javaPrimitiveType) {
                    args.add(userId)
                } else if (p[i] == String::class.java) {
                    // Treat the first String slot as callingPackage.
                    args.add(if (needsCallingPackage(args)) "com.android.shell" else null)
                } else if (p[i] == Int::class.javaPrimitiveType) {
                    args.add(0)
                } else {
                    args.add(null)
                }
            }
            return args.toTypedArray()
        }

        private fun needsCallingPackage(argsSoFar: MutableList<Any?>): Boolean {
            for (a in argsSoFar) {
                if ("com.android.shell" == a) return false
            }
            return true
        }

        /** Accept "pkg/.Class" (relative) or "pkg/full.Class".  */
        private fun parseComponent(s: String?): ComponentName? {
            if (s == null) return null
            val slash = s.indexOf('/')
            if (slash <= 0 || slash >= s.length - 1) return null
            val pkg = s.substring(0, slash)
            var cls = s.substring(slash + 1)
            if (cls[0] == '.') cls = pkg + cls
            else if (cls.indexOf('.') < 0) cls = "$pkg.$cls"
            return ComponentName(pkg, cls)
        }

        private fun ok(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}
