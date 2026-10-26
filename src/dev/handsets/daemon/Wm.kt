package dev.handsets.daemon

import android.graphics.Point
import android.hardware.display.DisplayManager
import android.util.DisplayMetrics
import android.view.Display
import dev.handsets.daemon.Binders.asInterface
import dev.handsets.daemon.Binders.service
import java.lang.reflect.InvocationTargetException
import java.lang.reflect.Method
import java.nio.charset.StandardCharsets

/**
 * window-manager helpers — direct binder against IWindowManager. Replaces the
 * `adb shell wm size|density|user-rotation` pattern which each spawns
 * a fresh `wm` app_process.
 */
internal class Wm(private val dm: DisplayManager) {
    // Display.getRealSize/getRealMetrics: WindowMetrics needs API 30 and a window context.
    @Suppress("DEPRECATION")
    fun info(): ByteArray {
        try {
            val d = dm.getDisplay(Display.DEFAULT_DISPLAY)
            val size = Point()
            d.getRealSize(size)
            val dmx = DisplayMetrics()
            d.getRealMetrics(dmx)
            val rot = d.rotation
            val json = ("{\"width\":" + size.x
                    + ",\"height\":" + size.y
                    + ",\"density\":" + dmx.densityDpi
                    + ",\"xdpi\":" + dmx.xdpi
                    + ",\"ydpi\":" + dmx.ydpi
                    + ",\"rotation\":" + rot
                    + "}")
            return json.toByteArray(StandardCharsets.UTF_8)
        } catch (t: Throwable) {
            return err("wm-info-failed:" + t.message)
        }
    }

    fun setRotation(rotation: Int): ByteArray {
        try {
            val iwm = asInterface(
                $$"android.view.IWindowManager$Stub", service("window")
            )
            // freezeRotation(int) is the long-stable shape; newer surfaces add
            // a String caller. Try both.
            var best: Method? = null
            for (m in iwm!!.javaClass.getMethods()) {
                if ("freezeRotation" != m.name) continue
                val p = m.getParameterTypes()
                if (p.size >= 1 && p[0] == Int::class.javaPrimitiveType && (best == null || p.size < best.getParameterTypes().size)) {
                    best = m
                }
            }
            if (best == null) return err("freezeRotation-missing")
            val p = best.getParameterTypes()
            val args = arrayOfNulls<Any>(p.size)
            args[0] = rotation
            for (i in 1..<p.size) {
                if (p[i] == String::class.java) args[i] = "com.android.shell"
                else if (p[i] == Int::class.javaPrimitiveType) args[i] = 0
                else if (p[i] == Boolean::class.javaPrimitiveType) args[i] = false
                else args[i] = null
            }
            best.invoke(iwm, *args)
            return ok("ok rotation=$rotation")
        } catch (ite: InvocationTargetException) {
            val c: Throwable = (ite.cause ?: ite)
            return err("rotation-failed:" + c.javaClass.getSimpleName() + ":" + c.message)
        } catch (t: Throwable) {
            return err("rotation-threw:" + t.javaClass.getSimpleName() + ":" + t.message)
        }
    }

    companion object {
        private fun ok(s: String): ByteArray {
            return s.toByteArray(StandardCharsets.UTF_8)
        }

        private fun err(tail: String?): ByteArray {
            return ("ERR:$tail").toByteArray(StandardCharsets.UTF_8)
        }
    }
}
