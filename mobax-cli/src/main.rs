// mobax CLI.
//
// Talks to the on-device daemon through the adb server (no host port) with
// a length-prefixed protocol: uint32 big-endian length + payload, both ways.
// One-shot calls open a fresh socket each invocation; `bench` and `mcp`
// keep theirs warm.

use std::io::{self, Write};
use std::process::ExitCode;

mod cli;
mod cmd;
mod device;
mod mcp;

// Modules keep their old `crate::name` paths.
use cli::{args, flags, output, term, usage};
use cmd::{
    act, bench, fan, fill, info, init, inspect, mirror, provider, run_script, screen, shell, snapshot, tap_text,
    tui, watch,
};
use device::{adb, daemon, session, state_cache};

// Shared with the TUI (and any Rust caller): see mobax-core.
pub(crate) use mobax_core::{errors, json, json_out, selector, ui_dump, xml_dump};
pub(crate) use mobax_core::screen as mcp_screen;
pub(crate) use mobax_core::selector::node_to_daemon_selector;
pub(crate) use mobax_core::device::is_local;

use bench::*;
use args::*;
use fill::*;
use info::*;
use inspect::*;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = mobax_core::adb_server::DEFAULT_PORT;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `mobax mcp [GLOBAL OPTIONS]`: stdio MCP server; the options ride along on
    // every tool call.
    if args.first().map(String::as_str) == Some("mcp") {
        return match mcp::serve(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                crate::errln!("mobax mcp: {e}");
                ExitCode::from(1)
            }
        };
    }
    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            crate::errln!("error: {e}\n");
            crate::errln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match run(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // If the verb funnelled its failure through `output::Reporter`,
            // the structured ErrCode rides in the error payload and was
            // already printed. Otherwise print it here; its code comes from
            // the typed error inside, or the I/O kind.
            if let Some(code) = output::err_code_of(&e) {
                ExitCode::from(code.exit_code())
            } else {
                crate::errln!("mobax: {e}");
                ExitCode::from(errors::code_of(&e).exit_code())
            }
        }
    }
}

use usage::USAGE;

/// `--device SERIAL` → host-side forwarded port for that device. Errors if
/// the device isn't attached or the daemon hasn't been brought up with
/// The host-side state mirror for the device this call talks to.
pub(crate) fn cached_state() -> Option<Vec<u8>> {
    state_cache::read_cached(&daemon::target_serial().ok()?)
}

