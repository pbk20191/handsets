// Host-side state mirror.
//
// `mobax connect` spawns a detached child process that subscribes to the
// daemon's `state_watch` stream and atomically rewrites
// `$HOME/.mobileax/state-<serial>.json` on every push. Subsequent
// `mobax state <field>` invocations read that file instead of going over
// the wire — turning a ~2 ms TCP round-trip into a sub-100µs file read.
//
// File layout:
//   ~/.mobileax/state-<serial>.json    latest snapshot (atomic via tmp+rename)
//   ~/.mobileax/state-<serial>.pid     PID of the running watcher
//
// One watcher per device, keyed by serial (sanitised for file names).

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::Conn;
use crate::Connect;

const FRESHNESS_LIMIT: Duration = Duration::from_secs(30);

fn dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".mobileax")
}

/// `emulator-5554` stays as it is; anything outside [A-Za-z0-9._-]
/// (a `host:port` serial's colon) becomes `_`.
fn file_key(serial: &str) -> String {
    serial.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect()
}

pub(crate) fn cache_path(serial: &str) -> PathBuf {
    dir().join(format!("state-{}.json", file_key(serial)))
}

fn pid_path(serial: &str) -> PathBuf {
    dir().join(format!("state-{}.pid", file_key(serial)))
}

/// Read the cached snapshot if it's fresher than {@link FRESHNESS_LIMIT}.
pub(crate) fn read_cached(serial: &str) -> Option<Vec<u8>> {
    let p = cache_path(serial);
    let meta = fs::metadata(&p).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).unwrap_or(Duration::MAX);
    if age > FRESHNESS_LIMIT {
        return None;
    }
    fs::read(&p).ok()
}

/// Foreground daemon loop: subscribe and persist forever. Used as the body
/// of `mobax --device SERIAL state-daemon`.
pub(crate) fn run_daemon(serial: &str, port: u16) -> io::Result<()> {
    fs::create_dir_all(dir())?;
    let pid_p = pid_path(serial);
    fs::write(&pid_p, std::process::id().to_string())?;

    let mut conn = Conn::connect_device(serial, port)?;
    conn.send_cmd("state_watch")?;

    let cache_p = cache_path(serial);
    let tmp_p = cache_p.with_extension("json.tmp");
    loop {
        let frame = conn.read_frame()?;
        if frame.starts_with(b"ERR:") {
            return Err(io::Error::other(String::from_utf8_lossy(&frame).into_owned()));
        }
        fs::write(&tmp_p, &frame)?;
        fs::rename(&tmp_p, &cache_p)?;
    }
}

/// Spawn the state-daemon as a detached background child. Returns the PID
/// that's now writing the cache file.
pub(crate) fn spawn_detached(serial: &str, port: u16) -> io::Result<u32> {
    // If something's already running for this device, leave it alone.
    if let Some(existing) = pid_alive(serial) {
        return Ok(existing);
    }
    let exe = std::env::current_exe()?;
    keep_std_handles_from_children();
    let child = Command::new(exe)
        .args(["--device", serial, "--port", &port.to_string(), "state-daemon"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(child.id())
}

/// Stop the watcher for this device, if any.
pub(crate) fn stop_watcher(serial: &str) -> io::Result<()> {
    let p = pid_path(serial);
    let Ok(raw) = fs::read_to_string(&p) else {
        return Ok(());
    };
    if let Ok(pid) = raw.trim().parse::<u32>() {
        terminate_pid(pid);
    }
    let _ = fs::remove_file(&p);
    let _ = fs::remove_file(cache_path(serial));
    Ok(())
}

fn pid_alive(serial: &str) -> Option<u32> {
    let raw = fs::read_to_string(pid_path(serial)).ok()?;
    let pid: u32 = raw.trim().parse().ok()?;
    if pid_is_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // signal 0 = existence probe.
    unsafe { libc_kill(pid as i32, 0) == 0 }
}

#[cfg(unix)]
fn terminate_pid(pid: u32) {
    unsafe {
        libc_kill(pid as i32, 15);
    } // SIGTERM
}

#[cfg(windows)]
fn pid_is_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// Windows hands every inheritable handle to a child (Rust creates processes
/// with handle inheritance on), so the long-lived watcher would hold this
/// process's stdout pipe open and `mobax use | cat` would never end. Marks
/// our std handles non-inheritable; children given `Stdio::inherit` still
/// get them, as Rust passes those as fresh inheritable duplicates.
#[cfg(windows)]
fn keep_std_handles_from_children() {
    use std::ffi::c_void;
    extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE
    for which in [-10i32, -11, -12] {
        unsafe {
            let h = GetStdHandle(which as u32);
            if !h.is_null() && h as isize != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(not(windows))]
fn keep_std_handles_from_children() {}

#[cfg(windows)]
fn terminate_pid(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(any(unix, windows)))]
fn pid_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(not(any(unix, windows)))]
fn terminate_pid(_pid: u32) {}

// Tiny Unix FFI shim — avoids pulling in the `libc` crate just for two calls.
#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

/// Extract one top-level field from a flat JSON object. The state snapshot
/// has only scalar values (bool / int / string) so we don't pull in a JSON
/// crate — handle the three cases directly.
pub(crate) fn extract_field(json: &[u8], key: &str) -> Option<String> {
    let s = std::str::from_utf8(json).ok()?;
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)?;
    let tail = &s[i + pat.len()..];
    let tail = tail.trim_start();
    if let Some(stripped) = tail.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(stripped[..end].to_string())
    } else if tail.starts_with("true") {
        Some("true".into())
    } else if tail.starts_with("false") {
        Some("false".into())
    } else if tail.starts_with("null") {
        Some("null".into())
    } else {
        let end = tail.find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
            .unwrap_or(tail.len());
        Some(tail[..end].to_string())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn serials_make_safe_file_names() {
        assert_eq!(super::file_key("emulator-5554"), "emulator-5554");
        assert_eq!(super::file_key("192.168.0.7:5555"), "192.168.0.7_5555");
        assert_eq!(super::file_key("../x"), ".._x");
    }
}
