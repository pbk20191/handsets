// `mobax mirror` — launch the standalone GUI viewer at
// `mobax-viewer/`, which renders the daemon's frames on the GPU: Metal +
// VideoToolbox (macOS), D3D11 + Media Foundation (Windows), OpenGL ES +
// VA-API (Linux).
//
// The previous Kitty-graphics-protocol terminal mirror has been removed; this
// crate just locates the viewer binary and exec's it with the right
// --host / --port pointing at the daemon.

use std::io;
use std::process::Command;

use crate::daemon;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Args;

pub(crate) fn run(host: &str, port: u16, _args: Args) -> io::Result<()> {
    let viewer = daemon::locate_viewer().ok_or_else(|| io::Error::new(
        io::ErrorKind::NotFound,
        "mobax-viewer binary not found. Build it with:\n  \
         cargo build --release -p mobax-viewer\n\
         Then ensure it's on PATH, or run mobax from the workspace root.",
    ))?;

    // The viewer speaks plain TCP: give it a host port to this device's
    // daemon for as long as it runs.
    let local = crate::is_local(host);
    let (serial, port) = if local {
        let serial = daemon::target_serial()?;
        let hp = daemon::temporary_forward(&serial, port)?;
        (Some(serial), hp)
    } else {
        (None, port)
    };
    let status = Command::new(viewer)
        .args(["--host", host, "--port", &port.to_string()])
        .status();
    if let Some(s) = &serial {
        daemon::remove_forward(s, port);
    }
    let status = status?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "mobax-viewer exited with status {status}"
        )));
    }
    Ok(())
}
