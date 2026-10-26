package dev.handsets.daemon

import android.content.Context
import android.os.IBinder
import android.os.Looper
import android.os.Process
import kotlin.concurrent.Volatile

/**
 * Central pool for Binder + system-Context lookups.
 * 
 * Everything goes through reflection because we want a single jar to build
 * against the public SDK while still calling hidden APIs at runtime
 * (ServiceManager, IPackageManager$Stub, IActivityManager$Stub).
 * 
 * [Main.disableHiddenApiRestrictions] runs at process start so all of
 * the reflective lookups below succeed without throwing.
 */
internal object Binders {
    /** ActivityThread.systemMain() binds to the calling thread's looper: first call on main. */
    private val systemContextLazy: Context by lazy {
        try {
            if (Looper.getMainLooper() == null) {
                // app_process has no ActivityThread to prepare the main looper for us.
                @Suppress("DEPRECATION")
                Looper.prepareMainLooper()
            }
            val at = Class.forName("android.app.ActivityThread")
            val inst = at.getMethod("systemMain").invoke(null)
            at.getMethod("getSystemContext").invoke(inst) as Context
        } catch (t: Throwable) {
            throw RuntimeException("system Context lookup failed", t)
        }
    }

    @JvmStatic
    fun systemContext(): Context? = systemContextLazy

    @JvmStatic
    fun service(name: String?): IBinder {
        try {
            val sm = Class.forName("android.os.ServiceManager")
            val get = sm.getMethod("getService", String::class.java)
            val b = get.invoke(null, name) as IBinder
            checkNotNull(b) { "service '" + name + "' not registered" }
            return b
        } catch (t: Throwable) {
            throw RuntimeException("ServiceManager.getService(" + name + ") failed", t)
        }
    }

    /** Resolve `<stubFqcn>.asInterface(IBinder)` and wrap the given binder.  */
    @JvmStatic
    fun asInterface(stubFqcn: String, b: IBinder?): Any? {
        try {
            val stub = Class.forName(stubFqcn)
            val m = stub.getMethod("asInterface", IBinder::class.java)
            return m.invoke(null, b)
        } catch (t: Throwable) {
            throw RuntimeException("asInterface failed for " + stubFqcn, t)
        }
    }

    @JvmStatic
    fun iPackageManager(): Any? {
        return asInterface("android.content.pm.IPackageManager\$Stub", service("package"))
    }

    /**
     * Permission management split out from IPackageManager around API 30.
     * Falls back to iPackageManager() so callers can try whichever interface
     * exposes the method they need.
     */
    @JvmStatic
    fun iPermissionManagerOrNull(): Any? {
        try {
            return asInterface(
                "android.permission.IPermissionManager\$Stub", service("permissionmgr")
            )
        } catch (t: Throwable) {
            return null
        }
    }

    @JvmStatic
    fun iActivityManager(): Any? {
        return asInterface("android.app.IActivityManager\$Stub", service("activity"))
    }

    /** `UserHandle.myUserId()` is @hide; reach it via reflection.  */
    @JvmStatic
    fun myUserId(): Int {
        try {
            val m = Class.forName("android.os.UserHandle").getMethod("myUserId")
            return (m.invoke(null) as kotlin.Int?)!!
        } catch (t: Throwable) {
            // Decode user-id directly from our UID (userId * 100000 + appId).
            return Process.myUid() / 100000
        }
    }

    /** API 30+. May be unavailable on older surfaces — callers handle null.  */
    @JvmStatic
    fun iActivityTaskManagerOrNull(): Any? {
        try {
            return asInterface(
                "android.app.IActivityTaskManager\$Stub", service("activity_task")
            )
        } catch (t: Throwable) {
            return null
        }
    }
}
