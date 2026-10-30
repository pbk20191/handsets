package dev.handsets.probe;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.Context;
import android.content.Intent;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Build;
import android.os.Bundle;
import android.os.Looper;
import android.os.MessageQueue;
import android.os.StrictMode;
import android.system.ErrnoException;
import android.system.Os;
import android.system.OsConstants;
import android.util.Log;
import android.view.View;
import android.view.ViewGroup;

import java.io.ByteArrayOutputStream;
import java.io.FileDescriptor;
import java.io.IOException;
import java.io.InterruptedIOException;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

import static android.os.MessageQueue.OnFileDescriptorEventListener.EVENT_ERROR;
import static android.os.MessageQueue.OnFileDescriptorEventListener.EVENT_INPUT;
import static android.os.MessageQueue.OnFileDescriptorEventListener.EVENT_OUTPUT;

/**
 * Started with {@code am instrument -w dev.handsets.probe/.ProbeInstrumentation}.
 * Android runs it in the target app's process (that is what the shared
 * signing key buys), so it can read the target's Compose semantics tree
 * directly: the unmerged tree, test tags, roles and actions that the
 * accessibility tree flattens or drops.
 *
 * Commands, one per line on {@code localabstract:hs-probe}:
 * <pre>
 *   tree               the semantics tree of the resumed activity, as JSON
 *   click TAG          run the node's OnClick action
 *   settext TAG TEXT   run the node's SetText action (any script, any state)
 *   quit               end the instrumentation (the target app goes with it)
 * </pre>
 *
 * All socket IO is event-driven on the main looper, where every command has
 * to run anyway (Compose state is main-thread only): no thread per client,
 * no hop per command, and any number of clients at once.
 *
 * For an R8-minified target, pass the obfuscated names from its mapping.txt
 * ({@code tools/probe_names.py}) as {@code -e names "key=kind:name;…"}.
 */
public final class ProbeInstrumentation extends Instrumentation {
    private static final String TAG = "hs-probe";
    private static final String SOCKET = "hs-probe";
    /**
     * Bytes written per EVENT_OUTPUT where a client fd stays blocking (API
     * 28–29: fcntlInt is public from 30). A unix socket only reports
     * writable with about half its buffer free, so a write this size never
     * waits.
     */
    private static final int WRITE_CHUNK = 4096;

    /** Logical member → "m:name" | "m3:name" | "f:name" | "c:class", from the target's mapping. */
    private static final Map<String, String> NAMES = new HashMap<>();

    private volatile Activity resumed;
    // Main-thread state below.
    private LocalServerSocket server;
    private final Set<Client> clients = new HashSet<>();
    private boolean quitting;

