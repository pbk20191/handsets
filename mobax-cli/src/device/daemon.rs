// connect / disconnect / devices — manages the on-device mobileax daemon
// lifecycle from the host (push jar, start app_process, adb-forward TCP,
// kill on disconnect).

use std::io;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use mobax_core::adb_server::{self, Forward as ForwardEntry};
use mobax_core::device::{clear_current, home_dir, is_set, write_current};

use crate::state_cache;

pub(crate) use mobax_core::adb_server::{device_stream, DEFAULT_PORT};
pub(crate) use mobax_core::device::{set_target, target_serial, with_target};

const DEVICE_JAR: &str = "/data/local/tmp/mobax.jar";
const DEVICE_LOG: &str = "/data/local/tmp/mobax.log";

/// Locate the on-device jar on the host. Probes (in order):
///   1. $MOBILEAX_JAR
///   2. <exe_dir>/mobax.jar          — release tarball layout
///   3. <exe_dir>/../build/mobax.jar (and ../../, ../../../)
///   4. ~/.mobileax/mobax.jar        — curl-installer layout (found even when
///      <exe_dir> is the /usr/local/bin a symlink points from)
///   5. ./build/mobax.jar            — dev checkout
pub(crate) fn locate_jar() -> io::Result<PathBuf> {
    if let Ok(env) = std::env::var("MOBILEAX_JAR") {
        let p = PathBuf::from(env);
        if p.is_file() { return Ok(p); }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for rel in [
                "mobax.jar",
                "../build/mobax.jar",
                // Dev checkout: target/release/mobax → ../../build/mobax.jar
                "../../build/mobax.jar",
                // target/<triple>/release/mobax → ../../../build/mobax.jar
                "../../../build/mobax.jar",
            ] {
                let p = dir.join(rel);
                if p.is_file() { return Ok(p); }
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".mobileax/mobax.jar");
        if p.is_file() { return Ok(p); }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let p = cwd.join("build/mobax.jar");
    if p.is_file() { return Ok(p); }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "mobax.jar not found. Set MOBILEAX_JAR, install via the official installer, \
         or run from a checkout with build/mobax.jar",
    ))
}

/// Try to find the mobax-viewer binary. PATH first, then alongside our
/// binary, then the curl-installer layout under ~/.mobileax.
pub(crate) fn locate_viewer() -> Option<PathBuf> {
    locate_sibling_binary("mobax-viewer")
}

/// Try to find the mobax-tui binary. Same probe order as the viewer.
pub(crate) fn locate_tui() -> Option<PathBuf> {
    locate_sibling_binary("mobax-tui")
}

fn locate_sibling_binary(name: &str) -> Option<PathBuf> {
    // `mobax-viewer.exe` on Windows.
    let name = &format!("{name}{}", std::env::consts::EXE_SUFFIX);
    if let Ok(p) = which(name) { return Some(p); }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Release archive, and the dev checkout too: the workspace
            // builds every binary into the same target/<profile>/.
            let cand = dir.join(name);
            if cand.is_file() { return Some(cand); }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(format!(".mobileax/{name}"));
        if p.is_file() { return Some(p); }
    }
    None
}

fn which(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("'{name}' not on PATH")))
}

/// Result of running an adb subcommand. We capture stdout/stderr so we can
/// surface the actual adb error on failure rather than just exit code.
struct AdbOut {
    code: i32,
    stdout: String,
    stderr: String,
}

/// The adb to run, resolved once per process. Probes (in order):
///   1. $MOBILEAX_ADB, then $ADB
///   2. ~/.mobileax/adb-path      — cached result of 3 (`android info` takes ~0.7 s)
///   3. `android info` → `sdk: <dir>` → <dir>/platform-tools/adb  (Google's Android CLI)
///   4. $ANDROID_HOME, then $ANDROID_SDK_ROOT → platform-tools/adb
///   5. `adb` on PATH
///   6. Android Studio's default SDK (~/Library/Android/sdk, ~/Android/Sdk,
///      %LOCALAPPDATA%\Android\Sdk) — GUI hosts such as an MCP client often
///      start mobax without the shell's PATH
fn adb_program() -> &'static Path {
    static ADB: OnceLock<PathBuf> = OnceLock::new();
    ADB.get_or_init(resolve_adb)
}

