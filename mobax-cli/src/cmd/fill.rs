// `fill` / `type --into` / `submit` / `paste`: finding the input a label
// or hint names, then typing into it.

use std::io;

use crate::*;

pub(crate) fn run_submit(opts: &Opts, sel: Option<&str>, flags: &flags::ActionFlags) -> io::Result<()> {
    let reporter = output::Reporter::new(flags.out(opts.out_fmt));
    let mut conn = Conn::connect(&opts.host, opts.port)?;
    let mut attempts = flags.total_attempts();
    loop {
        let wire = match sel {
            Some(s) => format!("submit {s}"),
            None => "submit".into(),
        };
        let body = conn.call(&wire)?;
        if let Some(e) = errors::parse_err(&body) {
            attempts = attempts.saturating_sub(1);
            if attempts > 0 {
                std::thread::sleep(std::time::Duration::from_millis(flags.retry_delay_ms));
                continue;
            }
            return Err(reporter.fail("submit", e));
        }
        return reporter.ok("submit", String::from_utf8_lossy(&body).trim_end(),
            json_out::Obj::new().opt_s("selector", sel));
    }
}

pub(crate) fn run_paste(opts: &Opts, sel: Option<&str>, flags: &flags::ActionFlags) -> io::Result<()> {
    let reporter = output::Reporter::new(flags.out(opts.out_fmt));
    let mut conn = Conn::connect(&opts.host, opts.port)?;
    let mut attempts = flags.total_attempts();
    loop {
        let wire = match sel {
            Some(s) => format!("paste {s}"),
            None => "paste".into(),
        };
        let body = conn.call(&wire)?;
        if let Some(e) = errors::parse_err(&body) {
            attempts = attempts.saturating_sub(1);
            if attempts > 0 {
                std::thread::sleep(std::time::Duration::from_millis(flags.retry_delay_ms));
                continue;
            }
            return Err(reporter.fail("paste", e));
        }
        return reporter.ok("paste", String::from_utf8_lossy(&body).trim_end(),
            json_out::Obj::new().opt_s("selector", sel));
    }
}

pub(crate) fn run_type_focused(opts: &Opts, text: &str, flags: &flags::ActionFlags) -> io::Result<()> {
    let reporter = output::Reporter::new(flags.out(opts.out_fmt));
    let mut conn = Conn::connect(&opts.host, opts.port)?;
    let body = conn.call(&format!("text {text}"))?;
    if let Some(e) = errors::parse_err(&body) {
        return Err(reporter.fail("type", e));
    }
    reporter.ok("type", String::from_utf8_lossy(&body).trim_end(),
        json_out::Obj::new().s("text", text))
}

