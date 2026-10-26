#!/usr/bin/env python3
"""Wire-protocol smoke test for the on-device daemon.

    wire-smoke.py run  --jar build/hs.jar --out a.json [--serial S] [--port 9008]
    wire-smoke.py diff a.json b.json          # exit 1 on any difference
    wire-smoke.py threads [--serial S]        # thread names of the running hsd
    wire-smoke.py bench   [-n 100]            # p50/p95 ms for hot verbs

`run` pushes and starts the jar, runs every case, and records normalized
responses: volatile values (timestamps, elapsed, battery, memory, image and
video bytes) are masked so two runs of the same daemon diff clean. Compare an
old and a new jar by running both and diffing.
"""
import argparse, json, os, re, socket, struct, subprocess, sys, threading, time

PORT = 9008
SERIAL = None


def adb_path():
    if os.environ.get("ADB"):
        return os.environ["ADB"]
    lp = os.path.join(os.path.dirname(__file__), "..", "local.properties")
    if os.path.exists(lp):
        for line in open(lp, encoding="utf-8"):
            if line.startswith("sdk.dir="):
                sdk = line.split("=", 1)[1].strip().replace("\\:", ":").replace("\\\\", "\\")
                return os.path.join(sdk, "platform-tools", "adb")
    return "adb"


def adb(*args):
    cmd = [adb_path()] + (["-s", SERIAL] if SERIAL else []) + list(args)
    return subprocess.run(cmd, capture_output=True, text=True).stdout


# ── wire ──────────────────────────────────────────────────────────────

def connect(timeout=15):
    s = socket.create_connection(("127.0.0.1", PORT), timeout=timeout)
    s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    return s


def send(s, b):
    s.sendall(struct.pack(">I", len(b)) + b)


def rx(s, n):
    b = b""
    while len(b) < n:
        c = s.recv(n - len(b))
        if not c:
            raise EOFError
        b += c
    return b


def recv(s):
    return rx(s, struct.unpack(">I", rx(s, 4))[0])


def call(cmd, raw=False):
    s = connect()
    send(s, cmd if raw else cmd.encode())
    try:
        return recv(s)
    finally:
        s.close()


def chunked(cmd, limit=None):
    """Frames until len=0 (marked b'<END>') or `limit` frames."""
    s = connect()
    send(s, cmd.encode())
    out = []
    try:
        while limit is None or len(out) < limit:
            n = struct.unpack(">I", rx(s, 4))[0]
            if n == 0:
                out.append(b"<END>")
                break
            out.append(rx(s, n))
    finally:
        s.close()
    return out


# ── normalization ─────────────────────────────────────────────────────

VOLATILE_KEYS = re.compile(r'"(ts|uptime\w*|battery\w*|mem\w*|storage\w*|ram\w*|\w*_kb|\w*temp\w*|elapsed\w*|\w*_ms|\w*_s|procs?|ip|time\w*)":\s*("[^"]*"|-?[\d.]+)')


def norm(b):
    if isinstance(b, list):
        return [norm(x) for x in b]
    if b.startswith(b"\xff\xd8"):
        return "<jpeg>"
    if b.startswith(b"\x89PNG"):
        return "<png>"
    try:
        t = b.decode("utf-8")
    except UnicodeDecodeError:
        return f"<binary {b[:1].hex()}>"
    t = VOLATILE_KEYS.sub(lambda m: f'"{m.group(1)}":"~"', t)
    t = re.sub(r"elapsed=\d+", "elapsed=~", t)
    if t.startswith("{") and '"root"' in t:
        return "<a11y-json>"
    return t


# ── cases ─────────────────────────────────────────────────────────────

def h264_summary(n=3):
    fr = chunked("stream_h264 size=720", n)
    return [f"nal{f[4] & 0x1f}" if len(f) > 4 else norm(f) for f in fr]


def tile_summary(n=3):
    return [f"type{f[0]}" for f in chunked("stream_tilejpeg size=400", n)]


def push_pull():
    data = os.urandom(300_000)
    s = connect()
    send(s, f"push path=/data/local/tmp/ws.bin size={len(data)}".encode())
    for i in range(0, len(data), 65536):
        send(s, data[i:i + 65536])
    s.sendall(b"\0\0\0\0")
    r = recv(s)
    s.close()
    back = b"".join(x for x in chunked("pull path=/data/local/tmp/ws.bin") if x != b"<END>")
    return [norm(r), back == data]


