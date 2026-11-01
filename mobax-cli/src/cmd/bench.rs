// `bench`: wire-level latency over one warm socket.

use std::io;
use std::time::{Duration, Instant};

use crate::*;

pub(crate) struct BenchCase {
    label: &'static str,
    wire: &'static str,
}

pub(crate) struct BenchResult {
    label: &'static str,
    wire: &'static str,
    min_ms: f64,
    p50_ms: f64,
    p90_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    mean_ms: f64,
    stddev_ms: f64,
    bytes: usize,
    errors: u32,
    last_error: Option<String>,
}

pub(crate) fn bench(host: &str, port: u16, n: u32, json_output: bool) -> io::Result<()> {
    let mut conn = Conn::connect(host, port)?;
    let meta = bench_metadata(&mut conn)?;
    // Discover available commands by probing — start with cheap ones.
    // Warm the default mirror first so the size-sweep timing doesn't include
    // the one-time VirtualDisplay creation for each new size.
    let _ = conn.call("screenshot")?;
    let _ = conn.call("screenshot size=480")?;
    let _ = conn.call("screenshot size=1080")?;
    let _ = conn.call("screenshot max=1")?;
    // Then re-warm 768 so the bench starts from the default.
    let _ = conn.call("screenshot size=768")?;

    let cases: &[BenchCase] = &[
        BenchCase { label: "ping",                       wire: "ping" },
        BenchCase { label: "info",                       wire: "info" },
        BenchCase { label: "wm_info",                    wire: "wm_info" },
        BenchCase { label: "state top",                  wire: "state top" },
        BenchCase { label: "wait_for_idle",              wire: "wait_for_idle idle_ms=0 timeout_ms=1000" },
        BenchCase { label: "dump_active",                wire: "dump_active" },
        BenchCase { label: "dump",                       wire: "dump" },
        BenchCase { label: "screenshot 480 q80 jpeg",    wire: "screenshot size=480" },
        BenchCase { label: "screenshot 768 q80 jpeg",    wire: "screenshot size=768" },
        BenchCase { label: "screenshot 1080 q80 jpeg",   wire: "screenshot size=1080" },
        BenchCase { label: "screenshot 1080 q95 jpeg",   wire: "screenshot size=1080 q=95" },
        BenchCase { label: "screenshot native q80 jpeg", wire: "screenshot max=1" },
        BenchCase { label: "screenshot 768 q95 jpeg",    wire: "screenshot size=768 q=95" },
        BenchCase { label: "screenshot 768 q80 webp",    wire: "screenshot size=768 fmt=webp" },
    ];

    let mut results = Vec::with_capacity(cases.len());
    if !json_output {
        crate::errln!("warm-socket benchmark, n={n} per command");
        crate::errln!(
            "device: {}{}{}  host: {}/{}  mobax {}",
            meta.device_label(),
            meta.display_label(),
            meta.top_label(),
            std::env::consts::OS,
            std::env::consts::ARCH,
            env!("CARGO_PKG_VERSION"),
        );
        crate::errln!(
            "{:<30}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}  {:>12}  {:>6}",
            "command", "min ms", "p50 ms", "p95 ms", "p99 ms", "mean ms", "bytes", "errors"
        );
    }
    for case in cases {
        // Warm-up.
        let _ = conn.call(case.wire);
        let mut samples = Vec::with_capacity(n as usize);
        let mut last_len = 0usize;
        let mut errors = 0u32;
        let mut last_error = None;
        for _ in 0..n {
            let t0 = Instant::now();
            match conn.call(case.wire) {
                Ok(body) if body.starts_with(b"ERR:") => {
                    errors += 1;
                    last_error = Some(String::from_utf8_lossy(&body).into_owned());
                }
                Ok(body) => {
                    samples.push(t0.elapsed());
                    last_len = body.len();
                }
                Err(e) => {
                    errors += 1;
                    last_error = Some(e.to_string());
                }
            }
        }
        if samples.is_empty() {
            let result = BenchResult {
                label: case.label,
                wire: case.wire,
                min_ms: 0.0,
                p50_ms: 0.0,
                p90_ms: 0.0,
                p95_ms: 0.0,
                p99_ms: 0.0,
                mean_ms: 0.0,
                stddev_ms: 0.0,
                bytes: last_len,
                errors,
                last_error,
            };
            if !json_output {
                crate::errln!(
                    "{:<30}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}  {:>12}  {:>6}",
                    case.label, "-", "-", "-", "-", "-", last_len, errors,
                );
            }
            results.push(result);
            continue;
        }
        samples.sort();
        let min_ms = ms(samples[0]);
        let p50_ms = ms(percentile(&samples, 0.50));
        let p90_ms = ms(percentile(&samples, 0.90));
        let p95_ms = ms(percentile(&samples, 0.95));
        let p99_ms = ms(percentile(&samples, 0.99));
        let mean_ms = samples.iter().map(|d| ms(*d)).sum::<f64>() / samples.len() as f64;
        let variance = samples.iter()
            .map(|d| {
                let delta = ms(*d) - mean_ms;
                delta * delta
            })
            .sum::<f64>() / samples.len() as f64;
        let result = BenchResult {
            label: case.label,
            wire: case.wire,
            min_ms,
            p50_ms,
            p90_ms,
            p95_ms,
            p99_ms,
            mean_ms,
            stddev_ms: variance.sqrt(),
            bytes: last_len,
            errors,
            last_error,
        };
        if !json_output {
            crate::errln!(
                "{:<30}  {:>9.3}  {:>9.3}  {:>9.3}  {:>9.3}  {:>9.3}  {:>12}  {:>6}",
                result.label,
                result.min_ms,
                result.p50_ms,
                result.p95_ms,
                result.p99_ms,
                result.mean_ms,
                result.bytes,
                result.errors,
            );
        }
        results.push(result);
    }
    if json_output {
        crate::outln!("{}", bench_json(n, &meta, &results));
    }
    Ok(())
}

