// Command-line parsing: argv → `Opts`.

use crate::*;

#[derive(Debug)]
pub(crate) struct Opts {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) out_fmt: flags::OutFmt,
    pub(crate) cmd: Cmd,
}

#[derive(Debug, Default)]
pub(crate) struct SeeOpts {
    pub(crate) path: Option<String>,
    pub(crate) size: Option<u32>,
    pub(crate) native: bool,
    pub(crate) secure_check: bool,
    /// .json / .xml: every window instead of the focused one (as `mobax ui --all`).
    pub(crate) all_windows: bool,
    /// Bare `mobax see`: flags for the viewer (`--codec h264`, …).
    pub(crate) viewer: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum Cmd {
    Ping,
    Find { selector: String, flags: flags::ActionFlags },
    Input(String),       // pre-built wire command, e.g. "tap x=720 y=1500"
    TapText { query: String, flags: flags::ActionFlags },
    TapXY { x: i32, y: i32, flags: flags::ActionFlags },
    Snapshot,
    Screen,
    /// `mobax watch [--json] [--min-ms N]`: follow the screen.
    Watch { json: bool, min_ms: Option<u64>, moves: bool },
    Quit,
    Bench { n: u32, json: bool },
    Adb(adb::Cmd),
    Connect(daemon::ConnectOpts),
    Disconnect(daemon::DisconnectOpts),
    See(SeeOpts),
    Wait { spec: String, flags: flags::ActionFlags },
    Cp { src: String, dst: String },
    ShowPkg(String),
    Info,
    TypeFocused { text: String, flags: flags::ActionFlags },
    TypeInto { selector: String, text: String, flags: flags::ActionFlags },
    SettingsListAll,
    Ui { format: UiFormat, windows: UiWindows },
    Devices,
    StateDaemon,
    Shell,
    Run { script: Option<String>, flags: flags::ActionFlags },  // mobax run [SCRIPT|-]
    Act(act::ActOpts),                                          // mobax act --tap … --until …
    Fan { serials: Vec<String>, argv: Vec<String> },            // mobax fan SERIALS -- VERB ARGS
    Init { path: Option<String> },                              // mobax init [PATH]
    Submit { sel: Option<String>, flags: flags::ActionFlags },
    Paste { sel: Option<String>, flags: flags::ActionFlags },
    Links(String),
    Sms { kind: String, limit: u32, json: bool },
    Calls { kind: String, limit: u32, json: bool },
    Contacts { limit: u32, json: bool },
    Calendar { from_ms: Option<i64>, to_ms: Option<i64>, days: Option<i64>, limit: u32, json: bool },
    Location { json: bool },
    Notif { pkg: Option<String>, history: bool, limit: u32, json: bool },
    Clip { text: Option<String>, watch: bool, interval_ms: u64 },
    Tui { active: bool },
}

/// `mobax logs`: logcat with filters. Default `logcat -d -t 100` (last 100
/// lines, exit); `--follow` streams instead.
///   --tail N            lines to print (default 100)
///   --pkg PKG           only PKG's running process (logcat --pid)
///   --level V|D|I|W|E|F minimum priority
///   --tag TAG[:L]       only these tags (repeatable); others are silenced
///   --grep REGEX        lines matching REGEX (logcat -e; no spaces)
///   --buffer B / --crash   main, system, crash, events, radio, all
///   --clear             clear the buffers and exit
pub(crate) fn parse_logs(rest: &[&str]) -> Result<Cmd, String> {
    let mut follow = false;
    let mut tail: u32 = 100;
    let (mut pkg, mut pid, mut level) = (None::<String>, None::<u32>, None::<char>);
    let mut tags: Vec<String> = Vec::new();
    let mut greps: Vec<String> = Vec::new();
    let mut buffers: Vec<String> = Vec::new();
    let mut formats: Vec<String> = Vec::new();
    let mut raw: Vec<String> = Vec::new();
    let mut q = adb::LogcatQuery::default();
    let mut clear = false;
    let mut i = 0;
    // The daemon splits logcat's argv on whitespace.
    let no_space = |flag: &str, v: &str| -> Result<String, String> {
        if v.is_empty() || v.chars().any(char::is_whitespace) {
            Err(format!("{flag} takes one word (no spaces): {v:?}"))
        } else {
            Ok(v.to_string())
        }
    };
    while i < rest.len() {
        let flag = rest[i];
        let mut value = || -> Result<&str, String> {
            i += 1;
            rest.get(i).copied().ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "-f" | "--follow" => follow = true,
            "-t" | "--tail" => {
                let v = value()?;
                tail = v.parse().map_err(|_| format!("bad --tail {v}"))?;
            }
            "-p" | "--pkg" | "--package" => pkg = Some(no_space(flag, value()?)?),
            "--pid" => {
                let v = value()?;
                pid = Some(v.parse().map_err(|_| format!("bad --pid {v}"))?);
            }
            "-l" | "--level" => {
                let v = value()?;
                level = match v.to_ascii_uppercase().as_str() {
                    l @ ("V" | "D" | "I" | "W" | "E" | "F") => l.chars().next(),
                    "VERBOSE" => Some('V'),
                    "DEBUG" => Some('D'),
                    "INFO" => Some('I'),
                    "WARN" | "WARNING" => Some('W'),
                    "ERROR" => Some('E'),
                    "FATAL" => Some('F'),
                    _ => return Err(format!("bad --level {v} (V D I W E F)")),
                };
            }
            "--tag" => tags.push(no_space(flag, value()?)?),
            "-e" | "--grep" => {
                let v = value()?;
                if v.is_empty() {
                    return Err(format!("{flag} needs a pattern"));
                }
                greps.push(v.to_string());
            }
            "-x" | "--exclude" => {
                let v = value()?;
                if v.is_empty() {
                    return Err(format!("{flag} needs text"));
                }
                q.exclude.push(v.to_string());
            }
            "-i" | "--ignore-case" => q.ignore_case = true,
            "--since" => {
                let v = value()?;
                q.since_secs = Some(parse_since(v)
                    .ok_or_else(|| format!("bad --since {v} (e.g. 90, 30s, 5m, 2h)"))?);
            }
            "-v" | "--format" => {
                let v = value()?;
                if !LOGCAT_FORMATS.contains(&v) {
                    return Err(format!("bad --format {v} ({})", LOGCAT_FORMATS.join(" ")));
                }
                formats.push(v.to_string());
            }
            "-b" | "--buffer" => buffers.push(no_space(flag, value()?)?),
            "--crash" => buffers.push("crash".into()),
            "-c" | "--clear" => clear = true,
            "--" => {
                for a in &rest[i + 1..] {
                    raw.push(no_space("logcat argument", a)?);
                }
                break;
            }
            other => return Err(format!("unknown logs arg: {other}")),
        }
        i += 1;
    }
    if pkg.is_some() && pid.is_some() {
        return Err("--pkg and --pid are exclusive".into());
    }
    let mut args: Vec<String> = Vec::new();
    for b in &buffers {
        args.extend(["-b".to_string(), b.clone()]);
    }
    if clear {
        args.push("-c".into());
        q.args = args;
        return Ok(Cmd::Adb(adb::Cmd::Logcat(q)));
    }
    if let Some(p) = pid {
        args.push(format!("--pid={p}"));
    }
    for f in &formats {
        args.extend(["-v".to_string(), f.clone()]);
    }
    // logcat's -t counts before its priority / tag / regex filters (and
    // before --exclude), so with any of those only the host's tail applies.
    let filtered = level.is_some() || !tags.is_empty() || !greps.is_empty()
        || !q.exclude.is_empty() || !raw.is_empty();
    if !follow {
        args.push("-d".into());
        if !filtered && q.since_secs.is_none() {
            args.extend(["-t".to_string(), tail.to_string()]);
        }
        q.tail = Some(tail as usize);
    }
    if !greps.is_empty() {
        args.extend(["-e".to_string(), logcat_regex(&greps, q.ignore_case)]);
    }
    args.extend(raw);
    // Filter specs go last: TAG:LEVEL … *:S, or *:LEVEL alone.
    let lvl = level.unwrap_or('V');
    if !tags.is_empty() {
        for t in tags {
            args.push(if t.contains(':') { t } else { format!("{t}:{lvl}") });
        }
        args.push("*:S".into());
    } else if let Some(l) = level {
        args.push(format!("*:{l}"));
    }
    q.args = args;
    q.pkg = pkg;
    Ok(Cmd::Adb(adb::Cmd::Logcat(q)))
}

/// logcat's `-v` formats and format modifiers.
pub(crate) const LOGCAT_FORMATS: &[&str] = &[
    "brief", "long", "process", "raw", "tag", "thread", "threadtime", "time",
    "color", "descriptive", "epoch", "monotonic", "printable", "uid", "usec", "UTC", "year", "zone",
];

/// `90`, `30s`, `5m`, `2h` → seconds.
pub(crate) fn parse_since(v: &str) -> Option<u64> {
    let v = v.trim();
    let (num, mult) = match v.chars().last()? {
        's' => (&v[..v.len() - 1], 1),
        'm' => (&v[..v.len() - 1], 60),
        'h' => (&v[..v.len() - 1], 3600),
        _ => (v, 1),
    };
    num.parse::<u64>().ok().filter(|&n| n > 0).map(|n| n * mult)
}

/// One `-e` regex for logcat (ECMAScript, matched on the device) from
/// several patterns, OR'd. Whitespace becomes `\xHH` because the daemon
/// splits logcat's argv on it; `ignore_case` spells each letter both ways,
/// since logcat's regex has no case-insensitive flag. Letters at either end
/// of a class range (`[a-z]`) stay as written.
pub(crate) fn logcat_regex(patterns: &[String], ignore_case: bool) -> String {
    let one = |p: &str| -> String {
        let chars: Vec<char> = p.chars().collect();
        let mut out = String::with_capacity(p.len() * 2);
        let mut in_class = false;
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                // Copy the escape whole: \xhh and \uhhhh carry hex letters.
                let n = match chars.get(i + 1) { Some('x') => 4, Some('u') => 6, Some(_) => 2, None => 1 };
                out.extend(&chars[i..(i + n).min(chars.len())]);
                i += n;
                continue;
            }
            if c.is_whitespace() {
                out.push_str(&format!("\\x{:02x}", c as u32 & 0xff));
            } else if c == '[' && !in_class {
                in_class = true;
                out.push(c);
            } else if c == ']' && in_class {
                in_class = false;
                out.push(c);
            } else if ignore_case && c.is_ascii_alphabetic() {
                let other = if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
                if !in_class {
                    out.push_str(&format!("[{c}{other}]"));
                } else if chars.get(i + 1) == Some(&'-') || (i > 0 && chars[i - 1] == '-') {
                    out.push(c);
                } else {
                    out.push(c);
                    out.push(other);
                }
            } else {
                out.push(c);
            }
            i += 1;
        }
        out
    };
    match patterns {
        [p] => one(p),
        _ => patterns.iter().map(|p| format!("(?:{})", one(p))).collect::<Vec<_>>().join("|"),
    }
}

