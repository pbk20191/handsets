package dev.handsets.daemon

import android.app.UiAutomation
import android.os.SystemClock
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.MotionEvent
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import android.view.MotionEvent.PointerCoords
import android.view.MotionEvent.PointerProperties

/**
 * Input event injection via UiAutomation.injectInputEvent. The daemon already
 * holds a connected UiAutomation (constructed in Main); using its inject path
 * works from shell UID on non-rooted devices — same privilege that lets
 * AndroidX / Espresso tests drive the UI.
 * 
 * All public methods are thread-safe; concurrent CLI + viewer inputs just
 * interleave events.
 */
internal class Input(private val ua: UiAutomation) {
    private val virtualKeyMap: KeyCharacterMap?

    // Streamed-pointer state. Guarded by `this`.
    private var pointerActive = false
    private var pointerDownTime = 0L

    /** Streamed pointer state (down/move/up from any connection), one event at a time. */
    private val pointer = Mutex()
    private var pointerLastX = 0
    private var pointerLastY = 0

    init {
        var kcm: KeyCharacterMap?
        try {
            kcm = KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD)
        } catch (t: Throwable) {
            kcm = null
        }
        this.virtualKeyMap = kcm
    }

    // ---------- single gestures: timing lives in Gestures.kt ----------
    // ---------- streamed pointer ----------
    suspend fun pointerDown(x: Int, y: Int) = pointer.withLock { withContext(Loop.Work) { pointerDownLocked(x, y) } }

    private fun pointerDownLocked(x: Int, y: Int) {
        if (pointerActive) {
            // A stray second DOWN — emit an UP first to keep the dispatcher happy.
            motion(
                pointerDownTime, SystemClock.uptimeMillis(),
                MotionEvent.ACTION_UP, pointerLastX, pointerLastY
            )
        }
        pointerActive = true
        pointerDownTime = SystemClock.uptimeMillis()
        pointerLastX = x
        pointerLastY = y
        motion(
            pointerDownTime, pointerDownTime,
            MotionEvent.ACTION_DOWN, x, y
        )
    }

    suspend fun pointerMove(x: Int, y: Int) = pointer.withLock { withContext(Loop.Work) { pointerMoveLocked(x, y) } }

    private fun pointerMoveLocked(x: Int, y: Int) {
        if (!pointerActive) return
        pointerLastX = x
        pointerLastY = y
        motion(
            pointerDownTime, SystemClock.uptimeMillis(),
            MotionEvent.ACTION_MOVE, x, y
        )
    }

    suspend fun pointerUp(x: Int, y: Int) = pointer.withLock { withContext(Loop.Work) { pointerUpLocked(x, y) } }

    private fun pointerUpLocked(x: Int, y: Int) {
        if (!pointerActive) return
        pointerLastX = x
        pointerLastY = y
        motion(
            pointerDownTime, SystemClock.uptimeMillis(),
            MotionEvent.ACTION_UP, x, y
        )
        pointerActive = false
    }

    // ---------- keys ----------
    fun key(code: Int) {
        keyPress(code)
    }

    /** Resolve a friendly name (BACK, HOME, etc.) to a KEYCODE_* and press.  */
    fun keyByName(name: String?) {
        val code = requireNotNull(keycodeForName(name)) { "unknown key name: " + name }
        keyPress(code)
    }

    private fun keyPress(code: Int) {
        val t = SystemClock.uptimeMillis()
        keyEvent(t, t, KeyEvent.ACTION_DOWN, code)
        keyEvent(t, t + 1, KeyEvent.ACTION_UP, code)
    }

    fun text(s: String?) {
        checkNotNull(virtualKeyMap) { "no virtual key map" }
        if (s == null || s.isEmpty()) return
        val events = virtualKeyMap.getEvents(s.toCharArray())
        requireNotNull(events) { "char(s) unsupported by virtual keymap" }
        val base = SystemClock.uptimeMillis()
        for (i in events.indices) {
            val src = events[i]
            // KeyEvent objects returned from KeyCharacterMap are immutable in
            // terms of source; rebuild with our timestamps + source set.
            val ke = KeyEvent(
                base,  // downTime
                base + i,  // eventTime
                src.getAction(),
                src.getKeyCode(),
                0,  // repeat
                src.getMetaState(),
                KeyCharacterMap.VIRTUAL_KEYBOARD,
                src.getScanCode(),
                src.getFlags() or KeyEvent.FLAG_FROM_SYSTEM,
                InputDevice.SOURCE_KEYBOARD
            )
            ua.injectInputEvent(ke, true)
        }
    }

    // ---------- low-level helpers ----------
    fun motion(downTime: Long, eventTime: Long, action: Int, x: Int, y: Int) {
        // Build a full-fidelity MotionEvent so gesture detectors that check
        // toolType + pressure (launcher swipes, ViewPager flings, AndroidX
        // GestureDetector) treat it as a real finger. The 6-arg
        // `MotionEvent.obtain` shortcut leaves toolType as TOOL_TYPE_UNKNOWN,
        // which the gesture stack quietly rejects.
        val pp = PointerProperties()
        pp.id = 0
        pp.toolType = MotionEvent.TOOL_TYPE_FINGER
        val pc = PointerCoords()
        pc.x = x.toFloat()
        pc.y = y.toFloat()
        pc.pressure = 1.0f
        pc.size = 1.0f
        val ev = MotionEvent.obtain(
            downTime, eventTime, action,
            1,
            arrayOf<PointerProperties>(pp),
            arrayOf<PointerCoords>(pc),  /* metaState */
            0,  /* buttonState */
            0,  /* xPrecision */
            1.0f,  /* yPrecision */
            1.0f,  /* deviceId */
            0,  /* edgeFlags */
            0,  /* source */
            InputDevice.SOURCE_TOUCHSCREEN,  /* flags */
            0
        )
        try {
            ua.injectInputEvent(ev, true)
        } finally {
            ev.recycle()
        }
    }

    private fun keyEvent(downTime: Long, eventTime: Long, action: Int, code: Int) {
        val ev = KeyEvent(
            downTime, eventTime, action, code, 0, 0,
            KeyCharacterMap.VIRTUAL_KEYBOARD, 0,
            KeyEvent.FLAG_FROM_SYSTEM, InputDevice.SOURCE_KEYBOARD
        )
        ua.injectInputEvent(ev, true)
    }

    companion object {
        private fun keycodeForName(raw: String?): Int? {
            if (raw == null) return null
            val name = raw.trim { it <= ' ' }.uppercase()
            // Common aliases.
            when (name) {
                "BACK" -> return KeyEvent.KEYCODE_BACK
                "HOME" -> return KeyEvent.KEYCODE_HOME
                "RECENTS", "APP_SWITCH", "OVERVIEW" -> return KeyEvent.KEYCODE_APP_SWITCH
                "MENU" -> return KeyEvent.KEYCODE_MENU
                "POWER" -> return KeyEvent.KEYCODE_POWER
                "VOLUME_UP", "VOLUP" -> return KeyEvent.KEYCODE_VOLUME_UP
                "VOLUME_DOWN", "VOLDOWN" -> return KeyEvent.KEYCODE_VOLUME_DOWN
                "ENTER", "RETURN" -> return KeyEvent.KEYCODE_ENTER
                "DEL", "BACKSPACE" -> return KeyEvent.KEYCODE_DEL
                "TAB" -> return KeyEvent.KEYCODE_TAB
                "ESCAPE", "ESC" -> return KeyEvent.KEYCODE_ESCAPE
                "SPACE" -> return KeyEvent.KEYCODE_SPACE
                "DPAD_UP" -> return KeyEvent.KEYCODE_DPAD_UP
                "DPAD_DOWN" -> return KeyEvent.KEYCODE_DPAD_DOWN
                "DPAD_LEFT" -> return KeyEvent.KEYCODE_DPAD_LEFT
                "DPAD_RIGHT" -> return KeyEvent.KEYCODE_DPAD_RIGHT
            }
            // Try KEYCODE_<NAME> via reflection — covers anything we didn't enumerate.
            try {
                val f = KeyEvent::class.java.getField("KEYCODE_" + name)
                return f.getInt(null)
            } catch (t: Throwable) {
                return null
            }
        }
    }
}
