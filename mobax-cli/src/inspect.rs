// Reading the screen: `ui`, `see`, `wait`, `find`.

use std::io::{self, Write};

use crate::*;

/// `mobax ui [--tree|--json|--xml] [--active|--all]` — UI tree dump in the
/// chosen format. The table and `--tree` cover the app's windows, so
/// popups and bottom sheets show up; `--active` limits them to the focused
/// window. `--json` / `--xml` are the raw dump: the focused window, or
/// every window with `--all`.
pub(crate) fn run_ui(host: &str, port: u16, format: UiFormat, windows: UiWindows) -> io::Result<()> {
    let raw = matches!(format, UiFormat::Json | UiFormat::Xml);
    let wire = match windows {
        UiWindows::All => "dump",
        UiWindows::App if !raw => "dump",
        _ => "dump_active",
    };
    let mut conn = Conn::connect(host, port)?;
    let body = conn.call(wire)?;
    let mut out = crate::output::stdout();
    match format {
        UiFormat::Json => {
            out.write_all(&body)?;
            if body.last() != Some(&b'\n') { out.write_all(b"\n")?; }
        }
        UiFormat::Xml => {
            let text = std::str::from_utf8(&body)
                .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
            let json = json::parse(text)
                .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
            out.write_all(xml_dump::render(&json, 0).as_bytes())?;
        }
        UiFormat::Human => {
            let text = std::str::from_utf8(&body)
                .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
            let json = json::parse(text)
                .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
            out.write_all(ui_dump::render_human(&ui_roots(&json, windows)).as_bytes())?;
        }
        UiFormat::Interactive => {
            let text = std::str::from_utf8(&body)
                .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
            let json = json::parse(text)
                .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
            out.write_all(ui_dump::render_interactive(&ui_roots(&json, windows)).as_bytes())?;
        }
    }
    out.flush()
}

pub(crate) fn ui_roots(dump: &json::Value, windows: UiWindows) -> Vec<&json::Value> {
    if windows == UiWindows::All { json::all_roots(dump) } else { json::app_roots(dump) }
}

/// `mobax see [PATH]` — bare opens the viewer; otherwise extension picks
/// the format (jpg/webp/png screenshot, xml/json UI hierarchy).
pub(crate) fn run_see(host: &str, port: u16, opts: &SeeOpts) -> io::Result<()> {
    let Some(path) = opts.path.as_deref() else {
        return mirror::run(host, port, mirror::Args);
    };
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let is_tree = matches!(ext.as_str(), "json" | "xml");
    if opts.all_windows && !is_tree {
        return Err(io::Error::other("see: --all only applies to .json/.xml"));
    }
    let dump_wire = if opts.all_windows { "dump" } else { "dump_active" };
    let mut conn = Conn::connect(host, port)?;
    let bytes: Vec<u8> = match ext.as_str() {
        "jpg" | "jpeg" => conn.call(&see_screenshot_wire(opts, "jpeg"))?,
        "webp"         => conn.call(&see_screenshot_wire(opts, "webp"))?,
        "png"          => conn.call(&see_screenshot_wire(opts, "png"))?,
        "json"         => conn.call(dump_wire)?,
        "xml" => {
            if opts.size.is_some() || opts.native || opts.secure_check {
                return Err(io::Error::other("see: screenshot flags only apply to jpg/webp/png"));
            }
            let body = conn.call(dump_wire)?;
            let text = std::str::from_utf8(&body)
                .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
            let json = json::parse(text)
                .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
            xml_dump::render(&json, 0).into_bytes()
        }
        other => return Err(io::Error::other(
            format!("see: unsupported extension '.{other}' (jpg|webp|png|xml|json)"))),
    };
    if bytes.starts_with(b"ERR:secure-window:") {
        let win = String::from_utf8_lossy(&bytes[b"ERR:secure-window:".len()..]);
        return Err(io::Error::other(format!(
            "screenshot blocked: {win} has FLAG_SECURE set — the system denies \
             screen capture of this window (common for banking, password, and \
             incognito screens). Not saving to {path}."
        )));
    }
    if bytes.starts_with(b"ERR:") {
        return Err(io::Error::other(String::from_utf8_lossy(&bytes).into_owned()));
    }
    std::fs::write(path, &bytes)?;
    crate::errln!("saved {} bytes to {path}", bytes.len());
    Ok(())
}

pub(crate) fn see_screenshot_wire(opts: &SeeOpts, fmt: &str) -> String {
    let mut wire = String::from("screenshot");
    if opts.native || opts.size.is_none() {
        wire.push_str(" max=1");
    } else if let Some(size) = opts.size {
        wire.push_str(&format!(" size={size}"));
    }
    if fmt != "jpeg" {
        wire.push_str(&format!(" fmt={fmt}"));
    }
    if opts.secure_check {
        wire.push_str(" secure_check=1");
    }
    wire
}