// SMS type codes — Android Telephony.TextBasedSmsColumns.
pub(crate) const SMS_TYPES: &[(i64, &str)] = &[
    (1, "inbox"), (2, "sent"), (3, "draft"),
    (4, "outbox"), (5, "failed"), (6, "queued"),
];
// Call log type codes — Android CallLog.Calls.TYPE.
pub(crate) const CALL_TYPES: &[(i64, &str)] = &[
    (1, "in"), (2, "out"), (3, "missed"), (4, "voicemail"),
    (5, "rejected"), (6, "blocked"), (7, "external"),
];
// Phone-number type codes — Android ContactsContract.CommonDataKinds.Phone.
pub(crate) const PHONE_TYPES: &[(i64, &str)] = &[
    (1, "home"),     (2, "mobile"),     (3, "work"),
    (4, "fax-work"), (5, "fax-home"),   (6, "pager"),
    (7, "other"),    (9, "car"),        (10, "company"),
    (12, "main"),    (17, "work-mob"),
];

#[derive(Debug, Clone, Copy)]
pub(crate) enum UiFormat { Human, Interactive, Json, Xml }

/// Which windows `mobax ui` reads: the app's own (activity plus its dialogs
/// and popups, the default), only the focused one, or every window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum UiWindows { App, Active, All }