fn resolve_adb() -> PathBuf {
    for var in ["MOBILEAX_ADB", "ADB"] {
        if let Some(v) = std::env::var_os(var).filter(|v| is_set(v)) {
            return PathBuf::from(v);
        }
    }
    let cache = home_dir().map(|h| h.join(".mobileax").join("adb-path"));
    if let Some(p) = cache.as_ref().and_then(|c| std::fs::read_to_string(c).ok()) {
        let p = PathBuf::from(p.trim());
        if p.is_file() {
            return p;
        }
    }
    if let Some(p) = android_cli_sdk().map(|sdk| sdk_adb(&sdk)).filter(|p| p.is_file()) {
        if let Some(c) = cache {
            let _ = std::fs::create_dir_all(c.parent().unwrap_or(Path::new(".")));
            let _ = std::fs::write(&c, p.to_string_lossy().as_bytes());
        }
        return p;
    }
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(sdk) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            let p = sdk_adb(Path::new(&sdk));
            if p.is_file() {
                return p;
            }
        }
    }
    if which(&format!("adb{}", std::env::consts::EXE_SUFFIX)).is_ok() {
        return PathBuf::from("adb");
    }
    let studio = [
        home_dir().map(|h| h.join("Library/Android/sdk")),
        home_dir().map(|h| h.join("Android/Sdk")),
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Android").join("Sdk")),
    ];
    studio
        .into_iter()
        .flatten()
        .map(|sdk| sdk_adb(&sdk))
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("adb"))
}

/// The SDK directory Google's Android CLI reports, if it is installed.
fn android_cli_sdk() -> Option<PathBuf> {
    let mut cmd = Command::new("android");
    // A set-but-empty ANDROID_HOME makes the CLI report an empty SDK path.
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if std::env::var_os(var).is_some_and(|v| v.is_empty()) {
            cmd.env_remove(var);
        }
    }
    let out = cmd
        .arg("info")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_android_info_sdk(&String::from_utf8_lossy(&out.stdout))
}

