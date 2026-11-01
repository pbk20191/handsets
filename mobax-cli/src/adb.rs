// ADB-CLI-equivalent commands routed over the existing mobileax socket.
//
// Wire shape matches what `Server.java` expects:
//   pull   →  "pull path=…"       ; server streams [len][chunk]* [len=0]
//   push   →  "push path=… mode=… size=N"  ; client streams chunks then reads
//                                            one final [ok|ERR:…] frame
//   install→  "install size=N reinstall=… grant=…"   ; same as push
//   pm/am  →  single text request → single text response

use std::fs::File;
use std::io::{self, Write};

use crate::{state_cache, Conn};

#[derive(Debug)]
pub(crate) enum Cmd {
    Pull { device: String, host: Option<String> },
    Push { host: String, device: String, mode: Option<u32> },
    Install { apk: String, reinstall: bool, grant: bool },
    InstallMulti { apks: Vec<String>, reinstall: bool, grant: bool },
    PmList { third: bool, system: bool },
    PmUninstall(String),
    AmStart { component: String, action: Option<String>, data: Option<String>, flag: Option<i32> },
    AmForceStop(String),
    GetProp(String),
    SetProp(String, String),
    Logcat(LogcatQuery),
    SettingsGet { namespace: String, key: String },
    SettingsPut { namespace: String, key: String, value: String },
    Shell(Vec<String>),
    Monitor,
    State(String),
}

/// `mobax logs`: logcat's own argv plus the parts `mobax` does on either side of it.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LogcatQuery {
    /// logcat argv (buffers, -d / -t, -v, -e, filter specs, raw extras).
    pub args: Vec<String>,
    /// Becomes `--pid=` for the package's main process, resolved on the device.
    pub pkg: Option<String>,
    /// Becomes `-T <epoch>`: this many seconds back on the device's clock.
    pub since_secs: Option<u64>,
    /// Keep the last N lines on the host. logcat's own `-t` counts lines
    /// before its priority / tag / regex filters, and before `exclude`.
    pub tail: Option<usize>,
    /// Drop lines containing any of these (host side).
    pub exclude: Vec<String>,
    pub ignore_case: bool,
}

impl LogcatQuery {
    fn keeps(&self, line: &str) -> bool {
        if line.starts_with("--------- beginning of") {
            return false;
        }
        if self.ignore_case {
            let low = line.to_lowercase();
            !self.exclude.iter().any(|x| low.contains(&x.to_lowercase()))
        } else {
            !self.exclude.iter().any(|x| line.contains(x.as_str()))
        }
    }
}

// ---------- parsers ----------

pub(crate) fn parse_install(rest: &[&str]) -> Result<Cmd, String> {
    let mut positional: Vec<&str> = Vec::new();
    let mut reinstall = false;
    let mut grant = false;
    for tok in rest {
        match *tok {
            "--reinstall" | "-r" => reinstall = true,
            "--grant" | "-g" => grant = true,
            other => positional.push(other),
        }
    }
    if positional.is_empty() {
        return Err("install needs at least one APK_PATH".into());
    }
    if positional.len() == 1 {
        Ok(Cmd::Install { apk: positional[0].to_string(), reinstall, grant })
    } else {
        Ok(Cmd::InstallMulti {
            apks: positional.iter().map(|s| s.to_string()).collect(),
            reinstall, grant,
        })
    }
}


// ---------- runner ----------

pub(crate) fn run(host: &str, port: u16, cmd: &Cmd) -> io::Result<()> {
    // Fast path: state queries served straight out of the host-side mirror
    // file (populated by the `state-daemon` watcher that `connect` spawns).
    // No TCP, no parsing on the wire — just a file read + tiny string scan.
    if let Cmd::State(field) = cmd {
        if let Some(snap) = crate::cached_state() {
            return print_state_field(&snap, field);
        }
    }

    let mut conn = Conn::connect(host, port)?;
    match cmd {
        Cmd::Pull { device, host } => do_pull(&mut conn, device, host.as_deref()),
        Cmd::Push { host, device, mode } => do_push(&mut conn, host, device, *mode),
        Cmd::Install { apk, reinstall, grant } => do_install(&mut conn, apk, *reinstall, *grant),
        Cmd::InstallMulti { apks, reinstall, grant } =>
            do_install_multi(&mut conn, apks, *reinstall, *grant),
        Cmd::GetProp(k) => text_call(&mut conn, &format!("getprop {k}")),
        Cmd::SetProp(k, v) => text_call(&mut conn, &format!("setprop {k} {v}")),
        Cmd::Logcat(q) => run_logcat(&mut conn, q),
        Cmd::SettingsGet { namespace, key } =>
            text_call(&mut conn, &format!("settings_get {namespace} {key}")),
        Cmd::SettingsPut { namespace, key, value } =>
            text_call(&mut conn, &format!("settings_put {namespace} {key} {value}")),
        Cmd::Shell(argv) => {
            let mut wire = String::from("shell");
            for a in argv { wire.push(' '); wire.push_str(a); }
            shell_stream(&mut conn, &wire)
        }
        Cmd::Monitor => stream_call(&mut conn, "monitor"),
        Cmd::State(field) => text_call(&mut conn, &format!("state {field}")),
        Cmd::PmList { third, system } => {
            let mut wire = String::from("pm_list");
            if *third { wire.push_str(" 3"); }
            if *system { wire.push_str(" s"); }
            text_call(&mut conn, &wire)
        }
        Cmd::PmUninstall(pkg) => text_call(&mut conn, &format!("pm_uninstall {pkg}")),
        Cmd::AmStart { component, action, data, flag } => {
            let mut wire = format!("am_start n={component}");
            if let Some(a) = action { wire.push_str(&format!(" a={a}")); }
            if let Some(d) = data   { wire.push_str(&format!(" d={d}")); }
            if let Some(f) = flag   { wire.push_str(&format!(" f={f}")); }
            text_call(&mut conn, &wire)
        }
        Cmd::AmForceStop(pkg) => text_call(&mut conn, &format!("am_force_stop {pkg}")),
    }
}