pub(crate) fn parse_use(rest: &[&str]) -> Result<daemon::ConnectOpts, String> {
    let mut opts = daemon::ConnectOpts { serial: None, port: None };
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "-s" | "--device" => {
                i += 1;
                opts.serial = Some(rest.get(i).ok_or("--device needs a SERIAL")?.to_string());
            }
            "--port" => {
                i += 1;
                opts.port = Some(rest.get(i).ok_or("--port needs a value")?
                    .parse().map_err(|_| "invalid --port".to_string())?);
            }
            serial if !serial.starts_with('-') && opts.serial.is_none() => {
                opts.serial = Some(serial.to_string());
            }
            other => return Err(format!("unknown connect arg: {other}")),
        }
        i += 1;
    }
    Ok(opts)
}

pub(crate) fn parse_drop(rest: &[&str]) -> Result<daemon::DisconnectOpts, String> {
    let mut opts = daemon::DisconnectOpts { serial: None, keep_jar: false };
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "-s" | "--device" => {
                i += 1;
                opts.serial = Some(rest.get(i).ok_or("--device needs a SERIAL")?.to_string());
            }
            "--keep-jar" => opts.keep_jar = true,
            other => return Err(format!("unknown disconnect arg: {other}")),
        }
        i += 1;
    }
    Ok(opts)
}

/// `mobax tap` — text-lookup when arg isn't a pair of integers, raw coords
/// when it is. Strips the shared ActionFlags surface first so RPA scripts
/// can `mobax tap "Login" --visible --unique --timeout 5s --retries 3`.
pub(crate) fn parse_tap(rest: &[&str]) -> Result<Cmd, String> {
    let mut flags = flags::ActionFlags::default();
    let positional = flags.take(rest)?;
    if positional.is_empty() {
        return Err("tap needs either TEXT or X Y coords".into());
    }
    if positional.len() == 2 {
        let (x, y) = (positional[0].parse::<i32>(), positional[1].parse::<i32>());
        if let (Ok(x), Ok(y)) = (x, y) {
            return Ok(Cmd::TapXY { x, y, flags });
        }
    }
    if positional.len() == 1 && positional[0].parse::<i32>().is_ok() {
        return Err("tap with a single number is ambiguous — pass two ints for coords, or quote text".into());
    }
    Ok(Cmd::TapText { query: positional.join(" "), flags })
}

/// True if `s` looks like a package name: dot-separated, no slash.
pub(crate) fn is_pkg(s: &str) -> bool {
    !s.contains('/') && s.contains('.') &&
        s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

/// Friendly "did you mean" for retired verbs.
pub(crate) fn suggest(old: &str) -> String {
    let hint = match old {
        "devices"      => Some("mobax"),
        "connect"      => Some("mobax use"),
        "disconnect"   => Some("mobax drop"),
        "screenshot"   => Some("mobax see <PATH.jpg|.png>"),
        "dump" | "dump-active" => Some("mobax see <PATH.xml|.json>"),
        "query"        => Some("mobax find <SELECTOR>"),
        "pull" | "push" => Some("mobax cp  (device:/path host:/path or reverse)"),
        "pm"           => Some("mobax apps / mobax install / mobax uninstall / mobax show <pkg>"),
        "am"           => Some("mobax open / mobax close / mobax events"),
        "getprop" | "setprop" => Some("mobax prop KEY [VAL]"),
        "logcat"       => Some("mobax logs"),
        "monitor"      => Some("mobax events"),
        "dumpsys"      => Some("mobax do dumpsys <svc>"),
        "state"        => Some("mobax show  /  mobax watch"),
        "input"        => Some("mobax tap / mobax type / mobax go / mobax swipe"),
        _              => None,
    };
    match hint {
        Some(h) => format!("unknown command: {old}  — try `{h}`"),
        None    => format!("unknown command: {old}"),
    }
}

/// `mobax dev <sub>` — explicit namespace for the daemon-debugging verbs.
/// Bare `mobax dev` prints what's available; `mobax dev ping` etc. dispatch the
/// same `Cmd` variants the top-level aliases produce.
pub(crate) fn parse_dev(rest: &[&str]) -> Result<Cmd, String> {
    const HELP: &str = "\
mobax dev <sub>:
  ping              round-trip the daemon socket
  snapshot          print the cached state JSON
  screen            print one screenshot frame to stdout
  bench [-n N] [--json]  timed wire-call benchmark (default 50 iterations)
  quit              ask the daemon to exit (`mobax drop` is friendlier)
  state-daemon      run the host-side state mirror (used by `mobax use`)";
    match rest.split_first() {
        None => Err(HELP.to_string()),
        Some((&"ping",         _))   => Ok(Cmd::Ping),
        Some((&"snapshot",     _))   => Ok(Cmd::Snapshot),
        Some((&"screen",       _))   => Ok(Cmd::Screen),
        Some((&"quit",         _))   => Ok(Cmd::Quit),
        Some((&"state-daemon", _))   => Ok(Cmd::StateDaemon),
        Some((&"bench",        more)) => parse_bench(more),
        Some((other, _)) => Err(format!("mobax dev: unknown sub-verb '{other}'\n{HELP}")),
    }
}

pub(crate) fn parse_bench(rest: &[&str]) -> Result<Cmd, String> {
    let mut n: u32 = 50;
    let mut json = false;
    let mut j = 0;
    while j < rest.len() {
        match rest[j] {
            "-n" => {
                j += 1;
                n = rest.get(j).ok_or("-n needs a value")?.parse()
                    .map_err(|_| "invalid -n value")?;
            }
            "--json" => json = true,
            other => return Err(format!("unknown bench arg: {other}")),
        }
        j += 1;
    }
    if n == 0 {
        return Err("-n must be greater than 0".into());
    }
    Ok(Cmd::Bench { n, json })
}

pub(crate) fn parse_see(rest: &[&str]) -> Result<SeeOpts, String> {
    let mut opts = SeeOpts::default();
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "--size" => {
                i += 1;
                let n: u32 = rest.get(i).ok_or("--size needs N")?.parse()
                    .map_err(|_| "invalid --size value")?;
                if n == 0 {
                    return Err("--size must be greater than 0".into());
                }
                opts.size = Some(n);
            }
            "--native" | "--max" => opts.native = true,
            "--secure-check" | "--secure" => opts.secure_check = true,
            "--all" => opts.all_windows = true,
            // The viewer's own flags, for a bare `mobax see`.
            "--no-native" => opts.viewer.push("--no-native".into()),
            flag @ ("--codec" | "--quality" | "--bitrate-kbps" | "--tile" | "--fps") => {
                i += 1;
                let v = rest.get(i).ok_or_else(|| format!("{flag} needs a value"))?;
                opts.viewer.extend([flag.to_string(), v.to_string()]);
            }
            path if path.starts_with('-') => return Err(format!("unknown see arg: {path}")),
            path => {
                if opts.path.is_some() {
                    return Err("see takes at most one PATH (extension picks format)".into());
                }
                opts.path = Some(path.to_string());
            }
        }
        i += 1;
    }
    if opts.native && opts.size.is_some() {
        return Err("see: --native and --size are mutually exclusive".into());
    }
    if opts.path.is_some() && !opts.viewer.is_empty() {
        return Err(format!("see: {} applies only to the viewer (bare `mobax see`)", opts.viewer[0]));
    }
    if opts.path.is_none() {
        if opts.secure_check || opts.all_windows {
            return Err("see: --secure-check and --all need a PATH; bare `mobax see` opens the viewer".into());
        }
        // The viewer streams at native size unless told a size.
        if let Some(n) = opts.size {
            opts.viewer.extend(["--size".to_string(), n.to_string()]);
        }
    }
    Ok(opts)
}

