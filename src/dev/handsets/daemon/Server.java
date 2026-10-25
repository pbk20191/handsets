package dev.handsets.daemon;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;

public final class Server {

    static final int MAX_CMD = 256;

    /** Bag of handler classes wired up in {@link Main}; lets the constructor
     *  stay legible as we grow the command surface. */
    public static final class Handlers {
        public Dumper dumper;
        public Screenshot shot;
        public Input input;
        public Files files;
        public Installer installer;
        public Pm pm;
        public Manifest manifest;
        public Providers providers;
        public Location location;
        public Notifications notifs;
        public Clipboard clip;
        public Am am;
        public Props props;
        public Dumpsys dumpsys;
        public Logcat logcat;
        public SettingsApi settings;
        public ShellExec shellExec;
        public Wm wm;
        public Lifecycle lifecycle;
        public State state;
        public UiEvents uiEvents;
        public NodeActions nodes;
    }

    private final Handlers h;

    public Server(Handlers h) {
        this.h = h;
    }

    Handlers handlers() { return h; }

    /**
     * Synchronous verbs: returns the single response frame, or null when
     * {@code head} is not one of them (streams, transfers and waits are
     * driven by {@link Dispatch}). Thread-safe; callers pick the thread.
     */
    byte[] respond(String head, String cmd) {
        switch (head) {
            case "dump":
                return utf8(safeDumpAll());
            case "dump_active":
                return utf8(safeDumpActive());
            case "info":
                return utf8(h.shot.sourceSize().x + " " + h.shot.sourceSize().y);
            case "screenshot":
                return safeScreenshot(cmd);
            case "pm_list":
                return runPmList(cmd);
            case "pm_path":
                return runPmPath(cmd);
            case "pm_uninstall":
                return runPmUninstall(cmd);
            case "pm_grant":
            case "pm_revoke":
                return runPmPerm(head, cmd);
            case "am_start":
                return runAmStart(cmd);
            case "am_force_stop":
                return runAmForceStop(cmd);
            case "am_kill":
                return runAmKill(cmd);
            case "am_broadcast":
                return runAmBroadcast(cmd);
            case "getprop":
                return runGetProp(cmd);
            case "setprop":
                return runSetProp(cmd);
            case "settings_get":
                return runSettingsGet(cmd);
            case "settings_put":
                return runSettingsPut(cmd);
            case "wm_info":
                return h.wm.info();
            case "wm_rotation":
                return runWmRotation(cmd);
            case "state":
                return runState(cmd);
            case "node_click":
                return runNodeClick(cmd, false);
            case "node_long_click":
                return runNodeClick(cmd, true);
            case "node_set_text":
                return runNodeSetText(cmd);
            case "node_scroll":
                return runNodeScroll(cmd);
            case "node_focus":
                return runNodeFocus(cmd);
            case "submit":
                return runSubmit(cmd);
            case "paste":
                return runPaste(cmd);
            case "deeplinks":
                return runDeeplinks(cmd);
            case "sms":
                return runSms(cmd);
            case "calls":
                return runCalls(cmd);
            case "contacts":
                return runContacts(cmd);
            case "calendar":
                return runCalendar(cmd);
            case "location":
                return h.location.last();
            case "notifications":
                return runNotifications(cmd);
            case "clip_get":
                return h.clip.get();
            case "clip_set":
                return h.clip.set(afterHead(cmd));
            default:
                return null;
        }
    }

    private String safeDumpAll() {
        try {
            return h.dumper.dumpAll();
        } catch (Throwable t) {
            return "ERR:INTERNAL:dump-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage();
        }
    }

    private String safeDumpActive() {
        try {
            return h.dumper.dumpActive();
        } catch (Throwable t) {
            return "ERR:INTERNAL:dump-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage();
        }
    }

    private byte[] safeScreenshot(String cmd) {
        try {
            Screenshot.CaptureArgs args = parseScreenshotArgs(cmd);
            return h.shot.capture(args);
        } catch (Throwable t) {
            return errBytes("screenshot-failed:" + t.getClass().getSimpleName() + ":" + t.getMessage());
        }
    }

