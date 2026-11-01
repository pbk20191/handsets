// The TUI's connection: mobax-core's, to the target device's daemon through
// the adb server (`--device`, else the device `mobax use` picked last, else
// the only one attached), or a TCP port on a remote `--host`.

use std::io;

pub struct Conn(mobax_core::Conn);

impl Conn {
    pub fn connect(host: &str, port: u16) -> io::Result<Self> {
        Ok(Self(mobax_core::Conn::from_stream(mobax_core::wire::daemon_stream(host, port)?)?))
    }

    pub fn call_str(&mut self, cmd: &str) -> io::Result<String> {
        let bytes = self.0.call(cmd)?;
        String::from_utf8(bytes).map_err(|e| io::Error::other(format!("bad utf-8: {e}")))
    }
}