/// Shared arg parser for the simple provider verbs (`sms`, `calls`,
/// `contacts`): consumes `--limit N` and `--json`, collects anything
/// else as positional tokens.
pub(crate) fn parse_provider_common<'a>(rest: &[&'a str]) -> Result<(u32, bool, Vec<&'a str>), String> {
    let mut limit = 50_u32;
    let mut json = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i] {
            "--limit" => {
                i += 1;
                limit = rest.get(i).ok_or("--limit needs N")?.parse()
                    .map_err(|_| "bad --limit".to_string())?;
            }
            "--json" => json = true,
            other => positional.push(other),
        }
        i += 1;
    }
    Ok((limit, json, positional))
}

pub(crate) fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut host = DEFAULT_HOST.to_string();
    let mut port = DEFAULT_PORT;
    let mut out_fmt = flags::OutFmt::from_env();
    let mut device_serial: Option<String> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--host" => {
                i += 1;
                host = args.get(i).ok_or("--host needs a value")?.clone();
            }
            "--port" => {
                i += 1;
                port = args
                    .get(i)
                    .ok_or("--port needs a value")?
                    .parse()
                    .map_err(|_| "invalid --port".to_string())?;
            }
            "-s" | "--device" => {
                i += 1;
                device_serial = Some(args.get(i)
                    .ok_or("--device needs a SERIAL")?.clone());
            }
            // `mobax ui --json` is the raw dump, not JSON result records.
            "--json" if positional.first() == Some(&"ui") => positional.push(a),
            "--json" => out_fmt = flags::OutFmt::Json,
            "-h" | "--help" => {
                crate::outln!("{USAGE}");
                std::process::exit(0);
            }
            // Everything after `--` belongs to the verb (`mobax logs -- -s TAG`).
            "--" => {
                positional.extend(args[i..].iter().map(String::as_str));
                break;
            }
            _ => positional.push(a),
        }
        i += 1;
    }

    // During `use` and `drop`, `--device SERIAL` selects the ADB device to
    // bootstrap or tear down. Other verbs talk to that device's daemon.
    // (Script lines without --device keep the device the script started on.)
    let selects_device = matches!(positional.first(), Some(&"use") | Some(&"drop"));
    if !selects_device && device_serial.is_some() {
        daemon::set_target(device_serial.clone());
    }

    let cmd = match positional.split_first() {
        // Bare `mobax` lists devices — Reitz-style: the noun is implicit.
        None => Cmd::Devices,

        // ─── Lifecycle ────────────────────────────────────────────────
        Some((&"use", rest)) => {
            let mut connect = parse_use(rest)?;
            if let Some(serial) = device_serial.as_deref() {
                if let Some(selected) = connect.serial.as_deref() {
                    if selected != serial {
                        return Err("conflicting device selectors for `mobax use`".into());
                    }
                }
                connect.serial = Some(serial.to_string());
            }
            Cmd::Connect(connect)
        }
        Some((&"drop", rest)) => {
            let mut drop = parse_drop(rest)?;
            if device_serial.is_some() {
                drop.serial = device_serial;
            }
            Cmd::Disconnect(drop)
        }

        // ─── See — capture by extension, bare = viewer ───────────────
        // `mirror`: what it was called before `see` took it over.
        Some((&"see", rest)) | Some((&"mirror", rest)) => Cmd::See(parse_see(rest)?),

        // ─── TUI — keyboard-driven inspector ─────────────────────────
        Some((&"tui", rest)) => match rest {
            [] => Cmd::Tui { active: false },
            ["--active"] => Cmd::Tui { active: true },
            _ => return Err(format!("tui takes only --active (got: {})", rest.join(" "))),
        },

        // ─── Apps ─────────────────────────────────────────────────────
        Some((&"apps", rest)) => {
            let mut third = false;
            for tok in rest {
                match *tok {
                    "--3" | "--3rd" | "--third-party" => third = true,
                    "--plain" => { /* placeholder for future formatting flag */ }
                    other => return Err(format!("unknown apps arg: {other}")),
                }
            }
            Cmd::Adb(adb::Cmd::PmList { third, system: false })
        }
        Some((&"open", rest)) => {
            let c = rest.first().ok_or("open needs COMPONENT (pkg or pkg/.Class)")?;
            Cmd::Adb(adb::Cmd::AmStart {
                component: c.to_string(),
                action: None, data: None, flag: None,
            })
        }
        Some((&"close", rest)) => {
            let p = rest.first().ok_or("close needs PKG")?;
            Cmd::Adb(adb::Cmd::AmForceStop(p.to_string()))
        }
        Some((&"install", rest)) => Cmd::Adb(adb::parse_install(rest)?),
        Some((&"uninstall", rest)) => {
            let p = rest.first().ok_or("uninstall needs PKG")?;
            Cmd::Adb(adb::Cmd::PmUninstall(p.to_string()))
        }

        // ─── Query / state ───────────────────────────────────────────
        Some((&"find", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            if positional.is_empty() { return Err("find needs a CSS-like SELECTOR".into()); }
            Cmd::Find { selector: positional.join(" "), flags }
        }
        Some((&"ui", rest)) => {
            // `mobax ui` defaults to the flat, agent-friendly interactive
            // table — that's the 99% caller in an LLM loop and what the
            // README sells. The indented outline is still available via
            // `--tree`; `-i` / `--interactive` remain accepted no-ops so
            // older scripts keep working.
            let mut fmt = UiFormat::Interactive;
            let mut windows = UiWindows::App;
            for tok in rest {
                match *tok {
                    "--json"            => fmt = UiFormat::Json,
                    "--xml"             => fmt = UiFormat::Xml,
                    "--tree"            => fmt = UiFormat::Human,
                    "-i" | "--interactive" => fmt = UiFormat::Interactive,
                    "--all"             => windows = UiWindows::All,
                    "--active"          => windows = UiWindows::Active,
                    other               => return Err(format!("unknown ui arg: {other}")),
                }
            }
            Cmd::Ui { format: fmt, windows }
        }
        Some((&"show", rest)) => match rest.first() {
            None => Cmd::Adb(adb::Cmd::State("device".into())),
            Some(&"top") => Cmd::Adb(adb::Cmd::State("top".into())),
            Some(arg) if is_pkg(arg) => Cmd::ShowPkg(arg.to_string()),
            Some(other) => return Err(format!("show: unknown target '{other}'")),
        }

        // ─── Wait — smart-dispatched at runtime ──────────────────────
        Some((&"wait", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            if positional.is_empty() { return Err("wait needs SPEC (idle | TEXT | PKG | Nms)".into()); }
            Cmd::Wait { spec: positional.join(" "), flags }
        }

        // ─── Input ───────────────────────────────────────────────────
        Some((&"tap", rest)) => parse_tap(rest)?,
        Some((&"type", rest)) => {
            // `type TEXT` types into the focused field via KeyEvents.
            // For atomic set-text on a specific node, use `mobax fill SELECTOR TEXT`
            // (Playwright vocabulary; ACTION_SET_TEXT, bypasses the IME).
            let mut tflags = flags::ActionFlags::default();
            let positional = tflags.take(rest)?;
            match positional.len() {
                0 => return Err("type needs TEXT".into()),
                1 => Cmd::TypeFocused { text: positional[0].into(), flags: tflags },
                _ => return Err(
                    "type takes one TEXT argument; use `mobax fill SELECTOR TEXT` to target a node".into()
                ),
            }
        }
        Some((&"fill", rest)) => {
            // ACTION_SET_TEXT against the matching node. Atomic, bypasses the
            // IME — matches Playwright's `page.fill(selector, value)`.
            let mut tflags = flags::ActionFlags::default();
            let positional = tflags.take(rest)?;
            match positional.len() {
                0 | 1 => return Err("fill needs SELECTOR TEXT".into()),
                2 => Cmd::TypeInto {
                    selector: positional[0].into(),
                    text:     positional[1].into(),
                    flags:    tflags,
                },
                _ => return Err(
                    "fill takes SELECTOR TEXT — quote multi-word arguments".into()
                ),
            }
        }
        Some((&"go", rest)) => {
            let k = rest.first().ok_or("go needs KEY (back|home|recents|enter|…)")?;
            Cmd::Input(format!("key {}", k.to_uppercase()))
        }
        Some((&"swipe", rest)) => {
            // Direction shortcut: `mobax swipe left|right|up|down [DUR_MS]`.
            // Daemon resolves the coordinates from the live display size.
            if let Some(&first) = rest.first() {
                let d = first.to_lowercase();
                if matches!(d.as_str(), "left" | "right" | "up" | "down") {
                    if rest.len() > 2 {
                        return Err("swipe DIR takes at most one extra arg (DUR_MS)".into());
                    }
                    // Only pass `dur=` if the user gave one; let the daemon's
                    // 500ms default win otherwise (reads as a drag, not a fling).
                    let wire = match rest.get(1) {
                        Some(s) => {
                            let dur: i32 = s.parse().map_err(|_| "bad swipe DUR_MS")?;
                            format!("swipe_dir {d} dur={dur}")
                        }
                        None => format!("swipe_dir {d}"),
                    };
                    return Ok(Opts { host, port, out_fmt, cmd: Cmd::Input(wire) });
                }
            }
            if rest.len() < 4 || rest.len() > 5 {
                return Err("swipe needs DIR [DUR_MS] or X1 Y1 X2 Y2 [DUR_MS]".into());
            }
            let x1: i32 = rest[0].parse().map_err(|_| "bad swipe X1")?;
            let y1: i32 = rest[1].parse().map_err(|_| "bad swipe Y1")?;
            let x2: i32 = rest[2].parse().map_err(|_| "bad swipe X2")?;
            let y2: i32 = rest[3].parse().map_err(|_| "bad swipe Y2")?;
            let dur: i32 = if rest.len() == 5 {
                rest[4].parse().map_err(|_| "bad swipe DUR_MS")?
            } else { 300 };
            Cmd::Input(format!("swipe x1={x1} y1={y1} x2={x2} y2={y2} dur={dur}"))
        }

        // ─── Files ───────────────────────────────────────────────────
        Some((&"cp", rest)) => {
            if rest.len() != 2 { return Err("cp needs SRC DST (one side starts with `device:`)".into()); }
            Cmd::Cp { src: rest[0].into(), dst: rest[1].into() }
        }

        // ─── Properties + settings (arity-dispatched) ────────────────
        Some((&"prop", rest)) => match rest.len() {
            // Bare `mobax prop` → dump every property via the on-device getprop tool.
            0 => Cmd::Adb(adb::Cmd::Shell(vec![
                "/system/bin/getprop".into(),
            ])),
            1 => Cmd::Adb(adb::Cmd::GetProp(rest[0].into())),
            2 => Cmd::Adb(adb::Cmd::SetProp(rest[0].into(), rest[1].into())),
            _ => return Err("prop needs nothing (list all), KEY (get) or KEY VALUE (set)".into()),
        }
        Some((&"settings", rest)) => match rest.len() {
            // `mobax settings` → composite of all three namespaces. The bulk
            // listing is a Settings variant handled by run() below; we use
            // a marker constant so the existing dispatcher routes correctly.
            0 => Cmd::SettingsListAll,
            1 => {
                let ns = rest[0];
                if !matches!(ns, "system" | "secure" | "global") {
                    return Err("settings NS must be system|secure|global".into());
                }
                Cmd::Adb(adb::Cmd::Shell(vec![
                    "/system/bin/settings".into(), "list".into(), ns.into(),
                ]))
            }
            2 => Cmd::Adb(adb::Cmd::SettingsGet { namespace: rest[0].into(), key: rest[1].into() }),
            n if n >= 3 => Cmd::Adb(adb::Cmd::SettingsPut {
                namespace: rest[0].into(),
                key: rest[1].into(),
                value: rest[2..].join(" "),
            }),
            _ => unreachable!(),
        }

        // ─── Logs / events (first-class) ─────────────────────────────
        Some((&"logs", rest)) => parse_logs(rest)?,
        Some((&"events", _)) => Cmd::Adb(adb::Cmd::Monitor),
        Some((&"info", _))   => Cmd::Info,

        // ─── Submit (IME action button) ──────────────────────────────
        Some((&"submit", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            let sel = if positional.is_empty() { None } else { Some(positional.join(" ")) };
            Cmd::Submit { sel, flags }
        }

        // ─── Paste (ACTION_PASTE on focused/selected field) ──────────
        Some((&"paste", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            let sel = if positional.is_empty() { None } else { Some(positional.join(" ")) };
            Cmd::Paste { sel, flags }
        }

        // ─── Batch / session execution ───────────────────────────────
        Some((&"run", rest)) => {
            let mut flags = flags::ActionFlags::default();
            let positional = flags.take(rest)?;
            let script = positional.first().map(|s| s.to_string());
            Cmd::Run { script, flags }
        }

        // ─── mobax act — one-shot tap-then-assert composite ─────────────
        Some((&"act", rest)) => Cmd::Act(act::parse(rest)?),

        // ─── mobax fan — multi-device fan-out ───────────────────────────
        Some((&"fan", rest)) => {
            // Form: `mobax fan SERIAL,SERIAL[,...] -- VERB ARGS`
            let idx = rest.iter().position(|t| *t == "--")
                .ok_or("fan: expected `--` between serial list and verb")?;
            let serials_blob = rest[..idx].join(" ");
            let serials: Vec<String> = serials_blob.split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
            if serials.is_empty() { return Err("fan: serial list is empty".into()); }
            let argv: Vec<String> = rest[idx + 1..].iter().map(|s| s.to_string()).collect();
            if argv.is_empty() { return Err("fan: nothing to run after `--`".into()); }
            Cmd::Fan { serials, argv }
        }

        // ─── mobax init — scaffold a starter script ─────────────────────
        Some((&"init", rest)) => Cmd::Init {
            path: rest.first().map(|s| s.to_string()),
        },

        // ─── Deeplinks (parsed straight from the APK's AndroidManifest) ─
        Some((&"links", rest)) => {
            let pkg = rest.first().ok_or("links needs PKG")?;
            Cmd::Links(pkg.to_string())
        }

        // ─── ContentProvider readers ─────────────────────────────────
        Some((&"sms", rest)) => {
            let (limit, json, positional) = parse_provider_common(rest)?;
            let kind = positional.first().copied().unwrap_or("inbox").to_string();
            Cmd::Sms { kind, limit, json }
        }
        Some((&"calls", rest)) => {
            let (limit, json, positional) = parse_provider_common(rest)?;
            let kind = positional.first().copied().unwrap_or("all").to_string();
            Cmd::Calls { kind, limit, json }
        }
        Some((&"contacts", rest)) => {
            let (limit, json, positional) = parse_provider_common(rest)?;
            if let Some(other) = positional.first() {
                return Err(format!("contacts takes no positional: {other}"));
            }
            Cmd::Contacts { limit, json }
        }
        Some((&"location" | &"gps", rest)) => {
            let mut json = false;
            for tok in rest {
                match *tok {
                    "--json" => json = true,
                    other => return Err(format!("unknown location arg: {other}")),
                }
            }
            Cmd::Location { json }
        }
        // ─── Clipboard ────────────────────────────────────────────────
        Some((&"clip", rest)) => {
            let mut watch = false;
            let mut interval_ms = 500_u64;
            let mut positional: Vec<&str> = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--watch" => watch = true,
                    "--interval" => {
                        i += 1;
                        interval_ms = rest.get(i).ok_or("--interval needs MS")?
                            .parse().map_err(|_| "bad --interval".to_string())?;
                    }
                    other => positional.push(other),
                }
                i += 1;
            }
            let text = if positional.is_empty() { None } else { Some(positional.join(" ")) };
            if watch && text.is_some() {
                return Err("clip --watch is read-only; can't combine with TEXT".into());
            }
            Cmd::Clip { text, watch, interval_ms }
        }

        Some((&"notif", rest)) => {
            let mut limit = 50_u32;
            let mut json = false;
            let mut history = false;
            let mut pkg: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--limit" => {
                        i += 1;
                        limit = rest.get(i).ok_or("--limit needs N")?.parse()
                            .map_err(|_| "bad --limit".to_string())?;
                    }
                    "--json"    => json = true,
                    "--history" => history = true,
                    other if pkg.is_none() && !other.starts_with("--") => {
                        pkg = Some(other.to_string());
                    }
                    other => return Err(format!("unknown notif arg: {other}")),
                }
                i += 1;
            }
            Cmd::Notif { pkg, history, limit, json }
        }
        Some((&"calendar", rest)) => {
            let mut limit = 50_u32;
            let mut json = false;
            let mut from_ms: Option<i64> = None;
            let mut to_ms: Option<i64> = None;
            let mut days: Option<i64> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--limit" => {
                        i += 1;
                        limit = rest.get(i).ok_or("--limit needs N")?.parse()
                            .map_err(|_| "bad --limit".to_string())?;
                    }
                    "--json" => json = true,
                    "--days" => {
                        i += 1;
                        days = Some(rest.get(i).ok_or("--days needs N")?.parse()
                            .map_err(|_| "bad --days".to_string())?);
                    }
                    "--from" => {
                        i += 1;
                        from_ms = Some(rest.get(i).ok_or("--from needs MS")?.parse()
                            .map_err(|_| "bad --from".to_string())?);
                    }
                    "--to" => {
                        i += 1;
                        to_ms = Some(rest.get(i).ok_or("--to needs MS")?.parse()
                            .map_err(|_| "bad --to".to_string())?);
                    }
                    other => return Err(format!("unknown calendar arg: {other}")),
                }
                i += 1;
            }
            Cmd::Calendar { from_ms, to_ms, days, limit, json }
        }

        // ─── Shell + raw wire ────────────────────────────────────────
        // `shell` and `do` are synonyms: shell is the natural REPL verb,
        // do <wire> is a one-shot raw call.
        Some((&"shell", _)) => Cmd::Shell,
        Some((&"do", rest)) => {
            if rest.is_empty() { Cmd::Shell } else { Cmd::Input(rest.join(" ")) }
        }

        // ─── Low-level / debugging ───────────────────────────────────
        // `mobax dev <sub>` is the documented namespace; the bare verbs
        // (`mobax ping`, `mobax snapshot`, …) stay as undocumented aliases so
        // older scripts and the test harness don't break.
        Some((&"dev", rest)) => parse_dev(rest)?,
        Some((&"watch", rest)) => {
            let mut json = out_fmt == flags::OutFmt::Json;
            let mut min_ms = None;
            let mut moves = false;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--json" => json = true,
                    "--moves" => moves = true,
                    "--min-ms" => min_ms = Some(it.next().and_then(|v| v.parse().ok())
                        .ok_or("--min-ms needs a number of milliseconds")?),
                    other => return Err(format!("watch: unknown argument {other:?}")),
                }
            }
            Cmd::Watch { json, min_ms, moves }
        }
        Some((&"ping", _))      => Cmd::Ping,
        Some((&"snapshot", _))  => Cmd::Snapshot,
        Some((&"screen", _))    => Cmd::Screen,
        Some((&"quit", _))      => Cmd::Quit,
        Some((&"state-daemon", _)) => Cmd::StateDaemon,
        Some((&"bench", rest)) => parse_bench(rest)?,

        Some((other, _)) => return Err(suggest(other)),
    };

    Ok(Opts { host, port, out_fmt, cmd })
}