/// Print a single field extracted from a cached snapshot. For "device" we
/// just dump the whole JSON. For other fields we scan-and-extract.
fn print_state_field(snap: &[u8], field: &str) -> io::Result<()> {
    let mut out = crate::output::stdout();
    if field == "device" {
        out.write_all(snap)?;
    } else {
        // CLI uses short names; map them to the JSON keys the daemon emits.
        let json_key = match field {
            "top" => "top_activity",
            other => other,
        };
        let v = state_cache::extract_field(snap, json_key).ok_or_else(|| io::Error::other(
            format!("field '{field}' not present in cached snapshot")
        ))?;
        out.write_all(v.as_bytes())?;
    }
    out.write_all(b"\n")?;
    out.flush()
}

fn text_call(conn: &mut Conn, wire: &str) -> io::Result<()> {
    let body = conn.call(wire)?;
    if body.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&body).into_owned()));
    }
    let mut out = crate::output::stdout();
    out.write_all(&body)?;
    // Always end on a newline so chained CLI calls don't run together.
    let needs_newline = !body.is_empty() && body[body.len() - 1] != b'\n';
    if needs_newline { out.write_all(b"\n")?; }
    out.flush()
}

/// Like `stream_call` but for `shell` passthrough: drops the daemon's
/// `__exit__ N` trailer (added by ShellExec.java to report the exit code).
/// Non-zero exit codes surface as an io::Error.
fn shell_stream(conn: &mut Conn, wire: &str) -> io::Result<()> {
    conn.send_cmd(wire)?;
    let mut out = crate::output::stdout();
    let mut first = true;
    loop {
        let frame = conn.read_frame()?;
        if frame.is_empty() { break; }                     // [len=0] terminator
        if first && frame.starts_with(b"ERR:") {
            return Err(io::Error::other(
                String::from_utf8_lossy(&frame).into_owned()));
        }
        first = false;
        if let Some(code) = parse_exit_trailer(&frame) {
            // Drain the terminator, then surface non-zero exits.
            let _ = conn.read_frame();
            out.flush()?;
            if code != 0 {
                return Err(io::Error::other(format!("shell exited {code}")));
            }
            return Ok(());
        }
        out.write_all(&frame)?;
    }
    out.flush()
}

fn parse_exit_trailer(b: &[u8]) -> Option<i32> {
    std::str::from_utf8(b).ok()?
        .trim()
        .strip_prefix("__exit__ ")?
        .parse()
        .ok()
}

/// Server-streamed call. Sends the command, then dumps chunked frames to
/// stdout until the 0-length terminator.
/// Runs a `shell` wire command and returns its stdout; a non-zero exit is
/// an error.
fn run_logcat(conn: &mut Conn, q: &LogcatQuery) -> io::Result<()> {
    let mut wire = String::from("logcat");
    if let Some(pkg) = &q.pkg {
        let pids = shell_capture(conn, &format!("shell pidof {pkg}"))
            .map_err(|_| io::Error::other(format!("{pkg} is not running")))?;
        // pidof lists every process of the package; the main one is first.
        let pid = pids.split_whitespace().next()
            .ok_or_else(|| io::Error::other(format!("{pkg} is not running")))?;
        wire.push_str(&format!(" --pid={pid}"));
    }
    if let Some(secs) = q.since_secs {
        // The device's clock, not the host's: emulators and phones drift.
        let now: u64 = shell_capture(conn, "shell date +%s")?.trim().parse()
            .map_err(|_| io::Error::other("since: could not read the device clock"))?;
        wire.push_str(&format!(" -T {}.000", now.saturating_sub(secs)));
    }
    for a in &q.args { wire.push(' '); wire.push_str(a); }
    conn.send_cmd(&wire)?;
    let Some(n) = q.tail else {
        let mut out = LineFilter { q, out: crate::output::stdout(), partial: Vec::new() };
        conn.recv_chunks_to(&mut out)?;
        return out.finish();
    };
    let mut all = Vec::new();
    conn.recv_chunks_to(&mut all)?;
    let text = String::from_utf8_lossy(&all);
    let lines: Vec<&str> = text.lines().filter(|l| q.keeps(l)).collect();
    let mut out = crate::output::stdout();
    for l in &lines[lines.len().saturating_sub(n)..] {
        writeln!(out, "{l}")?;
    }
    out.flush()
}

