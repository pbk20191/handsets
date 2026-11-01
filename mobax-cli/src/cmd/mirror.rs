// `mobax mirror` — launch the standalone GUI viewer at
// `mobax-viewer/`, which renders the daemon's frames on the GPU: Metal +
// VideoToolbox (macOS), D3D11 + Media Foundation (Windows), OpenGL ES +
// VA-API (Linux).
//
// The previous Kitty-graphics-protocol terminal mirror has been removed; this
// crate just locates the viewer binary and runs it with the device (or a
// remote --host / --port) to show.

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

    // The viewer reaches the device through the adb server itself
    // (mobax-core), so it only needs to know which device.
    let mut cmd = Command::new(viewer);
    cmd.args(["--host", host, "--port", &port.to_string()]);
    if crate::is_local(host) {
        cmd.args(["--device", &daemon::target_serial()?]);
    }
    let status = cmd.status()?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "mobax-viewer exited with status {status}"
        )));
    }
    Ok(())
}