fn run(opts: &Opts) -> io::Result<()> {
    match &opts.cmd {
        Cmd::Bench { n, json } => {
            bench(&opts.host, opts.port, *n, *json || opts.out_fmt == flags::OutFmt::Json)
        }
        Cmd::TapText { query, flags } => {
            let reporter = output::Reporter::new(flags.out(opts.out_fmt));
            let mut sess = session::Session::connect(
                &opts.host, opts.port, flags.clone(), opts.out_fmt)?;
            // One-shot: zero TTL so the dump never lingers between runs.
            sess.dump_ttl_ms = 0;
            tap_text::run_session(&mut sess, query, &reporter, "tap")
        }
        Cmd::TapXY { x, y, flags } => {
            let reporter = output::Reporter::new(flags.out(opts.out_fmt));
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call(&format!("tap x={x} y={y}"))?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("tap", e));
            }
            reporter.ok("tap", &String::from_utf8_lossy(&body),
                json_out::Obj::new().n("x", *x as i64).n("y", *y as i64))
        }
        Cmd::Snapshot => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            snapshot::run(&mut conn)
        }
        Cmd::Watch { json, min_ms, moves } => watch::run(&opts.host, opts.port, *json, *min_ms, *moves),
        Cmd::Screen => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            screen::run(&mut conn)
        }
        Cmd::Adb(sub) => adb::run(&opts.host, opts.port, sub),
        Cmd::Connect(o) => {
            let (serial, _) = daemon::connect(o)?;
            crate::errln!("daemon up on {serial}");
            Ok(())
        }
        Cmd::Disconnect(o) => daemon::disconnect(o),
        Cmd::Devices => print_devices(),
        Cmd::StateDaemon => state_cache::run_daemon(&daemon::target_serial()?, opts.port),
        Cmd::Shell => shell::run(&opts.host, opts.port),
        Cmd::Find { selector, flags } => {
            run_find(&mut Conn::connect(&opts.host, opts.port)?, opts.out_fmt, selector, flags)
        }
        Cmd::See(see) => run_see(&opts.host, opts.port, see),
        Cmd::Wait { spec, flags } => {
            run_wait(&mut Conn::connect(&opts.host, opts.port)?, opts.out_fmt, spec, flags)
        }
        Cmd::Run { script, flags } => {
            run_script::run(&opts.host, opts.port, opts.out_fmt, script.as_deref(), flags.clone())
        }
        Cmd::Act(a) => act::run(&opts.host, opts.port, opts.out_fmt, a),
        Cmd::Fan { serials, argv } => fan::run(opts.out_fmt, serials, argv),
        Cmd::Init { path } => init::run(path.as_deref()),
        Cmd::Cp { src, dst } => run_cp(&opts.host, opts.port, src, dst),
        Cmd::ShowPkg(pkg) => run_show_pkg(&opts.host, opts.port, pkg),
        Cmd::Info => run_info(&opts.host, opts.port),
        Cmd::Ui { format, windows } => run_ui(&opts.host, opts.port, *format, *windows),
        Cmd::Tui { active } => tui::run(&opts.host, opts.port, *active),
        Cmd::SettingsListAll => {
            // Three Shell-passthrough calls glued client-side with header
            // lines — keeps the daemon's shell command simple (no quote
            // parsing needed) and gives a single readable dump.
            let mut out = crate::output::stdout();
            for ns in ["system", "secure", "global"] {
                writeln!(out, "=== {ns} ===")?;
                out.flush()?;
                adb::run(&opts.host, opts.port, &adb::Cmd::Shell(vec![
                    "/system/bin/settings".into(), "list".into(), ns.into(),
                ]))?;
            }
            Ok(())
        }
        Cmd::Links(pkg) => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call(&format!("deeplinks {pkg}"))?;
            if body.starts_with(b"ERR:") {
                return Err(io::Error::other(String::from_utf8_lossy(&body).into_owned()));
            }
            let mut out = crate::output::stdout();
            out.write_all(&body)?;
            if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
            out.flush()
        }
        Cmd::Sms { kind, limit, json } => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire = format!("sms type={kind} limit={limit}");
            provider::run(&mut conn, &wire, *json || opts.out_fmt == flags::OutFmt::Json,
                &[provider::TypeMap { column: "type", map: SMS_TYPES }],
                &[])
        }
        Cmd::Calls { kind, limit, json } => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire = format!("calls type={kind} limit={limit}");
            provider::run(&mut conn, &wire, *json || opts.out_fmt == flags::OutFmt::Json,
                &[provider::TypeMap { column: "type", map: CALL_TYPES }],
                &[])
        }
        Cmd::Contacts { limit, json } => {
            // Daemon emits raw Phone columns (data1/data2/data3) since
            // ContactsContract rejects SQL AS aliases. Rename them to
            // friendly labels for display + JSON.
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire = format!("contacts limit={limit}");
            provider::run(&mut conn, &wire, *json || opts.out_fmt == flags::OutFmt::Json,
                &[provider::TypeMap { column: "type", map: PHONE_TYPES }],
                &[("data1", "number"), ("data2", "type"), ("data3", "label")])
        }
        Cmd::Clip { text, watch, interval_ms } => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            if *watch {
                let wire = format!("clip_watch interval_ms={interval_ms}");
                conn.send_cmd(&wire)?;
                let mut out = crate::output::stdout();
                loop {
                    let frame = conn.read_frame()?;
                    if frame.is_empty() { break; }
                    if frame.starts_with(b"ERR:") {
                        return Err(io::Error::other(
                            String::from_utf8_lossy(&frame).into_owned()));
                    }
                    out.write_all(&frame)?;
                    out.write_all(b"\n")?;
                    out.flush()?;
                }
                Ok(())
            } else if let Some(t) = text {
                let body = conn.call(&format!("clip_set {t}"))?;
                if body.starts_with(b"ERR:") {
                    return Err(io::Error::other(
                        String::from_utf8_lossy(&body).into_owned()));
                }
                Ok(())
            } else {
                let body = conn.call("clip_get")?;
                if body.starts_with(b"ERR:") {
                    return Err(io::Error::other(
                        String::from_utf8_lossy(&body).into_owned()));
                }
                let mut out = crate::output::stdout();
                out.write_all(&body)?;
                if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
                out.flush()
            }
        }
        Cmd::Notif { pkg, history, limit, json } => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let mut wire = format!("notifications limit={limit}");
            if *history { wire.push_str(" history=1"); }
            if let Some(p) = pkg { wire.push_str(&format!(" pkg={p}")); }
            provider::run(&mut conn, &wire, *json || opts.out_fmt == flags::OutFmt::Json, &[], &[])
        }
        Cmd::Location { json } => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            provider::run(&mut conn, "location", *json || opts.out_fmt == flags::OutFmt::Json, &[], &[])
        }
        Cmd::Calendar { from_ms, to_ms, days, limit, json } => {
            // Resolve window. --days N takes precedence over --from/--to if
            // both are given; default = now → now + 7 days.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let (from, to) = if let Some(d) = days {
                (now, now + d * 24 * 60 * 60 * 1000)
            } else {
                (from_ms.unwrap_or(now),
                 to_ms.unwrap_or(now + 7 * 24 * 60 * 60 * 1000))
            };
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let wire = format!("calendar from={from} to={to} limit={limit}");
            provider::run(&mut conn, &wire, *json || opts.out_fmt == flags::OutFmt::Json, &[], &[])
        }
        Cmd::Submit { sel, flags } => run_submit(opts, sel.as_deref(), flags),
        Cmd::Paste  { sel, flags } => run_paste (opts, sel.as_deref(), flags),
        Cmd::TypeFocused { text, flags } => run_type_focused(opts, text, flags),
        Cmd::TypeInto { selector, text, flags } => run_type_into(opts, selector, text, flags),
        Cmd::Ping => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call("ping")?;
            write_response(&opts.cmd, &body)
        }
        Cmd::Quit => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call("quit")?;
            write_response(&opts.cmd, &body)
        }
        Cmd::Input(wire) => {
            let mut conn = Conn::connect(&opts.host, opts.port)?;
            let body = conn.call(wire)?;
            write_response(&opts.cmd, &body)
        }
    }
}