    static final class InputArgs {
        int x, y, x1, y1, x2, y2, dur, dy, code;
        String keyName;
    }

    /** Parse k=v tokens plus a single positional name (for `key BACK`). */
    static InputArgs parseInputArgs(String cmd) {
        InputArgs a = new InputArgs();
        int sp = cmd.indexOf(' ');
        if (sp < 0) return a;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return a;
        for (String tok : tail.split("\\s+")) {
            int eq = tok.indexOf('=');
            if (eq < 0) {
                // positional — treat as key name for `key BACK` style
                if (a.keyName == null) a.keyName = tok;
                continue;
            }
            String k = tok.substring(0, eq);
            String v = tok.substring(eq + 1);
            try {
                switch (k) {
                    case "x":    a.x  = Integer.parseInt(v); break;
                    case "y":    a.y  = Integer.parseInt(v); break;
                    case "x1":   a.x1 = Integer.parseInt(v); break;
                    case "y1":   a.y1 = Integer.parseInt(v); break;
                    case "x2":   a.x2 = Integer.parseInt(v); break;
                    case "y2":   a.y2 = Integer.parseInt(v); break;
                    case "dur":  a.dur = Integer.parseInt(v); break;
                    case "dy":   a.dy  = Integer.parseInt(v); break;
                    case "code": a.code = Integer.parseInt(v); break;
                    default:
                        // ignore unknown for forward-compat
                }
            } catch (NumberFormatException ignored) {}
        }
        return a;
    }

    void runTileStream(String cmd, DataOutputStream out) {
        try {
            Screenshot.CaptureArgs args = parseScreenshotArgs(cmd);
            if (args.longEdge == 0 && !args.max) args.max = true;  // default native
            Screenshot.Mirror mirror = h.shot.mirrorFor(args);
            int tile = args.tile > 0 ? args.tile : 128;
            int q = clampQuality(args.quality);
            TileStreamer s = new TileStreamer(mirror, q, tile, out);
            s.serve();
        } catch (Throwable t) {
            try {
                writeFrame(out, errBytes("tilejpeg-failed:" + t.getClass().getSimpleName()
                        + ":" + t.getMessage()));
            } catch (IOException ignored) {}
        }
    }

    void runH264Stream(String cmd, DataOutputStream out) {
        try {
            Screenshot.CaptureArgs args = parseScreenshotArgs(cmd);
            // H.264 stream defaults to native resolution like JPEG stream does.
            if (args.longEdge == 0 && !args.max) args.max = true;
            int srcLong = Math.max(h.shot.sourceSize().x, h.shot.sourceSize().y);
            int targetLongEdge = args.max
                    ? srcLong
                    : (args.longEdge > 0 ? Math.min(args.longEdge, srcLong) : 768);
            H264Streamer s = new H264Streamer(
                    h.shot.displayManager(),
                    h.shot.sourceSize(),
                    targetLongEdge,
                    args.bitrateKbps,
                    args.fps,
                    args.gopSec,
                    out);
            s.serve();
        } catch (Throwable t) {
            try {
                writeFrame(out, errBytes("h264-failed:" + t.getClass().getSimpleName()
                        + ":" + t.getMessage()));
            } catch (IOException ignored) {}
        }
    }

    void runStream(String cmd, DataOutputStream out) {
        try {
            Screenshot.CaptureArgs args = parseScreenshotArgs(cmd);
            // Streams default to native resolution to match the snapshot users liked.
            if (args.longEdge == 0 && !args.max) args.max = true;
            Screenshot.Mirror mirror = h.shot.mirrorFor(args);
            Streamer s = new Streamer(mirror, clampQuality(args.quality), args.fps, out);
            s.serve();
        } catch (Throwable t) {
            try {
                writeFrame(out, errBytes("stream-failed:" + t.getClass().getSimpleName()
                        + ":" + t.getMessage()));
            } catch (IOException ignored) {}
        }
    }