def install(truncate=False):
    path = call("pm_path com.android.egg").decode()
    apk = b"".join(x for x in chunked(f"pull path={path}") if x != b"<END>")
    s = connect(60)
    send(s, f"install size={len(apk)} reinstall=1".encode())
    if truncate:
        send(s, apk[:100_000])
        s.close()
        time.sleep(1)
        return norm(call("ping"))
    for i in range(0, len(apk), 262144):
        send(s, apk[i:i + 262144])
    s.sendall(b"\0\0\0\0")
    r = recv(s)
    s.close()
    return norm(r)


def bad_len(n):
    s = connect()
    s.sendall(struct.pack(">I", n) + b"x" * min(n, 300))
    try:
        r = norm(recv(s))
        try:
            closed = s.recv(1) == b""
        except Exception:
            closed = True
        return [r, "closed" if closed else "open"]
    finally:
        s.close()


def parallel_ping_during_wait():
    t = threading.Thread(target=lambda: call("wait_for_text text=__nope__ timeout_ms=2500"))
    t.start()
    time.sleep(0.3)
    worst = 0
    for _ in range(10):
        t0 = time.time()
        call("ping")
        worst = max(worst, (time.time() - t0) * 1000)
    t.join()
    return worst < 100


def slow_client():
    s = connect()
    send(s, b"stream max=1")
    time.sleep(0.5)
    ts = []
    for _ in range(30):
        t0 = time.time()
        call("ping")
        ts.append((time.time() - t0) * 1000)
    s.close()
    return sorted(ts)[28] < 50


def disconnect_cleanup():
    s = connect()
    send(s, b"shell sleep 91")
    time.sleep(1)
    s.close()
    time.sleep(1.5)
    return "sleep 91" not in adb("shell", "ps -A -o ARGS")


CASES = {
    "ping": lambda: norm(call("ping")),
    "info": lambda: norm(call("info")),
    "dump": lambda: norm(call("dump")),
    "dump_active": lambda: norm(call("dump_active")),
    "screenshot_jpeg": lambda: norm(call("screenshot size=300")),
    "screenshot_png": lambda: norm(call("screenshot size=200 fmt=png")),
    "unknown": lambda: norm(call("nosuchverb")),
    "key": lambda: norm(call("key HOME")),
    "tap": lambda: norm(call("tap x=540 y=1200")),
    "tap_offscreen": lambda: norm(call("tap x=99999 y=1")),
    "swipe": lambda: norm(call("swipe x1=100 y1=1000 x2=900 y2=1000 dur=150")),
    "swipe_dir": lambda: norm(call("swipe_dir up")),
    "text_cjk": lambda: norm(call("text abc")),
    "getprop": lambda: norm(call("getprop ro.build.version.sdk")),
    "settings_get": lambda: norm(call("settings_get global adb_enabled")),
    "wm_info": lambda: norm(call("wm_info")),
    "pm_list": lambda: norm(call("pm_list s"))[:200],
    "pm_path": lambda: norm(call("pm_path com.android.settings")),
    "state_fields": lambda: [norm(call(f"state {f}")) for f in ("interactive", "top", "device")],
    "clip_roundtrip_cjk": lambda: [norm(call("clip_set 안녕 세상")), norm(call("clip_get"))],
    "deeplinks": lambda: norm(call("deeplinks com.android.settings"))[:200],
    "providers": lambda: [norm(call(f"{v} limit=1"))[:120] for v in ("sms", "calls", "contacts")],
    "wait_idle": lambda: norm(call("wait_for_idle idle_ms=100 timeout_ms=3000")),
    "wait_text_timeout": lambda: norm(call("wait_for_text text=__nope__ timeout_ms=500")),
    "wait_activity": lambda: norm(call("wait_for_activity n=com.google.android.apps.nexuslauncher/.X timeout_ms=2000")),
    "tap_and_dump": lambda: norm(call("tap_and_dump x=540 y=1200")),
    "tap_and_settle": lambda: norm(call("tap_and_settle x=540 y=1200")),
    "stream_n": lambda: norm(chunked("stream size=300", 2)),
    "stream_h264_n": h264_summary,
    "stream_tile_n": tile_summary,
    "state_watch_n": lambda: [x[:1].decode() for x in chunked("state_watch", 1)],
    "clip_watch_n": lambda: norm(chunked("clip_watch interval_ms=200", 1)),
    "logcat_head": lambda: chunked("logcat -d -t 3")[-1].decode(),
    "dumpsys_window": lambda: chunked("dumpsys window windows")[-1].decode(),
    "dumpsys_bad": lambda: norm(chunked("dumpsys __nosvc__")),
    "shell_echo": lambda: norm(chunked("shell echo hi")),
    "shell_noargv": lambda: norm(chunked("shell")),
    "pull_missing": lambda: norm(chunked("pull path=/data/local/tmp/__nope__")),
    "pull_push": push_pull,
    "install": install,
    "install_truncated": lambda: install(truncate=True),
    "pm_uninstall_noarg": lambda: norm(call("pm_uninstall")),
    "install_multi_nosizes": lambda: norm(call("install_multi")),
    "keyframe_idle": lambda: norm(call("keyframe")),
    "len_256": lambda: norm(call(b"ping" + b" " * 252, raw=True)),
    "len_257": lambda: bad_len(257),
    "len_0": lambda: bad_len(0),
    "parallel_ping_during_wait": parallel_ping_during_wait,
    "slow_client": slow_client,
    "disconnect_cleanup": disconnect_cleanup,
}


