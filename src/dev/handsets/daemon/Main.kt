package dev.handsets.daemon

import android.accessibilityservice.AccessibilityServiceInfo
import android.app.UiAutomation
import android.content.Context
import android.hardware.display.DisplayManager
import android.os.HandlerThread
import android.os.Looper
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.lang.reflect.InvocationTargetException
import kotlin.system.exitProcess

/**
 * Entry point for `app_process … dev.handsets.daemon.Main --port=N`.
 *
 * The main thread prepares the main looper, starts listening, and then runs
 * `Looper.loop()` forever. UiAutomation is bound to the main looper and
 * connected from a Work thread once the loop is running, because connect()
 * blocks until its service-connected callback is delivered on that looper.
 */
data object Main {

    @JvmStatic
    fun main(vararg args: String) {
        var port = 9008
        for (a in args) if (a.startsWith("--port=")) port = a.substring("--port=".length).toInt()

        try {
            disableHiddenApiRestrictions()
            if (Looper.getMainLooper() == null) Looper.prepareMainLooper()
            Loop.init()
            // ActivityThread.systemMain() binds handlers to the calling thread's
            // looper, so the system context must be created here, on main.
            val sysCtx = Binders.systemContext()!!

            val ready = CompletableDeferred<Server>()
            HsServer(port, Dispatch(ready)).start()
            Loop.scope.launch {
                try {
                    ready.complete(initDaemon(sysCtx))
                    println("hsd ready (sdk=${android.os.Build.VERSION.SDK_INT})")
                } catch (t: Throwable) {
                    t.printStackTrace()
                    exitProcess(2)
                }
            }
            Looper.loop()
        } catch (t: Throwable) {
            t.printStackTrace()
            exitProcess(2)
        }
    }

    private suspend fun initDaemon(sysCtx: Context): Server = withContext(Loop.Work) {
        val ua = try {
            connectedUiAutomation(Looper.getMainLooper()).also { System.err.println("hsd ua-looper=main") }
        } catch (t: Throwable) {
            System.err.println("hsd: UiAutomation on main looper failed ($t); using hs-ua thread")
            val ht = HandlerThread("hs-ua").apply { start() }
            connectedUiAutomation(ht.looper).also { System.err.println("hsd ua-looper=thread") }
        }
        registerShutdownHook(ua)
        enableInteractiveWindows(ua)
        try { ua.setRunAsMonkey(true) } catch (t: Throwable) { System.err.println("warn: setRunAsMonkey failed: $t") }

        val h = Server.Handlers()
        h.dumper = Dumper(ua)
        h.shot = Screenshot(ua).also { it.probeMirror() }
        h.input = Input(ua)
        h.sysCtx = sysCtx
        h.installer = Installer(sysCtx)
        h.pm = Pm(sysCtx)
        h.manifest = Manifest(sysCtx)
        h.providers = Providers(sysCtx)
        h.location = Location()
        h.notifs = Notifications()
        h.clip = Clipboard()
        h.am = Am(sysCtx)
        try { h.props = Props() } catch (t: Throwable) { System.err.println("warn: Props init failed: $t") }
        h.settings = SettingsApi(sysCtx)
        h.wm = Wm(sysCtx.getSystemService(Context.DISPLAY_SERVICE) as DisplayManager)
        h.uiEvents = UiEvents(ua)
        h.nodes = NodeActions(ua)
        h.state = State(sysCtx, ua, h.uiEvents)

        // Warm the dump path: the first dump_active on a cold VM costs ~1 s.
        Loop.scope.launch(Loop.Work) {
            try {
                val t0 = System.nanoTime()
                h.dumper.dumpActive()
                System.err.println("hsd warmup: dump_active in ${(System.nanoTime() - t0) / 1_000_000}ms")
            } catch (t: Throwable) {
                System.err.println("warn: dump_active warmup failed: $t")
            }
        }
        Server(h)
    }

    private fun connectedUiAutomation(looper: Looper): UiAutomation {
        val connCls = Class.forName("android.app.UiAutomationConnection")
        val conn = connCls.getDeclaredConstructor().apply { isAccessible = true }.newInstance()
        val iConnCls = Class.forName("android.app.IUiAutomationConnection")
        val ua = createUiAutomation(looper, conn, iConnCls)
        connectWithRetry(ua)
        return ua
    }

    private fun createUiAutomation(looper: Looper, conn: Any, iConnCls: Class<*>): UiAutomation {
        val ctor = try {
            UiAutomation::class.java.getDeclaredConstructor(Looper::class.java, iConnCls)
        } catch (e: NoSuchMethodException) {
            // Newer SDKs may only expose (Context, IUiAutomationConnection).
            try {
                val c = UiAutomation::class.java.getDeclaredConstructor(Context::class.java, iConnCls)
                c.isAccessible = true
                return c.newInstance(Binders.systemContext(), conn)
            } catch (t: Throwable) {
                throw IllegalStateException("no usable UiAutomation constructor on SDK ${android.os.Build.VERSION.SDK_INT}", t)
            }
        }
        ctor.isAccessible = true
        return ctor.newInstance(looper, conn)
    }

    private fun connectWithRetry(ua: UiAutomation) {
        var last: Throwable? = null
        repeat(20) {
            try {
                invokeConnect(ua)
                return
            } catch (ite: InvocationTargetException) {
                val cause = ite.cause ?: ite
                last = cause
                if (cause.toString().contains("already registered")) {
                    Thread.sleep(250)   // Work thread, never the looper
                } else throw ite
            }
        }
        throw IllegalStateException("connect failed after retries", last)
    }

    private fun invokeConnect(ua: UiAutomation) {
        try {
            UiAutomation::class.java.getDeclaredMethod("connect", Int::class.javaPrimitiveType)
                .apply { isAccessible = true }.invoke(ua, 1)
            return
        } catch (_: NoSuchMethodException) {
        }
        UiAutomation::class.java.getDeclaredMethod("connect").apply { isAccessible = true }.invoke(ua)
    }

    private fun enableInteractiveWindows(ua: UiAutomation) {
        try {
            val info = ua.serviceInfo ?: return
            info.flags = info.flags or AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
            ua.serviceInfo = info
        } catch (t: Throwable) {
            System.err.println("warn: enableInteractiveWindows failed: $t")
        }
    }

    private fun registerShutdownHook(ua: UiAutomation) {
        Runtime.getRuntime().addShutdownHook(Thread({
            try {
                UiAutomation::class.java.getDeclaredMethod("disconnect").apply { isAccessible = true }.invoke(ua)
            } catch (_: Throwable) {
            }
        }, "hs-shutdown"))
    }

    private fun disableHiddenApiRestrictions() {
        try {
            val vmRuntime = Class.forName("dalvik.system.VMRuntime")
            val runtime = vmRuntime.getDeclaredMethod("getRuntime").invoke(null)
            vmRuntime.getDeclaredMethod("setHiddenApiExemptions", Array<String>::class.java)
                .invoke(runtime, arrayOf("L") as Any)
        } catch (t: Throwable) {
            System.err.println("warn: could not lift hidden-api restrictions: $t")
        }
    }
}
