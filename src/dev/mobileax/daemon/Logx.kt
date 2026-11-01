package dev.mobileax.daemon

import android.util.Log

/**
 * Daemon logging. Runtime diagnostics go to logcat under tag `mobaxd`
 * (`mobax logcat -s mobaxd:*`). Startup lines also go to stdout/stderr, which the
 * host redirects to /data/local/tmp/mobax.log and shows when the daemon fails
 * to come up.
 */
internal object Logx {
    private const val TAG = "mobaxd"

    fun i(msg: String) {
        Log.i(TAG, msg)
    }

    fun w(msg: String, t: Throwable? = null) {
        Log.w(TAG, msg, t)
    }

    fun e(msg: String, t: Throwable? = null) {
        Log.e(TAG, msg, t)
    }

    /** Startup lifecycle line: mobax.log and logcat. */
    fun boot(msg: String) {
        println(msg)
        Log.i(TAG, msg)
    }

    /** Startup problem worth seeing in mobax.log when the daemon fails to start. */
    fun bootWarn(msg: String, t: Throwable? = null) {
        System.err.println(if (t == null) "warn: $msg" else "warn: $msg: $t")
        Log.w(TAG, msg, t)
    }

    /** Fatal startup error: full stack to mobax.log and logcat. */
    fun fatal(msg: String, t: Throwable) {
        System.err.println("mobaxd: $msg")
        t.printStackTrace()
        Log.e(TAG, msg, t)
    }
}