#[cfg(test)]
mod parse_logs_tests {
    use super::{adb, logcat_regex, parse_logs, Cmd};

    fn q(v: &[&str]) -> adb::LogcatQuery {
        match parse_logs(v).unwrap() {
            Cmd::Adb(adb::Cmd::Logcat(q)) => q,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn defaults_and_filters() {
        // Unfiltered: logcat's -t, and the host tail drops the buffer banners.
        let d = q(&[]);
        assert_eq!((d.args.as_slice(), d.tail), (&["-d", "-t", "100"].map(String::from)[..], Some(100)));
        // Filters: no logcat -t (it counts before filtering); the host tails.
        assert_eq!(q(&["--level", "warn"]).args, ["-d", "*:W"]);
        assert_eq!(
            q(&["--tag", "ActivityManager", "--tag", "mobaxd:D", "--level", "I"]).args,
            ["-d", "ActivityManager:I", "mobaxd:D", "*:S"]
        );
        let c = q(&["--crash", "-t", "20", "--grep", "FATAL"]);
        assert_eq!((c.args.as_slice(), c.tail), (&["-b", "crash", "-d", "-e", "FATAL"].map(String::from)[..], Some(20)));
        assert_eq!(q(&["--crash", "-t", "20"]).args, ["-b", "crash", "-d", "-t", "20"]);
        assert_eq!(q(&["--clear", "--crash"]).args, ["-b", "crash", "-c"]);
        let f = q(&["--follow", "--exclude", "chatty"]);
        assert_eq!((f.args.len(), f.tail, f.exclude.as_slice()), (0, None, &["chatty".to_string()][..]));
        let p = q(&["--pkg", "com.android.settings"]);
        assert_eq!((p.pkg.as_deref(), p.args.as_slice()), (Some("com.android.settings"), &["-d", "-t", "100"].map(String::from)[..]));
    }

    #[test]
    fn host_side_options() {
        // --exclude and --since turn logcat's -t off; the host keeps the tail.
        assert_eq!(q(&["--exclude", "frozen", "-t", "5"]).args, ["-d"]);
        let s = q(&["--since", "5m"]);
        assert_eq!((s.args.as_slice(), s.since_secs), (&["-d".to_string()][..], Some(300)));
        assert_eq!(q(&["--pid", "744", "-v", "brief", "-v", "uid"]).args,
            ["--pid=744", "-v", "brief", "-v", "uid", "-d", "-t", "100"]);
        assert_eq!(q(&["--", "-s", "PackageManager"]).args, ["-d", "-s", "PackageManager"]);
    }

    #[test]
    fn regex_for_logcat() {
        let r = |v: &[&str], ic| logcat_regex(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>(), ic);
        assert_eq!(r(&["Not removing"], false), r"Not\x20removing");
        assert_eq!(r(&["a b", "c"], false), r"(?:a\x20b)|(?:c)");
        assert_eq!(r(&["Ab1"], true), "[Aa][bB]1");
        // Escapes and class ranges stay as written; lone class letters gain a case.
        assert_eq!(r(&[r"\x41\d[a-z_q]"], true), r"\x41\d[a-z_qQ]");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_logs(&["--level", "loud"]).is_err());
        assert!(parse_logs(&["--tag", "two words"]).is_err());
        assert!(parse_logs(&["--tag"]).is_err());
        assert!(parse_logs(&["--bogus"]).is_err());
        assert!(parse_logs(&["--since", "soon"]).is_err());
        assert!(parse_logs(&["--format", "fancy"]).is_err());
        assert!(parse_logs(&["--pkg", "a.b", "--pid", "1"]).is_err());
        assert!(parse_logs(&["--", "two words"]).is_err());
        assert!(parse_logs(&["--grep", "two words"]).is_ok());
    }
}

#[cfg(test)]
mod parse_see_tests {
    use super::parse_see;