    private static int clampQuality(int q) {
        if (q <= 0) return 80;
        if (q > 100) return 100;
        return q;
    }

    private static Screenshot.CaptureArgs parseScreenshotArgs(String cmd) {
        Screenshot.CaptureArgs a = new Screenshot.CaptureArgs();
        // cmd = "screenshot[ k=v[ k=v...]]"
        int sp = cmd.indexOf(' ');
        if (sp < 0) return a;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return a;
        for (String tok : tail.split("\\s+")) {
            int eq = tok.indexOf('=');
            if (eq <= 0) continue;
            String k = tok.substring(0, eq);
            String v = tok.substring(eq + 1);
            try {
                switch (k) {
                    case "size":
                        a.longEdge = Integer.parseInt(v);
                        break;
                    case "q":
                    case "quality":
                        a.quality = Integer.parseInt(v);
                        break;
                    case "fmt":
                    case "format":
                        a.format = v;
                        break;
                    case "max":
                        a.max = !"0".equals(v) && !"false".equalsIgnoreCase(v);
                        break;
                    case "fps":
                        a.fps = Integer.parseInt(v);
                        break;
                    case "bitrate":
                    case "kbps":
                        a.bitrateKbps = Integer.parseInt(v);
                        break;
                    case "tile":
                        a.tile = Integer.parseInt(v);
                        break;
                    case "gop":
                        a.gopSec = Integer.parseInt(v);
                        break;
                    case "secure_check":
                    case "secure":
                        a.secureCheck = !"0".equals(v) && !"false".equalsIgnoreCase(v);
                        break;
                    default:
                        // unknown key — ignore so old clients keep working
                }
            } catch (NumberFormatException ignored) {}
        }
        return a;
    }

    static byte[] utf8(String s) {
        return s.getBytes(StandardCharsets.UTF_8);
    }