/// Dispatch a verb line parsed inside `mobax run`. Re-uses the one-shot verb
/// parser, then routes the cmd through a session-aware handler when one
/// exists (TapText, Find, Wait, …) or the standard one-shot path otherwise.
pub(crate) fn dispatch_session_verb(
    sess: &mut session::Session,
    argv: &[String],
) -> io::Result<()> {
    let opts = match parse_args(argv) {
        Ok(o) => o,
        Err(e) => return Err(io::Error::other(e)),
    };
    // Pull defaults from the session unless the verb line set its own.
    match opts.cmd {
        Cmd::TapText { query, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            sess.defaults = flags.clone();
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            tap_text::run_session(sess, &query, &reporter, "tap")
        }
        Cmd::TapXY { x, y, flags } => {
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            let body = sess.conn.call(&format!("tap x={x} y={y}"))?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("tap", e));
            }
            reporter.ok("tap", String::from_utf8_lossy(&body).trim_end(),
                json_out::Obj::new().n("x", x as i64).n("y", y as i64))
        }
        Cmd::Wait { spec, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            run_wait(&mut sess.conn, sess.default_out, &spec, &flags)
        }
        Cmd::Find { selector: sel, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            run_find(&mut sess.conn, sess.default_out, &sel, &flags)
        }
        Cmd::Submit { sel, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            let wire = match &sel {
                Some(s) => format!("submit {s}"),
                None => "submit".into(),
            };
            let body = sess.conn.call(&wire)?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("submit", e));
            }
            reporter.ok("submit", String::from_utf8_lossy(&body).trim_end(),
                json_out::Obj::new().opt_s("selector", sel.as_deref()))
        }
        Cmd::Paste { sel, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            let wire = match &sel {
                Some(s) => format!("paste {s}"),
                None => "paste".into(),
            };
            let body = sess.conn.call(&wire)?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("paste", e));
            }
            reporter.ok("paste", String::from_utf8_lossy(&body).trim_end(),
                json_out::Obj::new().opt_s("selector", sel.as_deref()))
        }
        Cmd::TypeFocused { text, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            let body = sess.conn.call(&format!("text {text}"))?;
            if let Some(e) = errors::parse_err(&body) {
                return Err(reporter.fail("type", e));
            }
            reporter.ok("type", String::from_utf8_lossy(&body).trim_end(),
                json_out::Obj::new().s("text", &text))
        }
        Cmd::TypeInto { selector, text, mut flags } => {
            merge_session_defaults(&mut flags, sess);
            let reporter = output::Reporter::new(flags.out(sess.default_out));
            fill_into(&mut sess.conn, &reporter, &selector, &text, &flags)
        }
        Cmd::Input(wire) => {
            let body = sess.conn.call(&wire)?;
            if let Some(e) = errors::parse_err(&body) {
                let reporter = output::Reporter::new(sess.default_out);
                return Err(reporter.fail("do", e));
            }
            let mut out = crate::output::stdout();
            out.write_all(&body)?;
            if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
            out.flush()
        }
        other => {
            // For verbs that aren't session-aware yet, build a fresh Opts
            // and reuse the one-shot dispatch. We deliberately lose the
            // warm-socket benefit here — but it's still strictly better
            // than the user spawning a child `mobax` process per line.
            let synthetic = Opts {
                host: sess.peer_host(),
                port: sess.peer_port(),
                out_fmt: sess.default_out,
                cmd: other,
            };
            run(&synthetic)
        }
    }
}

