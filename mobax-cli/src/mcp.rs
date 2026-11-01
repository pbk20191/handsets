// `mobax mcp` — a Model Context Protocol server on stdio, so MCP hosts (Claude
// Desktop, Claude Code, the .mcpb bundle, …) can drive a device with the
// same verbs as the CLI.
//
// Transport: newline-delimited JSON-RPC 2.0 on stdin/stdout; logs go to
// stderr. Each tool call runs on its own thread, so a long `wait` never
// holds up a `ui` or a `screenshot`.
//
// Tool calls run the CLI's verbs in this process on warm sockets: the
// server keeps a small pool of connections per device, opened through the
// adb server itself (no host port per device), and reuses them. Actions on
// one device run one at a time; after each, the server waits for the screen
// to settle and replies with what changed (see `mcp_screen`). Lifecycle
// calls (`use`, `drop`) and the catch-all `mobax` tool still run this binary
// as a child process: they are rare, and some leave helpers running.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::json::{self, Value};
use crate::json_out::encode_str;
use crate::mcp_screen::{Change, Screen};
use crate::session::Session;
use crate::{output, Conn};
use crate::Connect;

/// Protocol revisions this server speaks; the newest is offered when the
/// client asks for one we don't know.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Upper bound for one child-process tool call (a verb's own --timeout is
/// usually far lower); stops a stuck child from wedging the server.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// First line of `mobax --help`, printed after argument errors.
const USAGE_HEAD: &str = "mobax — drive Android from the shell.";

/// Verbs the generic `mobax` tool refuses: interactive or never-ending, or
/// they would open a window or recurse.
const BLOCKED_VERBS: &[&str] = &["mcp", "shell", "tui", "mirror", "fan", "run"];

/// Warm sockets kept per device between calls.
const POOL_PER_DEVICE: usize = 4;

const INSTRUCTIONS: &str = "Drive an Android device or emulator. Call `use` first \
to connect and start the on-device daemon.\n\
\n\
`ui` lists the screen as rows numbered @1, @2, …: verb (tap / fill / - for text), \
class, label, #resource-id, centre x,y and state. Act on a row by its ref \
(tap target \"@3\", fill selector \"@5\"), or by text, #resource_id or a CSS-like \
selector ('Button[text=\"OK\"]').\n\
\n\
`tap`, `fill`, `type`, `swipe`, `go` and `open` wait for the screen to settle and \
reply with what changed: + added, ~ changed and - removed rows, or the whole new \
screen. You rarely need `ui` after an action. A ref stays valid until a reply \
shows a new screen.\n\
\n\
When you already know several steps (tap a field, fill it, press enter, wait for \
the result), send them as one `batch`: one call instead of one per step.\n\
\n\
To wait for the screen, use `watch` (any change, or `until` some text shows) or \
`wait` (a selector) instead of calling `ui` repeatedly; the device pushes \
changes, so waiting is free. Use `screenshot` only when the rows don't tell \
you enough (an image costs far more). \
An error starts with ERR:CODE and says what to do next. If `use` finds no device, \
an emulator can be started on the host with `android emulator start NAME` (give \
the AVD hw.gpu.mode=host).";

/// `mobax mcp [GLOBAL OPTIONS]`: serves until stdin closes. The options
/// (e.g. `--device SERIAL`, `--port N`) apply to every tool call.
pub(crate) fn serve(global: &[String]) -> io::Result<()> {
    let engine = Arc::new(Engine::new(global));
    let out = Arc::new(Mutex::new(io::stdout()));
    let workers = Workers::new(WORKERS);
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if line.contains("\"tools/call\"") {
            let (engine, out) = (engine.clone(), out.clone());
            workers.spawn(move || {
                if let Some(reply) = handle(&line, &engine) {
                    send(&out, &reply);
                }
            });
        } else if let Some(reply) = handle(&line, &engine) {
            send(&out, &reply);
        }
    }
    // stdin closed: let calls in flight finish and reply.
    workers.join();
    Ok(())
}

/// Tool calls running at once; more wait in the queue. A blocked `wait`
/// holds one, so this bounds how many devices can be waited on together.
const WORKERS: usize = 8;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Where tool calls run. A fixed set of threads takes jobs from a queue;
/// callers only `spawn`, so how calls are scheduled (threads today, an async
/// runtime if this ever drives far more devices) stays in this one type.
struct Workers {
    queue: Option<std::sync::mpsc::Sender<Job>>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Workers {
    fn new(n: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let threads = (0..n)
            .map(|i| {
                let rx = rx.clone();
                std::thread::Builder::new()
                    .name(format!("mcp-{i}"))
                    .spawn(move || loop {
                        // Hold the queue only to take a job, not to run it.
                        let job = lock(&rx).recv();
                        match job {
                            Ok(job) => job(),
                            Err(_) => return,
                        }
                    })
                    .expect("spawn mcp worker")
            })
            .collect();
        Workers { queue: Some(tx), threads }
    }

    fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        if let Some(q) = &self.queue {
            let _ = q.send(Box::new(job));
        }
    }

    /// Runs what is queued, then stops the threads.
    fn join(mut self) {
        self.queue = None;
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// One reply line on stdout; replies from parallel calls never interleave.
fn send(out: &Mutex<io::Stdout>, reply: &str) {
    let mut o = lock(out);
    let _ = o.write_all(reply.as_bytes());
    let _ = o.write_all(b"\n");
    let _ = o.flush();
}

/// One JSON-RPC message in, at most one reply out (none for notifications).
fn handle(line: &str, engine: &Engine) -> Option<String> {
    let msg = match json::parse(line) {
        Ok(v) => v,
        Err(e) => return Some(error_reply("null", -32700, &format!("parse error: {e}"))),
    };
    let id = json::obj_get(&msg, "id").map(to_json);
    let method = json::obj_get(&msg, "method").and_then(json::as_str).unwrap_or("");
    let params = json::obj_get(&msg, "params");
    // Notifications (no id) get no reply.
    let id = id?;
    let result = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok("{}".to_string()),
        "tools/list" => Ok(tools_list()),
        "tools/call" => tools_call(params, engine),
        "" => Err((-32600, "invalid request: no method".to_string())),
        m => Err((-32601, format!("method not found: {m}"))),
    };
    Some(match result {
        Ok(r) => format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{r}}}"#),
        Err((code, msg)) => error_reply(&id, code, &msg),
    })
}

fn error_reply(id: &str, code: i32, msg: &str) -> String {
    let mut m = String::new();
    encode_str(&mut m, msg);
    format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":{m}}}}}"#)
}

fn initialize(params: Option<&Value>) -> String {
    let asked = params
        .and_then(|p| json::obj_get(p, "protocolVersion"))
        .and_then(json::as_str)
        .unwrap_or("");
    let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
    let mut instructions = String::new();
    encode_str(&mut instructions, INSTRUCTIONS);
    format!(
        r#"{{"protocolVersion":"{version}","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"mobileax","version":"{}"}},"instructions":{instructions}}}"#,
        env!("CARGO_PKG_VERSION")
    )
}