/// `mobax wait <spec>` — smart-dispatch on `spec` shape. `flags` lets RPA
/// scripts override the daemon-side default (10 s) so long-running things
/// like cold app launches don't silently hit the timeout.
pub(crate) fn run_wait(
    conn: &mut Conn,
    out_fmt: flags::OutFmt,
    spec: &str,
    f: &flags::ActionFlags,
) -> io::Result<()> {
    let reporter = output::Reporter::new(f.out(out_fmt));
    let spec = spec.trim();
    // "Nms" / "Ns" — sleep client-side, no daemon hop needed.
    if let Some(ms) = parse_duration_ms(spec) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        return reporter.ok("wait", "ok",
            json_out::Obj::new().n("slept_ms", ms as i64));
    }
    let timeout_ms = f.timeout_ms.unwrap_or(10_000);
    if let Some(sels) = css_selector(spec) {
        return run_wait_selector(conn, &reporter, spec, &sels, f, timeout_ms);
    }
    let wire: String = if let Some(rest) = spec.strip_prefix("idle") {
        let rest = rest.trim();
        let idle_ms = if rest.is_empty() {
            200
        } else if let Some(ms) = parse_duration_ms(rest) {
            ms
        } else {
            return Err(io::Error::other(format!("wait idle: bad duration '{rest}'")));
        };
        format!("wait_for_idle idle_ms={idle_ms} timeout_ms={timeout_ms}")
    } else if is_component(spec) || is_pkg(spec) {
        format!("wait_for_activity n={spec} timeout_ms={timeout_ms}")
    } else {
        // Same windows as tap / find: the app's, or the focused one.
        let scope = if f.active_window { "active" } else { "app" };
        format!("wait_for_text text={spec:?} scope={scope} timeout_ms={timeout_ms}")
    };
    // Retry layer: timeout exhaustion gets retried `retries` additional
    // times. For most waits this is a no-op (retries=0), but it lets RPA
    // scripts chain `--timeout 2s --retries 5` for cheap polling.
    let mut attempts = f.total_attempts();
    loop {
        let body = conn.call(&wire)?;
        if let Some(e) = errors::parse_err(&body) {
            attempts = attempts.saturating_sub(1);
            if attempts > 0 && e.code == errors::ErrCode::Timeout {
                std::thread::sleep(std::time::Duration::from_millis(f.retry_delay_ms));
                continue;
            }
            return Err(reporter.fail("wait", e));
        }
        return reporter.ok("wait", String::from_utf8_lossy(&body).trim_end(),
            json_out::Obj::new().s("spec", spec).n("timeout_ms", timeout_ms as i64));
    }
}

/// A `wait` / `fill` argument that reads as a CSS-like selector rather than text: `#id`, an
/// `[attr]` predicate, or a relational / text pseudo-class.
pub(crate) fn css_selector(spec: &str) -> Option<Vec<selector::Selector>> {
    const PSEUDOS: [&str; 6] = [":has-text(", ":text-is(", ":in(", ":below(", ":right-of(", ":near("];
    let shaped = spec.starts_with('#') || spec.contains('[') || PSEUDOS.iter().any(|p| spec.contains(p));
    shaped.then(|| selector::Selector::parse(spec).ok()).flatten()
}