/// Apply session-level defaults to a verb-line flags struct: only fields
/// the verb didn't set itself get inherited. Lets `set timeout=8s` at the
/// top of a script flow into every subsequent `tap`/`wait`/etc.
fn merge_session_defaults(f: &mut flags::ActionFlags, sess: &session::Session) {
    if f.timeout_ms.is_none() { f.timeout_ms = sess.defaults.timeout_ms; }
    if f.retries == 0           { f.retries = sess.defaults.retries; }
    if f.retry_delay_ms == 200  { f.retry_delay_ms = sess.defaults.retry_delay_ms; }
    if !f.require_visible       { f.require_visible   = sess.defaults.require_visible; }
    if !f.require_clickable     { f.require_clickable = sess.defaults.require_clickable; }
    if !f.require_enabled       { f.require_enabled   = sess.defaults.require_enabled; }
    if !f.active_window         { f.active_window     = sess.defaults.active_window; }
    if f.out_fmt.is_none()      { f.out_fmt = sess.defaults.out_fmt; }
}

/// Parse "Nms", "Ns" (where N is unsigned int).
fn parse_duration_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("ms") {
        n.trim().parse::<u64>().ok()
    } else if let Some(n) = s.strip_suffix('s') {
        n.trim().parse::<u64>().ok().map(|n| n * 1000)
    } else {
        None
    }
}

fn is_component(s: &str) -> bool {
    s.contains('/') && s.chars().all(|c|
        c.is_ascii_alphanumeric() || c == '.' || c == '/' || c == '_' || c == '$')
}

/// `mobax cp` — direction from the `device:` prefix.
fn run_cp(host: &str, port: u16, src: &str, dst: &str) -> io::Result<()> {
    let to_dev = dst.starts_with("device:");
    let from_dev = src.starts_with("device:");
    if to_dev && from_dev {
        return Err(io::Error::other("cp: both sides device — not supported"));
    }
    if !to_dev && !from_dev {
        return Err(io::Error::other("cp: one side must start with 'device:'"));
    }
    let cmd = if from_dev {
        let device = src.strip_prefix("device:").unwrap().to_string();
        adb::Cmd::Pull { device, host: Some(dst.to_string()) }
    } else {
        let device = dst.strip_prefix("device:").unwrap().to_string();
        adb::Cmd::Push { host: src.to_string(), device, mode: None }
    };
    adb::run(host, port, &cmd)
}