// ---------- in-process execution ----------

/// A device daemon: its serial and the daemon instance (`mobaxd_<port>`).
#[derive(Clone, PartialEq, Eq, Hash)]
struct Target {
    serial: String,
    port: u16,
}

struct Engine {
    /// Options from the `mobax mcp` command line, passed to child calls.
    global: Vec<String>,
    host: String,
    port: u16,
    device: Option<String>,
    targets: Mutex<HashMap<Option<String>, Target>>,
    pool: Mutex<HashMap<Target, Vec<Session>>>,
    screens: Mutex<HashMap<Target, Screen>>,
    /// One action at a time per device.
    locks: Mutex<HashMap<Target, Arc<Mutex<()>>>>,
    /// One daemon start at a time per device.
    starting: Mutex<HashMap<Target, Arc<Mutex<()>>>>,
}

/// A tool's text reply and whether it succeeded.
type Reply = (bool, String);

impl Engine {
    fn new(global: &[String]) -> Self {
        let (mut host, mut port, mut device) = ("127.0.0.1".to_string(), crate::daemon::DEFAULT_PORT, None);
        let mut it = global.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-s" | "--device" => device = it.next().cloned(),
                "--port" => port = it.next().and_then(|p| p.parse().ok()).unwrap_or(port),
                "--host" => host = it.next().cloned().unwrap_or(host),
                _ => {}
            }
        }
        Engine {
            global: global.to_vec(),
            host,
            port,
            device,
            targets: Mutex::default(),
            pool: Mutex::default(),
            screens: Mutex::default(),
            locks: Mutex::default(),
            starting: Mutex::default(),
        }
    }

    /// In-process calls reach the daemon through the local adb server; a
    /// remote `--host` goes through child processes as before.
    fn in_process(&self) -> bool {
        crate::is_local(&self.host)
    }

    fn target(&self, device: Option<&str>) -> Result<Target, String> {
        let key = device.map(str::to_string).or_else(|| self.device.clone());
        if let Some(t) = lock(&self.targets).get(&key) {
            return Ok(t.clone());
        }
        let t = match &key {
            Some(serial) => Target { serial: serial.clone(), port: self.port },
            None => Target {
                serial: crate::daemon::target_serial()
                    .map_err(|e| format!("ERR:{}:{e}\nPass `device`, or call `use` first.", e.code().as_short()))?,
                port: self.port,
            },
        };
        lock(&self.targets).insert(key, t.clone());
        Ok(t)
    }

    /// Drops everything known about devices (after `use` / `drop`).
    fn forget(&self) {
        lock(&self.targets).clear();
        lock(&self.pool).clear();
        lock(&self.screens).clear();
    }

    fn checkout(&self, t: &Target) -> Result<Session, String> {
        loop {
            let pooled = lock(&self.pool).get_mut(t).and_then(Vec::pop);
            match pooled {
                Some(mut s) if s.conn.is_alive() => {
                    s.invalidate_dump();
                    return Ok(s);
                }
                Some(_) => continue,
                None => break,
            }
        }
        let conn = match Conn::connect_device(&t.serial, t.port) {
            Ok(c) => c,
            // The device is there but its daemon isn't: never started, or the
            // device was reset under us (a snapshot load, a reboot). Start it
            // and carry on; screens seen before are stale.
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                self.start_daemon(t).map_err(|e| {
                    format!("ERR:DEVICE_GONE:no daemon on {} and starting one failed ({e})\nCall `use`.", t.serial)
                })?;
                Conn::connect_device(&t.serial, t.port)
                    .map_err(|e| format!("ERR:DEVICE_GONE:{} ({e})\nCall `use`.", t.serial))?
            }
            Err(e) => return Err(format!("ERR:DEVICE_GONE:cannot reach {} ({e})\nCall `use` with an attached device.", t.serial)),
        };
        Ok(Session::with_conn(conn, "127.0.0.1", t.port))
    }

    /// Starts the daemon on `t` once, however many calls find it missing at
    /// the same time.
    fn start_daemon(&self, t: &Target) -> io::Result<()> {
        let guard = lock(&self.starting).entry(t.clone()).or_default().clone();
        let _one = lock(&guard);
        // Another call may have started it while this one waited.
        if crate::daemon::ping_daemon(&t.serial, t.port).is_ok() {
            return Ok(());
        }
        crate::daemon::connect(&crate::daemon::ConnectOpts { serial: Some(t.serial.clone()), port: Some(t.port) })?;
        lock(&self.screens).remove(t);
        lock(&self.pool).remove(t);
        Ok(())
    }

    fn checkin(&self, t: &Target, s: Session) {
        let mut pool = lock(&self.pool);
        let idle = pool.entry(t.clone()).or_default();
        if idle.len() < POOL_PER_DEVICE {
            idle.push(s);
        }
    }

    fn action_lock(&self, t: &Target) -> Arc<Mutex<()>> {
        lock(&self.locks).entry(t.clone()).or_default().clone()
    }

    /// Runs one CLI verb on a pooled socket.
    fn run(&self, device: Option<&str>, argv: &[String]) -> Reply {
        let t = match self.target(device) {
            Ok(t) => t,
            Err(e) => return (false, e),
        };
        let mut sess = match self.checkout(&t) {
            Ok(s) => s,
            Err(e) => return (false, e),
        };
        let r = run_verb(&t.serial, &mut sess, argv);
        if r.keep {
            self.checkin(&t, sess);
        }
        (r.ok, r.text)
    }

    /// Runs an action verb, then (with `observe`) waits for the screen to
    /// settle and appends what changed, or with `brief` only how it settled
    /// (a `batch` step: the batch describes the screen once, at the end).
    fn act(&self, device: Option<&str>, argv: &[String], observe: bool, brief: bool) -> Reply {
        let t = match self.target(device) {
            Ok(t) => t,
            Err(e) => return (false, e),
        };
        let guard = self.action_lock(&t);
        let _one_at_a_time = lock(&guard);
        let mut sess = match self.checkout(&t) {
            Ok(s) => s,
            Err(e) => return (false, e),
        };
        let seq = if observe { event_seq(&mut sess.conn) } else { None };
        let mut r = run_verb(&t.serial, &mut sess, argv);
        if r.ok && observe {
            let budget = settle_budget(argv.first().map(String::as_str).unwrap_or(""));
            match self.observe_change(&t, &mut sess.conn, seq, budget) {
                Ok((note, _)) if brief => r.text.push_str(&format!("\n{note}")),
                Ok((_, change)) => r.text.push_str(&format!("\n{change}")),
                Err(e) => {
                    r.text.push_str(&format!("\n(could not read the screen afterwards: {e})"));
                    r.keep = false;
                }
            }
        }
        if r.keep {
            self.checkin(&t, sess);
        }
        (r.ok, r.text)
    }

    /// Waits for the action that followed `seq` to settle, reads the screen,
    /// and describes it against the last one this server saw: (how it
    /// settled, that plus what changed).
    fn observe_change(&self, t: &Target, conn: &mut Conn, seq: Option<u64>, b: Settle) -> io::Result<(String, String)> {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(b.timeout_ms as u64);
        let settle = |conn: &mut Conn, since: Option<u64>, first_ms: u32| -> io::Result<Vec<u8>> {
            match since {
                Some(n) => conn.call(&format!(
                    "settle since={n} first_ms={first_ms} idle_ms={} timeout_ms={}", b.idle_ms, b.timeout_ms)),
                // An older daemon: a plain quiet period.
                None => conn.call(&format!("wait_for_idle idle_ms={} timeout_ms={}", b.idle_ms, b.timeout_ms)),
            }
        };
        let first = settle(conn, seq, b.first_ms)?;
        let mut reacted = !String::from_utf8_lossy(&first).contains("changed=0");
        let still_moving = first.starts_with(b"ERR:");
        let had_rows = lock(&self.screens).get(t).is_some_and(|s| !s.is_empty());
        let mut dump = self.read_settled(conn, had_rows, deadline)?;
        // A tap that starts another activity can go quiet before the new
        // screen draws (another process starting). If nothing visible changed,
        // give it one more short window to show up.
        if b.second_look_ms > 0 && self.unchanged(t, &dump) {
            let since = event_seq(conn);
            let again = settle(conn, since, b.second_look_ms)?;
            if since.is_some() && String::from_utf8_lossy(&again).contains("changed=1") {
                reacted = true;
                dump = self.read_settled(conn, had_rows, deadline)?;
            }
        }
        let ms = started.elapsed().as_millis();
        let note = if still_moving {
            format!("still changing after {ms} ms")
        } else if reacted {
            format!("settled in {ms} ms")
        } else {
            "no reaction on screen".to_string()
        };
        let mut screens = lock(&self.screens);
        let prev = screens.get(t);
        let screen = Screen::from_dump(&dump, prev);
        let out = screen.describe(&note, screen.change_from(prev));
        screens.insert(t.clone(), screen);
        Ok((note, out))
    }

    /// `watch`: waits, without polling, until the screen differs from the
    /// one this server last showed the agent, or until a row shows `until`.
    /// A dedicated socket carries the daemon's `ui_watch` push stream.
    fn watch(&self, device: Option<&str>, until: Option<&str>, timeout_ms: u64) -> Reply {
        let t = match self.target(device) {
            Ok(t) => t,
            Err(e) => return (false, e),
        };
        // Taken from the pool but never given back: the stream owns it.
        let mut sess = match self.checkout(&t) {
            Ok(s) => s,
            Err(e) => return (false, e),
        };
        let started = Instant::now();
        let deadline = started + Duration::from_millis(timeout_ms);
        if let Err(e) = sess.conn.send_cmd("ui_watch min_ms=100") {
            return (false, format!("ERR:DAEMON_ERROR:{e}"));
        }
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let _ = sess.conn.set_read_timeout(Some(left));
            let frame = match sess.conn.read_frame() {
                Ok(f) => f,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
                Err(e) => return (false, format!("ERR:DAEMON_ERROR:{e}")),
            };
            if frame.starts_with(b"ERR:") {
                return (false, String::from_utf8_lossy(&frame).into_owned());
            }
            let dump = match mobax_core::wire::parse_watch_frame(&frame) {
                Ok((_, _, d)) => d,
                Err(e) => return (false, format!("ERR:DAEMON_ERROR:{e}")),
            };
            let mut screens = lock(&self.screens);
            let prev = screens.get(&t);
            // Skip mid-transition frames (a starting window, rows that all
            // vanished while the next activity draws); the next frame follows.
            let had_rows = prev.is_some_and(|s| !s.is_empty());
            if Screen::is_starting(&dump) || (had_rows && Screen::row_count(&dump) == 0) {
                continue;
            }
            let screen = Screen::from_dump(&dump, prev);
            let change = screen.change_from(prev);
            let hit = match until {
                Some(text) => screen.shows(text),
                None => !matches!(change, Change::Same),
            };
            if hit {
                let note = format!("after {} ms", started.elapsed().as_millis());
                let text = screen.describe(&note, change);
                screens.insert(t, screen);
                return (true, text);
            }
        }
        let ms = started.elapsed().as_millis();
        match until {
            Some(text) => (false, format!(
                "ERR:TIMEOUT:{text:?} did not appear within {ms} ms\nCall `ui` to see where the screen is.")),
            None => (true, format!("no change on screen in {ms} ms")),
        }
    }

    /// The screen now, skipping mid-transition reads: a starting window, or
    /// a screen whose rows have all gone while the next activity draws. A
    /// screen that had no rows to begin with (a game's canvas) is taken as it
    /// is.
    fn read_settled(&self, conn: &mut Conn, had_rows: bool, deadline: Instant) -> io::Result<Value> {
        loop {
            let dump = parse_dump(&conn.call("dump")?)?;
            let transitional = Screen::is_starting(&dump) || (had_rows && Screen::row_count(&dump) == 0);
            if !transitional || Instant::now() >= deadline {
                return Ok(dump);
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// `dump` shows nothing new against the last screen seen on `t`.
    fn unchanged(&self, t: &Target, dump: &Value) -> bool {
        let screens = lock(&self.screens);
        let prev = screens.get(t);
        matches!(Screen::from_dump(dump, prev).change_from(prev), Change::Same)
    }

    /// `ui`: the screen as a table of @N rows.
    fn ui(&self, device: Option<&str>) -> Reply {
        let t = match self.target(device) {
            Ok(t) => t,
            Err(e) => return (false, e),
        };
        if let Err(e) = self.reread(&t) {
            return (false, e);
        }
        let screens = lock(&self.screens);
        let Some(screen) = screens.get(&t) else { return (false, "ERR:INTERNAL:no screen".into()) };
        (true, format!("{} · {} rows\n{}", screen.title(), screen.len(), screen.table().trim_end()))
    }

    /// Reads the screen now and remembers it (refs carried over).
    fn reread(&self, t: &Target) -> Result<(), String> {
        let mut sess = self.checkout(t)?;
        let dump = sess.conn.call("dump").and_then(|b| parse_dump(&b)).map_err(|e| format!("ERR:DAEMON_ERROR:{e}"))?;
        self.checkin(t, sess);
        let mut screens = lock(&self.screens);
        let screen = Screen::from_dump(&dump, screens.get(t));
        screens.insert(t.clone(), screen);
        Ok(())
    }

    /// The remembered screen is no longer `seen` (another window set), so
    /// refs taken from `seen` mean nothing now.
    fn left(&self, t: &Target, seen: Option<&Screen>) -> bool {
        let screens = lock(&self.screens);
        match (screens.get(t), seen) {
            (Some(now), Some(seen)) => matches!(now.change_from(Some(seen)), Change::New),
            _ => true,
        }
    }

    /// The x,y and selector of row `@n`, checked against the screen as it
    /// is now: a ref from a screen that has since gone is refused (with the
    /// new screen), so a late transition can't send a tap to the wrong row.
    fn row(&self, device: Option<&str>, n: u32) -> Result<(i64, i64, String), String> {
        let t = self.target(device)?;
        let mut sess = self.checkout(&t)?;
        let dump = sess.conn.call("dump").and_then(|b| parse_dump(&b)).map_err(|e| format!("ERR:DAEMON_ERROR:{e}"))?;
        self.checkin(&t, sess);
        let mut screens = lock(&self.screens);
        let Some(prev) = screens.get(&t) else {
            return Err(format!("ERR:NOT_FOUND:no @{n} yet\nCall `ui` to number the screen."));
        };
        let had = prev.get(n).is_some();
        let now = Screen::from_dump(&dump, Some(prev));
        let same_screen = matches!(now.change_from(Some(prev)), Change::Same | Change::Rows(_));
        let found = now.get(n).map(|r| (r.x, r.y, r.sel.clone()));
        let reply = format!("{} · {} rows\n{}", now.title(), now.len(), now.table().trim_end());
        screens.insert(t, now);
        match found {
            Some(r) if had && same_screen => Ok(r),
            _ => Err(format!(
                "ERR:PRECONDITION:@{n} is not on the screen any more; nothing was done\nThe screen now · {reply}"
            )),
        }
    }

    fn screenshot(&self, device: Option<&str>, size: Option<i64>, native: bool) -> Result<Vec<u8>, String> {
        let t = self.target(device)?;
        let mut sess = self.checkout(&t)?;
        let wire = if native { "screenshot max=1".to_string() } else { format!("screenshot size={}", size.unwrap_or(768)) };
        let bytes = sess.conn.call(&wire).map_err(|e| format!("ERR:DAEMON_ERROR:{e}"))?;
        self.checkin(&t, sess);
        if bytes.starts_with(b"ERR:") {
            return Err(String::from_utf8_lossy(&bytes).into_owned());
        }
        Ok(bytes)
    }
}

/// How long to wait for an action's effect to show and then calm down.
#[derive(Clone, Copy)]
struct Settle {
    /// For the first accessibility event after the action.
    first_ms: u32,
    /// Quiet that counts as settled.
    idle_ms: u32,
    timeout_ms: u32,
    /// After a settle that changed nothing visible, wait this long for a
    /// late screen (0: don't).
    second_look_ms: u32,
}

/// An app launch takes a few hundred ms to show anything and arrives in
/// bursts; a tap or a key may open another screen; typing and scrolling
/// react at once.
fn settle_budget(verb: &str) -> Settle {
    match verb {
        "open" => Settle { first_ms: 3000, idle_ms: 300, timeout_ms: 6000, second_look_ms: 0 },
        "tap" | "go" => Settle { first_ms: 500, idle_ms: 200, timeout_ms: 3000, second_look_ms: 700 },
        "swipe" => Settle { first_ms: 300, idle_ms: 200, timeout_ms: 2000, second_look_ms: 0 },
        _ => Settle { first_ms: 300, idle_ms: 150, timeout_ms: 1500, second_look_ms: 0 },
    }
}

struct VerbRun {
    ok: bool,
    text: String,
    /// The socket is still in step with the daemon and can go back to the pool.
    keep: bool,
}

/// One CLI verb on `sess` (a socket to `serial`), its output captured.
/// Verbs that open their own connections go to the same device.
fn run_verb(serial: &str, sess: &mut Session, argv: &[String]) -> VerbRun {
    let (res, text) = crate::daemon::with_target(serial, || {
        output::capture(|| crate::dispatch_session_verb(sess, argv))
    });
    let text = text.trim_end().to_string();
    match res {
        Ok(()) => VerbRun { ok: true, text, keep: true },
        Err(e) => match output::err_code_of(&e) {
            // A reported failure: the daemon's reply was read in full.
            Some(code) => VerbRun { ok: false, text: with_hint(text, code), keep: true },
            // Not reported: setup or the socket failed. Only a bad argument
            // is caught before anything is sent, so only then is the socket
            // still in step.
            None => {
                let code = crate::errors::code_of(&e);
                let msg = format!("{text}\nERR:{}:{e}", code.as_short());
                let msg = msg.trim_start().to_string();
                let text = match e.get_ref().and_then(|i| i.downcast_ref::<mobax_core::Error>()) {
                    Some(mobax_core::Error::WhichDevice(_)) => format!("{msg}\nPass `device`, or call `use` with one."),
                    _ => with_hint(msg, code),
                };
                VerbRun { ok: false, text, keep: code == crate::errors::ErrCode::BadArg }
            }
        },
    }
}

/// Adds what an agent should do next after an error.
fn with_hint(text: String, code: crate::errors::ErrCode) -> String {
    use crate::errors::ErrCode::*;
    let hint = match code {
        NotFound => "Not on screen. Call `ui` to see what is there, or `swipe` to scroll.",
        Timeout => "It did not happen in time. Call `ui` to see where the screen is.",
        Ambiguous => "Several nodes match. Use an @N ref from `ui`, or a more specific selector.",
        DeviceGone | DaemonError => "Call `use` to reconnect.",
        SecureWindow => "The app blocks screen capture here; use `ui` instead.",
        BadArg | UnknownCmd => "Check the arguments against the tool's schema.",
        Precondition | Internal => "",
    };
    if hint.is_empty() { text } else { format!("{text}\n{hint}") }
}

/// The daemon's event counter, to settle against after an action.
fn event_seq(conn: &mut Conn) -> Option<u64> {
    let body = conn.call("event_seq").ok()?;
    String::from_utf8_lossy(&body).trim().strip_prefix("seq=")?.parse().ok()
}

fn parse_dump(body: &[u8]) -> io::Result<Value> {
    if body.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(body).into_owned()));
    }
    let text = std::str::from_utf8(body).map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
    json::parse(text).map_err(|e| io::Error::other(format!("dump not json: {e}")))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `@12` → 12.