pub(crate) fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

pub(crate) fn percentile(samples: &[Duration], q: f64) -> Duration {
    let idx = ((samples.len() as f64 * q).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len() - 1);
    samples[idx]
}

pub(crate) struct BenchMeta {
    model: String,
    manufacturer: String,
    sdk: String,
    release: String,
    width: String,
    height: String,
    rotation: String,
    top_activity: String,
}

impl BenchMeta {
    fn device_label(&self) -> String {
        let name = match (self.manufacturer.is_empty(), self.model.is_empty()) {
            (false, false) => format!("{} {}", self.manufacturer, self.model),
            (_, false) => self.model.clone(),
            _ => "unknown device".into(),
        };
        let os = match (self.release.is_empty(), self.sdk.is_empty()) {
            (false, false) => format!(" Android {} SDK {}", self.release, self.sdk),
            (true, false) => format!(" SDK {}", self.sdk),
            (false, true) => format!(" Android {}", self.release),
            (true, true) => String::new(),
        };
        format!("{name}{os}")
    }

    fn display_label(&self) -> String {
        if self.width.is_empty() || self.height.is_empty() {
            String::new()
        } else if self.rotation.is_empty() {
            format!("  display: {}x{}", self.width, self.height)
        } else {
            format!("  display: {}x{} r{}", self.width, self.height, self.rotation)
        }
    }

    fn top_label(&self) -> String {
        if self.top_activity.is_empty() || self.top_activity == "null" {
            String::new()
        } else {
            format!("  top: {}", self.top_activity)
        }
    }
}

pub(crate) fn bench_metadata(conn: &mut Conn) -> io::Result<BenchMeta> {
    let snap = match cached_state() {
        Some(b) => b,
        None => conn.call("state device")?,
    };
    let state = json::parse(std::str::from_utf8(&snap).unwrap_or("{}"))
        .unwrap_or(json::Value::Null);
    let get = |k: &str| json_field_string(&state, k);
    Ok(BenchMeta {
        model: get("model"),
        manufacturer: get("manufacturer"),
        sdk: get("sdk"),
        release: get("release"),
        width: get("width"),
        height: get("height"),
        rotation: get("rotation"),
        top_activity: get("top_activity"),
    })
}

pub(crate) fn json_field_string(v: &json::Value, key: &str) -> String {
    if let json::Value::Obj(fields) = v {
        for (k, val) in fields {
            if k == key {
                return match val {
                    json::Value::Str(s) => s.clone(),
                    json::Value::Num(n) => n.to_string(),
                    json::Value::Bool(b) => b.to_string(),
                    _ => String::new(),
                };
            }
        }
    }
    String::new()
}

pub(crate) fn bench_json(n: u32, meta: &BenchMeta, results: &[BenchResult]) -> String {
    let metadata = json_out::Obj::new()
        .s("model", &meta.model)
        .s("manufacturer", &meta.manufacturer)
        .s("sdk", &meta.sdk)
        .s("release", &meta.release)
        .s("width", &meta.width)
        .s("height", &meta.height)
        .s("rotation", &meta.rotation)
        .s("top_activity", &meta.top_activity)
        .s("host_os", std::env::consts::OS)
        .s("host_arch", std::env::consts::ARCH)
        .s("mobax_version", env!("CARGO_PKG_VERSION"))
        .finish();
    let mut cases = String::from("[");
    for (i, r) in results.iter().enumerate() {
        if i > 0 { cases.push(','); }
        cases.push_str(&bench_result_json(r));
    }
    cases.push(']');
    json_out::Obj::new()
        .s("type", "mobax_bench")
        .n("n", n as i64)
        .s("mode", "warm_socket")
        .raw("metadata", &metadata)
        .raw("cases", &cases)
        .finish()
}

pub(crate) fn bench_result_json(r: &BenchResult) -> String {
    let mut s = json_out::Obj::new()
        .s("command", r.label)
        .s("wire", r.wire)
        .raw("min_ms", &format!("{:.3}", r.min_ms))
        .raw("p50_ms", &format!("{:.3}", r.p50_ms))
        .raw("p90_ms", &format!("{:.3}", r.p90_ms))
        .raw("p95_ms", &format!("{:.3}", r.p95_ms))
        .raw("p99_ms", &format!("{:.3}", r.p99_ms))
        .raw("mean_ms", &format!("{:.3}", r.mean_ms))
        .raw("stddev_ms", &format!("{:.3}", r.stddev_ms))
        .n("bytes", r.bytes as i64)
        .n("errors", r.errors as i64);
    if let Some(e) = r.last_error.as_deref() {
        s = s.s("last_error", e);
    }
    s.finish()
}