/// The `sdk: <dir>` line of `android info`, ANSI colours stripped.
fn parse_android_info_sdk(info: &str) -> Option<PathBuf> {
    info.lines().find_map(|line| {
        let line = strip_ansi(line);
        let dir = line.trim().strip_prefix("sdk:")?.trim();
        (!dir.is_empty()).then(|| PathBuf::from(dir))
    })
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ params final-byte
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn sdk_adb(sdk: &Path) -> PathBuf {
    sdk.join("platform-tools").join(format!("adb{}", std::env::consts::EXE_SUFFIX))
}

fn adb(serial: Option<&str>, args: &[&str]) -> io::Result<AdbOut> {
    let mut cmd = Command::new(adb_program());
    if let Some(s) = serial { cmd.args(["-s", s]); }
    cmd.args(args).stdin(Stdio::null());
    let out = cmd.output().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            io::Error::new(
                e.kind(),
                "adb not found: install Google's Android CLI (`android`) or the SDK \
                 platform-tools, or set MOBILEAX_ADB",
            )
        } else {
            e
        }
    })?;
    Ok(AdbOut {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn adb_ok(serial: Option<&str>, args: &[&str], what: &str) -> io::Result<String> {
    let r = adb(serial, args)?;
    if r.code != 0 {
        return Err(io::Error::other(format!(
            "{what}: adb exit {} — {}",
            r.code,
            if !r.stderr.trim().is_empty() { r.stderr.trim() } else { r.stdout.trim() }
        )));
    }
    Ok(r.stdout)
}

// ---------- public command entrypoints ----------

#[derive(Debug, Clone)]
pub(crate) struct ConnectOpts {
    pub serial: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone)]
pub(crate) struct DisconnectOpts {
    pub serial: Option<String>,
    pub keep_jar: bool,
}

/// `mobax use`: make sure the daemon runs on the device and answers.
/// Returns the device's serial and the daemon instance (port).
pub(crate) fn connect(opts: &ConnectOpts) -> io::Result<(String, u16)> {
    // Talk to the adb-server directly over its host protocol. Each query is
    // a sub-ms round-trip vs ~40ms for spawning the adb subprocess. Falls
    // back to the subprocess if the server isn't reachable (e.g. first
    // `mobax use` since boot), which is the only path that still needs to
    // start the server anyway.
    let serial = match adb_server::resolve_serial(opts.serial.as_deref()) {
        Ok(s) => s,
        Err(_) => resolve_serial(opts.serial.as_deref())?,
    };
    let port = opts.port.unwrap_or(DEFAULT_PORT);
    // Older versions reached the daemon through a host port forward; this
    // one goes through the adb server and binds no host port.
    remove_daemon_forwards(&serial);

    // Fast path: the daemon already answers. The common case when scripts
    // re-run `mobax use` defensively before each step.
    if ping_daemon(&serial, port).is_ok() {
        write_current(&serial);
        let _ = state_cache::spawn_detached(&serial, port);
        return Ok((serial, port));
    }

    // --- slow path: (re)start the daemon ---

    let jar = locate_jar()?;

    // 1. Hard-kill any prior daemon. linkToDeath inside system_server takes a
    //    moment to clear, so if we actually killed something we briefly wait
    //    until pgrep stops finding it. Skip the wait entirely when nothing
    //    matched (pkill exits non-zero) — saves ~500ms on a clean boot.
    let killed = adb(Some(&serial), &["shell", "pkill", "-9", "-f", "mobaxd"])
        .map(|r| r.code == 0).unwrap_or(false);
    if killed {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let r = adb(Some(&serial), &["shell", "pgrep", "-f", "mobaxd"])?;
            if r.stdout.trim().is_empty() { break; }
            thread::sleep(Duration::from_millis(100));
        }
        thread::sleep(Duration::from_millis(200));
    }

    // 2. Push jar.
    let jar_str = jar.to_string_lossy();
    adb_ok(Some(&serial), &["push", &jar_str, DEVICE_JAR], "push jar")?;

    // 3. Start the daemon detached.
    let start = format!(
        "CLASSPATH={DEVICE_JAR} nohup app_process /system/bin \
         --nice-name=mobaxd dev.mobileax.daemon.Main --port={port} > {DEVICE_LOG} 2>&1 &"
    );
    adb_ok(Some(&serial), &["shell", &start], "start daemon")?;

    // 4. Wait until the daemon answers `ping` (each probe is a few ms
    //    through the adb server).
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if ping_daemon(&serial, port).is_ok() {
            write_current(&serial);
            let _ = state_cache::spawn_detached(&serial, port);
            return Ok((serial, port));
        }
        thread::sleep(Duration::from_millis(40));
    }
    let r = adb(Some(&serial), &["shell", "cat", DEVICE_LOG])?;
    Err(io::Error::other(format!(
        "daemon did not come up within 6s\n--- device log ---\n{}",
        r.stdout
    )))
}

/// Send a length-prefixed `ping` and expect `pong`. Tight timeouts so a
/// daemon that isn't there fails quickly into the slow path.
pub(crate) fn ping_daemon(serial: &str, port: u16) -> io::Result<()> {
    let mut sock = device_stream(serial, port)?;
    sock.set_nodelay(true)?;
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    sock.set_write_timeout(Some(Duration::from_millis(250)))?;
    let payload = b"ping";
    sock.write_all(&(payload.len() as u32).to_be_bytes())?;
    sock.write_all(payload)?;
    let mut hdr = [0u8; 4];
    sock.read_exact(&mut hdr)?;
    let n = u32::from_be_bytes(hdr) as usize;
    if n > 64 {
        return Err(io::Error::other("ping reply too large"));
    }
    let mut buf = vec![0u8; n];
    sock.read_exact(&mut buf)?;
    if buf == b"pong" { Ok(()) } else { Err(io::Error::other("ping: unexpected reply")) }
}

/// Removes host port forwards to this device's daemon (older versions made them).
fn remove_daemon_forwards(serial: &str) {
    let forwards = adb_server::list_forwards().unwrap_or_else(|_| list_forwards().unwrap_or_default());
    for f in forwards.iter().filter(|f| f.serial == serial && f.remote_spec.starts_with("localabstract:mobaxd_")) {
        let _ = adb(Some(serial), &["forward", "--remove", &f.host_spec]);
        // Their state watchers were keyed by host port (state-9008.*).
        if let Some(port) = f.host_spec.strip_prefix("tcp:") {
            let _ = state_cache::stop_watcher(port);
        }
    }
}

