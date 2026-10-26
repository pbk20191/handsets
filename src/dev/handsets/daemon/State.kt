package dev.handsets.daemon

import android.app.ActivityManager.RunningAppProcessInfo
import android.app.ActivityManager.RunningTaskInfo
import android.app.UiModeManager
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ApplicationInfo
import android.content.res.Configuration
import android.graphics.Point
import android.hardware.display.DisplayManager
import android.hardware.display.DisplayManager.DisplayListener
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.wifi.WifiManager
import android.os.BatteryManager
import android.os.Build
import android.os.Handler
import android.os.StatFs
import android.provider.Settings
import android.view.Display
import android.view.accessibility.AccessibilityEvent
import dev.handsets.daemon.Binders.asInterface
import dev.handsets.daemon.Binders.iActivityManager
import dev.handsets.daemon.Binders.iActivityTaskManagerOrNull
import dev.handsets.daemon.Binders.service
import dev.handsets.daemon.Loop.mainHandler
import kotlinx.coroutines.flow.StateFlow
import java.io.BufferedReader
import java.io.FileReader
import java.lang.reflect.Method
import java.net.Inet4Address
import java.net.NetworkInterface
import java.nio.charset.StandardCharsets
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.Volatile

/**
 * Fast state-query surface. Every method here is a single direct-binder call
 * (or a manager wrapper that's itself a thin shim over one) — the goal is
 * sub-millisecond wire time so callers don't have to parse `dumpsys`
 * output.
 * 
 * Each public method returns a UTF-8 byte payload ready to ship as one frame.
 */