    #[test]
    fn a_bare_see_hands_the_viewer_its_flags() {
        let o = parse_see(&["--codec", "h264", "--size", "720", "--fps", "15", "--no-native"]).unwrap();
        assert!(o.path.is_none());
        assert_eq!(o.viewer, ["--codec", "h264", "--fps", "15", "--no-native", "--size", "720"]);
        assert!(parse_see(&["shot.jpg", "--codec", "h264"]).unwrap_err().contains("only to the viewer"));
        assert!(parse_see(&["--codec"]).is_err());
        // A capture keeps --size for itself.
        let o = parse_see(&["shot.jpg", "--size", "480"]).unwrap();
        assert_eq!((o.size, o.viewer.len()), (Some(480), 0));
    }
}

#[cfg(test)]
mod parse_use_tests {
    use super::{parse_args, Cmd};

    fn selected_serial(args: &[&str]) -> Option<String> {
        let args = args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
        let opts = parse_args(&args).expect("use arguments should parse");
        match opts.cmd {
            Cmd::Connect(connect) => connect.serial,
            other => panic!("expected connect command, got {other:?}"),
        }
    }

    #[test]
    fn ui_json_is_the_raw_dump_not_json_records() {
        use super::{flags::OutFmt, UiFormat};
        let parse = |v: &[&str]| parse_args(&v.iter().map(|a| a.to_string()).collect::<Vec<_>>()).unwrap();
        let o = parse(&["ui", "--json"]);
        assert!(matches!(o.cmd, Cmd::Ui { format: UiFormat::Json, .. }));
        assert!(matches!(o.out_fmt, OutFmt::Human));
        // Before the verb, --json is still the global output mode.
        let o = parse(&["--json", "ui"]);
        assert!(matches!((o.cmd, o.out_fmt), (Cmd::Ui { format: UiFormat::Interactive, .. }, OutFmt::Json)));
    }

