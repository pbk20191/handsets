package dev.handsets.daemon;

import android.app.UiAutomation;
import android.os.SystemClock;
import android.view.InputDevice;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.MotionEvent;

import java.lang.reflect.Field;
import java.util.Locale;

/**
 * Input event injection via UiAutomation.injectInputEvent. The daemon already
 * holds a connected UiAutomation (constructed in Main); using its inject path
 * works from shell UID on non-rooted devices — same privilege that lets
 * AndroidX / Espresso tests drive the UI.
 *
 * All public methods are thread-safe; concurrent CLI + viewer inputs just
 * interleave events.
 */
final class Input {

    private final UiAutomation ua;
    private final KeyCharacterMap virtualKeyMap;

    // Streamed-pointer state. Guarded by `this`.
    private boolean pointerActive = false;
    private long pointerDownTime = 0L;
    private int pointerLastX = 0;
    private int pointerLastY = 0;

    Input(UiAutomation ua) {
        this.ua = ua;
        KeyCharacterMap kcm;
        try {
            kcm = KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD);
        } catch (Throwable t) {
            kcm = null;
        }
        this.virtualKeyMap = kcm;
    }

    // ---------- single gestures: timing lives in Gestures.kt ----------

    // ---------- streamed pointer ----------

    synchronized void pointerDown(int x, int y) {
        if (pointerActive) {
            // A stray second DOWN — emit an UP first to keep the dispatcher happy.
            motion(pointerDownTime, SystemClock.uptimeMillis(),
                    MotionEvent.ACTION_UP, pointerLastX, pointerLastY);
        }
        pointerActive = true;
        pointerDownTime = SystemClock.uptimeMillis();
        pointerLastX = x;
        pointerLastY = y;
        motion(pointerDownTime, pointerDownTime,
                MotionEvent.ACTION_DOWN, x, y);
    }

    synchronized void pointerMove(int x, int y) {
        if (!pointerActive) return;
        pointerLastX = x;
        pointerLastY = y;
        motion(pointerDownTime, SystemClock.uptimeMillis(),
                MotionEvent.ACTION_MOVE, x, y);
    }

    synchronized void pointerUp(int x, int y) {
        if (!pointerActive) return;
        pointerLastX = x;
        pointerLastY = y;
        motion(pointerDownTime, SystemClock.uptimeMillis(),
                MotionEvent.ACTION_UP, x, y);
        pointerActive = false;
    }

    // ---------- keys ----------

    void key(int code) {
        keyPress(code);
    }

    /** Resolve a friendly name (BACK, HOME, etc.) to a KEYCODE_* and press. */
    void keyByName(String name) {
        Integer code = keycodeForName(name);
        if (code == null) {
            throw new IllegalArgumentException("unknown key name: " + name);
        }
        keyPress(code);
    }

    private void keyPress(int code) {
        long t = SystemClock.uptimeMillis();
        keyEvent(t, t, KeyEvent.ACTION_DOWN, code);
        keyEvent(t, t + 1, KeyEvent.ACTION_UP, code);
    }

    void text(String s) {
        if (virtualKeyMap == null) {
            throw new IllegalStateException("no virtual key map");
        }
        if (s == null || s.isEmpty()) return;
        KeyEvent[] events = virtualKeyMap.getEvents(s.toCharArray());
        if (events == null) {
            throw new IllegalArgumentException("char(s) unsupported by virtual keymap");
        }
        long base = SystemClock.uptimeMillis();
        for (int i = 0; i < events.length; i++) {
            KeyEvent src = events[i];
            // KeyEvent objects returned from KeyCharacterMap are immutable in
            // terms of source; rebuild with our timestamps + source set.
            KeyEvent ke = new KeyEvent(
                    base,                       // downTime
                    base + i,                   // eventTime
                    src.getAction(),
                    src.getKeyCode(),
                    0,                          // repeat
                    src.getMetaState(),
                    KeyCharacterMap.VIRTUAL_KEYBOARD,
                    src.getScanCode(),
                    src.getFlags() | KeyEvent.FLAG_FROM_SYSTEM,
                    InputDevice.SOURCE_KEYBOARD);
            ua.injectInputEvent(ke, true);
        }
    }

    // ---------- low-level helpers ----------

    void motion(long downTime, long eventTime, int action, int x, int y) {
        // Build a full-fidelity MotionEvent so gesture detectors that check
        // toolType + pressure (launcher swipes, ViewPager flings, AndroidX
        // GestureDetector) treat it as a real finger. The 6-arg
        // `MotionEvent.obtain` shortcut leaves toolType as TOOL_TYPE_UNKNOWN,
        // which the gesture stack quietly rejects.
        MotionEvent.PointerProperties pp = new MotionEvent.PointerProperties();
        pp.id = 0;
        pp.toolType = MotionEvent.TOOL_TYPE_FINGER;
        MotionEvent.PointerCoords pc = new MotionEvent.PointerCoords();
        pc.x = x; pc.y = y;
        pc.pressure = 1.0f;
        pc.size = 1.0f;
        MotionEvent ev = MotionEvent.obtain(
                downTime, eventTime, action,
                1,
                new MotionEvent.PointerProperties[]{pp},
                new MotionEvent.PointerCoords[]{pc},
                /* metaState */    0,
                /* buttonState */  0,
                /* xPrecision */   1.0f,
                /* yPrecision */   1.0f,
                /* deviceId */     0,
                /* edgeFlags */    0,
                /* source */       InputDevice.SOURCE_TOUCHSCREEN,
                /* flags */        0);
        try {
            ua.injectInputEvent(ev, true);
        } finally {
            ev.recycle();
        }
    }

    private void keyEvent(long downTime, long eventTime, int action, int code) {
        KeyEvent ev = new KeyEvent(
                downTime, eventTime, action, code, 0, 0,
                KeyCharacterMap.VIRTUAL_KEYBOARD, 0,
                KeyEvent.FLAG_FROM_SYSTEM, InputDevice.SOURCE_KEYBOARD);
        ua.injectInputEvent(ev, true);
    }

    private static Integer keycodeForName(String raw) {
        if (raw == null) return null;
        String name = raw.trim().toUpperCase(Locale.ROOT);
        // Common aliases.
        switch (name) {
            case "BACK":        return KeyEvent.KEYCODE_BACK;
            case "HOME":        return KeyEvent.KEYCODE_HOME;
            case "RECENTS":
            case "APP_SWITCH":
            case "OVERVIEW":    return KeyEvent.KEYCODE_APP_SWITCH;
            case "MENU":        return KeyEvent.KEYCODE_MENU;
            case "POWER":       return KeyEvent.KEYCODE_POWER;
            case "VOLUME_UP":
            case "VOLUP":       return KeyEvent.KEYCODE_VOLUME_UP;
            case "VOLUME_DOWN":
            case "VOLDOWN":     return KeyEvent.KEYCODE_VOLUME_DOWN;
            case "ENTER":
            case "RETURN":      return KeyEvent.KEYCODE_ENTER;
            case "DEL":
            case "BACKSPACE":   return KeyEvent.KEYCODE_DEL;
            case "TAB":         return KeyEvent.KEYCODE_TAB;
            case "ESCAPE":
            case "ESC":         return KeyEvent.KEYCODE_ESCAPE;
            case "SPACE":       return KeyEvent.KEYCODE_SPACE;
            case "DPAD_UP":     return KeyEvent.KEYCODE_DPAD_UP;
            case "DPAD_DOWN":   return KeyEvent.KEYCODE_DPAD_DOWN;
            case "DPAD_LEFT":   return KeyEvent.KEYCODE_DPAD_LEFT;
            case "DPAD_RIGHT":  return KeyEvent.KEYCODE_DPAD_RIGHT;
        }
        // Try KEYCODE_<NAME> via reflection — covers anything we didn't enumerate.
        try {
            Field f = KeyEvent.class.getField("KEYCODE_" + name);
            return f.getInt(null);
        } catch (Throwable t) {
            return null;
        }
    }
}
