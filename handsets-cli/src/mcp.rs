// `hs mcp` — a Model Context Protocol server on stdio, so MCP hosts (Claude
// Desktop, the .mcpb bundle, …) can drive a device with the same verbs as
// the CLI.
//
// Transport: newline-delimited JSON-RPC 2.0 on stdin/stdout; logs go to
// stderr. Every tool call runs this same `hs` binary as a child process
// (`hs [GLOBAL…] VERB ARGS…`), so tools behave exactly like the CLI —
// selectors, timeouts, exit codes and the agent-oriented text output —
// and a crashing verb can't take the server down. Screenshots come back as
// MCP image content.

use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::json::{self, Value};
use crate::json_out::encode_str;

/// Protocol revisions this server speaks; the newest is offered when the
/// client asks for one we don't know.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Upper bound for one tool call (a verb's own --timeout is usually far
/// lower); stops a stuck child from wedging the server.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// First line of `hs --help`, printed after argument errors.
const USAGE_HEAD: &str = "hs — drive Android from the shell.";

/// Verbs the generic `hs` tool refuses: interactive or never-ending, or
/// they would open a window or recurse.
const BLOCKED_VERBS: &[&str] = &["mcp", "shell", "tui", "mirror", "fan"];

const INSTRUCTIONS: &str = "Drive an Android device or emulator. Call `use` first \
to connect and start the on-device daemon. Then loop: `ui` (or `screenshot`) to \
see the screen, `tap` / `type` / `fill` / `swipe` / `go` to act, `wait` to confirm. \
Selectors: plain text (\"Sign in\"), #resource_id, or CSS-like \
'Button[text=\"OK\"]', 'EditText:below(TextView[text=Email])'.";

/// `hs mcp [GLOBAL OPTIONS]`: serves until stdin closes. The options
/// (e.g. `--device SERIAL`, `--port N`) are passed to every tool call.
pub(crate) fn serve(global: &[String]) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(&line, global) {
            stdout.write_all(reply.as_bytes())?;
            stdout.write_all(b"\n")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// One JSON-RPC message in, at most one reply out (none for notifications).
fn handle(line: &str, global: &[String]) -> Option<String> {
    let msg = match json::parse(line) {
        Ok(v) => v,
        Err(e) => return Some(error_reply("null", -32700, &format!("parse error: {e}"))),
    };
    let id = json::obj_get(&msg, "id").map(to_json);
    let method = json::obj_get(&msg, "method").and_then(json::as_str).unwrap_or("");
    let params = json::obj_get(&msg, "params");
    // Notifications (no id) get no reply.
    let id = id?;
    let result = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok("{}".to_string()),
        "tools/list" => Ok(tools_list()),
        "tools/call" => tools_call(params, global),
        "" => Err((-32600, "invalid request: no method".to_string())),
        m => Err((-32601, format!("method not found: {m}"))),
    };
    Some(match result {
        Ok(r) => format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{r}}}"#),
        Err((code, msg)) => error_reply(&id, code, &msg),
    })
}

fn error_reply(id: &str, code: i32, msg: &str) -> String {
    let mut m = String::new();
    encode_str(&mut m, msg);
    format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":{code},"message":{m}}}}}"#)
}

fn initialize(params: Option<&Value>) -> String {
    let asked = params
        .and_then(|p| json::obj_get(p, "protocolVersion"))
        .and_then(json::as_str)
        .unwrap_or("");
    let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
    let mut instructions = String::new();
    encode_str(&mut instructions, INSTRUCTIONS);
    format!(
        r#"{{"protocolVersion":"{version}","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"handsets","version":"{}"}},"instructions":{instructions}}}"#,
        env!("CARGO_PKG_VERSION")
    )
}

// ---------- tools ----------

struct Tool {
    name: &'static str,
    description: &'static str,
    /// JSON Schema of the arguments object.
    schema: &'static str,
}

const DEVICE: &str = r#""device":{"type":"string","description":"Device serial (adb devices); omit for the current one"}"#;
const TIMEOUT: &str = r#""timeout_ms":{"type":"integer","description":"Wait budget in ms (default 10000)"}"#;