fn write_response(cmd: &Cmd, body: &[u8]) -> io::Result<()> {
    let mut out = crate::output::stdout();
    out.write_all(body)?;
    // Trailing newline only for text commands on a terminal — never for binary.
    let is_text = matches!(
        cmd,
        Cmd::Ping | Cmd::Quit | Cmd::Input(_) | Cmd::Bench { .. } | Cmd::Find { .. }
    );
    if is_text && !body.is_empty() && body[body.len() - 1] != b'\n' {
        out.write_all(b"\n")?;
    }
    out.flush()
}

// ---------- protocol ----------

pub(crate) use mobax_core::Conn;

/// How the CLI connects: the target device's daemon through the adb server,
/// starting it when it isn't running, or a TCP port on a remote `--host`.
pub(crate) trait Connect: Sized {
    fn connect(host: &str, port: u16) -> io::Result<Self>;
    /// The daemon on `serial`, as an `io::Error` on failure (its kind tells a
    /// missing device, NotFound, from a missing daemon, ConnectionRefused).
    fn connect_device(serial: &str, port: u16) -> io::Result<Self>;
}

impl Connect for Conn {
    fn connect(host: &str, port: u16) -> io::Result<Self> {
        if !is_local(host) {
            return Conn::tcp(host, port);
        }
        let serial = daemon::target_serial()?;
        match Conn::device(&serial, port) {
            Ok(c) => Ok(c),
            // No daemon there yet: start it, so users don't have to run
            // `mobax use` first.
            Err(mobax_core::Error::NoDaemon(why_not)) => match try_auto_use(&serial, port) {
                Ok(()) => Ok(Conn::device(&serial, port)?),
                Err(why) => Err(io::Error::new(io::ErrorKind::ConnectionRefused, format!("{why_not}; {why}"))),
            },
            Err(e) => Err(e.into()),
        }
    }

    fn connect_device(serial: &str, port: u16) -> io::Result<Self> {
        Ok(Conn::device(serial, port)?)
    }
}

fn print_devices() -> io::Result<()> {
    let mut rows = daemon::devices()?;
    if rows.is_empty() {
        crate::outln!("no devices attached");
        return Ok(());
    }
    // If exactly one device is attached and no daemon is running for it
    // yet, transparently `mobax use` so the listing shows it ready to go.
    if rows.len() == 1 && rows[0].state == "device" && !rows[0].running {
        let opts = daemon::ConnectOpts {
            serial: Some(rows[0].serial.clone()),
            port: None,
        };
        if let Ok((serial, _)) = daemon::connect(&opts) {
            crate::errln!("daemon up on {serial} (auto)");
            rows = daemon::devices()?;
        }
    }
    crate::outln!(
        "{:<24} {:<10} {:<7} {:<8} {:<8} {:<16} model",
        "serial", "state", "jar", "running", "daemon", "sdk"
    );
    for r in rows {
        crate::outln!(
            "{:<24} {:<10} {:<7} {:<8} {:<8} {:<16} {}",
            r.serial,
            r.state,
            if r.jar_present { "yes" } else { "no" },
            if r.running { "yes" } else { "no" },
            if r.daemon { "up" } else { "-" },
            r.sdk.unwrap_or_else(|| "-".into()),
            r.model.unwrap_or_else(|| "-".into()),
        );
    }
    Ok(())
}

/// Run the equivalent of `mobax use` against `serial` so daemon-needing
/// verbs work without an explicit `mobax use` first. Returns a short human
/// reason on the error path.
fn try_auto_use(serial: &str, port: u16) -> Result<(), String> {
    let opts = daemon::ConnectOpts { serial: Some(serial.to_string()), port: Some(port) };
    let (serial, _) = daemon::connect(&opts).map_err(|e| format!("starting the daemon failed: {e}"))?;
    crate::errln!("daemon up on {serial} (auto)");
    Ok(())
}
