package dev.handsets.probe;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.Context;
import android.content.Intent;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.view.ViewGroup;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

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
 * For an R8-minified target, pass the obfuscated names from its mapping.txt
 * ({@code tools/probe_names.py}) as {@code -e names "key=kind:name;…"}.
 */
public final class ProbeInstrumentation extends Instrumentation {
    private static final String TAG = "hs-probe";
    private static final String SOCKET = "hs-probe";

    /** Logical member → "m:name" | "m3:name" | "f:name" | "c:class", from the target's mapping. */
    private static final Map<String, String> NAMES = new HashMap<>();

    private volatile Activity resumed;
    private volatile boolean running = true;

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
        try (LocalServerSocket server = new LocalServerSocket(SOCKET)) {
            Bundle ready = new Bundle();
            ready.putString("probe", "ready on localabstract:" + SOCKET + " (" + NAMES.size() + " mapped names)");
            sendStatus(1, ready);
            Log.i(TAG, "ready on localabstract:" + SOCKET);
            while (running) {
                try (LocalSocket s = server.accept()) {
                    serve(s);
                }
            }
        } catch (Exception e) {
            Log.e(TAG, "server failed", e);
            end("server failed: " + e);
            return;
        }
        end("quit");
    }

    private void end(String why) {
        Bundle b = new Bundle();
        b.putString("probe", why);
        finish(Activity.RESULT_OK, b);
    }

    private void serve(LocalSocket s) throws Exception {
        BufferedReader in = new BufferedReader(new InputStreamReader(s.getInputStream(), StandardCharsets.UTF_8));
        OutputStream out = s.getOutputStream();
        String line;
        while ((line = in.readLine()) != null) {
            String reply;
            try {
                reply = handle(line.trim());
            } catch (Throwable t) {
                Throwable cause = t.getCause() != null ? t.getCause() : t;
                reply = "ERR:" + cause;
            }
            out.write((reply + "\n").getBytes(StandardCharsets.UTF_8));
            out.flush();
            if (!running) return;
        }
    }

    private String handle(String line) throws Exception {
        String[] parts = line.split(" ", 3);
        switch (parts[0]) {
            case "tree":
                return onMain(this::dumpTree);
            case "click":
                if (parts.length < 2) return "ERR:click needs TAG";
                return onMain(() -> perform(parts[1], "OnClick", null));
            case "settext":
                if (parts.length < 3) return "ERR:settext needs TAG TEXT";
                return onMain(() -> perform(parts[1], "SetText", parts[2]));
            case "quit":
                running = false;
                return "ok";
            default:
                return "ERR:unknown command " + parts[0];
        }
    }

    private interface Job { String run() throws Exception; }

    /** Compose state is only safe to touch on the main thread. */
    private String onMain(Job job) throws Exception {
        String[] result = new String[1];
        Exception[] error = new Exception[1];
        runOnMainSync(() -> {
            try { result[0] = job.run(); } catch (Exception e) { error[0] = e; }
        });
        if (error[0] != null) throw error[0];
        return result[0];
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