fn row_ref(s: &str) -> Option<u32> {
    s.trim().strip_prefix('@')?.parse().ok()
}

// ---------- tools ----------

struct Tool {
    name: &'static str,
    description: &'static str,
    /// JSON Schema of the arguments object.
    schema: &'static str,
}

const DEVICE: &str = r#""device":{"type":"string","description":"Device serial (adb devices); omit for the current one"}"#;
const TIMEOUT: &str = r#""timeout_ms":{"type":"integer","description":"Wait budget in ms (default 10000)"}"#;
const OBSERVE: &str = r#""observe":{"type":"boolean","description":"Wait for the screen to settle and reply with what changed (default true)"}"#;

fn tools() -> Vec<Tool> {
    // Schemas are assembled at startup from the shared fragments above.
    fn obj(props: &[&str], required: &[&str]) -> &'static str {
        let req: Vec<String> = required.iter().map(|r| format!("\"{r}\"")).collect();
        Box::leak(
            format!(
                r#"{{"type":"object","properties":{{{}}},"required":[{}],"additionalProperties":false}}"#,
                props.join(","),
                req.join(",")
            )
            .into_boxed_str(),
        )
    }
    vec![
        Tool {
            name: "use",
            description: "Connect to a device (or the only one attached) and start the on-device daemon. Call this first.",
            schema: obj(&[r#""serial":{"type":"string","description":"Device serial from adb devices; omit when only one is attached"}"#], &[]),
        },
        Tool {
            name: "ui",
            description: "List what is on screen as rows numbered @1, @2, …: verb (tap / fill / - for text), class, label, #resource-id, centre x,y and state. The main way to read the screen; act on a row by its @N.",
            schema: obj(&[r#""scope":{"type":"string","enum":["app","active","all"],"description":"app: every app window incl. dialogs (default); active: focused window; all: system windows too"}"#, DEVICE], &[]),
        },
        Tool {
            name: "screenshot",
            description: "Capture the screen as a JPEG image (768 px long edge by default).",
            schema: obj(&[r#""size":{"type":"integer","description":"Long edge in px"}"#, r#""native":{"type":"boolean","description":"Full device resolution"}"#, DEVICE], &[]),
        },
        Tool {
            name: "find",
            description: "Find nodes matching a selector and print them, without acting.",
            schema: obj(&[r#""selector":{"type":"string","description":"Text, #resource_id, or CSS-like selector"}"#, TIMEOUT, DEVICE], &["selector"]),
        },
        Tool {
            name: "tap",
            description: "Tap a row by its @N from `ui`, a node by text / #resource_id / selector, or coordinates x,y. Replies with what changed on screen.",
            schema: obj(&[r#""target":{"type":"string","description":"@N, text, #resource_id, or selector"}"#, r#""x":{"type":"integer"}"#, r#""y":{"type":"integer"}"#, TIMEOUT, OBSERVE, DEVICE], &[]),
        },
        Tool {
            name: "type",
            description: "Type text into the focused field at the cursor, in any language (Korean, emoji, …). Replies with what changed on screen.",
            schema: obj(&[r#""text":{"type":"string"}"#, OBSERVE, DEVICE], &["text"]),
        },
        Tool {
            name: "fill",
            description: "Set the text of an edit field in one step (ACTION_SET_TEXT). The selector is a row's @N, or free text that picks the best-matching EditText. Replies with what changed on screen.",
            schema: obj(&[r#""selector":{"type":"string","description":"@N, #resource_id, selector, or the field's label"}"#, r#""text":{"type":"string"}"#, OBSERVE, DEVICE], &["selector", "text"]),
        },
        Tool {
            name: "swipe",
            description: "Swipe in a direction (the finger's direction: up scrolls content down). Replies with what changed on screen.",
            schema: obj(&[r#""direction":{"type":"string","enum":["up","down","left","right"]}"#, r#""duration_ms":{"type":"integer"}"#, OBSERVE, DEVICE], &["direction"]),
        },
        Tool {
            name: "go",
            description: "Press a navigation / system key: back, home, recents, enter, … Replies with what changed on screen.",
            schema: obj(&[r#""key":{"type":"string","description":"back, home, recents, enter, menu, …"}"#, OBSERVE, DEVICE], &["key"]),
        },
        Tool {
            name: "batch",
            description: "Run several steps in one call when you already know the sequence, e.g. [{\"tool\":\"tap\",\"target\":\"@3\"},{\"tool\":\"fill\",\"selector\":\"Search\",\"text\":\"cats\"},{\"tool\":\"go\",\"key\":\"enter\"},{\"tool\":\"wait\",\"target\":\"Results\"}]. Each step takes that tool's arguments; each action waits for the screen to settle before the next. Stops at the first failure. Replies with one line per step, then what changed on screen over the whole batch. @N refs are from the screen you last saw: once a step opens another screen, name later targets by text or selector.",
            schema: obj(&[
                r#""steps":{"type":"array","maxItems":20,"items":{"type":"object","properties":{"tool":{"type":"string","enum":["tap","type","fill","swipe","go","open","close","wait","find","watch"]}},"required":["tool"]},"description":"Steps in order: {\"tool\": NAME, ...that tool's arguments}"}"#,
                DEVICE,
            ], &["steps"]),
        },
        Tool {
            name: "wait",
            description: "Wait until text / a selector appears on screen.",
            schema: obj(&[r#""target":{"type":"string"}"#, TIMEOUT, DEVICE], &["target"]),
        },
        Tool {
            name: "watch",
            description: "Wait until the screen differs from what you last saw, or until some text shows, then reply with what changed. The device pushes changes, so this costs nothing while waiting. Use it instead of calling `ui` again and again (a download finishing, a reply arriving, an animation ending).",
            schema: obj(&[
                r#""until":{"type":"string","description":"Wait for a row showing this text (case-insensitive) instead of any change"}"#,
                r#""timeout_ms":{"type":"integer","description":"How long to wait (default 10000)"}"#,
                DEVICE,
            ], &[]),
        },
        Tool {
            name: "open",
            description: "Launch an app by package name (com.android.settings), PKG/.Activity, or app label. Replies with the screen it opens.",
            schema: obj(&[r#""app":{"type":"string"}"#, OBSERVE, DEVICE], &["app"]),
        },
        Tool {
            name: "close",
            description: "Force-stop an app by package name.",
            schema: obj(&[r#""package":{"type":"string"}"#, DEVICE], &["package"]),
        },
        Tool {
            name: "logcat",
            description: "Recent device log lines (logcat), newest last. Filter by app or pid, priority, tags, a pattern or time; drop noise with exclude; use crash=true for crash stacks.",
            schema: obj(&[
                r#""package":{"type":"string","description":"Only this app's running process"}"#,
                r#""pid":{"type":"integer","description":"Only this process id"}"#,
                r#""level":{"type":"string","enum":["V","D","I","W","E","F"],"description":"Minimum priority"}"#,
                r#""tags":{"type":"array","items":{"type":"string"},"description":"Only these tags (TAG or TAG:LEVEL)"}"#,
                r#""grep":{"type":"array","items":{"type":"string"},"description":"Regexes (ECMAScript); a line must match one of them"}"#,
                r#""exclude":{"type":"array","items":{"type":"string"},"description":"Drop lines containing any of these texts"}"#,
                r#""ignore_case":{"type":"boolean","description":"Case-insensitive grep and exclude"}"#,
                r#""since":{"type":"string","description":"Only lines from the last N seconds/minutes/hours: 90, 30s, 5m, 2h"}"#,
                r#""format":{"type":"string","description":"logcat -v output format: brief, threadtime (default), time, tag, raw, long, …"}"#,
                r#""lines":{"type":"integer","description":"How many lines (default 100)"}"#,
                r#""crash":{"type":"boolean","description":"Read the crash buffer"}"#,
                r#""clear":{"type":"boolean","description":"Clear the log buffers instead of reading"}"#,
                DEVICE,
            ], &[]),
        },
        Tool {
            name: "info",
            description: "Device and daemon info: model, Android version, display size, foreground app.",
            schema: obj(&[DEVICE], &[]),
        },
        Tool {
            name: "drop",
            description: "Stop the on-device daemon.",
            schema: obj(&[DEVICE], &[]),
        },
        Tool {
            name: "mobax",
            description: "Run any other mobax CLI verb, e.g. [\"apps\",\"--3rd\"], [\"prop\",\"ro.build.version.sdk\"], [\"logs\",\"--tail\",\"50\"]. See `mobax --help`.",
            schema: obj(&[r#""args":{"type":"array","items":{"type":"string"},"description":"Verb and arguments, without the leading mobax"}"#, DEVICE], &["args"]),
        },
    ]
}

fn tools_list() -> String {
    let mut out = String::from(r#"{"tools":["#);
    for (i, t) in tools().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let (mut name, mut desc) = (String::new(), String::new());
        encode_str(&mut name, t.name);
        encode_str(&mut desc, t.description);
        out.push_str(&format!(r#"{{"name":{name},"description":{desc},"inputSchema":{}}}"#, t.schema));
    }
    out.push_str("]}");
    out
}

fn tools_call(params: Option<&Value>, engine: &Engine) -> Result<String, (i32, String)> {
    let params = params.ok_or((-32602, "tools/call needs params".to_string()))?;
    let name = json::obj_get(params, "name").and_then(json::as_str).unwrap_or("");
    let empty = Value::Obj(Vec::new());
    let args = json::obj_get(params, "arguments").unwrap_or(&empty);
    if name == "batch" {
        return Ok(reply(batch(args, engine)));
    }
    Ok(match call_tool(name, args, engine, None, false)? {
        Out::Text(r) => reply(r),
        Out::Json(result) => result,
    })
}

/// A tool's result: text, or a ready `tools/call` result (an image).
enum Out {
    Text(Reply),
    Json(String),
}

/// Runs tool `name`. `device` is the batch's device, for a step that names
/// none; `brief` keeps an action's reply to how the screen settled.
fn call_tool(name: &str, args: &Value, engine: &Engine, device: Option<&str>, brief: bool) -> Result<Out, (i32, String)> {
    let s = |k: &str| json::obj_get(args, k).and_then(json::as_str).map(str::to_string);
    let n = |k: &str| json::obj_get(args, k).and_then(json::as_num);
    let b = |k: &str| matches!(json::obj_get(args, k), Some(Value::Bool(true)));
    let need = |k: &str| s(k).ok_or_else(|| (-32602, format!("{name}: missing \"{k}\"")));
    // Actions report what changed unless the caller opts out.
    let observe = !matches!(json::obj_get(args, "observe"), Some(Value::Bool(false)));
    let device = s("device").or_else(|| device.map(str::to_string));
    let dev = device.as_deref();

    let mut argv: Vec<String> = Vec::new();
    let timeout = |argv: &mut Vec<String>| {
        if let Some(t) = n("timeout_ms") {
            argv.extend(["--timeout".to_string(), t.to_string()]);
        }
    };
    // How the verb runs: child process, plain in-process, or as an action.
    enum Via { Child, Run, Act }
    let via = match name {
        "use" => {
            argv.push("use".into());
            argv.extend(s("serial"));
            Via::Child
        }
        "drop" => {
            argv.push("drop".into());
            Via::Child
        }
        "ui" => match s("scope").as_deref() {
            Some("active") => {
                argv.extend(["ui".into(), "--active".into()]);
                Via::Run
            }
            Some("all") => {
                argv.extend(["ui".into(), "--all".into()]);
                Via::Run
            }
            _ if engine.in_process() => return Ok(Out::Text(engine.ui(dev))),
            _ => {
                argv.push("ui".into());
                Via::Run
            }
        },
        "screenshot" => {
            if !engine.in_process() {
                return screenshot_child(n("size"), b("native"), device, &engine.global).map(Out::Json);
            }
            return Ok(Out::Json(match engine.screenshot(dev, n("size"), b("native")) {
                Ok(jpeg) => format!(
                    r#"{{"content":[{{"type":"image","mimeType":"image/jpeg","data":"{}"}}],"isError":false}}"#,
                    base64(&jpeg)
                ),
                Err(e) => text_result(&e, true),
            }));
        }
        "find" => {
            argv.extend(["find".into(), need("selector")?]);
            timeout(&mut argv);
            Via::Run
        }
        "tap" => {
            argv.push("tap".into());
            match (s("target"), n("x"), n("y")) {
                (Some(t), _, _) => match row_ref(&t) {
                    Some(r) => match engine.row(dev, r) {
                        Ok((x, y, _)) => argv.extend([x.to_string(), y.to_string()]),
                        Err(e) => return Ok(Out::Text((false, e))),
                    },
                    None => argv.push(t),
                },
                (None, Some(x), Some(y)) => argv.extend([x.to_string(), y.to_string()]),
                _ => return Err((-32602, "tap: give \"target\" or both \"x\" and \"y\"".into())),
            }
            timeout(&mut argv);
            Via::Act
        }
        "type" => {
            argv.extend(["type".into(), need("text")?]);
            Via::Act
        }
        "fill" => {
            let sel = need("selector")?;
            let sel = match row_ref(&sel) {
                Some(r) => match engine.row(dev, r) {
                    Ok((_, _, daemon_sel)) => daemon_sel,
                    Err(e) => return Ok(Out::Text((false, e))),
                },
                None => sel,
            };
            argv.extend(["fill".into(), sel, need("text")?]);
            Via::Act
        }
        "swipe" => {
            argv.extend(["swipe".into(), need("direction")?]);
            argv.extend(n("duration_ms").map(|d| d.to_string()));
            Via::Act
        }
        "go" => {
            argv.extend(["go".into(), need("key")?]);
            Via::Act
        }
        "wait" => {
            argv.extend(["wait".into(), need("target")?]);
            timeout(&mut argv);
            Via::Run
        }
        "watch" => {
            if !engine.in_process() {
                return Ok(Out::Text((false, "ERR:PRECONDITION:watch needs the daemon reachable through the local adb server".into())));
            }
            let timeout = n("timeout_ms").filter(|t| *t > 0).unwrap_or(10_000) as u64;
            return Ok(Out::Text(engine.watch(dev, s("until").as_deref(), timeout)));
        }
        "open" => {
            argv.extend(["open".into(), need("app")?]);
            Via::Act
        }
        "close" => {
            argv.extend(["close".into(), need("package")?]);
            Via::Run
        }
        "logcat" => {
            argv.push("logs".into());
            if let Some(p) = s("package") {
                argv.extend(["--pkg".into(), p]);
            }
            if let Some(l) = s("level") {
                argv.extend(["--level".into(), l]);
            }
            if let Some(p) = n("pid") {
                argv.extend(["--pid".into(), p.to_string()]);
            }
            for (key, flag) in [("tags", "--tag"), ("grep", "--grep"), ("exclude", "--exclude")] {
                // A lone string is accepted too.
                let v = json::obj_get(args, key);
                let items = v.and_then(json::as_arr).map(|a| a.iter().filter_map(json::as_str).collect())
                    .or_else(|| v.and_then(json::as_str).map(|s| vec![s]))
                    .unwrap_or_default();
                for item in items {
                    argv.extend([flag.into(), item.to_string()]);
                }
            }
            if b("ignore_case") {
                argv.push("--ignore-case".into());
            }
            if let Some(v) = s("since") {
                argv.extend(["--since".into(), v]);
            }
            if let Some(v) = s("format") {
                argv.extend(["--format".into(), v]);
            }
            if let Some(n) = n("lines") {
                argv.extend(["--tail".into(), n.to_string()]);
            }
            if b("crash") {
                argv.push("--crash".into());
            }
            if b("clear") {
                argv.push("--clear".into());
            }
            Via::Run
        }
        "info" => {
            argv.push("info".into());
            Via::Run
        }
        "mobax" => {
            let list = json::obj_get(args, "args").and_then(json::as_arr).unwrap_or(&[]);
            argv = list.iter().filter_map(json::as_str).map(str::to_string).collect();
            let verb = argv.first().map(String::as_str).unwrap_or("");
            if verb.is_empty() {
                return Err((-32602, "mobax: \"args\" needs at least a verb".into()));
            }
            if BLOCKED_VERBS.contains(&verb) || (verb == "see" && argv.len() == 1) {
                return Ok(Out::Text((false, format!("`mobax {verb}` is interactive or opens a window; it isn't available over MCP"))));
            }
            Via::Child
        }
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };
    let (ok, text) = match via {
        Via::Run | Via::Act if !engine.in_process() => child(engine, device, &argv),
        Via::Run => engine.run(dev, &argv),
        Via::Act => engine.act(dev, &argv, observe, brief),
        Via::Child => {
            let r = child(engine, device, &argv);
            if matches!(name, "use" | "drop") {
                engine.forget();
            }
            r
        }
    };
    Ok(Out::Text((ok, text)))
}

/// Tools a `batch` step may be: the ones that act or wait on the screen.
const BATCH_TOOLS: &[&str] = &["tap", "type", "fill", "swipe", "go", "open", "close", "wait", "find", "watch"];

/// Most steps in one `batch`.
const BATCH_MAX: usize = 20;

/// `batch`: several steps in one call, so an agent spends one round trip on
/// a sequence it already knows. Each action still waits for the screen to
/// settle (the next step needs a still screen), but replies only with how
/// it settled; the reply ends with what changed over the whole batch. Stops
/// at the first failing step.
fn batch(args: &Value, engine: &Engine) -> Reply {
    let steps = json::obj_get(args, "steps").and_then(json::as_arr).unwrap_or(&[]);
    if steps.is_empty() || steps.len() > BATCH_MAX {
        return (false, format!("ERR:BAD_ARG:batch needs 1 to {BATCH_MAX} steps"));
    }
    if !engine.in_process() {
        return (false, "ERR:PRECONDITION:batch needs the daemon reachable through the local adb server".into());
    }
    let device = json::obj_get(args, "device").and_then(json::as_str);
    let t = match engine.target(device) {
        Ok(t) => t,
        Err(e) => return (false, e),
    };
    // @N refs name rows of the screen the agent last saw.
    let seen = lock(&engine.screens).get(&t).cloned();
    let mut out = String::new();
    let mut ok = true;
    for (i, step) in steps.iter().enumerate() {
        let name = json::obj_get(step, "tool").and_then(json::as_str).unwrap_or("");
        let text = if !BATCH_TOOLS.contains(&name) {
            Err(format!("ERR:BAD_ARG:\"{name}\" can't be a batch step; use one of {}", BATCH_TOOLS.join(", ")))
        } else if i > 0 && uses_ref(step) && engine.left(&t, seen.as_ref()) {
            Err("ERR:PRECONDITION:an earlier step opened another screen, so its @N refs are gone; nothing was done\n\
                 Name the target by text or selector, or call `ui`."
                .to_string())
        } else {
            match call_tool(name, step, engine, Some(&t.serial), true) {
                Ok(Out::Text((true, text))) => Ok(text),
                Ok(Out::Text((false, text))) => Err(text),
                Ok(Out::Json(_)) => Err(format!("ERR:BAD_ARG:\"{name}\" can't be a batch step")),
                Err((_, e)) => Err(format!("ERR:BAD_ARG:{e}")),
            }
        };
        let (mark, text) = match text {
            Ok(t) => ("ok", t),
            Err(t) => ("FAILED", t),
        };
        let text = text.trim();
        out.push_str(&format!("{}. {name} {mark}{}{}\n", i + 1, if text.is_empty() { "" } else { ": " }, text.replace('\n', " · ")));
        if mark == "FAILED" {
            ok = false;
            break;
        }
    }
    // The screen now, against the one the agent saw before the batch.
    match engine.reread(&t) {
        Ok(()) => {
            let screens = lock(&engine.screens);
            if let Some(now) = screens.get(&t) {
                out.push_str(&now.describe("after the batch", now.change_from(seen.as_ref())));
            }
        }
        Err(e) => out.push_str(&format!("(could not read the screen afterwards: {e})")),
    }
    (ok, out)
}

/// The step names a row by @N.
fn uses_ref(step: &Value) -> bool {
    ["target", "selector"]
        .iter()
        .any(|k| json::obj_get(step, k).and_then(json::as_str).and_then(row_ref).is_some())
}

fn reply((ok, text): Reply) -> String {
    text_result(&text, !ok)
}

/// Runs `mobax ARGV…` as a child process.
fn child(engine: &Engine, device: Option<String>, argv: &[String]) -> Reply {
    let out = run_hs(&engine.global, device, argv);
    (out.ok, out.text)
}

fn screenshot_child(size: Option<i64>, native: bool, device: Option<String>, global: &[String]) -> Result<String, (i32, String)> {

    let path = std::env::temp_dir().join(format!("mobax-mcp-{}-{}.jpg", std::process::id(), now_ns()));
    let mut argv = vec!["see".to_string(), path.to_string_lossy().into_owned()];
    if native {
        argv.push("--native".into());
    } else if let Some(n) = size {
        argv.extend(["--size".into(), n.to_string()]);
    }
    let out = run_hs(global, device, &argv);
    let bytes = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    match bytes {
        Ok(b) if out.ok && !b.is_empty() => Ok(format!(
            r#"{{"content":[{{"type":"image","mimeType":"image/jpeg","data":"{}"}}],"isError":false}}"#,
            base64(&b)
        )),
        _ => Ok(text_result(&out.text, true)),
    }
}

fn text_result(text: &str, is_error: bool) -> String {
    let mut t = String::new();
    encode_str(&mut t, if text.is_empty() { "ok" } else { text });
    format!(r#"{{"content":[{{"type":"text","text":{t}}}],"isError":{is_error}}}"#)
}

struct Output {
    ok: bool,
    text: String,
}

/// Runs `mobax [GLOBAL] [-s DEVICE] ARGS…` with no stdin; stdout and stderr
/// are merged into the tool result, and a non-zero exit marks it an error.
///
/// Output goes to a temp file, not a pipe: `mobax use` leaves long-lived
/// processes behind (the state-cache helper, an adb server) that inherit
/// the child's handles, so a pipe would never reach EOF.
fn run_hs(global: &[String], device: Option<String>, argv: &[String]) -> Output {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return Output { ok: false, text: format!("cannot locate mobax: {e}") },
    };
    let log = std::env::temp_dir().join(format!("mobax-mcp-{}-{}.out", std::process::id(), now_ns()));
    let file = match std::fs::File::create(&log) {
        Ok(f) => f,
        Err(e) => return Output { ok: false, text: format!("cannot create {}: {e}", log.display()) },
    };
    let err_file = match file.try_clone() {
        Ok(f) => f,
        Err(e) => return Output { ok: false, text: format!("cannot duplicate output handle: {e}") },
    };
    let mut cmd = Command::new(exe);
    cmd.args(global);
    if let Some(d) = device {
        cmd.args(["-s", &d]);
    }
    cmd.args(argv)
        // Plain text is the agent-oriented format; ignore a JSON default.
        .env_remove("HS_FORMAT")
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err_file));
    let status = match cmd.spawn() {
        Ok(mut child) => {
            let deadline = Instant::now() + CALL_TIMEOUT;
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => break Some(s),
                    Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                }
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(&log);
            return Output { ok: false, text: format!("cannot run mobax: {e}") };
        }
    };
    let mut text = std::fs::read(&log).map(|b| String::from_utf8_lossy(&b).trim_end().to_string()).unwrap_or_default();
    // A parse error prints the whole usage after the message; keep the message.
    if let Some(i) = text.find(USAGE_HEAD) {
        text.truncate(i);
        text = text.trim_end().to_string();
    }
    // A helper left running may still hold the file on Windows; the temp
    // dir is cleaned eventually either way.
    let _ = std::fs::remove_file(&log);
    match status {
        Some(s) => Output { ok: s.success(), text },
        None => Output { ok: false, text: format!("timed out after {}s
{text}", CALL_TIMEOUT.as_secs()) },
    }
}

// ---------- helpers ----------

/// Re-serializes a parsed JSON value (used to echo request ids verbatim).
fn to_json(v: &Value) -> String {
    let mut out = String::new();
    write_json(&mut out, v);
    out
}

fn write_json(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => out.push_str(&n.to_string()),
        Value::Float(f) => out.push_str(&f.to_string()),
        Value::Str(s) => encode_str(out, s),
        Value::Arr(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, it);
            }
            out.push(']');
        }
        Value::Obj(pairs) => {
            out.push('{');
            for (i, (k, val)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_str(out, k);
                out.push(':');
                write_json(out, val);
            }
            out.push('}');
        }
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn initialize_echoes_known_version() {
        let r = handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains(r#""protocolVersion":"2025-03-26""#), "{r}");
        let r = handle(r#"{"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains(r#""id":"a""#) && r.contains(PROTOCOL_VERSIONS[0]), "{r}");
    }

    #[test]
    fn notifications_get_no_reply() {
        assert!(handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &Engine::new(&[])).is_none());
    }

    #[test]
    fn tools_list_is_valid_json() {
        let r = handle(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &Engine::new(&[])).unwrap();
        let v = json::parse(&r).expect("valid JSON");
        let tools = json::obj_get(json::obj_get(&v, "result").unwrap(), "tools").and_then(json::as_arr).unwrap();
        assert_eq!(tools.len(), super::tools().len());
    }

    /// The .mcpb manifest lists the tools for hosts to show before the
    /// server runs; keep it in step with `tools()`.
    #[test]
    fn mcpb_manifest_lists_every_tool() {
        let manifest = json::parse(include_str!("../../mcpb/manifest.json")).expect("manifest is JSON");
        let listed: Vec<&str> = json::obj_get(&manifest, "tools")
            .and_then(json::as_arr)
            .unwrap()
            .iter()
            .filter_map(|t| json::obj_get(t, "name").and_then(json::as_str))
            .collect();
        let served: Vec<&str> = super::tools().iter().map(|t| t.name).collect();
        assert_eq!(listed, served);
    }

    #[test]
    fn unknown_method_and_bad_args() {
        let r = handle(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains("-32601"), "{r}");
        let r = handle(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"tap","arguments":{}}}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains("-32602"), "{r}");
        let r = handle(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"mobax","arguments":{"args":["tui"]}}}"#, &Engine::new(&[])).unwrap();
        assert!(r.contains(r#""isError":true"#), "{r}");
    }

    #[test]
    fn batch_checks_its_steps_before_touching_a_device() {
        let call = |args: &str| {
            handle(&format!(r#"{{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{{"name":"batch","arguments":{args}}}}}"#), &Engine::new(&[])).unwrap()
        };
        let r = call(r#"{"steps":[]}"#);
        assert!(r.contains("BAD_ARG") && r.contains(r#""isError":true"#), "{r}");
        let many = vec![r#"{"tool":"go","key":"back"}"#; super::BATCH_MAX + 1].join(",");
        assert!(call(&format!(r#"{{"steps":[{many}]}}"#)).contains("BAD_ARG"));
    }

    #[test]
    fn a_step_names_a_row_by_ref() {
        let step = |s: &str| json::parse(s).unwrap();
        assert!(super::uses_ref(&step(r#"{"tool":"tap","target":"@3"}"#)));
        assert!(super::uses_ref(&step(r#"{"tool":"fill","selector":"@12","text":"x"}"#)));
        assert!(!super::uses_ref(&step(r#"{"tool":"tap","target":"OK"}"#)));
        assert!(!super::uses_ref(&step(r#"{"tool":"type","text":"@3"}"#)));
    }
}