/// Passes whole lines that `LogcatQuery::keeps` through while streaming.
struct LineFilter<'a, W: Write> {
    q: &'a LogcatQuery,
    out: W,
    partial: Vec<u8>,
}

impl<W: Write> LineFilter<'_, W> {
    fn emit(&mut self, line: &[u8]) -> io::Result<()> {
        if self.q.keeps(String::from_utf8_lossy(line).trim_end()) {
            self.out.write_all(line)?;
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        let rest = std::mem::take(&mut self.partial);
        if !rest.is_empty() {
            self.emit(&rest)?;
        }
        self.out.flush()
    }
}

impl<W: Write> Write for LineFilter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.partial.extend_from_slice(buf);
        while let Some(i) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=i).collect();
            self.emit(&line)?;
        }
        self.out.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

fn shell_capture(conn: &mut Conn, wire: &str) -> io::Result<String> {
    conn.send_cmd(wire)?;
    let mut out = Vec::new();
    loop {
        let frame = conn.read_frame()?;
        if frame.is_empty() { break; }
        if out.is_empty() && frame.starts_with(b"ERR:") {
            return Err(io::Error::other(String::from_utf8_lossy(&frame).into_owned()));
        }
        if let Some(code) = parse_exit_trailer(&frame) {
            let _ = conn.read_frame();
            if code != 0 {
                return Err(io::Error::other(format!("shell exited {code}")));
            }
            break;
        }
        out.extend_from_slice(&frame);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn stream_call(conn: &mut Conn, wire: &str) -> io::Result<()> {
    conn.send_cmd(wire)?;
    let mut out = crate::output::stdout();
    conn.recv_chunks_to(&mut out)?;
    out.flush()
}

fn do_install_multi(conn: &mut Conn, apks: &[String], reinstall: bool, grant: bool)
        -> io::Result<()> {
    let mut sizes: Vec<u64> = Vec::with_capacity(apks.len());
    for p in apks {
        sizes.push(File::open(p)?.metadata()?.len());
    }
    let sizes_str = sizes.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(",");
    let mut wire = format!("install_multi sizes={sizes_str}");
    if reinstall { wire.push_str(" reinstall=1"); }
    if grant     { wire.push_str(" grant=1"); }
    conn.send_cmd(&wire)?;
    for p in apks {
        let mut f = File::open(p)?;
        let _ = conn.send_chunks_from(&mut f)?;
    }
    let reply = conn.read_frame()?;
    if reply.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&reply).into_owned()));
    }
    let mut out = crate::output::stderr();
    out.write_all(&reply)?;
    out.write_all(b"\n")?;
    Ok(())
}

fn do_pull(conn: &mut Conn, device: &str, host: Option<&str>) -> io::Result<()> {
    conn.send_cmd(&format!("pull path={device}"))?;
    let written = match host {
        Some(p) => {
            let mut f = File::create(p)?;
            let n = conn.recv_chunks_to(&mut f)?;
            f.flush()?;
            n
        }
        None => {
            let mut out = crate::output::stdout();
            conn.recv_chunks_to(&mut out)?
        }
    };
    if host.is_some() {
        crate::errln!("pulled {written} bytes from {device}");
    }
    Ok(())
}

fn do_push(conn: &mut Conn, host: &str, device: &str, mode: Option<u32>) -> io::Result<()> {
    let mut f = File::open(host)?;
    let size = f.metadata()?.len();
    let mut wire = format!("push path={device} size={size}");
    if let Some(m) = mode { wire.push_str(&format!(" mode=0{m:o}")); }
    conn.send_cmd(&wire)?;
    let written = conn.send_chunks_from(&mut f)?;
    let reply = conn.read_frame()?;
    if reply.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&reply).into_owned()));
    }
    crate::errln!("pushed {written} bytes to {device}");
    Ok(())
}

fn do_install(conn: &mut Conn, apk: &str, reinstall: bool, grant: bool) -> io::Result<()> {
    let mut f = File::open(apk)?;
    let size = f.metadata()?.len();
    let mut wire = format!("install size={size}");
    if reinstall { wire.push_str(" reinstall=1"); }
    if grant     { wire.push_str(" grant=1"); }
    conn.send_cmd(&wire)?;
    let _ = conn.send_chunks_from(&mut f)?;
    let reply = conn.read_frame()?;
    if reply.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&reply).into_owned()));
    }
    let mut out = crate::output::stderr();
    out.write_all(&reply)?;
    out.write_all(b"\n")?;
    Ok(())
}