pub(crate) fn disconnect(opts: &DisconnectOpts) -> io::Result<()> {
    let serial = resolve_serial(opts.serial.as_deref())?;
    // Every step below ignores adb failures, so an unknown serial would
    // otherwise "succeed" without touching anything.
    if opts.serial.is_some() {
        let listing = adb_ok(None, &["devices"], "adb devices")?;
        if !is_listed(&listing, &serial) {
            return Err(io::Error::other(format!(
                "--device {serial}: not attached — check `adb devices`")));
        }
    }
    // 1. Stop the host-side state watcher.
    let _ = state_cache::stop_watcher(&serial);
    // 2. Kill the device-side daemon.
    let _ = adb(Some(&serial), &["shell", "pkill", "-9", "-f", "mobaxd"]);
    // 3. Remove forwards older versions left.
    remove_daemon_forwards(&serial);
    // 4. Optionally delete the on-device jar.
    if !opts.keep_jar {
        let _ = adb(Some(&serial), &["shell", "rm", "-f", DEVICE_JAR, DEVICE_LOG]);
    }
    clear_current(&serial);
    Ok(())
}

pub(crate) struct DeviceRow {
    pub serial: String,
    pub state: String,
    pub jar_present: bool,
    pub running: bool,
    /// The daemon answers on the default instance.
    pub daemon: bool,
    pub sdk: Option<String>,
    pub model: Option<String>,
}