internal class State(
    private val ctx: Context, // for accessibility events
    private val uiEvents: UiEvents
) {
    private val bm: BatteryManager = ctx.getSystemService(BatteryManager::class.java)
    private val displayMgr: DisplayManager =
        ctx.getSystemService(DisplayManager::class.java) // for screen state changes

    // Cached binders/methods so the hot path doesn't pay reflective lookup
    // each call. All set lazily on first use; null means "not available on
    // this SDK level / not yet probed".

    @Volatile
    private var isInteractive1: Method? = null // (int displayId)

    @Volatile
    private var isInteractive0: Method? = null // ()


    @Volatile
    private var getTasks: Method? = null


    @Volatile
    private var getRunningAppProcesses: Method? = null

    private val cachedDevice = AtomicReference<ByteArray?>()
    private val cachedDeviceStamp = AtomicReference(longArrayOf(0L))
    private val refresher = StateRefresher(this, DEBOUNCE_MS, HEARTBEAT_MS)

    private val refresherStarted = java.util.concurrent.atomic.AtomicBoolean(false)

    @Volatile
    private var listenersRegistered = false

    /**
     * Every fresh device snapshot, latest only (push mode for state_watch).
     * Seeded with the current snapshot so a new collector gets it at once;
     * computes one if none exists yet, so call it off the main thread.
     */
    fun snapshots(): StateFlow<ByteArray?> {
        var snap = cachedDevice.get()
        if (snap == null) snap = device()
        refresher.snapshots.compareAndSet(null, snap)
        ensureRefresher()
        return refresher.snapshots
    }

    // ---------- power ----------
    fun interactive(): ByteArray {
        try {
            val pm = powerBinder()
            if (isInteractive1 != null) {
                val b = isInteractive1!!.invoke(pm, Display.DEFAULT_DISPLAY) as Boolean
                return bytes(b.toString())
            }
            if (isInteractive0 != null) {
                val b = isInteractive0!!.invoke(pm) as Boolean
                return bytes(b.toString())
            }
            return err("isInteractive-not-found")
        } catch (t: Throwable) {
            return err("interactive-failed:" + t.message)
        }
    }

    // ---------- battery ----------
    fun batteryLevel(): ByteArray {
        try {
            val level = bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY)
            return bytes(level.toString())
        } catch (t: Throwable) {
            return err("battery-level-failed:" + t.message)
        }
    }

    fun batteryCharging(): ByteArray {
        return try {
            bytes(bm.isCharging.toString())
        } catch (t: Throwable) {
            err("battery-charging-failed:" + t.message)
        }
    }

    // ---------- foreground / tasks ----------
    fun topActivity(): ByteArray {
        try {
            val cn = resolveTopComponent()
            return bytes(cn?.flattenToShortString() ?: "null")
        } catch (t: Throwable) {
            return err("top-failed:" + t.message)
        }
    }

    @Throws(Exception::class)
    private fun resolveTopComponent(): ComponentName? {
        val iatm = activityTaskBinder() ?: return null
        val gt = getTasks ?: return null
        val p = gt.getParameterTypes()
        val args = arrayOfNulls<Any>(p.size)
        // First arg is maxNum: 1. Remaining bool/int slots: false/0.
        for (i in p.indices) {
            if (i == 0 && p[i] == Int::class.javaPrimitiveType) args[i] = 1
            else if (p[i] == Int::class.javaPrimitiveType) args[i] = 0
            else if (p[i] == Boolean::class.javaPrimitiveType) args[i] = false
            else args[i] = null
        }
        @Suppress("UNCHECKED_CAST") // reflective IActivityTaskManager.getTasks
        val tasks =
            gt.invoke(iatm, *args) as MutableList<RunningTaskInfo?>?
        if (tasks.isNullOrEmpty()) return null
        return tasks[0]!!.topActivity
    }

    // ---------- running processes ----------
    fun procs(): ByteArray {
        try {
            val iam = activityBinder()
            if (iam == null || getRunningAppProcesses == null) return err("not-bound")
            @Suppress("UNCHECKED_CAST") // reflective IActivityManager.getRunningAppProcesses
            val ps =
                getRunningAppProcesses!!.invoke(iam) as MutableList<RunningAppProcessInfo>?
            val sb = StringBuilder(2048)
            if (ps != null) {
                for (i in ps) {
                    sb.append(i.pid).append('\t')
                        .append(i.uid).append('\t')
                        .append(i.importance).append('\t')
                        .append(i.processName).append('\n')
                }
            }
            return sb.toString().toByteArray(StandardCharsets.UTF_8)
        } catch (t: Throwable) {
            return err("procs-failed:" + t.message)
        }
    }

    // ---------- composite snapshot ----------
    /**
     * Single-frame JSON with every common state field a UI driver wants.
     * Returns a cached snapshot that's at most {@value #CACHE_REFRESH_MS}ms
     * stale. First call starts the background refresher; subsequent calls
     * just hand back the latest byte[] (no binder hops).
     */
    fun device(): ByteArray {
        ensureRefresher()
        var snap = cachedDevice.get()
        if (snap != null) return snap
        // First call before the refresher has produced anything — block once.
        snap = computeDevice()
        cachedDevice.set(snap)
        cachedDeviceStamp.set(longArrayOf(System.currentTimeMillis()))
        return snap
    }

    /** Force-collect a fresh snapshot, update the cache, return the bytes.  */
    fun deviceFresh(): ByteArray {
        val snap = computeDevice()
        cachedDevice.set(snap)
        cachedDeviceStamp.set(longArrayOf(System.currentTimeMillis()))
        ensureRefresher()
        return snap
    }

    /** Starts listeners + refresher on the main looper (any thread, idempotent).  */
    private fun ensureRefresher() {
        if (!refresherStarted.compareAndSet(false, true)) return
        Loop.post {
            registerListeners()
            refresher.start()
        }
    }

    /** Main thread: store a fresh snapshot and push it to collectors.  */
    fun publish(s: ByteArray?) {
        cachedDevice.set(s)
        cachedDeviceStamp.set(longArrayOf(System.currentTimeMillis()))
        refresher.snapshots.value = s
    }

    /** Main thread: something changed; refresh soon.  */
    private fun markDirty() {
        refresher.mark()
    }

    /** Main thread.  */
    private fun registerListeners() {
        if (listenersRegistered) return
        listenersRegistered = true

        // 1. Accessibility events → top-activity change detection. Route
        //    through the shared UiEvents dispatcher so wait-for-* primitives
        //    can subscribe to the same stream.
        uiEvents.subscribe { ev ->
            val t = ev.eventType
            if (t == AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED
                || t == AccessibilityEvent.TYPE_WINDOWS_CHANGED
            ) {
                markDirty()
            }
        }

        // 1b. Package install/remove → invalidate app counts.
        try {
            val pkgFilter = IntentFilter()
            pkgFilter.addAction(Intent.ACTION_PACKAGE_ADDED)
            pkgFilter.addAction(Intent.ACTION_PACKAGE_REMOVED)
            pkgFilter.addAction(Intent.ACTION_PACKAGE_REPLACED)
            pkgFilter.addDataScheme("package")
            val pkgH = mainHandler
            val pkgR: BroadcastReceiver = object : BroadcastReceiver() {
                override fun onReceive(c: Context?, i: Intent) {
                    Logx.i("pkg broadcast ${i.action} data=${i.dataString}")
                    invalidateAppCounts()
                    markDirty()
                }
            }
            // System broadcasts (PACKAGE_*) come from outside our process, so
            // the receiver must be EXPORTED on API 34+. The 5-arg overload
            // (flag) exists from API 33; use it via reflection so we keep
            // compiling against any SDK.
            try {
                val m = Context::class.java.getMethod(
                    "registerReceiver",
                    BroadcastReceiver::class.java,
                    IntentFilter::class.java,
                    String::class.java, Handler::class.java, Int::class.javaPrimitiveType
                )
                m.invoke(ctx, pkgR, pkgFilter, null, pkgH,  /* RECEIVER_EXPORTED */2)
            } catch (_: Throwable) {
                ctx.registerReceiver(pkgR, pkgFilter, null, pkgH)
            }
        } catch (t: Throwable) {
            Logx.w("package broadcast listener failed", t)
        }

        // 2. Display state changes → screen on/off.
        try {
            val handler = mainHandler
            displayMgr.registerDisplayListener(object : DisplayListener {
                override fun onDisplayAdded(id: Int) {
                    markDirty()
                }

                override fun onDisplayRemoved(id: Int) {
                    markDirty()
                }

                override fun onDisplayChanged(id: Int) {
                    markDirty()
                }
            }, handler)
        } catch (t: Throwable) {
            Logx.w("State display listener failed", t)
        }

        // 3. Configuration changes → uiMode (dark/light theme), locale,
        //    orientation. ACTION_CONFIGURATION_CHANGED is what UiModeManager
        //    fires when night mode toggles; ComponentCallbacks dispatch isn't
        //    reliable from a shell-uid Context (our Resources don't get
        //    updated by ActivityThread the way an app process's would), so
        //    we listen for the system broadcast directly.
        try {
            val cfgFilter = IntentFilter(
                Intent.ACTION_CONFIGURATION_CHANGED
            )
            val cfgH = mainHandler
            val cfgR: BroadcastReceiver = object : BroadcastReceiver() {
                override fun onReceive(c: Context?, i: Intent?) {
                    markDirty()
                }
            }
            try {
                val m = Context::class.java.getMethod(
                    "registerReceiver",
                    BroadcastReceiver::class.java,
                    IntentFilter::class.java,
                    String::class.java, Handler::class.java, Int::class.javaPrimitiveType
                )
                m.invoke(ctx, cfgR, cfgFilter, null, cfgH,  /* RECEIVER_EXPORTED */2)
            } catch (_: Throwable) {
                ctx.registerReceiver(cfgR, cfgFilter, null, cfgH)
            }
        } catch (t: Throwable) {
            Logx.w("State config-change listener failed", t)
        }
    }

    /** Static fields — props, total RAM/storage, CPU model. Computed once.  */
    private val staticJsonLazy: String by lazy { computeStaticJson() }

    private fun staticJson(): String = staticJsonLazy

    private fun computeStaticJson(): String {
        val sb = StringBuilder(512)
        sb.append("\"model\":").append(jsonStr(getprop("ro.product.model"))).append(',')
        sb.append("\"manufacturer\":").append(jsonStr(getprop("ro.product.manufacturer")))
            .append(',')
        sb.append("\"device\":").append(jsonStr(getprop("ro.product.device"))).append(',')
        sb.append("\"release\":").append(jsonStr(getprop("ro.build.version.release"))).append(',')
        sb.append("\"codename\":").append(jsonStr(getprop("ro.build.version.codename"))).append(',')
        sb.append("\"build_type\":").append(jsonStr(getprop("ro.build.type"))).append(',')
        sb.append("\"fingerprint\":").append(jsonStr(getprop("ro.build.fingerprint"))).append(',')
        sb.append("\"abi\":").append(jsonStr(getprop("ro.product.cpu.abi"))).append(',')
        sb.append("\"abi_list\":").append(jsonStr(getprop("ro.product.cpu.abilist"))).append(',')
        sb.append("\"bootloader\":").append(jsonStr(getprop("ro.bootloader"))).append(',')
        sb.append("\"hardware\":").append(jsonStr(getprop("ro.hardware"))).append(',')
        sb.append("\"locale\":").append(jsonStr(getprop("persist.sys.locale"))).append(',')
        sb.append("\"timezone\":").append(jsonStr(getprop("persist.sys.timezone"))).append(',')
        sb.append("\"kernel\":").append(jsonStr(readKernelVersion())).append(',')
        sb.append("\"cpu_cores\":").append(readCpuCores()).append(',')
        sb.append("\"cpu_model\":").append(jsonStr(readCpuModel())).append(',')
        sb.append("\"total_ram_kb\":").append(readMemTotalKb()).append(',')
        sb.append("\"total_storage_kb\":").append(readStorageTotalKb()).append(',')
        sb.append("\"sdk\":").append(Build.VERSION.SDK_INT).append(',')
        return sb.toString()
    }

    @Volatile
    private var appCounts: LongArray? = null // [total, third-party]

    @Volatile
    private var appCountsAt: Long = 0 // ms-since-epoch when computed

    private fun appCounts(): LongArray {
        val cur = appCounts
        if (cur != null && System.currentTimeMillis() - appCountsAt < APP_COUNT_TTL_MS) {
            return cur
        }
    // Two callers racing past the TTL check both recompute; harmless.
        var total: Long = 0
        var third: Long = 0
        try {
            for (p in ctx.packageManager.getInstalledPackages(0)) {
                total++
                val ai = p.applicationInfo
                if (ai != null && (ai.flags and ApplicationInfo.FLAG_SYSTEM) == 0) {
                    third++
                }
            }
        } catch (_: Throwable) {
        }
        appCounts = longArrayOf(total, third)
        appCountsAt = System.currentTimeMillis()
        return appCounts!!
    }

    private fun invalidateAppCounts() {
        appCounts = null
    }

    fun computeDevice(): ByteArray {
        val sb = StringBuilder(1024)
        sb.append('{')

        // Static (one-time): model / sdk / abi / kernel / totals / locale.
        sb.append(staticJson())

        // Dynamic — live binder fields.
        appendField(sb, "interactive", interactive(), true)
        appendField(sb, "battery_level", batteryLevel(), true)
        appendField(sb, "battery_charging", batteryCharging(), true)
        sb.append("\"battery_temp_c\":").append(readBatteryTempC()).append(',')
        try {
            val top = resolveTopComponent()
            sb.append("\"top_activity\":")
                .append(if (top == null) "null" else ("\"" + top.flattenToShortString() + "\""))
                .append(',')
        } catch (_: Throwable) {
            sb.append("\"top_activity\":null,")
        }

        // Dynamic — derived from /proc and StatFs (cheap, ~ms total).
        sb.append("\"uptime_s\":").append(readUptimeS()).append(',')
        sb.append("\"mem_available_kb\":").append(readMemAvailableKb()).append(',')
        sb.append("\"storage_free_kb\":").append(readStorageFreeKb()).append(',')

        // Dynamic — app counts (event-driven via PACKAGE_* broadcasts).
        val ac = appCounts()
        sb.append("\"app_count\":").append(ac[0]).append(',')
        sb.append("\"app_count_3rd\":").append(ac[1]).append(',')

        // Dynamic — theme (changes when user toggles dark mode).
        sb.append("\"theme\":").append(jsonStr(readTheme())).append(',')

        // Dynamic — network state (transport + IP).
        sb.append("\"network\":").append(jsonStr(readNetworkType())).append(',')
        sb.append("\"ip\":").append(jsonStr(readIp())).append(',')
        sb.append("\"wifi_ssid\":").append(jsonStr(readWifiSsid())).append(',')

        // Dynamic — display.
        val d = ctx.getSystemService(DisplayManager::class.java)
            .getDisplay(Display.DEFAULT_DISPLAY)
        val sz = Point()
        @Suppress("DEPRECATION") // WindowMetrics needs API 30 and a window context
        d.getRealSize(sz)
        sb.append("\"width\":").append(sz.x).append(',')
        sb.append("\"height\":").append(sz.y).append(',')
        sb.append("\"rotation\":").append(d.rotation).append(',')

        sb.append("\"computed_at_ms\":").append(System.currentTimeMillis())
        sb.append('}')
        return sb.toString().toByteArray(StandardCharsets.UTF_8)
    }

    private fun readTheme(): String {
        // The shell-UID Context we run under has no app Resources attached,
        // so its Configuration stays frozen at UI_MODE_NIGHT_UNDEFINED no
        // matter what the user toggles, and UiModeManager.getNightMode() in
        // this process can also lag behind the system service. The
        // authoritative source is the Settings.Secure entry the
        // UiModeManagerService itself writes to: 1=NO, 2=YES, 0/3=AUTO/CUSTOM.
        try {
            val v = Settings.Secure.getInt(
                ctx.contentResolver, "ui_night_mode", -1
            )
            if (v == 2) return "dark"
            if (v == 1) return "light"
        } catch (_: Throwable) {
        }
        try {
            val uim = ctx.getSystemService(UiModeManager::class.java)
            if (uim != null) {
                val mode = uim.nightMode
                if (mode == UiModeManager.MODE_NIGHT_YES) return "dark"
                if (mode == UiModeManager.MODE_NIGHT_NO) return "light"
            }
        } catch (_: Throwable) {
        }
        try {
            val night = (ctx.resources.configuration.uiMode
                    and Configuration.UI_MODE_NIGHT_MASK)
            return if (night == Configuration.UI_MODE_NIGHT_YES) "dark" else "light"
        } catch (_: Throwable) {
            return ""
        }
    }

    /** "wifi" / "cellular" / "ethernet" / "vpn" / "" if no usable link.
     * Prefers ConnectivityManager but falls back to interface-name
     * heuristics — the emulator and some odd shell-UID contexts return
     * a null active network from ConnectivityManager.  */
    private fun readNetworkType(): String {
        try {
            val cm = ctx.getSystemService(ConnectivityManager::class.java)
            val active = cm.activeNetwork
            if (active != null) {
                val caps = cm.getNetworkCapabilities(active)
                if (caps != null) {
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) return "wifi"
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) return "cellular"
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET)) return "ethernet"
                    if (caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) return "vpn"
                    return "other"
                }
            }
        } catch (_: Throwable) {
        }
        // Fallback: pick the first non-loopback interface that has an IPv4.
        try {
            val ifs =
                NetworkInterface.getNetworkInterfaces()
            while (ifs.hasMoreElements()) {
                val ni = ifs.nextElement()
                if (!ni.isUp() || ni.isLoopback) continue
                if (!has4(ni)) continue
                val n = ni.name
                if (n.startsWith("wlan")) return "wifi"
                if (n.startsWith("eth")) return "ethernet"
                if (n.startsWith("rmnet")) return "cellular"
                if (n.startsWith("tun") || n.startsWith("ppp")) return "vpn"
                return n
            }
        } catch (_: Throwable) {
        }
        return ""
    }

    /** First non-loopback, non-link-local IPv4 address on any up interface.  */
    private fun readIp(): String {
        // ConnectivityManager first (cleaner attribution).
        try {
            val cm = ctx.getSystemService(ConnectivityManager::class.java)
            val active = cm.activeNetwork
            if (active != null) {
                val lp = cm.getLinkProperties(active)
                if (lp != null) {
                    for (la in lp.linkAddresses) {
                        val a = la.address
                        if (a is Inet4Address && !a.isLoopbackAddress() && !a.isLinkLocalAddress()) {
                            a.hostAddress?.let { return it }
                        }
                    }
                }
            }
        } catch (_: Throwable) {
        }
        // Fallback: walk NetworkInterface — works on the emulator where
        // ConnectivityManager returns no active network even though
        // eth0/wlan0 has a real IPv4 address.
        try {
            val ifs =
                NetworkInterface.getNetworkInterfaces()
            while (ifs.hasMoreElements()) {
                val ni = ifs.nextElement()
                if (!ni.isUp() || ni.isLoopback) continue
                val a = ni.getInetAddresses()
                while (a.hasMoreElements()) {
                    val addr = a.nextElement()
                    if (addr is Inet4Address && !addr.isLinkLocalAddress()) {
                        addr.hostAddress?.let { return it }
                    }
                }
            }
        } catch (_: Throwable) {
        }
        return ""
    }

    /** Connected wifi SSID, stripped of WifiInfo's surrounding quotes.  */
    private fun readWifiSsid(): String {
        try {
            val wm = ctx.getSystemService(WifiManager::class.java) ?: return ""
            @Suppress("DEPRECATION") // WifiManager.getConnectionInfo: the replacement needs a network callback
            val info = wm.connectionInfo ?: return ""
            var s = info.getSSID()
            if (s == null || s == "<unknown ssid>") return ""
            if (s.length >= 2 && s[0] == '"' && s[s.length - 1] == '"') {
                s = s.substring(1, s.length - 1)
            }
            return s
        } catch (_: Throwable) {
            return ""
        }
    }

    // ---------- lazy binder/method caches ----------
    /** Resolved once (thread-safe `lazy`); also wires the methods it probes. */
    private val powerBinderLazy: Any? by lazy {
            val svc = asInterface(
                $$"android.os.IPowerManager$Stub", service("power")
            )
            // Probe both overloads exactly once.
            for (m in svc!!.javaClass.getMethods()) {
                if ("isInteractive" != m.name) continue
                val pt = m.getParameterTypes()
                if (pt.size == 1 && pt[0] == Int::class.javaPrimitiveType && isInteractive1 == null) {
                    isInteractive1 = m
                } else if (pt.size == 0 && isInteractive0 == null) {
                    isInteractive0 = m
                }
            }
            svc
    }

    private fun powerBinder(): Any? = powerBinderLazy

    /** Resolved once (thread-safe `lazy`); also wires the methods it probes. */
    private val activityTaskBinderLazy: Any? by lazy {
        val svc = iActivityTaskManagerOrNull() ?: return@lazy null
        // Pick the widest `getTasks` overload — first arg is max task count.
            var best: Method? = null
            for (m in svc.javaClass.getMethods()) {
                if ("getTasks" != m.name) continue
                val pt = m.getParameterTypes()
                if (pt.size >= 1 && pt[0] == Int::class.javaPrimitiveType && (best == null || pt.size > best.getParameterTypes().size)) {
                    best = m
                }
            }
            getTasks = best
            svc
    }

    private fun activityTaskBinder(): Any? = activityTaskBinderLazy

    /** Resolved once (thread-safe `lazy`); also wires the methods it probes. */
    private val activityBinderLazy: Any? by lazy {
            val svc = iActivityManager()
            for (m in svc!!.javaClass.getMethods()) {
                if ("getRunningAppProcesses" == m.name
                    && m.getParameterTypes().size == 0
                ) {
                    getRunningAppProcesses = m
                    break
                }
            }
            svc
    }

    private fun activityBinder(): Any? = activityBinderLazy

    companion object {
        // ---------- device-JSON cache ----------
        //
        // device() collects ~5 binder fields. At ~1ms each the wire-level cost
        // dominates. We refresh in the background — but event-driven instead of
        // on a fixed timer:
        //   • UiAutomation accessibility events flag changes to the top activity.
        //   • DisplayManager.DisplayListener flags changes to the screen state.
        //   • A 5s heartbeat covers fields we don't have an event hook for
        //     (battery level/charging — those move slowly enough that 5s is fine).
        // Bursts get coalesced by a small post-trigger debounce.
        private const val HEARTBEAT_MS = 5000L
        private const val DEBOUNCE_MS = 50L

        // ---------- app-count cache ----------
        //
        // Ideally event-driven: PACKAGE_ADDED / PACKAGE_REMOVED / PACKAGE_REPLACED
        // BroadcastReceiver below invalidates this cache the moment a package
        // changes. In practice the receiver doesn't fire from our app_process
        // Context on every Android build (same calling-identity wall as the
        // SettingsProvider), so we *also* TTL the cache as a safety net.
        // Slightly under the heartbeat so every heartbeat-driven snapshot
        // refresh re-counts and picks up any silent install / uninstall.
        private const val APP_COUNT_TTL_MS: Long = HEARTBEAT_MS - 500

        // ---------- field readers ----------
        @Volatile
        private var spGet: Method? = null
        private fun getprop(key: String?): String {
            try {
                if (spGet == null) {
                    spGet = Class.forName("android.os.SystemProperties")
                        .getDeclaredMethod("get", String::class.java)
                }
                val v: Any? = spGet!!.invoke(null, key)
                return v?.toString() ?: ""
            } catch (_: Throwable) {
                return ""
            }
        }

        private fun readFile(path: String): String {
            try {
                BufferedReader(FileReader(path)).use { r ->
                    val sb = StringBuilder(256)
                    var line: String?
                    while ((r.readLine().also { line = it }) != null) sb.append(line).append('\n')
                    return sb.toString()
                }
            } catch (_: Throwable) {
                return ""
            }
        }

        private fun readKernelVersion(): String? {
            val raw: String = readFile("/proc/version")
            if (raw.isEmpty()) return ""
            // "Linux version 6.6.30-android15-…" → grab the version token.
            val parts: Array<String?> =
                raw.split("\\s+".toRegex()).dropLastWhile { it.isEmpty() }.toTypedArray()
            var i = 0
            while (i + 1 < parts.size) {
                if ("version" == parts[i]) return parts[i + 1]
                i++
            }
            return raw.trim { it <= ' ' }
        }

        private fun readCpuCores(): Long {
            val raw: String = readFile("/proc/cpuinfo")
            var n: Long = 0
            for (line in raw.split("\n".toRegex()).dropLastWhile { it.isEmpty() }
                .toTypedArray()) if (line.startsWith("processor")) n++
            return n
        }

        private fun readCpuModel(): String {
            val raw: String = readFile("/proc/cpuinfo")
            for (line in raw.split("\n".toRegex()).dropLastWhile { it.isEmpty() }.toTypedArray()) {
                if (line.startsWith("Hardware")) return line.substring(line.indexOf(':') + 1)
                    .trim { it <= ' ' }
                if (line.startsWith("model name")) return line.substring(line.indexOf(':') + 1)
                    .trim { it <= ' ' }
            }
            return ""
        }

        private fun parseKBField(content: String, prefix: String): Long {
            for (line in content.split("\n".toRegex()).dropLastWhile { it.isEmpty() }
                .toTypedArray()) {
                if (line.startsWith(prefix)) {
                    val toks: Array<String?> = line.trim { it <= ' ' }.split("\\s+".toRegex())
                        .dropLastWhile { it.isEmpty() }.toTypedArray()
                    if (toks.size >= 2) {
                        try {
                            return toks[1]!!.toLong()
                        } catch (_: NumberFormatException) {
                        }
                    }
                }
            }
            return 0L
        }

        private fun readMemTotalKb(): Long {
            return parseKBField(readFile("/proc/meminfo"), "MemTotal:")
        }

        private fun readMemAvailableKb(): Long {
            return parseKBField(readFile("/proc/meminfo"), "MemAvailable:")
        }

        private fun readUptimeS(): Long {
            val raw = readFile("/proc/uptime").trim { it <= ' ' }
            if (raw.isEmpty()) return 0
            try {
                return raw.split("\\s+".toRegex()).dropLastWhile { it.isEmpty() }
                    .toTypedArray()[0].toDouble().toLong()
            } catch (_: Throwable) {
                return 0
            }
        }

        private fun readStorageTotalKb(): Long {
            try {
                val sf = StatFs("/data")
                return sf.totalBytes / 1024
            } catch (_: Throwable) {
                return 0
            }
        }

        private fun readStorageFreeKb(): Long {
            try {
                val sf = StatFs("/data")
                return sf.availableBytes / 1024
            } catch (_: Throwable) {
                return 0
            }
        }

        /** Battery temperature in °C (driver value is tenths-of-a-degree).  */
        private fun readBatteryTempC(): String {
            val raw = readFile("/sys/class/power_supply/battery/temp").trim { it <= ' ' }
            if (raw.isEmpty()) return "null"
            try {
                val tenths = raw.toInt()
                return (tenths / 10.0).toString()
            } catch (_: NumberFormatException) {
                return "null"
            }
        }

        private fun has4(ni: NetworkInterface): Boolean {
            val a = ni.getInetAddresses()
            while (a.hasMoreElements()) {
                if (a.nextElement() is Inet4Address) return true
            }
            return false
        }

        private fun jsonStr(s: String?): String {
            if (s == null) return "null"
            return "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"") + "\""
        }

        private fun appendField(
            sb: StringBuilder,
            key: String?,
            valBytes: ByteArray,
            trailingComma: Boolean
        ) {
            var v = String(valBytes, StandardCharsets.UTF_8)
            var quote = ("true" != v) && ("false" != v) && !v.matches("-?\\d+".toRegex())
            if (v.startsWith("ERR:")) {
                v = "null"
                quote = false
            }
            sb.append('"').append(key).append("\":")
            if (quote) sb.append('"').append(v.replace("\"", "\\\"")).append('"')
            else sb.append(v)
            if (trailingComma) sb.append(',')
        }

        private fun bytes(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}
