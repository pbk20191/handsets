// The TUI's connection: mobax-core's, to the target device's daemon through
// the adb server (`--device`, else the device `mobax use` picked last, else
// the only one attached), or a TCP port on a remote `--host`.

use std::io;

pub struct Conn(mobax_core::Conn);

impl Conn {
    pub fn connect(host: &str, port: u16) -> io::Result<Self> {
        let conn = if mobax_core::device::is_local(host) {
            mobax_core::Conn::device(&mobax_core::device::target_serial()?, port)?
        } else {
            mobax_core::Conn::tcp(host, port)?
        };
        Ok(Self(conn))
    }

    pub fn call_str(&mut self, cmd: &str) -> io::Result<String> {
        let bytes = self.0.call(cmd)?;
        String::from_utf8(bytes).map_err(|e| io::Error::other(format!("bad utf-8: {e}")))
    }
}