fn tools() -> Vec<Tool> {
    // Schemas are assembled at startup from the shared fragments above.
    fn obj(props: &[&str], required: &[&str]) -> &'static str {
        let req: Vec<String> = required.iter().map(|r| format!("\"{r}\"")).collect();
        Box::leak(
            format!(
                r#"{{"type":"object","properties":{{{}}},"required":[{}],"additionalProperties":false}}"#,
                props.join(","),
                req.join(",")
            )
            .into_boxed_str(),
        )
    }
    vec![
        Tool {
            name: "use",
            description: "Connect to a device (or the only one attached) and start the on-device daemon. Call this first.",
            schema: obj(&[r#""serial":{"type":"string","description":"Device serial from adb devices; omit when only one is attached"}"#], &[]),
        },
        Tool {
            name: "ui",
            description: "List what is on screen as a flat table: tappable nodes with their text, #resource-id and centre x,y. The main way to read the screen.",
            schema: obj(&[r#""scope":{"type":"string","enum":["app","active","all"],"description":"app: every app window incl. dialogs (default); active: focused window; all: system windows too"}"#, DEVICE], &[]),
        },
        Tool {
            name: "screenshot",
            description: "Capture the screen as a JPEG image (768 px long edge by default).",
            schema: obj(&[r#""size":{"type":"integer","description":"Long edge in px"}"#, r#""native":{"type":"boolean","description":"Full device resolution"}"#, DEVICE], &[]),
        },
        Tool {
            name: "find",
            description: "Find nodes matching a selector and print them, without acting.",
            schema: obj(&[r#""selector":{"type":"string","description":"Text, #resource_id, or CSS-like selector"}"#, TIMEOUT, DEVICE], &["selector"]),
        },
        Tool {
            name: "tap",
            description: "Tap a node by text / #resource_id / selector, or tap coordinates x,y.",
            schema: obj(&[r#""target":{"type":"string","description":"Text, #resource_id, or selector"}"#, r#""x":{"type":"integer"}"#, r#""y":{"type":"integer"}"#, TIMEOUT, DEVICE], &[]),
        },
        Tool {
            name: "type",
            description: "Type text into the focused field (keystrokes).",
            schema: obj(&[r#""text":{"type":"string"}"#, DEVICE], &["text"]),
        },
        Tool {
            name: "fill",
            description: "Set the text of an edit field in one step (ACTION_SET_TEXT). The selector may be free text that picks the best-matching EditText.",
            schema: obj(&[r#""selector":{"type":"string"}"#, r#""text":{"type":"string"}"#, DEVICE], &["selector", "text"]),
        },
        Tool {
            name: "swipe",
            description: "Swipe in a direction (the finger's direction: up scrolls content down).",
            schema: obj(&[r#""direction":{"type":"string","enum":["up","down","left","right"]}"#, r#""duration_ms":{"type":"integer"}"#, DEVICE], &["direction"]),
        },
        Tool {
            name: "go",
            description: "Press a navigation / system key: back, home, recents, enter, …",
            schema: obj(&[r#""key":{"type":"string","description":"back, home, recents, enter, menu, …"}"#, DEVICE], &["key"]),
        },
        Tool {
            name: "wait",
            description: "Wait until text / a selector appears on screen.",
            schema: obj(&[r#""target":{"type":"string"}"#, TIMEOUT, DEVICE], &["target"]),
        },
        Tool {
            name: "open",
            description: "Launch an app by package name (com.android.settings), PKG/.Activity, or app label.",
            schema: obj(&[r#""app":{"type":"string"}"#, DEVICE], &["app"]),
        },
        Tool {
            name: "close",
            description: "Force-stop an app by package name.",
            schema: obj(&[r#""package":{"type":"string"}"#, DEVICE], &["package"]),
        },
        Tool {
            name: "logcat",
            description: "Recent device log lines (logcat), newest last. Filter by app, priority, tags or a pattern; use crash=true for crash stacks.",
            schema: obj(&[
                r#""package":{"type":"string","description":"Only this app's running process"}"#,
                r#""level":{"type":"string","enum":["V","D","I","W","E","F"],"description":"Minimum priority"}"#,
                r#""tags":{"type":"array","items":{"type":"string"},"description":"Only these tags (TAG or TAG:LEVEL)"}"#,
                r#""grep":{"type":"string","description":"Regex the line must match (one word, no spaces)"}"#,
                r#""lines":{"type":"integer","description":"How many lines (default 100)"}"#,
                r#""crash":{"type":"boolean","description":"Read the crash buffer"}"#,
                r#""clear":{"type":"boolean","description":"Clear the log buffers instead of reading"}"#,
                DEVICE,
            ], &[]),
        },
        Tool {
            name: "info",
            description: "Device and daemon info: model, Android version, display size, foreground app.",
            schema: obj(&[DEVICE], &[]),
        },
        Tool {
            name: "drop",
            description: "Stop the on-device daemon and remove its port forward.",
            schema: obj(&[DEVICE], &[]),
        },
        Tool {
            name: "hs",
            description: "Run any other hs CLI verb, e.g. [\"apps\",\"--3rd\"], [\"prop\",\"ro.build.version.sdk\"], [\"logs\",\"--tail\",\"50\"]. See `hs --help`.",
            schema: obj(&[r#""args":{"type":"array","items":{"type":"string"},"description":"Verb and arguments, without the leading hs"}"#, DEVICE], &["args"]),
        },
    ]
}

fn tools_list() -> String {
    let mut out = String::from(r#"{"tools":["#);
    for (i, t) in tools().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let (mut name, mut desc) = (String::new(), String::new());
        encode_str(&mut name, t.name);
        encode_str(&mut desc, t.description);
        out.push_str(&format!(r#"{{"name":{name},"description":{desc},"inputSchema":{}}}"#, t.schema));
    }
    out.push_str("]}");
    out
}

fn tools_call(params: Option<&Value>, global: &[String]) -> Result<String, (i32, String)> {
    let params = params.ok_or((-32602, "tools/call needs params".to_string()))?;
    let name = json::obj_get(params, "name").and_then(json::as_str).unwrap_or("");
    let empty = Value::Obj(Vec::new());
    let args = json::obj_get(params, "arguments").unwrap_or(&empty);
    let s = |k: &str| json::obj_get(args, k).and_then(json::as_str).map(str::to_string);
    let n = |k: &str| json::obj_get(args, k).and_then(json::as_num);
    let b = |k: &str| matches!(json::obj_get(args, k), Some(Value::Bool(true)));
    let need = |k: &str| s(k).ok_or_else(|| (-32602, format!("{name}: missing \"{k}\"")));

    let mut argv: Vec<String> = Vec::new();
    let timeout = |argv: &mut Vec<String>| {
        if let Some(t) = n("timeout_ms") {
            argv.extend(["--timeout".to_string(), t.to_string()]);
        }
    };
    match name {
        "use" => {
            argv.push("use".into());
            argv.extend(s("serial"));
        }
        "ui" => {
            argv.push("ui".into());
            match s("scope").as_deref() {
                Some("active") => argv.push("--active".into()),
                Some("all") => argv.push("--all".into()),
                _ => {}
            }
        }
        "screenshot" => return screenshot(n("size"), b("native"), s("device"), global),
        "find" => {
            argv.extend(["find".into(), need("selector")?]);
            timeout(&mut argv);
        }
        "tap" => {
            argv.push("tap".into());
            match (s("target"), n("x"), n("y")) {
                (Some(t), _, _) => argv.push(t),
                (None, Some(x), Some(y)) => argv.extend([x.to_string(), y.to_string()]),
                _ => return Err((-32602, "tap: give \"target\" or both \"x\" and \"y\"".into())),
            }
            timeout(&mut argv);
        }
        "type" => argv.extend(["type".into(), need("text")?]),
        "fill" => argv.extend(["fill".into(), need("selector")?, need("text")?]),
        "swipe" => {
            argv.extend(["swipe".into(), need("direction")?]);
            argv.extend(n("duration_ms").map(|d| d.to_string()));
        }
        "go" => argv.extend(["go".into(), need("key")?]),
        "wait" => {
            argv.extend(["wait".into(), need("target")?]);
            timeout(&mut argv);
        }
        "open" => argv.extend(["open".into(), need("app")?]),
        "close" => argv.extend(["close".into(), need("package")?]),
        "logcat" => {
            argv.push("logs".into());
            if let Some(p) = s("package") {
                argv.extend(["--pkg".into(), p]);
            }
            if let Some(l) = s("level") {
                argv.extend(["--level".into(), l]);
            }
            for t in json::obj_get(args, "tags").and_then(json::as_arr).unwrap_or(&[]).iter().filter_map(json::as_str) {
                argv.extend(["--tag".into(), t.to_string()]);
            }
            if let Some(g) = s("grep") {
                argv.extend(["--grep".into(), g]);
            }
            if let Some(n) = n("lines") {
                argv.extend(["--tail".into(), n.to_string()]);
            }
            if b("crash") {
                argv.push("--crash".into());
            }
            if b("clear") {
                argv.push("--clear".into());
            }
        }
        "info" => argv.push("info".into()),
        "drop" => argv.push("drop".into()),
        "hs" => {
            let list = json::obj_get(args, "args").and_then(json::as_arr).unwrap_or(&[]);
            argv = list.iter().filter_map(json::as_str).map(str::to_string).collect();
            let verb = argv.first().map(String::as_str).unwrap_or("");
            if verb.is_empty() {
                return Err((-32602, "hs: \"args\" needs at least a verb".into()));
            }
            if BLOCKED_VERBS.contains(&verb) || (verb == "see" && argv.len() == 1) {
                return Ok(text_result(&format!("`hs {verb}` is interactive or opens a window; it isn't available over MCP"), true));
            }
        }
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    }
    let out = run_hs(global, s("device"), &argv);
    Ok(text_result(&out.text, !out.ok))
}

fn screenshot(size: Option<i64>, native: bool, device: Option<String>, global: &[String]) -> Result<String, (i32, String)> {
    let path = std::env::temp_dir().join(format!("hs-mcp-{}-{}.jpg", std::process::id(), now_ns()));
    let mut argv = vec!["see".to_string(), path.to_string_lossy().into_owned()];
    if native {
        argv.push("--native".into());
    } else if let Some(n) = size {
        argv.extend(["--size".into(), n.to_string()]);
    }
    let out = run_hs(global, device, &argv);
    let bytes = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    match bytes {
        Ok(b) if out.ok && !b.is_empty() => Ok(format!(
            r#"{{"content":[{{"type":"image","mimeType":"image/jpeg","data":"{}"}}],"isError":false}}"#,
            base64(&b)
        )),
        _ => Ok(text_result(&out.text, true)),
    }
}

fn text_result(text: &str, is_error: bool) -> String {
    let mut t = String::new();
    encode_str(&mut t, if text.is_empty() { "ok" } else { text });
    format!(r#"{{"content":[{{"type":"text","text":{t}}}],"isError":{is_error}}}"#)
}

struct Output {
    ok: bool,
    text: String,
}

/// Runs `hs [GLOBAL] [-s DEVICE] ARGS…` with no stdin; stdout and stderr
/// are merged into the tool result, and a non-zero exit marks it an error.
///
/// Output goes to a temp file, not a pipe: `hs use` leaves long-lived
/// processes behind (the state-cache helper, an adb server) that inherit
/// the child's handles, so a pipe would never reach EOF.
fn run_hs(global: &[String], device: Option<String>, argv: &[String]) -> Output {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return Output { ok: false, text: format!("cannot locate hs: {e}") },
    };
    let log = std::env::temp_dir().join(format!("hs-mcp-{}-{}.out", std::process::id(), now_ns()));
    let file = match std::fs::File::create(&log) {
        Ok(f) => f,
        Err(e) => return Output { ok: false, text: format!("cannot create {}: {e}", log.display()) },
    };
    let err_file = match file.try_clone() {
        Ok(f) => f,
        Err(e) => return Output { ok: false, text: format!("cannot duplicate output handle: {e}") },
    };
    let mut cmd = Command::new(exe);
    cmd.args(global);
    if let Some(d) = device {
        cmd.args(["-s", &d]);
    }
    cmd.args(argv)
        // Plain text is the agent-oriented format; ignore a JSON default.
        .env_remove("HS_FORMAT")
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(err_file));
    let status = match cmd.spawn() {
        Ok(mut child) => {
            let deadline = Instant::now() + CALL_TIMEOUT;
            loop {
                match child.try_wait() {
                    Ok(Some(s)) => break Some(s),
                    Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                }
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(&log);
            return Output { ok: false, text: format!("cannot run hs: {e}") };
        }
    };
    let mut text = std::fs::read(&log).map(|b| String::from_utf8_lossy(&b).trim_end().to_string()).unwrap_or_default();
    // A parse error prints the whole usage after the message; keep the message.
    if let Some(i) = text.find(USAGE_HEAD) {
        text.truncate(i);
        text = text.trim_end().to_string();
    }
    // A helper left running may still hold the file on Windows; the temp
    // dir is cleaned eventually either way.
    let _ = std::fs::remove_file(&log);
    match status {
        Some(s) => Output { ok: s.success(), text },
        None => Output { ok: false, text: format!("timed out after {}s
{text}", CALL_TIMEOUT.as_secs()) },
    }
}

// ---------- helpers ----------

/// Re-serializes a parsed JSON value (used to echo request ids verbatim).
fn to_json(v: &Value) -> String {
    let mut out = String::new();
    write_json(&mut out, v);
    out
}

fn write_json(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => out.push_str(&n.to_string()),
        Value::Float(f) => out.push_str(&f.to_string()),
        Value::Str(s) => encode_str(out, s),
        Value::Arr(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, it);
            }
            out.push(']');
        }
        Value::Obj(pairs) => {
            out.push('{');
            for (i, (k, val)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_str(out, k);
                out.push(':');
                write_json(out, val);
            }
            out.push('}');
        }
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn initialize_echoes_known_version() {
        let r = handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#, &[]).unwrap();
        assert!(r.contains(r#""protocolVersion":"2025-03-26""#), "{r}");
        let r = handle(r#"{"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#, &[]).unwrap();
        assert!(r.contains(r#""id":"a""#) && r.contains(PROTOCOL_VERSIONS[0]), "{r}");
    }

    #[test]
    fn notifications_get_no_reply() {
        assert!(handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &[]).is_none());
    }

    #[test]
    fn tools_list_is_valid_json() {
        let r = handle(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &[]).unwrap();
        let v = json::parse(&r).expect("valid JSON");
        let tools = json::obj_get(json::obj_get(&v, "result").unwrap(), "tools").and_then(json::as_arr).unwrap();
        assert_eq!(tools.len(), super::tools().len());
    }

    /// The .mcpb manifest lists the tools for hosts to show before the
    /// server runs; keep it in step with `tools()`.
    #[test]
    fn mcpb_manifest_lists_every_tool() {
        let manifest = json::parse(include_str!("../../mcpb/manifest.json")).expect("manifest is JSON");
        let listed: Vec<&str> = json::obj_get(&manifest, "tools")
            .and_then(json::as_arr)
            .unwrap()
            .iter()
            .filter_map(|t| json::obj_get(t, "name").and_then(json::as_str))
            .collect();
        let served: Vec<&str> = super::tools().iter().map(|t| t.name).collect();
        assert_eq!(listed, served);
    }

    #[test]
    fn unknown_method_and_bad_args() {
        let r = handle(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#, &[]).unwrap();
        assert!(r.contains("-32601"), "{r}");
        let r = handle(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"tap","arguments":{}}}"#, &[]).unwrap();
        assert!(r.contains("-32602"), "{r}");
        let r = handle(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"hs","arguments":{"args":["tui"]}}}"#, &[]).unwrap();
        assert!(r.contains(r#""isError":true"#), "{r}");
    }
}
