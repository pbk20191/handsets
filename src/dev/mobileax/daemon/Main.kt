package dev.mobileax.daemon

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
 * Entry point for `app_process … dev.mobileax.daemon.Main --port=N`.
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
            // app_process has no ActivityThread to prepare the main looper for us.
            @Suppress("DEPRECATION")
            if (Looper.getMainLooper() == null) Looper.prepareMainLooper()
            Loop.init()
            // ActivityThread.systemMain() binds handlers to the calling thread's
            // looper, so the system context must be created here, on main.
            val sysCtx = Binders.systemContext()!!

            val ready = CompletableDeferred<Server>()
            lateinit var server: MobaxServer
            server = MobaxServer(port, Dispatch(ready, onQuit = { server.close() }))
            server.start()
            Loop.scope.launch {
                try {
                    ready.complete(initDaemon(sysCtx))
                    Logx.boot("mobaxd ready (sdk=${android.os.Build.VERSION.SDK_INT})")
                } catch (t: Throwable) {
                    Logx.fatal("init failed", t)
                    exitProcess(2)
                }
            }
            Looper.loop()
        } catch (t: Throwable) {
            Logx.fatal("startup failed", t)
            exitProcess(2)
        }
    }

    private suspend fun initDaemon(sysCtx: Context): Server = withContext(Loop.Work) {
        val ua = try {
            connectedUiAutomation(Looper.getMainLooper()).also { Logx.boot("mobaxd ua-looper=main") }
        } catch (t: Throwable) {
            Logx.bootWarn("UiAutomation on main looper failed; using mobax-ua thread", t)
            val ht = HandlerThread("mobax-ua").apply { start() }
            connectedUiAutomation(ht.looper).also { Logx.boot("mobaxd ua-looper=thread") }
        }
        registerShutdownHook(ua)
        enableInteractiveWindows(ua)
        try { ua.setRunAsMonkey(true) } catch (t: Throwable) { Logx.bootWarn("setRunAsMonkey failed", t) }

        val h = Server.Handlers()
        h.dumper = Dumper(ua)
        h.shot = Screenshot(ua).also { it.probeMirror() }
        h.input = Input(ua)
        h.sysCtx = sysCtx
        h.installer = Installer(sysCtx)
        h.pm = Pm(sysCtx)
        h.manifest = Manifest(sysCtx)
        h.providers = Providers()
        h.location = Location()
        h.notifs = Notifications()
        h.clip = Clipboard()
        h.am = Am(sysCtx)
        try { h.props = Props() } catch (t: Throwable) { Logx.bootWarn("Props init failed", t) }
        h.settings = SettingsApi(sysCtx)
        h.wm = Wm(sysCtx.getSystemService(Context.DISPLAY_SERVICE) as DisplayManager)
        h.uiEvents = UiEvents(ua)
        h.nodes = NodeActions(ua)
        h.state = State(sysCtx, h.uiEvents)

        // Warm the dump path: the first dump_active on a cold VM costs ~1 s.
        Loop.scope.launch(Loop.Work) {
            try {
                val t0 = System.nanoTime()
                h.dumper.dumpActive()
                Logx.i("warmup: dump_active in ${(System.nanoTime() - t0) / 1_000_000}ms")
            } catch (t: Throwable) {
                Logx.w("dump_active warmup failed", t)
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
            Logx.bootWarn("enableInteractiveWindows failed", t)
        }
    }

    private fun registerShutdownHook(ua: UiAutomation) {
        Runtime.getRuntime().addShutdownHook(Thread({
            try {
                UiAutomation::class.java.getDeclaredMethod("disconnect").apply { isAccessible = true }.invoke(ua)
            } catch (_: Throwable) {
            }
        }, "mobax-shutdown"))
    }

    private fun disableHiddenApiRestrictions() {
        try {
            val vmRuntime = Class.forName("dalvik.system.VMRuntime")
            val runtime = vmRuntime.getDeclaredMethod("getRuntime").invoke(null)
            vmRuntime.getDeclaredMethod("setHiddenApiExemptions", Array<String>::class.java)
                .invoke(runtime, arrayOf("L") as Any)
        } catch (t: Throwable) {
            Logx.bootWarn("could not lift hidden-api restrictions", t)
        }
    }
}