/// `mobax fill` accepts both grammars. Any selector containing `=` is treated
/// as daemon-grammar (`id=…`, `class=EditText text~=…`) and passed through
/// unchanged. Bare strings are free-text queries: dump the app's windows
/// (the focused one with `--active`), find the best-matching input widget, then rebuild a daemon selector
/// (preferring `id=<full-rid>`, falling back to class + exact text) so the
/// daemon's narrower parser can still resolve it.
pub(crate) fn resolve_fill_selector(conn: &mut Conn, sel_in: &str, flags: &flags::ActionFlags) -> io::Result<String> {
    let css = css_selector(sel_in);
    if css.is_none() && sel_in.contains('=') {
        return Ok(sel_in.to_string());
    }
    // The field may still be on its way in (a search view opening), so
    // look until the wait budget runs out, as `tap` and `wait` do.
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(flags.timeout_ms.unwrap_or(10_000));
    loop {
        let body = conn.call(flags.dump_wire())?;
        if errors::parse_err(&body).is_some() {
            // Couldn't read the tree — fall through and let the daemon error.
            return Ok(sel_in.to_string());
        }
        let text = std::str::from_utf8(&body)
            .map_err(|e| io::Error::other(format!("dump not utf-8: {e}")))?;
        let dump = json::parse(text)
            .map_err(|e| io::Error::other(format!("dump not json: {e}")))?;
        let roots: Vec<&json::Value> = json::app_roots(&dump).into_iter().rev().collect();
        let found = match &css {
            Some(sels) => {
                let ctx = selector::MatchCtx::from_roots(roots.clone());
                let mut matches = selector::find_all_with(&ctx, sels);
                selector::apply_filters(&mut matches, flags.require());
                // An input widget among the matches wins; else the first match.
                matches.iter().find(|n| is_input_widget(n)).or(matches.first()).map(|n| node_to_daemon_selector(n))
            }
            None => find_fill_target(&roots, sel_in).map(node_to_daemon_selector),
        };
        if let Some(sel) = found {
            return Ok(sel);
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::other(if css.is_some() {
                format!("fill: no node matched {sel_in:?}")
            } else {
                format!("fill: no input widget matched {sel_in:?} — pass a selector                          (`#id`, `EditText[hint~=...]`) or daemon grammar (`id=...`)")
            }));
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

pub(crate) fn is_input_widget(node: &json::Value) -> bool {
    let cls = selector::get_str(node, "cls").unwrap_or("");
    let simple = cls.rsplit('.').next().unwrap_or(cls);
    matches!(simple,
        "EditText" | "AutoCompleteTextView" | "MultiAutoCompleteTextView")
}

/// Score how well an input widget matches the free-text query. Mirrors the
/// priority ladder in `tap_text::classify` so `mobax fill` and `mobax tap` rank
/// candidates the same way.
pub(crate) fn fill_priority(q: &str, q_low: &str, text: &str, desc: &str, hint: &str) -> Option<u8> {
    if text == q || desc == q || hint == q { return Some(0); }
    let t = text.to_lowercase();
    let d = desc.to_lowercase();
    let h = hint.to_lowercase();
    if t == q_low || d == q_low || h == q_low { return Some(1); }
    if !q_low.is_empty() && (t.contains(q_low) || d.contains(q_low) || h.contains(q_low)) {
        return Some(2);
    }
    None
}

/// `roots` are window roots, topmost first; on equal rank the upper
/// window's input wins.
pub(crate) fn find_fill_target<'a>(roots: &[&'a json::Value], query: &str) -> Option<&'a json::Value> {
    let q_low = query.to_lowercase();

    // Phase 1: prefer an input widget whose own text/desc/hint matches.
    // Empty EditTexts surface their hint as `text` in the dump, so this
    // catches the common "fill the field whose placeholder mentions X" case.
    let mut best: Option<(u8, &json::Value)> = None;
    for root in roots { find_input_match(root, query, &q_low, &mut best); }
    if let Some((_, n)) = best { return Some(n); }

    // Phase 1b: a label inside the field. Compose's TextField puts its
    // `label` and `placeholder` under the EditText as their own nodes, and
    // the field's hint stays empty.
    for root in roots {
        if let Some(n) = find_input_holding_label(root, &q_low, None) { return Some(n); }
    }

    // Phase 2: label-anchored — find any node matching the query, then
    // take the nearest input widget below or right-of it. Mirrors how a
    // human reads a form: label on the left or above, field next to it.
    // The field is looked for in the label's own window.
    for root in roots {
        let mut labels: Vec<&json::Value> = Vec::new();
        collect_labels(root, &q_low, &mut labels);
        for label in labels {
            let Some(lb) = selector::bounds(label) else { continue; };
            let mut nearest: Option<(i64, &json::Value)> = None;
            find_nearest_input(root, lb, &mut nearest);
            if let Some((_, n)) = nearest { return Some(n); }
        }
    }
    None
}

pub(crate) fn find_input_match<'a>(
    node: &'a json::Value,
    q: &str,
    q_low: &str,
    best: &mut Option<(u8, &'a json::Value)>,
) {
    if is_input_widget(node) {
        let text = selector::get_str(node, "text").unwrap_or("");
        let desc = selector::get_str(node, "desc").unwrap_or("");
        let hint = selector::get_str(node, "hint").unwrap_or("");
        if let Some(p) = fill_priority(q, q_low, text, desc, hint) {
            let beat = match *best { None => true, Some((bp, _)) => p < bp };
            if beat { *best = Some((p, node)); }
        }
    }
    if let Some(kids) = selector::children(node) {
        for c in kids { find_input_match(c, q, q_low, best); }
    }
}

/// The input widget enclosing the first node whose text or desc contains `q_low`.
pub(crate) fn find_input_holding_label<'a>(
    node: &'a json::Value,
    q_low: &str,
    input: Option<&'a json::Value>,
) -> Option<&'a json::Value> {
    let input = if is_input_widget(node) { Some(node) } else { input };
    if let Some(field) = input.filter(|f| !std::ptr::eq(*f, node)) {
        let text = selector::get_str(node, "text").unwrap_or("").to_lowercase();
        let desc = selector::get_str(node, "desc").unwrap_or("").to_lowercase();
        if !q_low.is_empty() && (text.contains(q_low) || desc.contains(q_low)) {
            return Some(field);
        }
    }
    selector::children(node)?.iter().find_map(|c| find_input_holding_label(c, q_low, input))
}

