// Which device a call talks to.
//
// The daemon is reached through the adb server, so a device is named by its
// serial: one chosen for this thread (`with_target`), else for this process
// (`set_target`, i.e. `--device`), else $MOBILEAX_SERIAL / $ANDROID_SERIAL,
// else the device `mobax use` picked last (~/.mobileax/current) if it is
// still attached, else the only one attached.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::adb_server;
use crate::errors::Error;

thread_local! {
    /// A per-thread choice, for an MCP server's calls on several devices.
    static THREAD_TARGET: RefCell<Option<String>> = const { RefCell::new(None) };
}

static PROCESS_TARGET: Mutex<Option<String>> = Mutex::new(None);

/// The device named on the command line (`--device`), for this process.
pub fn set_target(serial: Option<String>) {
    *PROCESS_TARGET.lock().unwrap_or_else(|e| e.into_inner()) = serial;
}

/// Runs `f` with calls on this thread going to `serial`.
pub fn with_target<R>(serial: &str, f: impl FnOnce() -> R) -> R {
    let prev = THREAD_TARGET.with(|t| t.borrow_mut().replace(serial.to_string()));
    let r = f();
    THREAD_TARGET.with(|t| *t.borrow_mut() = prev);
    r
}

/// The serial calls on this thread go to (see the module comment).
pub fn target_serial() -> Result<String, Error> {
    if let Some(s) = THREAD_TARGET.with(|t| t.borrow().clone()) {
        return Ok(s);
    }
    if let Some(s) = PROCESS_TARGET.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return Ok(s);
    }
    for var in ["MOBILEAX_SERIAL", "ANDROID_SERIAL"] {
        if let Some(s) = std::env::var_os(var).filter(|v| is_set(v)) {
            return Ok(s.to_string_lossy().into_owned());
        }
    }
    let attached = adb_server::attached_serials()?;
    if let Some(cur) = read_current().filter(|c| attached.contains(c)) {
        return Ok(cur);
    }
    match attached.len() {
        0 => Err(Error::NoDevice("no device attached — check `adb devices`".into())),
        1 => Ok(attached[0].clone()),
        n => Err(Error::WhichDevice(format!(
            "{n} devices attached ({}); pass --device SERIAL or `mobax use SERIAL` one",
            attached.join(", ")))),
    }
}

/// An env override that is actually set. Claude Desktop passes an MCPB
/// `${user_config.…}` placeholder through verbatim when the option is blank.
pub fn is_set(v: &std::ffi::OsStr) -> bool {
    let s = v.to_string_lossy();
    let s = s.trim();
    !s.is_empty() && !(s.starts_with("${") && s.ends_with('}'))
}

/// A daemon on this machine is reached through the adb server; any other
/// host through a TCP port there.
pub fn is_local(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// $HOME, else %USERPROFILE%.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn current_path() -> Option<PathBuf> {
    Some(home_dir()?.join(".mobileax").join("current"))
}

/// The device `mobax use` picked last.
pub fn read_current() -> Option<String> {
    let s = std::fs::read_to_string(current_path()?).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

pub fn write_current(serial: &str) {
    if let Some(p) = current_path() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, serial);
    }
}

/// Forgets `serial` as the current device, if it is.
pub fn clear_current(serial: &str) {
    if read_current().as_deref() == Some(serial) {
        if let Some(p) = current_path() {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unsubstituted_placeholder_is_unset() {
        use std::ffi::OsStr;
        assert!(super::is_set(OsStr::new(r"C:\sdk\platform-tools\adb.exe")));
        assert!(!super::is_set(OsStr::new("")));
        assert!(!super::is_set(OsStr::new("${user_config.adb_path}")));
    }

    #[test]
    fn a_thread_target_wins_and_is_restored() {
        super::set_target(Some("from-flag".into()));
        assert_eq!(super::target_serial().unwrap(), "from-flag");
        super::with_target("for-this-call", || assert_eq!(super::target_serial().unwrap(), "for-this-call"));
        assert_eq!(super::target_serial().unwrap(), "from-flag");
        super::set_target(None);
    }
}