pub(crate) fn devices() -> io::Result<Vec<DeviceRow>> {
    // Prefer the adb-server host protocol — sub-ms per query vs ~30ms per
    // `adb` subprocess fork. Fall back if adb-server is unreachable.
    let listing = match adb_server::query("host:devices") {
        Ok(body) => body,
        Err(_) => adb_ok(None, &["devices"], "adb devices")?,
    };
    let mut rows = Vec::new();
    for line in listing.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("List of devices") { continue; }
        // `adb devices` is space-separated, `host:devices` is tab-separated;
        // split_whitespace handles both.
        let mut it = line.split_whitespace();
        let serial = match it.next() { Some(s) => s.to_string(), None => continue };
        let state = it.next().unwrap_or("?").to_string();
        let mut row = DeviceRow {
            serial: serial.clone(),
            state: state.clone(),
            jar_present: false,
            running: false,
            daemon: false,
            sdk: None,
            model: None,
        };
        if state == "device" {
            // Fast path: if the daemon answers `ping`,
            // we know the daemon is running and the on-device jar must be
            // present (the daemon can't start without it). sdk/model don't
            // change between reboots, so serve them from a per-serial cache
            // and skip the 16ms shell round-trip entirely. Falls through to
            // the shell probe if any of these aren't met.
            let pinged = ping_daemon(&serial, DEFAULT_PORT).is_ok();
            row.daemon = pinged;
            let cached = if pinged { read_devinfo_cache(&serial) } else { None };
            if let (true, Some((sdk, model))) = (pinged, cached) {
                row.running = true;
                row.jar_present = true;
                row.sdk = Some(sdk);
                row.model = Some(model);
            } else {
                // One batched shell call covers all four probes — saves 3
                // round-trips vs the original four separate `adb shell`s.
                let cmd = format!(
                    "test -f {DEVICE_JAR} && echo jar=1 || echo jar=0; \
                     pgrep -f mobaxd >/dev/null && echo run=1 || echo run=0; \
                     echo sdk=$(getprop ro.build.version.sdk); \
                     echo model=$(getprop ro.product.model)"
                );
                let probe = adb_server::shell(&serial, &cmd)
                    .or_else(|_| adb(Some(&serial), &["shell", &cmd]).map(|r| r.stdout))
                    .unwrap_or_default();
                for ln in probe.lines() {
                    let ln = ln.trim();
                    if let Some(v) = ln.strip_prefix("jar=")   { row.jar_present = v == "1"; }
                    else if let Some(v) = ln.strip_prefix("run=")   { row.running = v == "1"; }
                    else if let Some(v) = ln.strip_prefix("sdk=")   { if !v.is_empty() { row.sdk = Some(v.to_string()); } }
                    else if let Some(v) = ln.strip_prefix("model=") { if !v.is_empty() { row.model = Some(v.to_string()); } }
                }
                if let (Some(sdk), Some(model)) = (row.sdk.as_deref(), row.model.as_deref()) {
                    let _ = write_devinfo_cache(&serial, sdk, model);
                }
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

// ---------- helpers ----------

fn list_forwards() -> io::Result<Vec<ForwardEntry>> {
    let r = adb(None, &["forward", "--list"])?;
    if r.code != 0 { return Ok(Vec::new()); }
    Ok(adb_server::parse_forwards(&r.stdout))
}

/// If no serial is specified and exactly one device is attached, use it.
/// Otherwise error out so the user picks.
/// Whether `adb devices` output lists `serial`, in any state (offline and
/// unauthorized devices still own forwards worth removing).
fn is_listed(listing: &str, serial: &str) -> bool {
    listing.lines().skip(1).any(|l| l.split_whitespace().next() == Some(serial))
}

fn resolve_serial(explicit: Option<&str>) -> io::Result<String> {
    if let Some(s) = explicit { return Ok(s.to_string()); }
    let listing = adb_ok(None, &["devices"], "adb devices")?;
    let mut found: Vec<String> = listing
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let s = it.next()?;
            let st = it.next().unwrap_or("");
            if st == "device" { Some(s.to_string()) } else { None }
        })
        .collect();
    match found.len() {
        0 => Err(io::Error::other("no devices in 'device' state — check `adb devices`")),
        1 => Ok(found.remove(0)),
        _ => Err(io::Error::other(format!(
            "multiple devices attached ({}); pass --device SERIAL",
            found.join(", ")
        ))),
    }
}

// ---------- per-serial device-info cache (sdk + model) ----------
//
// sdk and model never change for a given device, so we cache them in
// ~/.mobileax/devinfo-<serial>.txt and serve `mobax` (the no-args device
// listing) without an `adb shell getprop` round-trip when the daemon is
// already responding to ping. Worst case the cache is wrong for a
// reflashed device — easy fix is to rm the file.

fn devinfo_cache_path(serial: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    // Sanitise the serial — it's user-controlled-ish and we don't want a
    // serial containing `/` or `..` to escape ~/.mobileax/.
    let safe: String = serial.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    Some(PathBuf::from(home).join(format!(".mobileax/devinfo-{safe}.txt")))
}

fn read_devinfo_cache(serial: &str) -> Option<(String, String)> {
    let p = devinfo_cache_path(serial)?;
    let raw = std::fs::read_to_string(p).ok()?;
    let mut sdk = None;
    let mut model = None;
    for ln in raw.lines() {
        if let Some(v) = ln.strip_prefix("sdk=")   { sdk = Some(v.to_string()); }
        else if let Some(v) = ln.strip_prefix("model=") { model = Some(v.to_string()); }
    }
    Some((sdk?, model?))
}

fn write_devinfo_cache(serial: &str, sdk: &str, model: &str) -> io::Result<()> {
    let p = devinfo_cache_path(serial)
        .ok_or_else(|| io::Error::other("no $HOME for devinfo cache"))?;
    if let Some(dir) = p.parent() { std::fs::create_dir_all(dir)?; }
    std::fs::write(p, format!("sdk={sdk}\nmodel={model}\n"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn android_info_sdk_line() {
        let info = "sdk: E:\\packages\\Android\\Sdk\nversion: 1.0.16625370\n";
        assert_eq!(
            super::parse_android_info_sdk(info),
            Some(std::path::PathBuf::from("E:\\packages\\Android\\Sdk"))
        );
        let coloured = "\u{1b}[33mA new version is available\u{1b}[39m\n\u{1b}[1msdk:\u{1b}[0m /opt/android\n";
        assert_eq!(super::parse_android_info_sdk(coloured), Some(std::path::PathBuf::from("/opt/android")));
        assert_eq!(super::parse_android_info_sdk("version: 1\n"), None);
    }

    use super::is_listed;

    #[test]
    fn is_listed_matches_whole_serials_in_any_state() {
        let listing = "List of devices attached\nemulator-5554\tdevice\nR58M\toffline\n\n";
        assert!(is_listed(listing, "emulator-5554"));
        assert!(is_listed(listing, "R58M"));
        assert!(!is_listed(listing, "emulator-555"));
        assert!(!is_listed(listing, "List"));
    }
}