pub(crate) fn collect_labels<'a>(node: &'a json::Value, q_low: &str, out: &mut Vec<&'a json::Value>) {
    let text = selector::get_str(node, "text").unwrap_or("").to_lowercase();
    let desc = selector::get_str(node, "desc").unwrap_or("").to_lowercase();
    if !q_low.is_empty()
        && ((!text.is_empty() && text.contains(q_low))
            || (!desc.is_empty() && desc.contains(q_low)))
    {
        out.push(node);
    }
    if let Some(kids) = selector::children(node) {
        for c in kids { collect_labels(c, q_low, out); }
    }
}

pub(crate) fn find_nearest_input<'a>(
    node: &'a json::Value,
    lb: (i64, i64, i64, i64),
    best: &mut Option<(i64, &'a json::Value)>,
) {
    if is_input_widget(node) {
        if let Some(eb) = selector::bounds(node) {
            // 8px slack so a label whose bottom exactly meets the field
            // top still counts as "above" rather than "overlapping".
            let below = eb.1 >= lb.3 - 8;
            let right = eb.0 >= lb.2 - 8;
            if below || right {
                let lcx = (lb.0 + lb.2) / 2;
                let lcy = (lb.1 + lb.3) / 2;
                let ecx = (eb.0 + eb.2) / 2;
                let ecy = (eb.1 + eb.3) / 2;
                let dx = ecx - lcx;
                let dy = ecy - lcy;
                let dist = dx * dx + dy * dy;
                let beat = match *best { None => true, Some((bd, _)) => dist < bd };
                if beat { *best = Some((dist, node)); }
            }
        }
    }
    if let Some(kids) = selector::children(node) {
        for c in kids { find_nearest_input(c, lb, best); }
    }
}

pub(crate) fn run_type_into(
    opts: &Opts, selector: &str, text: &str, flags: &flags::ActionFlags,
) -> io::Result<()> {
    let reporter = output::Reporter::new(flags.out(opts.out_fmt));
    let mut conn = Conn::connect(&opts.host, opts.port)?;
    fill_into(&mut conn, &reporter, selector, text, flags)
}

/// `mobax fill SELECTOR TEXT`: ACTION_SET_TEXT on the matched field. Some
/// fields refuse it (a SearchView's EditText, say); then the field is
/// clicked to focus it and the text goes in as keystrokes instead.
pub(crate) fn fill_into(
    conn: &mut Conn, reporter: &output::Reporter, selector: &str, text: &str, flags: &flags::ActionFlags,
) -> io::Result<()> {
    // Free-text and CSS-like selectors are resolved against the live dump
    // into a daemon-grammar selector. Mirrors `mobax tap "TEXT"`.
    let resolved = resolve_fill_selector(conn, selector, flags)?;
    let mut attempts = flags.total_attempts();
    loop {
        // Daemon wire command: node_set_text <selector> value="..."
        let body = conn.call(&format!("node_set_text {resolved} value={text:?}"))?;
        let mut method = "set_text";
        let body = match errors::parse_err(&body) {
            None => body,
            Some(e) if e.detail.contains("set_text-rejected") => {
                // Usually the field isn't focused or ready yet (a search view
                // still opening): focus it, let it settle, and set it again.
                let _ = conn.call(&format!("node_click {resolved}"))?;
                conn.call("wait_for_idle idle_ms=300 timeout_ms=2000")?;
                let mut again = None;
                for _ in 0..10 {
                    let r = conn.call(&format!("node_set_text {resolved} value={text:?}"))?;
                    if errors::parse_err(&r).is_none() {
                        again = Some(r);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                if let Some(r) = again {
                    method = "set_text after click";
                    r
                } else if text.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                    method = "keys";
                    conn.call(&format!("text {text}"))?
                } else {
                    // The virtual key map has no keys for Hangul, CJK, emoji, …
                    method = "paste";
                    let set = conn.call(&format!("clip_set {text}"))?;
                    if errors::parse_err(&set).is_some() {
                        return Err(reporter.fail("fill", e));
                    }
                    conn.call("paste")?
                }
            }
            Some(_) => body,
        };
        if let Some(e) = errors::parse_err(&body) {
            attempts = attempts.saturating_sub(1);
            if attempts > 0 {
                std::thread::sleep(std::time::Duration::from_millis(flags.retry_delay_ms));
                continue;
            }
            return Err(reporter.fail("fill", e));
        }
        let human = String::from_utf8_lossy(&body);
        let human = match method {
            "set_text after click" => format!("{} (after focusing the field)", human.trim_end()),
            "keys" => format!("{} (set_text refused; typed after a click)", human.trim_end()),
            "paste" => format!("{} (set_text refused; pasted after a click)", human.trim_end()),
            _ => human.trim_end().to_string(),
        };
        return reporter.ok("fill", &human,
            json_out::Obj::new()
                .s("selector", &resolved)
                .s("query", selector)
                .s("text", text)
                .s("method", method));
    }
}
