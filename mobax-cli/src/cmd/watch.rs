// `mobax watch [--json] [--min-ms N] [--moves]` — follow the screen as it changes, pushed by the
// daemon (`ui_watch`), until interrupted. For code that reacts to the
// screen without asking (a test harness, a reflex loop, a dashboard).
//
// Each frame is shown against the one before it, as the MCP server's
// action replies are: the whole table on a new screen, else the rows that
// were added (+), changed (~) or removed (-). Rows carry stable @N refs.
// With --json each change is one line of NDJSON:
//
//   {"seq":3,"ev":812,"pkg":"com.android.settings","change":"new","rows":[{"ref":1,"line":"tap …","x":540,"y":178},…]}
//   {"seq":4,"ev":830,"pkg":"com.android.settings","change":"rows","lines":["~ @3 …","+ @21 …"]}

use std::io::{self, Write};

use crate::json_out::Obj;
use crate::mcp_screen::{Change, Screen};
use crate::Conn;
use crate::Connect;

pub(crate) fn run(host: &str, port: u16, json_out: bool, min_ms: Option<u64>, moves: bool) -> io::Result<()> {
    let mut conn = Conn::connect(host, port)?;
    // A frame may be far apart from the next on a still screen.
    conn.set_read_timeout(None)?;
    let mut cmd = "ui_watch".to_string();
    if let Some(ms) = min_ms {
        cmd.push_str(&format!(" min_ms={ms}"));
    }
    if moves {
        cmd.push_str(" moves=1");
    }
    conn.send_cmd(&cmd)?;
    let mut prev: Option<Screen> = None;
    let mut out = crate::output::stdout();
    loop {
        let frame = conn.read_frame()?;
        if frame.starts_with(b"ERR:") {
            return Err(io::Error::other(String::from_utf8_lossy(&frame).into_owned()));
        }
        let (seq, ev, dump) = mobax_core::wire::parse_watch_frame(&frame)?;
        let screen = Screen::from_dump(&dump, prev.as_ref());
        let change = screen.change_from(prev.as_ref());
        if json_out {
            let head = Obj::new().n("seq", seq).n("ev", ev).s("pkg", &screen.pkg).b("keyboard", screen.keyboard);
            let line = match change {
                Change::Same => None,
                Change::New => Some(head.s("change", "new").raw("rows", &screen.rows_json()).finish()),
                Change::Rows(lines) => Some(head.s("change", "rows").raw("lines", &json_strings(&lines)).finish()),
            };
            if let Some(l) = line {
                writeln!(out, "{l}")?;
            }
        } else {
            match change {
                Change::Same => {}
                Change::New => writeln!(out, "#{seq} {} · new screen, {} rows\n{}", screen.title(), screen.len(), screen.table().trim_end())?,
                Change::Rows(lines) => writeln!(out, "#{seq} {}\n{}", screen.title(), lines.join("\n"))?,
            }
        }
        out.flush()?;
        prev = Some(screen);
    }
}


fn json_strings(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        crate::json_out::encode_str(&mut out, s);
    }
    out.push(']');
    out
}