    #[test]
    fn use_accepts_serial_as_a_positional_argument() {
        assert_eq!(selected_serial(&["use", "192.168.0.10:5555"]),
            Some("192.168.0.10:5555".into()));
    }

    #[test]
    fn global_device_selects_the_device_during_bootstrap() {
        assert_eq!(selected_serial(&["--device", "SERIAL", "use"]),
            Some("SERIAL".into()));
        assert_eq!(selected_serial(&["use", "--device", "SERIAL"]),
            Some("SERIAL".into()));
    }

    #[test]
    fn tui_takes_an_optional_active_flag() {
        let parse = |args: &[&str]| {
            let args = args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
            parse_args(&args).map(|opts| opts.cmd)
        };
        assert!(matches!(parse(&["tui"]), Ok(Cmd::Tui { active: false })));
        assert!(matches!(parse(&["tui", "--active"]), Ok(Cmd::Tui { active: true })));
        assert!(parse(&["tui", "--bogus"]).is_err());
    }

    #[test]
    fn global_device_selects_the_device_to_drop() {
        let parse = |args: &[&str]| {
            let args = args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
            parse_args(&args).map(|opts| opts.cmd)
        };
        // Must not need a forwarded daemon: drop also cleans up dead ones.
        for args in [&["--device", "NOT_ATTACHED", "drop"][..], &["drop", "--device", "NOT_ATTACHED"]] {
            match parse(args) {
                Ok(Cmd::Disconnect(d)) => assert_eq!(d.serial.as_deref(), Some("NOT_ATTACHED")),
                other => panic!("expected disconnect, got {other:?}"),
            }
        }
    }

