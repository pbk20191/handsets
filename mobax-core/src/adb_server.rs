// The adb server's own protocol (tcp:5037 by default), spoken directly:
// a 4-char hex length then an ASCII request; the server answers "OKAY"
// (with a hex-length body for host queries) or "FAIL" + hex length +
// reason. Talking to it directly skips the ~40 ms `adb` subprocess per
// query, and `device_stream` reaches the daemon on a device with no host
// port forward.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::errors::Error;

/// The daemon instance (`mobaxd_<port>`) when none is named; every device
/// can run its own.
pub const DEFAULT_PORT: u16 = 9008;

/// The daemon's abstract socket on the device. Must match
/// MobaxServer.socketName.
pub fn socket_name(port: u16) -> String {
    format!("localabstract:mobaxd_{port}")
}

/// Where the adb server listens: what `adb` itself would use. adb reads
/// $ADB_SERVER_SOCKET (`tcp:PORT` or `tcp:HOST:PORT`), else
/// $ANDROID_ADB_SERVER_ADDRESS and $ANDROID_ADB_SERVER_PORT, else
/// 127.0.0.1:5037.
pub fn addr() -> io::Result<SocketAddr> {
    use std::net::ToSocketAddrs;
    let env = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let (host, port) = if let Some(spec) = env("ADB_SERVER_SOCKET") {
        let rest = spec.strip_prefix("tcp:").ok_or_else(|| io::Error::other(format!(
            "ADB_SERVER_SOCKET={spec}: only tcp:[HOST:]PORT is supported")))?;
        match rest.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.to_string()),
            None => ("127.0.0.1".to_string(), rest.to_string()),
        }
    } else {
        (
            env("ANDROID_ADB_SERVER_ADDRESS").unwrap_or_else(|| "127.0.0.1".to_string()),
            env("ANDROID_ADB_SERVER_PORT").unwrap_or_else(|| "5037".to_string()),
        )
    };
    let port: u16 = port.parse().map_err(|_| io::Error::other(format!("adb server port {port:?} is not a number")))?;
    (host.as_str(), port).to_socket_addrs()?.next()
        .ok_or_else(|| io::Error::other(format!("adb server host {host:?} does not resolve")))
}

/// A `host:` query and its body (`host:devices`, `host:list-forward`, …).
pub fn query(payload: &str) -> io::Result<String> {
    let mut s = TcpStream::connect_timeout(&addr()?, Duration::from_millis(200))?;
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    s.set_write_timeout(Some(Duration::from_millis(500)))?;
    s.write_all(format!("{:04x}", payload.len()).as_bytes())?;
    s.write_all(payload.as_bytes())?;
    let mut status = [0u8; 4];
    s.read_exact(&mut status)?;
    let mut ln_buf = [0u8; 4];
    let ln = if s.read_exact(&mut ln_buf).is_ok() {
        usize::from_str_radix(std::str::from_utf8(&ln_buf)
            .map_err(|_| io::Error::other("adb-server: bad length"))?, 16)
            .map_err(|_| io::Error::other("adb-server: bad length"))?
    } else { 0 };
    let mut body = vec![0u8; ln];
    if ln > 0 { s.read_exact(&mut body)?; }
    let body = String::from_utf8(body)
        .map_err(|_| io::Error::other("adb-server: non-utf8 body"))?;
    if &status != b"OKAY" {
        return Err(io::Error::other(format!("adb-server FAIL: {body}")));
    }
    Ok(body)
}

/// Serials in the `device` state.
pub fn attached_serials() -> io::Result<Vec<String>> {
    let body = query("host:devices")?;
    Ok(body.lines().filter_map(|l| {
        let mut it = l.split_whitespace();
        let s = it.next()?;
        (it.next() == Some("device")).then(|| s.to_string())
    }).collect())
}