/// `mobax wait SELECTOR` — poll the app's windows until a node matches.
pub(crate) fn run_wait_selector(
    conn: &mut Conn,
    reporter: &output::Reporter,
    spec: &str,
    sels: &[selector::Selector],
    f: &flags::ActionFlags,
    timeout_ms: u64,
) -> io::Result<()> {
    let start = std::time::Instant::now();
    let budget = std::time::Duration::from_millis(timeout_ms * f.total_attempts() as u64);
    loop {
        let body = conn.call(f.dump_wire())?;
        if let Some(e) = errors::parse_err(&body) {
            return Err(reporter.fail("wait", e));
        }
        let text = std::str::from_utf8(&body)
            .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
        let dump = json::parse(text)
            .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
        let ctx = selector::MatchCtx::from_roots(json::app_roots(&dump).into_iter().rev().collect());
        let mut matches = selector::find_all_with(&ctx, sels);
        selector::apply_filters(&mut matches, f.require());
        if let Some(n) = matches.first() {
            let elapsed = start.elapsed().as_millis() as i64;
            let (l, t, r, b) = selector::bounds(n).unwrap_or((0, 0, 0, 0));
            let (x, y) = ((l + r) / 2, (t + b) / 2);
            return reporter.ok("wait", &format!("ok x={x} y={y} elapsed={elapsed}"),
                json_out::Obj::new().s("spec", spec).n("x", x).n("y", y).n("elapsed_ms", elapsed));
        }
        if start.elapsed() >= budget {
            return Err(reporter.fail("wait", errors::ErrInfo::new(
                errors::ErrCode::Timeout, format!("wait_for_selector:{spec}"))));
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

/// `mobax find SELECTOR` — match a CSS-like selector against the current
/// dump_active tree and print one node summary per match. Honours
/// --visible/--clickable/--enabled/--unique/--nth.
pub(crate) fn run_find(
    conn: &mut Conn,
    out_fmt: flags::OutFmt,
    sel: &str,
    f: &flags::ActionFlags,
) -> io::Result<()> {
    let reporter = output::Reporter::new(f.out(out_fmt));
    let selectors = match selector::Selector::parse(sel) {
        Ok(s) => s,
        Err(e) => return Err(reporter.fail("find",
            errors::ErrInfo::new(errors::ErrCode::BadArg, e))),
    };
    let body = conn.call(f.dump_wire())?;
    if let Some(e) = errors::parse_err(&body) {
        return Err(reporter.fail("find", e));
    }
    let text = std::str::from_utf8(&body)
        .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
    let json = json::parse(text)
        .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
    // App windows, topmost first — the order `mobax tap` prefers.
    let ctx = selector::MatchCtx::from_roots(json::app_roots(&json).into_iter().rev().collect());
    let mut matches = selector::find_all_with(&ctx, &selectors);
    selector::apply_filters(&mut matches, f.require());

    if matches.is_empty() {
        return Err(reporter.fail("find",
            errors::ErrInfo::new(errors::ErrCode::NotFound,
                format!("no node matched '{sel}'"))));
    }
    if f.require_unique && matches.len() > 1 {
        return Err(reporter.fail("find",
            errors::ErrInfo::new(errors::ErrCode::Ambiguous,
                format!("--unique: '{sel}' matched {} nodes", matches.len()))));
    }
    if let Some(n) = f.nth {
        if n == 0 || n > matches.len() {
            return Err(reporter.fail("find",
                errors::ErrInfo::new(errors::ErrCode::NotFound,
                    format!("--nth {n} out of range (have {})", matches.len()))));
        }
        let picked = matches[n - 1];
        matches.clear();
        matches.push(picked);
    }

    // Output: either one human row per match, or one JSON line per match
    // (so machine consumers can read with --json | jq -c).
    match f.out(out_fmt) {
        flags::OutFmt::Human => {
            let mut out = crate::output::stdout();
            for n in &matches {
                write_find_row(&mut out, n)?;
            }
            out.flush()?;
        }
        flags::OutFmt::Json => {
            let mut out = crate::output::stdout();
            for n in &matches {
                let row = find_row_obj(n).finish();
                let line = json_out::Obj::new()
                    .s("verb", "find")
                    .b("ok", true)
                    .raw("result", &row)
                    .finish();
                writeln!(out, "{line}")?;
            }
            out.flush()?;
        }
    }
    Ok(())
}

pub(crate) fn write_find_row(out: &mut impl Write, n: &json::Value) -> io::Result<()> {
    let cls = selector::get_str(n, "cls").unwrap_or("");
    let id  = selector::get_str(n, "rid").unwrap_or("");
    let t   = selector::get_str(n, "text").unwrap_or("");
    let d   = selector::get_str(n, "desc").unwrap_or("");
    let bs  = match selector::bounds(n) {
        Some((x1, y1, x2, y2)) => {
            let cx = (x1 + x2) / 2;
            let cy = (y1 + y2) / 2;
            format!("[{x1},{y1}][{x2},{y2}] center=({cx},{cy})")
        }
        None => "[]".into(),
    };
    write!(out, "{bs}\tclass={cls}\tid={id}\ttext={t:?}\tdesc={d:?}")?;
    for key in ["role", "state", "actions"] {
        if let Some(v) = selector::get_str(n, key) {
            write!(out, "\t{key}={v:?}")?;
        }
    }
    writeln!(out)
}

pub(crate) fn find_row_obj(n: &json::Value) -> json_out::Obj {
    let mut o = json_out::Obj::new()
        .s("class", selector::get_str(n, "cls").unwrap_or(""))
        .s("id",    selector::get_str(n, "rid").unwrap_or(""))
        .s("text",  selector::get_str(n, "text").unwrap_or(""))
        .s("desc",  selector::get_str(n, "desc").unwrap_or(""))
        .s("flags", selector::get_str(n, "flags").unwrap_or(""));
    for key in ["role", "state", "error", "range", "collection", "item", "actions"] {
        o = o.opt_s(key, selector::get_str(n, key));
    }
    if let Some((x1, y1, x2, y2)) = selector::bounds(n) {
        let cx = (x1 + x2) / 2;
        let cy = (y1 + y2) / 2;
        o = o.n("x1", x1).n("y1", y1).n("x2", x2).n("y2", y2)
             .n("cx", cx).n("cy", cy);
    }
    o
}