    static byte[] errBytes(String tail) {
        return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8);
    }

    private static void writeFrame(DataOutputStream out, byte[] bytes) throws IOException {
        out.writeInt(bytes.length);
        out.write(bytes);
        out.flush();
    }

    // ---------- adb-like command handlers ----------

    void runPull(String cmd, DataOutputStream out) throws IOException {
        FileArgs a = parseFileArgs(cmd);
        if (a.path == null) {
            writeFrame(out, errBytes("pull-needs-path"));
            out.writeInt(0);
            out.flush();
            return;
        }
        try {
            h.files.pull(a.path, out);
        } catch (Throwable t) {
            // Files.pull already best-effort-terminates the stream.
            System.err.println("pull failed: " + t);
        }
    }

    byte[] runPush(String cmd, DataInputStream in) {
        FileArgs a = parseFileArgs(cmd);
        if (a.path == null) return errBytes("push-needs-path");
        return h.files.push(a.path, a.mode, a.size, in);
    }

    byte[] runInstall(String cmd, DataInputStream in) {
        FileArgs a = parseFileArgs(cmd);
        return h.installer.install(a.size, a.reinstall, a.grant, in);
    }

    private byte[] runPmList(String cmd) {
        boolean thirdParty = cmd.contains(" 3") || cmd.contains("third=1");
        boolean systemOnly = cmd.contains(" s") || cmd.contains("system=1");
        return h.pm.list(thirdParty, systemOnly);
    }

    private byte[] runPmPath(String cmd) {
        String pkg = positional(cmd);
        if (pkg == null) return errBytes("pm_path-needs-pkg");
        return h.pm.path(pkg);
    }

    private byte[] runPmUninstall(String cmd) {
        String pkg = positional(cmd);
        if (pkg == null) return errBytes("pm_uninstall-needs-pkg");
        return h.pm.uninstall(pkg);
    }

    private byte[] runPmPerm(String head, String cmd) {
        String[] parts = positionalPair(cmd);
        if (parts == null) return errBytes(head + "-needs-pkg-and-perm");
        if ("pm_grant".equals(head)) return h.pm.grant(parts[0], parts[1]);
        return h.pm.revoke(parts[0], parts[1]);
    }

    private byte[] runAmStart(String cmd) {
        AmArgs a = parseAmArgs(cmd);
        if (a.component == null) return errBytes("am_start-needs-n=pkg/.Class");
        return h.am.start(a.component, a.action, a.data, a.flags);
    }

    private byte[] runAmForceStop(String cmd) {
        String pkg = positional(cmd);
        if (pkg == null) return errBytes("am_force_stop-needs-pkg");
        return h.am.forceStop(pkg);
    }

    private byte[] runAmKill(String cmd) {
        String pkg = positional(cmd);
        if (pkg == null) return errBytes("am_kill-needs-pkg");
        return h.am.kill(pkg);
    }

    private byte[] runAmBroadcast(String cmd) {
        AmArgs a = parseAmArgs(cmd);
        return h.am.broadcast(a.action, a.component, a.data);
    }

    /**
     * `swipe_dir <left|right|up|down> [dur=N]` — synthesises an 80%-of-screen
     * swipe in the named direction starting from the screen centre. Screen
     * dimensions come from the existing Screenshot mirror so we don't
     * round-trip another binder call per swipe.
     */
    // ---------- arg parsers for the new commands ----------

    private static final class FileArgs {
        String path;
        int mode;
        long size;
        boolean reinstall;
        boolean grant;
    }

    private static FileArgs parseFileArgs(String cmd) {
        FileArgs a = new FileArgs();
        int sp = cmd.indexOf(' ');
        if (sp < 0) return a;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return a;
        for (String tok : tail.split("\\s+")) {
            int eq = tok.indexOf('=');
            if (eq <= 0) continue;
            String k = tok.substring(0, eq);
            String v = tok.substring(eq + 1);
            try {
                switch (k) {
                    case "path":      a.path = v; break;
                    case "mode":      a.mode = Integer.parseInt(v, 8); break;
                    case "size":      a.size = Long.parseLong(v); break;
                    case "reinstall": a.reinstall = !"0".equals(v); break;
                    case "grant":     a.grant = !"0".equals(v); break;
                    default:
                        // ignore unknown for forward-compat
                }
            } catch (NumberFormatException ignored) {}
        }
        return a;
    }

    private static final class AmArgs {
        String component, action, data;
        int flags;
    }

    private static AmArgs parseAmArgs(String cmd) {
        AmArgs a = new AmArgs();
        int sp = cmd.indexOf(' ');
        if (sp < 0) return a;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return a;
        for (String tok : tail.split("\\s+")) {
            int eq = tok.indexOf('=');
            if (eq <= 0) continue;
            String k = tok.substring(0, eq);
            String v = tok.substring(eq + 1);
            try {
                switch (k) {
                    case "n":     a.component = v; break;
                    case "a":     a.action = v; break;
                    case "d":     a.data = v; break;
                    case "f":     a.flags = Integer.decode(v); break;
                    default:
                        // ignore
                }
            } catch (NumberFormatException ignored) {}
        }
        return a;
    }

    /** Return the first whitespace-separated token after the command head. */
    static String positional(String cmd) {
        int sp = cmd.indexOf(' ');
        if (sp < 0) return null;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return null;
        int sp2 = tail.indexOf(' ');
        return sp2 < 0 ? tail : tail.substring(0, sp2);
    }

    // ---------- second-wave command handlers ----------

    private byte[] runGetProp(String cmd) {
        String key = positional(cmd);
        if (key == null) return errBytes("getprop-needs-key");
        return h.props.doGet(key);
    }

    private byte[] runSetProp(String cmd) {
        String[] pair = positionalPair(cmd);
        if (pair == null) return errBytes("setprop-needs-key-and-value");
        return h.props.doSet(pair[0], pair[1]);
    }

    void runDumpsys(String cmd, DataOutputStream out) {
        String tail = afterHead(cmd);
        if (tail == null || tail.isEmpty()) {
            writeErrAndTerminator(out, "dumpsys-needs-service");
            return;
        }
        String[] parts = tail.split("\\s+");
        String svc = parts[0];
        String[] args = new String[parts.length - 1];
        System.arraycopy(parts, 1, args, 0, args.length);
        h.dumpsys.dump(svc, args, out);
    }

    void runLogcat(String cmd, DataOutputStream out) {
        String tail = afterHead(cmd);
        java.util.List<String> args = new java.util.ArrayList<>();
        if (tail != null && !tail.isEmpty()) {
            for (String t : tail.split("\\s+")) args.add(t);
        }
        h.logcat.stream(args, out);
    }

    private byte[] runSettingsGet(String cmd) {
        String[] parts = positionalPair(cmd);
        if (parts == null) return errBytes("settings_get-needs-namespace-and-key");
        return h.settings.get(parts[0], parts[1]);
    }

    private byte[] runSettingsPut(String cmd) {
        // Need namespace + key + value (3 positional tokens).
        String tail = afterHead(cmd);
        if (tail == null) return errBytes("settings_put-needs-namespace-key-value");
        String[] parts = tail.split("\\s+", 3);
        if (parts.length < 3) return errBytes("settings_put-needs-namespace-key-value");
        return h.settings.put(parts[0], parts[1], parts[2]);
    }

    void runShell(String cmd, DataOutputStream out) {
        String tail = afterHead(cmd);
        if (tail == null || tail.isEmpty()) {
            writeErrAndTerminator(out, "shell-needs-argv");
            return;
        }
        java.util.List<String> argv = new java.util.ArrayList<>();
        for (String t : tail.split("\\s+")) argv.add(t);
        h.shellExec.run(argv, out);
    }

    private byte[] runWmRotation(String cmd) {
        String v = positional(cmd);
        if (v == null) return errBytes("wm_rotation-needs-value");
        try { return h.wm.setRotation(Integer.parseInt(v)); }
        catch (NumberFormatException nf) { return errBytes("bad-rotation:" + v); }
    }

    byte[] runInstallMulti(String cmd, DataInputStream in) {
        // install_multi sizes=N0,N1,N2 [reinstall=1] [grant=1]
        FileArgs base = parseFileArgs(cmd);
        long[] sizes = null;
        for (String tok : cmd.split("\\s+")) {
            if (tok.startsWith("sizes=")) {
                String[] parts = tok.substring("sizes=".length()).split(",");
                sizes = new long[parts.length];
                try {
                    for (int i = 0; i < parts.length; i++) sizes[i] = Long.parseLong(parts[i]);
                } catch (NumberFormatException e) {
                    return errBytes("install_multi-bad-sizes");
                }
            }
        }
        if (sizes == null || sizes.length == 0) {
            return errBytes("install_multi-needs-sizes");
        }
        return h.installer.installMulti(sizes, base.reinstall, base.grant, in);
    }

    void runMonitor(DataOutputStream out) {
        h.lifecycle.monitor(out);
    }

    // ---------- wait-for / composite handlers ----------

    static final long DEFAULT_IDLE_MS = 200;
    static final long DEFAULT_TIMEOUT_MS = 5000;
    static final long DEFAULT_TAP_TIMEOUT_MS = 2000;

    // ---------- node_* handlers ----------

    private byte[] runNodeClick(String cmd, boolean longClick) {
        String sel = afterHead(cmd);
        if (sel == null) return errBytes((longClick ? "node_long_click" : "node_click") + "-needs-selector");
        return longClick ? h.nodes.longClick(sel) : h.nodes.click(sel);
    }

    private byte[] runNodeSetText(String cmd) {
        String tail = afterHead(cmd);
        if (tail == null) return errBytes("node_set_text-needs-selector-and-value");
        // Pull value= out, treat the rest as the selector.
        String value = extractKey(tail, "value");
        if (value == null) return errBytes("node_set_text-needs-value=");
        // Empty selector is intentional: NodeActions.setText handles it by
        // targeting whichever EditText currently has input focus. Kept
        // explicit here so callers don't need a sentinel value — they just
        // omit the selector and `node_set_text value="..."` writes to the
        // focused field, which is what hs tui needs after a tap-to-focus.
        return h.nodes.setText(removeKey(tail, "value").trim(), value);
    }

    private byte[] runNodeScroll(String cmd) {
        String tail = afterHead(cmd);
        if (tail == null) return errBytes("node_scroll-needs-selector-and-dir");
        String dir = extractKey(tail, "dir");
        if (dir == null) dir = "forward";
        String sel = removeKey(tail, "dir");
        if (sel.isEmpty()) return errBytes("node_scroll-needs-selector");
        return h.nodes.scroll(sel, dir);
    }

    private byte[] runNodeFocus(String cmd) {
        String sel = afterHead(cmd);
        if (sel == null) return errBytes("node_focus-needs-selector");
        return h.nodes.focus(sel);
    }

    private byte[] runSubmit(String cmd) {
        // `submit` with no args targets the focused EditText. A trailing
        // selector overrides that — `submit text~=Email`.
        return h.nodes.imeAction(afterHead(cmd));
    }

    private byte[] runPaste(String cmd) {
        // `paste` with no args inserts the system clipboard into the
        // focused EditText. Trailing selector overrides — `paste id=...`.
        return h.nodes.pasteAction(afterHead(cmd));
    }

    private byte[] runDeeplinks(String cmd) {
        String pkg = positional(cmd);
        return h.manifest.deeplinks(pkg);
    }

    // ---------- user-data ContentProvider verbs ----------

    private byte[] runSms(String cmd) {
        String kind = strArgOr(cmd, "type", "inbox");
        int limit = (int) longArg(cmd, "limit", 50);
        return h.providers.sms(kind, limit);
    }

    private byte[] runCalls(String cmd) {
        String kind = strArgOr(cmd, "type", "all");
        int limit = (int) longArg(cmd, "limit", 50);
        return h.providers.calls(kind, limit);
    }

    private byte[] runContacts(String cmd) {
        int limit = (int) longArg(cmd, "limit", 50);
        return h.providers.contacts(limit);
    }

    private byte[] runCalendar(String cmd) {
        long now = System.currentTimeMillis();
        long from = longArg(cmd, "from", now);
        long to   = longArg(cmd, "to",   now + 7L * 24 * 60 * 60 * 1000);
        int limit = (int) longArg(cmd, "limit", 50);
        return h.providers.calendar(from, to, limit);
    }

    private byte[] runNotifications(String cmd) {
        String pkg = strArg(cmd, "pkg");
        int limit  = (int) longArg(cmd, "limit", 50);
        boolean history = longArg(cmd, "history", 0) != 0;
        return h.notifs.dump(pkg, limit, history);
    }

    void runClipWatch(String cmd, java.io.DataOutputStream out) {
        long interval = longArg(cmd, "interval_ms", 500);
        h.clip.watch(out, interval);
    }

    // ---------- key=value arg helpers ----------

    static NodeActions.Selector textSelector(String text, String mode) {
        NodeActions.Selector s = new NodeActions.Selector();
        if ("exact".equalsIgnoreCase(mode)) s.textExact = text;
        else                                s.textSub = text;
        return s;
    }

    /** Parse {@code key=value} from a whitespace-tokenised string. */
    static String strArg(String cmd, String key) {
        return strArgOr(cmd, key, null);
    }

    static String strArgOr(String cmd, String key, String dflt) {
        int sp = cmd.indexOf(' ');
        if (sp < 0) return dflt;
        String tail = cmd.substring(sp + 1);
        String pat = key + "=";
        int i = 0;
        while ((i = tail.indexOf(pat, i)) >= 0) {
            if (i == 0 || Character.isWhitespace(tail.charAt(i - 1))) {
                int start = i + pat.length();
                if (start < tail.length() && tail.charAt(start) == '"') {
                    int end = tail.indexOf('"', start + 1);
                    if (end > 0) return tail.substring(start + 1, end);
                    return dflt;
                }
                int end = start;
                while (end < tail.length() && !Character.isWhitespace(tail.charAt(end))) end++;
                return tail.substring(start, end);
            }
            i += pat.length();
        }
        return dflt;
    }

    static long longArg(String cmd, String key, long dflt) {
        String v = strArg(cmd, key);
        if (v == null) return dflt;
        try { return Long.parseLong(v); }
        catch (NumberFormatException nf) { return dflt; }
    }

    /** Same as strArg but works on an arbitrary token string (no command head). */
    private static String extractKey(String tail, String key) {
        String fakeCmd = "_ " + tail;
        return strArg(fakeCmd, key);
    }

    /** Strip a {@code key="..."} or {@code key=value} token from {@code tail}. */
    private static String removeKey(String tail, String key) {
        StringBuilder out = new StringBuilder(tail.length());
        int i = 0;
        boolean inQuote = false;
        StringBuilder tok = new StringBuilder();
        while (i < tail.length()) {
            char c = tail.charAt(i);
            if (c == '"') inQuote = !inQuote;
            if (Character.isWhitespace(c) && !inQuote) {
                String t = tok.toString();
                if (!t.isEmpty()) {
                    if (!t.startsWith(key + "=")) {
                        if (out.length() > 0) out.append(' ');
                        out.append(t);
                    }
                    tok.setLength(0);
                }
            } else {
                tok.append(c);
            }
            i++;
        }
        String t = tok.toString();
        if (!t.isEmpty() && !t.startsWith(key + "=")) {
            if (out.length() > 0) out.append(' ');
            out.append(t);
        }
        return out.toString();
    }

    /**
     * Streaming subscription to State.device() snapshots. Server pushes one
     * frame whenever the cache is recomputed (driven by the existing a11y +
     * display listeners). The client side hands these straight to stdout /
     * its local mirror — no polling, no per-read binder hops.
     *
     * The connection thread blocks on the queue here. The State refresher
     * thread re-fires every HEARTBEAT_MS even if nothing changed, so a
     * write failure surfaces a dead client within that window.
     */
    private byte[] runState(String cmd) {
        String field = positional(cmd);
        if (field == null) return errBytes("state-needs-field");
        switch (field) {
            case "interactive":       return h.state.interactive();
            case "battery_level":     return h.state.batteryLevel();
            case "battery_charging":  return h.state.batteryCharging();
            case "top":               return h.state.topActivity();
            case "procs":             return h.state.procs();
            case "device":            return h.state.device();
            case "device_fresh":      return h.state.deviceFresh();
            default: return errBytes("unknown-state-field:" + field);
        }
    }

    /** Return everything after the command head (the first space-separated token), or null. */
    private static String afterHead(String cmd) {
        int sp = cmd.indexOf(' ');
        if (sp < 0) return null;
        String tail = cmd.substring(sp + 1).trim();
        return tail.isEmpty() ? null : tail;
    }

    /** Write an ERR frame then the chunked-stream EOF terminator. */
    private static void writeErrAndTerminator(DataOutputStream out, String tail) {
        try {
            byte[] msg = errBytes(tail);
            synchronized (out) {
                out.writeInt(msg.length);
                out.write(msg);
                out.writeInt(0);
                out.flush();
            }
        } catch (IOException ignored) {}
    }

    private static String[] positionalPair(String cmd) {
        int sp = cmd.indexOf(' ');
        if (sp < 0) return null;
        String tail = cmd.substring(sp + 1).trim();
        if (tail.isEmpty()) return null;
        String[] parts = tail.split("\\s+", 2);
        if (parts.length < 2) return null;
        return parts;
    }
}