/// `explicit`, else the only attached device.
pub fn resolve_serial(explicit: Option<&str>) -> Result<String, Error> {
    if let Some(s) = explicit { return Ok(s.to_string()); }
    let mut found = attached_serials()?;
    match found.len() {
        0 => Err(Error::NoDevice("no devices in 'device' state".into())),
        1 => Ok(found.remove(0)),
        _ => Err(Error::WhichDevice(format!(
            "multiple devices attached ({}); pass --device SERIAL", found.join(", ")))),
    }
}

/// Runs `cmd` on the device through the server's `shell:` channel and reads
/// its output to the end. ~5-15 ms, against ~30 ms for `adb shell`.
pub fn shell(serial: &str, cmd: &str) -> io::Result<String> {
    let mut s = TcpStream::connect_timeout(&addr()?, Duration::from_millis(200))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    for (req, what) in [(format!("host:transport:{serial}"), "transport switch"), (format!("shell:{cmd}"), "shell open")] {
        s.write_all(format!("{:04x}", req.len()).as_bytes())?;
        s.write_all(req.as_bytes())?;
        let mut status = [0u8; 4];
        s.read_exact(&mut status)?;
        if &status != b"OKAY" {
            return Err(io::Error::other(format!("adb-server {what} failed")));
        }
    }
    let mut out = Vec::with_capacity(256);
    s.read_to_end(&mut out)?;
    String::from_utf8(out).map_err(|_| io::Error::other("shell: non-utf8 output"))
}

/// A stream to the daemon on `serial` through the adb server itself
/// (`host:transport:SERIAL`, then `localabstract:mobaxd_PORT`), so no host
/// port is bound. After the two OKAYs the socket is a plain byte pipe to
/// the daemon. Same data path as a forward (adb opens a forward's device
/// stream on its first byte), so a request costs the same once connected.
///
/// A refused transport is [`Error::NoDevice`]; a refused socket (the device
/// is there, nothing listens) is [`Error::NoDaemon`].
pub fn device_stream(serial: &str, port: u16) -> Result<TcpStream, Error> {
    let mut s = TcpStream::connect_timeout(&addr()?, Duration::from_millis(500))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    for (i, req) in [format!("host:transport:{serial}"), socket_name(port)].into_iter().enumerate() {
        s.write_all(format!("{:04x}{req}", req.len()).as_bytes())?;
        let mut status = [0u8; 4];
        s.read_exact(&mut status)?;
        if &status != b"OKAY" {
            let mut len = [0u8; 4];
            let n = s.read_exact(&mut len).ok()
                .and_then(|_| usize::from_str_radix(std::str::from_utf8(&len).ok()?, 16).ok())
                .unwrap_or(0);
            let mut why = vec![0u8; n.min(512)];
            let _ = s.read_exact(&mut why);
            let msg = format!("{serial}: {req}: {}", String::from_utf8_lossy(&why).trim());
            return Err(if i == 0 { Error::NoDevice(msg) } else { Error::NoDaemon(msg) });
        }
    }
    Ok(s)
}

/// One `adb forward` entry.
pub struct Forward {
    pub serial: String,
    /// `tcp:N`
    pub host_spec: String,
    /// `localabstract:…`, `tcp:N`, …
    pub remote_spec: String,
}

/// The adb server's forwards.
pub fn list_forwards() -> io::Result<Vec<Forward>> {
    Ok(parse_forwards(&query("host:list-forward")?))
}

/// `SERIAL HOST REMOTE` lines, as `host:list-forward` and `adb forward
/// --list` print them.
pub fn parse_forwards(listing: &str) -> Vec<Forward> {
    listing.lines().filter_map(|line| {
        let mut it = line.split_whitespace();
        let serial = it.next()?.to_string();
        let host_spec = it.next()?.to_string();
        let remote_spec = it.next().unwrap_or("").to_string();
        Some(Forward { serial, host_spec, remote_spec })
    }).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn forward_listing_is_parsed() {
        let f = super::parse_forwards("emulator-5554 tcp:9008 localabstract:mobaxd_9008\n\nbad\n");
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].serial.as_str(), f[0].host_spec.as_str()), ("emulator-5554", "tcp:9008"));
        assert_eq!(super::socket_name(9008), f[0].remote_spec);
    }
}