    @Override
    public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        String names = arguments == null ? null : arguments.getString("names");
        if (names != null) {
            for (String pair : names.split(";")) {
                int eq = pair.indexOf('=');
                if (eq > 0) NAMES.put(pair.substring(0, eq).trim(), pair.substring(eq + 1).trim());
            }
        }
        start();
    }

    @Override
    public void callActivityOnResume(Activity activity) {
        super.callActivityOnResume(activity);
        resumed = activity;
    }

    /**
     * On the instrumentation thread: launch the target, open the socket and
     * hand it to the main looper. The instrumentation lives on until finish().
     */
    @Override
    public void onStart() {
        Context target = getTargetContext();
        Intent launch = target.getPackageManager().getLaunchIntentForPackage(target.getPackageName());
        if (launch == null) {
            end("no launcher activity in " + target.getPackageName());
            return;
        }
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK);
        resumed = startActivitySync(launch);
        try {
            server = new LocalServerSocket(SOCKET);
        } catch (IOException e) {
            Log.e(TAG, "cannot listen", e);
            end("cannot listen on localabstract:" + SOCKET + ": " + e);
            return;
        }
        queue().addOnFileDescriptorEventListener(server.getFileDescriptor(), EVENT_INPUT | EVENT_ERROR,
            (fd, events) -> onAccept(events));
        Bundle ready = new Bundle();
        ready.putString("probe", "ready on localabstract:" + SOCKET + " (" + NAMES.size() + " mapped names)");
        sendStatus(1, ready);
        Log.i(TAG, "ready on localabstract:" + SOCKET);
    }

    private static MessageQueue queue() {
        return Looper.getMainLooper().getQueue();
    }

    private void end(String why) {
        Bundle b = new Bundle();
        b.putString("probe", why);
        finish(Activity.RESULT_OK, b);
    }

    /** The listening socket is readable: one connection waits, so accept() returns at once. */
    private int onAccept(int events) {
        if ((events & EVENT_ERROR) != 0) {
            shutdown("listening socket failed");
            return 0;
        }
        StrictMode.ThreadPolicy policy = permitSocketIo();
        try {
            Client c = new Client(server.accept());
            clients.add(c);
            queue().addOnFileDescriptorEventListener(c.fd, EVENT_INPUT | EVENT_ERROR, c);
        } catch (IOException e) {
            Log.w(TAG, "accept failed", e);
        } finally {
            StrictMode.setThreadPolicy(policy);
        }
        return EVENT_INPUT | EVENT_ERROR;
    }

    /**
     * The target app's thread policy kills it for socket calls on the main
     * thread. These calls only run once the looper reports the fd ready, so
     * they don't wait; lift the network check for the call and hand back the
     * app's own policy to restore.
     */
    private static StrictMode.ThreadPolicy permitSocketIo() {
        StrictMode.ThreadPolicy app = StrictMode.getThreadPolicy();
        StrictMode.setThreadPolicy(new StrictMode.ThreadPolicy.Builder(app).permitNetwork().build());
        return app;
    }

    /** After `quit`: once every reply is out, close everything and end. */
    private void shutdown(String why) {
        for (Client c : new ArrayList<>(clients)) c.close();
        if (server != null) {
            queue().removeOnFileDescriptorEventListener(server.getFileDescriptor());
            try { server.close(); } catch (IOException ignored) { }
            server = null;
        }
        end(why);
    }

    /** One connection: lines in, replies queued and written as the socket drains. */
    private final class Client implements MessageQueue.OnFileDescriptorEventListener {
        final LocalSocket socket;
        final FileDescriptor fd;
        private final ByteArrayOutputStream line = new ByteArrayOutputStream();
        private final byte[] readBuf = new byte[8192];
        private byte[] out = new byte[0];
        private int outPos;
        /** The peer closed its end; finish sending, then close. */
        private boolean draining;

        /** O_NONBLOCK set (API 30+, where fcntlInt is public); else the chunked fallback. */
        private final boolean nonBlocking;

        Client(LocalSocket socket) {
            this.socket = socket;
            this.fd = socket.getFileDescriptor();
            boolean nb = false;
            if (Build.VERSION.SDK_INT >= 30) {
                try {
                    int fl = Os.fcntlInt(fd, OsConstants.F_GETFL, 0);
                    Os.fcntlInt(fd, OsConstants.F_SETFL, fl | OsConstants.O_NONBLOCK);
                    nb = true;
                } catch (ErrnoException e) {
                    Log.w(TAG, "O_NONBLOCK failed; chunked writes", e);
                }
            }
            nonBlocking = nb;
        }

        @Override
        public int onFileDescriptorEvents(FileDescriptor fd, int events) {
            StrictMode.ThreadPolicy policy = permitSocketIo();
            try {
                if ((events & EVENT_ERROR) != 0) {
                    close();
                    return 0;
                }
                if ((events & EVENT_INPUT) != 0) readAll();
                if ((events & EVENT_OUTPUT) != 0) writeSome();
            } catch (ErrnoException | IOException e) {
                close();
                return 0;
            } finally {
                StrictMode.setThreadPolicy(policy);
            }
            boolean pending = outPos < out.length;
            if (!pending && quitting) {
                shutdown("quit");
                return 0;
            }
            if (!pending && draining) {
                close();
                return 0;
            }
            return (draining ? 0 : EVENT_INPUT) | EVENT_ERROR | (pending ? EVENT_OUTPUT : 0);
        }

        /** Reads what has arrived (non-blocking: until EAGAIN; else one read) and runs each full line. */
        private void readAll() throws ErrnoException, InterruptedIOException {
            while (true) {
                int n;
                try {
                    n = Os.read(fd, readBuf, 0, readBuf.length);
                } catch (ErrnoException e) {
                    if (e.errno == OsConstants.EAGAIN) return;
                    throw e;
                }
                if (n <= 0) {
                    draining = true;
                    return;
                }
                for (int i = 0; i < n; i++) {
                    if (readBuf[i] != '\n') {
                        line.write(readBuf[i]);
                        continue;
                    }
                    String cmd = new String(line.toByteArray(), StandardCharsets.UTF_8).trim();
                    line.reset();
                    if (!cmd.isEmpty()) enqueue((reply(cmd) + "\n").getBytes(StandardCharsets.UTF_8));
                }
                if (!nonBlocking) return;  // a second read could wait
            }
        }

        /** Writes as much as the socket takes (non-blocking), or one chunk that cannot wait. */
        private void writeSome() throws ErrnoException, InterruptedIOException {
            while (outPos < out.length) {
                int len = nonBlocking ? out.length - outPos : Math.min(WRITE_CHUNK, out.length - outPos);
                try {
                    outPos += Os.write(fd, out, outPos, len);
                } catch (ErrnoException e) {
                    if (e.errno == OsConstants.EAGAIN) return;
                    throw e;
                }
                if (!nonBlocking) return;
            }
        }

        private void enqueue(byte[] bytes) {
            byte[] rest = new byte[out.length - outPos + bytes.length];
            System.arraycopy(out, outPos, rest, 0, out.length - outPos);
            System.arraycopy(bytes, 0, rest, out.length - outPos, bytes.length);
            out = rest;
            outPos = 0;
        }

        void close() {
            if (!clients.remove(this)) return;
            queue().removeOnFileDescriptorEventListener(fd);
            try { socket.close(); } catch (IOException ignored) { }
        }
    }

    /** Runs one command, on the main thread. */
    private String reply(String line) {
        try {
            return handle(line);
        } catch (Throwable t) {
            Throwable cause = t.getCause() != null ? t.getCause() : t;
            return "ERR:" + cause;
        }
    }

    private String handle(String line) throws Exception {
        String[] parts = line.split(" ", 3);
        switch (parts[0]) {
            case "tree":
                return dumpTree();
            case "click":
                if (parts.length < 2) return "ERR:click needs TAG";
                return perform(parts[1], "OnClick", null);
            case "settext":
                if (parts.length < 3) return "ERR:settext needs TAG TEXT";
                return perform(parts[1], "SetText", parts[2]);
            case "quit":
                quitting = true;
                return "ok";
            default:
                return "ERR:unknown command " + parts[0];
        }
    }

    // ---- reading the tree ----

    /** Every Compose root view (AndroidComposeView) under the resumed activity. */
    private List<View> composeRoots() {
        List<View> roots = new ArrayList<>();
        Activity a = resumed;
        if (a != null) collect(a.getWindow().getDecorView(), roots);
        return roots;
    }

    private static void collect(View v, List<View> out) {
        // Framework views never own semantics; the app's own ones may.
        if (!v.getClass().getName().startsWith("android.") && has(v, "view.semanticsOwner", "getSemanticsOwner")) {
            out.add(v);
            return;
        }
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) collect(g.getChildAt(i), out);
        }
    }

    private static Object rootNode(View composeView) throws Exception {
        Object owner = get(composeView, "view.semanticsOwner", "getSemanticsOwner");
        return get(owner, "owner.unmergedRoot", "getUnmergedRootSemanticsNode");
    }

    private String dumpTree() throws Exception {
        Activity a = resumed;
        StringBuilder sb = new StringBuilder("{\"activity\":");
        str(sb, a == null ? "" : a.getComponentName().flattenToShortString());
        sb.append(",\"roots\":[");
        List<View> roots = composeRoots();
        int[] loc = new int[2];
        for (int i = 0; i < roots.size(); i++) {
            if (i > 0) sb.append(',');
            roots.get(i).getLocationOnScreen(loc);
            node(sb, rootNode(roots.get(i)), loc[0], loc[1]);
        }
        return sb.append("]}").toString();
    }

    @SuppressWarnings("unchecked")
    private static void node(StringBuilder sb, Object n, int dx, int dy) throws Exception {
        sb.append("{\"id\":").append(get(n, "node.id", "getId"));
        Object r = get(n, "node.boundsInRoot", "getBoundsInRoot");
        sb.append(",\"bounds\":[")
            .append(Math.round((float) get(r, "rect.left", "getLeft")) + dx).append(',')
            .append(Math.round((float) get(r, "rect.top", "getTop")) + dy).append(',')
            .append(Math.round((float) get(r, "rect.right", "getRight")) + dx).append(',')
            .append(Math.round((float) get(r, "rect.bottom", "getBottom")) + dy).append(']');
        String actionClass = mapped("class.AccessibilityAction", "androidx.compose.ui.semantics.AccessibilityAction");
        List<String> actions = new ArrayList<>();
        for (Object o : (Iterable<Object>) get(n, "node.config", "getConfig")) {
            Map.Entry<Object, Object> e = (Map.Entry<Object, Object>) o;
            String key = String.valueOf(get(e.getKey(), "key.name", "getName"));
            Object value = e.getValue();
            if (value != null && value.getClass().getName().equals(actionClass)) {
                actions.add(key);
                continue;
            }
            sb.append(',');
            str(sb, key);
            sb.append(':');
            str(sb, show(value));
        }
        if (!actions.isEmpty()) {
            sb.append(",\"actions\":[");
            for (int i = 0; i < actions.size(); i++) {
                if (i > 0) sb.append(',');
                str(sb, actions.get(i));
            }
            sb.append(']');
        }
        List<Object> kids = (List<Object>) get(n, "node.children", "getChildren");
        if (!kids.isEmpty()) {
            sb.append(",\"children\":[");
            for (int i = 0; i < kids.size(); i++) {
                if (i > 0) sb.append(',');
                node(sb, kids.get(i), dx, dy);
            }
            sb.append(']');
        }
        sb.append('}');
    }

    /** A semantics value as text: lists joined, Unit (a flag) as "true". */
    private static String show(Object v) {
        if (v == null) return "";
        if (v instanceof List) {
            StringBuilder b = new StringBuilder();
            for (Object o : (List<?>) v) {
                if (b.length() > 0) b.append(" | ");
                b.append(o);
            }
            return b.toString();
        }
        if (mapped("class.Unit", "kotlin.Unit").equals(v.getClass().getName())) return "true";
        String s = String.valueOf(v);
        return s.length() > 200 ? s.substring(0, 200) + "…" : s;
    }

    // ---- acting on a node ----

    private String perform(String tag, String actionName, String text) throws Exception {
        for (View root : composeRoots()) {
            Object n = findByTag(rootNode(root), tag);
            if (n == null) continue;
            Object action = configValue(n, actionName);
            if (action == null) return "ERR:" + tag + " has no " + actionName + " action";
            Object fn = get(action, "action.action", "getAction");
            Object ok;
            if (text == null) {
                ok = fn.getClass().getMethod(mapped("fn0.invoke", "invoke")).invoke(fn);
            } else {
                Object annotated = annotatedString(n.getClass().getClassLoader(), text);
                ok = fn.getClass().getMethod(mapped("fn1.invoke", "invoke"), Object.class).invoke(fn, annotated);
            }
            return Boolean.TRUE.equals(ok) ? "ok" : "ERR:" + actionName + " returned " + ok;
        }
        return "ERR:no node tagged " + tag;
    }

    @SuppressWarnings("unchecked")
    private static Object findByTag(Object n, String tag) throws Exception {
        if (tag.equals(configValue(n, "TestTag"))) return n;
        for (Object kid : (List<Object>) get(n, "node.children", "getChildren")) {
            Object hit = findByTag(kid, tag);
            if (hit != null) return hit;
        }
        return null;
    }

    @SuppressWarnings("unchecked")
    private static Object configValue(Object n, String keyName) throws Exception {
        for (Object o : (Iterable<Object>) get(n, "node.config", "getConfig")) {
            Map.Entry<Object, Object> e = (Map.Entry<Object, Object>) o;
            if (keyName.equals(get(e.getKey(), "key.name", "getName"))) return e.getValue();
        }
        return null;
    }

    /** AnnotatedString(text) from the target's own Compose, whatever its constructor shape. */
    private static Object annotatedString(ClassLoader cl, String text) throws Exception {
        Class<?> c = Class.forName(mapped("class.AnnotatedString", "androidx.compose.ui.text.AnnotatedString"), true, cl);
        for (Constructor<?> k : c.getDeclaredConstructors()) {
            Class<?>[] p = k.getParameterTypes();
            if (p.length == 0 || p[0] != String.class) continue;
            Object[] args = new Object[p.length];
            args[0] = text;
            boolean fits = true;
            for (int i = 1; i < p.length; i++) {
                if (p[i] == List.class) args[i] = Collections.emptyList();
                else { fits = false; break; }
            }
            if (fits) {
                k.setAccessible(true);
                return k.newInstance(args);
            }
        }
        throw new IllegalStateException("no AnnotatedString(String, List…) constructor");
    }

    // ---- reflection and JSON helpers ----

    /** A Compose member by its logical key: the mapped name, else the original getter. */
    private static Object get(Object o, String key, String getter) throws Exception {
        String spec = NAMES.get(key);
        if (spec == null) return call(o, getter);
        String name = spec.substring(spec.indexOf(':') + 1);
        if (spec.startsWith("f:")) {
            Field f = field(o, name);
            if (f == null) throw new NoSuchFieldException(o.getClass().getName() + "." + name + " (" + key + ")");
            return f.get(o);
        }
        if (spec.startsWith("m3:")) {
            // getChildren$ui_release(includeReplacedSubtree, includeFakeNodes,
            // includeDeactivatedNodes) as (true, false, false). R8 drops
            // parameters that are constant at every call site; the first one
            // varies, so whatever is left is (true, false…).
            for (Class<?> c = o.getClass(); c != null; c = c.getSuperclass()) {
                for (Method m : c.getDeclaredMethods()) {
                    Class<?>[] p = m.getParameterTypes();
                    if (!m.getName().equals(name) || p.length == 0 || !List.class.isAssignableFrom(m.getReturnType())) continue;
                    Object[] args = new Object[p.length];
                    boolean bools = true;
                    for (int i = 0; i < p.length; i++) {
                        if (p[i] != boolean.class) { bools = false; break; }
                        args[i] = i == 0;
                    }
                    if (!bools) continue;
                    m.setAccessible(true);
                    return m.invoke(o, args);
                }
            }
            throw new NoSuchMethodException(o.getClass().getName() + "." + name + "(Z…) (" + key + ")");
        }
        return call(o, name);
    }

    private static boolean has(Object o, String key, String getter) {
        String spec = NAMES.get(key);
        if (spec == null) return method(o, getter) != null;
        String name = spec.substring(spec.indexOf(':') + 1);
        return spec.startsWith("f:") ? field(o, name) != null : method(o, name) != null;
    }

    /** The mapped name for a key (class or member), else the original. */
    private static String mapped(String key, String original) {
        String spec = NAMES.get(key);
        return spec == null ? original : spec.substring(spec.indexOf(':') + 1);
    }

    private static Method method(Object o, String name) {
        for (Class<?> c = o.getClass(); c != null; c = c.getSuperclass()) {
            try {
                Method m = c.getDeclaredMethod(name);
                m.setAccessible(true);
                return m;
            } catch (NoSuchMethodException ignored) {
            }
        }
        return null;
    }

    private static Field field(Object o, String name) {
        for (Class<?> c = o.getClass(); c != null; c = c.getSuperclass()) {
            try {
                Field f = c.getDeclaredField(name);
                f.setAccessible(true);
                return f;
            } catch (NoSuchFieldException ignored) {
            }
        }
        return null;
    }

    private static Object call(Object o, String name) throws Exception {
        Method m = method(o, name);
        if (m == null) throw new NoSuchMethodException(o.getClass().getName() + "." + name);
        return m.invoke(o);
    }

    private static void str(StringBuilder sb, String s) {
        sb.append('"');
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"': sb.append("\\\""); break;
                case '\\': sb.append("\\\\"); break;
                case '\n': sb.append("\\n"); break;
                case '\r': sb.append("\\r"); break;
                case '\t': sb.append("\\t"); break;
                default:
                    if (c < 0x20) sb.append(String.format("\\u%04x", (int) c));
                    else sb.append(c);
            }
        }
        sb.append('"');
    }
}
