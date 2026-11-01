// `mobax tui` — launch the standalone keyboard-driven UI inspector at
// `mobax-tui/`. Mirrors mirror.rs: we just locate the binary and exec
// it with --host / --port pointing at the daemon, so the `mobax` binary
// itself stays free of ratatui / crossterm deps.

use std::io;
use std::process::Command;

use crate::daemon;

pub(crate) fn run(host: &str, port: u16, active: bool) -> io::Result<()> {
    let bin = daemon::locate_tui().ok_or_else(|| io::Error::new(
        io::ErrorKind::NotFound,
        "mobax-tui binary not found. Build it with:\n  \
         cargo build --release -p mobax-tui\n\
         Then ensure it's on PATH, or run mobax from the workspace root.",
    ))?;

    // The TUI speaks plain TCP: give it a host port to this device's
    // daemon for as long as it runs.
    let local = crate::is_local(host);
    let (serial, port) = if local {
        let serial = daemon::target_serial()?;
        let hp = daemon::temporary_forward(&serial, port)?;
        (Some(serial), hp)
    } else {
        (None, port)
    };
    let mut cmd = Command::new(bin);
    cmd.args(["--host", host, "--port", &port.to_string()]);
    if active { cmd.arg("--active"); }
    let status = cmd.status();
    if let Some(s) = &serial {
        daemon::remove_forward(s, port);
    }
    let status = status?;

    if !status.success() {
        return Err(io::Error::other(format!("mobax-tui exited with status {status}")));
    }
    Ok(())
}
