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

use crate::json::{self, Value};
use crate::json_out::Obj;
use crate::mcp_screen::{Change, Screen};
use crate::Conn;

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
        let (seq, ev, dump) = parse_frame(&frame)?;
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

/// `{"seq":N,"ev":E,"dump":{…}}` → (N, E, dump).
pub(crate) fn parse_frame(frame: &[u8]) -> io::Result<(i64, i64, Value)> {
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

#[cfg(test)]
mod tests {
    #[test]
    fn frames_carry_seq_events_and_the_dump() {
        let (seq, ev, dump) = super::parse_frame(br#"{"seq":3,"ev":812,"dump":{"ts":1,"windows":[]}}"#).unwrap();
        assert_eq!((seq, ev), (3, 812));
        assert!(crate::json::obj_get(&dump, "windows").is_some());
        assert!(super::parse_frame(br#"{"seq":1}"#).is_err());
    }
}
