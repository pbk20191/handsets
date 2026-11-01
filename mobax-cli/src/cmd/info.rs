// `info` and `show PKG`.

use std::io::{self, Write};

use crate::*;

pub(crate) fn format_uptime(s: u64) -> String {
    if s == 0 { return String::new(); }
    let d = s / 86400;
    let h = (s % 86400) / 3600;
    let m = (s % 3600) / 60;
    if d > 0 { format!("{d}d {h}h {m}m") }
    else if h > 0 { format!("{h}h {m}m") }
    else { format!("{m}m") }
}

pub(crate) fn format_bytes_pair(used_kb: u64, total_kb: u64) -> String {
    if total_kb == 0 { return String::new(); }
    let to_gib = |kb: u64| (kb as f64) / 1024.0 / 1024.0;
    format!("{:.1} / {:.1} GiB", to_gib(used_kb), to_gib(total_kb))
}

/// `mobax info` — neofetch-style snapshot. Reads the local push-mirrored
/// cache file (the state-daemon spawned by `mobax use` keeps it fresh via
/// event hooks + 5-s heartbeat). No daemon round-trips on the hot path —
/// total cost is one file read + JSON parse + format.
pub(crate) fn run_info(host: &str, port: u16) -> io::Result<()> {
    // 1. Fast path — cache file written by the host-side state-daemon.
    let snap_bytes = match cached_state() {
        Some(b) => b,
        None => {
            // No daemon yet (or stale). Fall back to a single wire call.
            let mut conn = Conn::connect(host, port)?;
            conn.call("state device")?
        }
    };
    let state = json::parse(std::str::from_utf8(&snap_bytes).unwrap_or("{}"))
        .unwrap_or(json::Value::Null);
    let get_str = |k: &str| -> String {
        if let json::Value::Obj(fields) = &state {
            for (kk, v) in fields {
                if kk == k {
                    return match v {
                        json::Value::Str(s) => s.clone(),
                        json::Value::Num(n) => n.to_string(),
                        json::Value::Bool(b) => b.to_string(),
                        _ => String::new(),
                    };
                }
            }
        }
        String::new()
    };
    // Pulled from the snapshot — static (cached once on the daemon side)
    // and dynamic (refreshed on every snapshot refresh).
    let model       = get_str("model");
    let mfg         = get_str("manufacturer");
    let device      = get_str("device");
    let release     = get_str("release");
    let codename    = get_str("codename");
    let build_type  = get_str("build_type");
    let fingerprint = get_str("fingerprint");
    let abi         = get_str("abi");
    let sdk         = get_str("sdk");
    let width       = get_str("width");
    let height      = get_str("height");
    let rotation    = get_str("rotation");
    let battery     = get_str("battery_level");
    let charging    = get_str("battery_charging");
    let inter       = get_str("interactive");
    let top         = get_str("top_activity");
    let temp_c      = get_str("battery_temp_c");
    let uptime_s    = get_str("uptime_s");
    let mem_total_k = get_str("total_ram_kb");
    let mem_avail_k = get_str("mem_available_kb");
    let stor_total_k = get_str("total_storage_kb");
    let stor_free_k = get_str("storage_free_kb");
    let cpu_cores   = get_str("cpu_cores");
    let cpu_model   = get_str("cpu_model");
    let kernel      = get_str("kernel");
    let locale      = get_str("locale");
    let timezone    = get_str("timezone");
    let theme       = get_str("theme");
    let net         = get_str("network");
    let ip          = get_str("ip");
    let ssid        = get_str("wifi_ssid");
    let app_count   = get_str("app_count");
    let app_3rd     = get_str("app_count_3rd");
    let _ = (host, fingerprint.clone());   // hush unused warnings

    // Compose the right-hand-side rows.
    let title  = if !model.is_empty() && !mfg.is_empty() {
        format!("{mfg} {model}")
    } else if !model.is_empty() { model.clone() } else { device.clone() };
    let title_under = "─".repeat(title.chars().count().max(1));

    let mut rows: Vec<(String, String)> = Vec::new();
    let push = |rows: &mut Vec<(String, String)>, k: &str, v: String| {
        if !v.is_empty() { rows.push((k.into(), v)); }
    };
    push(&mut rows, "OS",      if release.is_empty() {
            String::new()
        } else {
            format!("Android {release}{}{}{}",
                if codename.is_empty() || codename == "REL" { String::new() }
                    else { format!(" ({codename})") },
                if sdk.is_empty() { String::new() } else { format!(" — SDK {sdk}") },
                if build_type.is_empty() || build_type == "user" { String::new() }
                    else { format!(" ({build_type})") })
        });
    push(&mut rows, "Kernel",  kernel.clone());
    push(&mut rows, "Uptime",  format_uptime(uptime_s.parse().unwrap_or(0)));
    push(&mut rows, "Display", if width.is_empty() { String::new() }
        else { format!("{width} × {height}   (rotation {rotation})") });
    push(&mut rows, "CPU",     if cpu_cores.is_empty() { abi.clone() }
        else if cpu_model.is_empty() { format!("{cpu_cores}× {abi}") }
        else { format!("{cpu_cores}× {abi}  ({cpu_model})") });
    let mem_t: u64 = mem_total_k.parse().unwrap_or(0);
    let mem_a: u64 = mem_avail_k.parse().unwrap_or(0);
    push(&mut rows, "Memory", format_bytes_pair(mem_t.saturating_sub(mem_a), mem_t));
    let stor_t: u64 = stor_total_k.parse().unwrap_or(0);
    let stor_f: u64 = stor_free_k.parse().unwrap_or(0);
    push(&mut rows, "Storage", format_bytes_pair(stor_t.saturating_sub(stor_f), stor_t));
    push(&mut rows, "Battery", if battery.is_empty() { String::new() }
        else {
            let onoff = if inter == "true" { "screen on" } else { "screen off" };
            let chg   = if charging == "true" { ", charging" } else { "" };
            let temp  = if temp_c.is_empty() || temp_c == "null" { String::new() }
                        else { format!(", {temp_c} °C") };
            format!("{battery}%  ({onoff}{chg}{temp})")
        });
    push(&mut rows, "Network", if net.is_empty() { String::new() } else {
        let mut s = net.clone();
        if !ssid.is_empty() { s.push_str(" · "); s.push_str(&ssid); }
        if !ip.is_empty()   { s.push_str(" · "); s.push_str(&ip); }
        s
    });
    push(&mut rows, "Theme",   theme.clone());
    push(&mut rows, "Locale",  if locale.is_empty() { String::new() }
        else if timezone.is_empty() { locale.clone() }
        else { format!("{locale}  ·  {timezone}") });
    push(&mut rows, "Apps",    if app_count.is_empty() { String::new() }
        else if app_3rd.is_empty() || app_3rd == "0" { format!("{app_count} packages") }
        else { format!("{app_count} packages  ({app_3rd} third-party)") });
    push(&mut rows, "Top",     top);
    push(&mut rows, "Daemon",  format!("mobaxd on tcp:{port}"));

    // ASCII android bugdroid (7 rows, ~13 wide).
    let logo: [&str; 12] = [
        "                ",
        "    \\       /   ",
        "     \\ ___ /    ",
        "      /   \\     ",
        "     | o o |    ",
        "     |  _  |    ",
        "      \\___/     ",
        "     |     |    ",
        "     |     |    ",
        "     |_____|    ",
        "      |   |     ",
        "                ",
    ];

    // Header rows: title and underline.
    let mut header = vec![
        ("".to_string(), title),
        ("".to_string(), title_under),
    ];
    header.extend(rows);
    let rows = header;

    let mut out = crate::output::stdout();
    let n = rows.len().max(logo.len());
    for i in 0..n {
        let logo_line = logo.get(i).copied().unwrap_or("                ");
        let (key, val) = rows.get(i).cloned().unwrap_or_default();
        if key.is_empty() {
            writeln!(out, "{logo_line}{val}")?;
        } else {
            writeln!(out, "{logo_line}{key:<8}  {val}")?;
        }
    }
    out.flush()
}

/// `mobax show <pkg>` — pm_path + the daemon's dumpsys package summary.
pub(crate) fn run_show_pkg(host: &str, port: u16, pkg: &str) -> io::Result<()> {
    let mut conn = Conn::connect(host, port)?;
    let path = conn.call(&format!("pm_path {pkg}"))?;
    let path_s = std::str::from_utf8(&path).unwrap_or("").trim();
    let mut out = crate::output::stdout();
    if path_s.starts_with("ERR:") {
        return Err(io::Error::other(path_s.to_string()));
    }
    writeln!(out, "package: {pkg}")?;
    writeln!(out, "  path:  {path_s}")?;
    // Try a short dumpsys package — the first few lines usually cover
    // versionName / versionCode / firstInstallTime.
    let info = conn.call(&format!("dumpsys package {pkg}"));
    if info.is_ok() {
        // dumpsys returns chunked frames; for now skip detailed parsing.
        // The path alone is the most-asked-for field.
    }
    out.flush()
}