def start_daemon(jar):
    adb("shell", "pkill -f hsd")
    time.sleep(0.3)
    adb("push", jar, "/data/local/tmp/hs.jar")
    adb("forward", f"tcp:{PORT}", f"localabstract:hsd_{PORT}")
    adb("shell", f"CLASSPATH=/data/local/tmp/hs.jar nohup app_process /system/bin --nice-name=hsd "
                 f"dev.handsets.daemon.Main --port={PORT} > /data/local/tmp/hsd.log 2>&1 &")
    t0 = time.time()
    while time.time() - t0 < 10:
        try:
            if call("ping") == b"pong" and call("info"):
                return
        except Exception:
            time.sleep(0.2)
    sys.exit("daemon did not start:\n" + adb("shell", "cat /data/local/tmp/hsd.log"))


def cmd_run(a):
    start_daemon(a.jar)
    only = set(a.only.split(",")) if a.only else None
    res = {}
    for name, fn in CASES.items():
        if only and name not in only:
            continue
        try:
            res[name] = fn()
        except Exception as e:
            res[name] = f"EXC {type(e).__name__}: {e}"
        print(f"{name:28} {str(res[name])[:90]}")
    json.dump(res, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)


def cmd_diff(a):
    x = json.load(open(a.a, encoding="utf-8"))
    y = json.load(open(a.b, encoding="utf-8"))
    bad = 0
    for k in sorted(set(x) | set(y)):
        if x.get(k) != y.get(k):
            bad += 1
            print(f"DIFF {k}\n  - {str(x.get(k))[:300]}\n  + {str(y.get(k))[:300]}")
    print(f"{bad} differing case(s)")
    sys.exit(1 if bad else 0)


def cmd_threads(a):
    print(adb("shell", "for t in /proc/$(pidof hsd)/task/*; do cat $t/comm; done"))


def cmd_bench(a):
    for verb in ("ping", "tap x=540 y=1200", "dump_active", "screenshot size=768"):
        ts = []
        for _ in range(a.n):
            t0 = time.time()
            call(verb)
            ts.append((time.time() - t0) * 1000)
        ts.sort()
        print(f"{verb:22} p50={ts[len(ts) // 2]:.1f}ms p95={ts[int(len(ts) * .95) - 1]:.1f}ms")


def main():
    global PORT, SERIAL
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=9008)
    p.add_argument("--serial")
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run"); r.add_argument("--jar", required=True); r.add_argument("--out", required=True); r.add_argument("--only")
    d = sub.add_parser("diff"); d.add_argument("a"); d.add_argument("b")
    sub.add_parser("threads")
    b = sub.add_parser("bench"); b.add_argument("-n", type=int, default=100)
    a = p.parse_args()
    PORT, SERIAL = a.port, a.serial
    {"run": cmd_run, "diff": cmd_diff, "threads": cmd_threads, "bench": cmd_bench}[a.cmd](a)


if __name__ == "__main__":
    main()
