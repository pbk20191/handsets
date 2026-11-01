// A connection to the daemon and its framing: `uint32 BE length + payload`
// both ways. Requests are ASCII command lines (`tap x=1 y=2`); replies are
// one frame, or for streaming verbs `[len][chunk]…[len=0]`.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::adb_server;
use crate::errors::Error;
use crate::json::{self, Value};

pub struct Conn {
    sock: TcpStream,
}

impl Conn {
    /// The daemon on device `serial` (instance `port`), through the adb
    /// server: no host port is bound.
    pub fn device(serial: &str, port: u16) -> Result<Self, Error> {
        Ok(Self::from_stream(adb_server::device_stream(serial, port)?)?)
    }

    /// A daemon on a TCP port (a remote host's forward).
    pub fn tcp(host: &str, port: u16) -> io::Result<Self> {
        Self::from_stream(TcpStream::connect((host, port))?)
    }

    pub fn from_stream(sock: TcpStream) -> io::Result<Self> {
        sock.set_nodelay(true)?;
        sock.set_read_timeout(Some(Duration::from_secs(30)))?;
        sock.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self { sock })
    }

    /// False once the peer has closed (or sent bytes nobody asked for): a
    /// pooled socket idle across a daemon restart.
    pub fn is_alive(&self) -> bool {
        if self.sock.set_nonblocking(true).is_err() { return false; }
        let mut b = [0u8; 1];
        let alive = matches!(self.sock.peek(&mut b), Err(ref e) if e.kind() == io::ErrorKind::WouldBlock);
        self.sock.set_nonblocking(false).is_ok() && alive
    }

    /// One command, one reply frame.
    pub fn call(&mut self, cmd: &str) -> io::Result<Vec<u8>> {
        self.send_cmd(cmd)?;
        self.read_frame()
    }

    /// `None` waits as long as it takes (a push stream on a still screen).
    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.sock.set_read_timeout(t)
    }

    /// Length-prefixed command write; doesn't read a reply.
    pub fn send_cmd(&mut self, cmd: &str) -> io::Result<()> {
        let payload = cmd.as_bytes();
        let len = u32::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cmd too long"))?;
        self.sock.write_all(&len.to_be_bytes())?;
        self.sock.write_all(payload)?;
        Ok(())
    }

    /// Reads one length-prefixed reply frame.
    pub fn read_frame(&mut self) -> io::Result<Vec<u8>> {
        let mut hdr = [0u8; 4];
        self.sock.read_exact(&mut hdr)?;
        let n = u32::from_be_bytes(hdr) as usize;
        if n > 256 * 1024 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("oversized response: {n} bytes")));
        }
        let mut buf = vec![0u8; n];
        self.sock.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Drains a server-streamed chunked response into `out`: `[len][chunk]…`
    /// until `len == 0`. A leading `ERR:` payload is surfaced as an error.
    pub fn recv_chunks_to(&mut self, out: &mut dyn Write) -> io::Result<u64> {
        let mut total = 0u64;
        let mut first = true;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let mut hdr = [0u8; 4];
            self.sock.read_exact(&mut hdr)?;
            let n = u32::from_be_bytes(hdr) as usize;
            if n == 0 {
                return Ok(total);
            }
            if n > 8 * 1024 * 1024 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("oversized chunk: {n} bytes")));
            }
            if buf.len() < n { buf.resize(n, 0); }
            self.sock.read_exact(&mut buf[..n])?;
            if first && n >= 4 && &buf[..4] == b"ERR:" {
                let msg = String::from_utf8_lossy(&buf[..n]).into_owned();
                // Drain the terminating 0-frame so the socket stays in sync.
                let mut h2 = [0u8; 4];
                let _ = self.sock.read_exact(&mut h2);
                return Err(io::Error::other(msg));
            }
            first = false;
            out.write_all(&buf[..n])?;
            total += n as u64;
        }
    }

    /// Streams `src` to the server as `[len][chunk]…[len=0]` frames.
    pub fn send_chunks_from(&mut self, src: &mut dyn Read) -> io::Result<u64> {
        let mut total = 0u64;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                self.sock.write_all(&0u32.to_be_bytes())?;
                return Ok(total);
            }
            let len = u32::try_from(n).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "chunk too large"))?;
            self.sock.write_all(&len.to_be_bytes())?;
            self.sock.write_all(&buf[..n])?;
            total += n as u64;
        }
    }
}

/// A `ui_watch` frame, `{"seq":N,"ev":E,"dump":{…}}` → (N, E, dump).
pub fn parse_watch_frame(frame: &[u8]) -> io::Result<(i64, i64, Value)> {
    let text = std::str::from_utf8(frame).map_err(|e| io::Error::other(format!("ui_watch frame not utf-8: {e}")))?;
    let v = json::parse(text).map_err(|e| io::Error::other(format!("ui_watch frame not json: {e}")))?;
    let num = |k: &str| json::obj_get(&v, k).and_then(json::as_num).unwrap_or(0);
    let (seq, ev) = (num("seq"), num("ev"));
    let dump = match v {
        Value::Obj(pairs) => pairs.into_iter().find(|(k, _)| k == "dump").map(|(_, d)| d),
        _ => None,
    }
    .ok_or_else(|| io::Error::other("ui_watch frame has no dump"))?;
    Ok((seq, ev, dump))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn frames_round_trip_over_a_socket() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut hdr = [0u8; 4];
            s.read_exact(&mut hdr).unwrap();
            let mut cmd = vec![0u8; u32::from_be_bytes(hdr) as usize];
            s.read_exact(&mut cmd).unwrap();
            assert_eq!(cmd, b"ping");
            s.write_all(&4u32.to_be_bytes()).unwrap();
            s.write_all(b"pong").unwrap();
        });
        let mut c = super::Conn::tcp("127.0.0.1", port).unwrap();
        assert_eq!(c.call("ping").unwrap(), b"pong");
        server.join().unwrap();
    }

    #[test]
    fn watch_frames_carry_seq_events_and_the_dump() {
        let (seq, ev, dump) = super::parse_watch_frame(br#"{"seq":3,"ev":812,"dump":{"ts":1,"windows":[]}}"#).unwrap();
        assert_eq!((seq, ev), (3, 812));
        assert!(crate::json::obj_get(&dump, "windows").is_some());
        assert!(super::parse_watch_frame(br#"{"seq":1}"#).is_err());
    }
}