    #[test]
    fn fill_prefers_the_input_in_the_upper_window() {
        let dump = crate::json::parse(r#"{"windows":[
            {"type":1,"layer":5,"root":{"cls":"android.widget.FrameLayout","bounds":[0,1600,1080,2400],"children":[
                {"cls":"android.widget.EditText","rid":"com.foo:id/sheet_comment","text":"Comment","bounds":[40,2200,1040,2300],"children":[]}]}},
            {"type":1,"layer":2,"root":{"cls":"android.widget.FrameLayout","bounds":[0,0,1080,2400],"children":[
                {"cls":"android.widget.EditText","rid":"com.foo:id/comment","text":"Comment","bounds":[40,100,1040,200],"children":[]}]}}
        ]}"#).unwrap();
        let roots: Vec<_> = crate::json::app_roots(&dump).into_iter().rev().collect();
        let node = super::find_fill_target(&roots, "Comment").expect("an input matches");
        assert_eq!(super::node_to_daemon_selector(node), "id=com.foo:id/sheet_comment");
    }

    #[test]
    fn fill_finds_a_compose_field_by_the_label_inside_it() {
        // Compose TextField: the label is a child of the EditText, the hint is empty.
        let dump = crate::json::parse(r#"{"windows":[
            {"type":1,"layer":2,"root":{"cls":"android.view.View","bounds":[0,0,1080,2400],"children":[
                {"cls":"android.widget.TextView","text":"과일 검색","bounds":[42,42,290,134]},
                {"cls":"android.widget.EditText","bounds":[42,134,1038,302],"flags":"cLfevE","children":[
                    {"cls":"android.widget.TextView","text":"검색어","bounds":[84,197,205,260]}]},
                {"cls":"android.widget.EditText","bounds":[42,400,1038,560],"children":[
                    {"cls":"android.widget.TextView","text":"메모","bounds":[84,460,160,520]}]}]}}
        ]}"#).unwrap();
        let roots: Vec<_> = crate::json::app_roots(&dump).into_iter().rev().collect();
        let field = |q| super::find_fill_target(&roots, q).and_then(crate::selector::bounds);
        assert_eq!(field("검색어"), Some((42, 134, 1038, 302)));
        assert_eq!(field("메모"), Some((42, 400, 1038, 560)));
    }

    #[test]
    fn conflicting_bootstrap_selectors_are_rejected() {
        let args = ["--device", "SERIAL_A", "use", "SERIAL_B"]
            .iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
        let error = parse_args(&args).expect_err("conflicting selectors must fail");
        assert!(error.contains("conflicting device selectors"));
    }
}
