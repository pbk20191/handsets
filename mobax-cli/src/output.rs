// Unified output sink for action-verb results.
//
// Every RPA-facing verb funnels its success/failure payload through this
// module so the human and `--json` paths stay in lock-step. Two surfaces:
//
//   * `Reporter::ok(verb, builder)` — write the success record.
//   * `Reporter::fail(verb, err)`   — write the failure record and return
//                                     an `io::Error` carrying the structured
//                                     code so `main` can map it to an exit
//                                     status.
//
// In Human mode we keep the historical free-form text; in Json mode each
// call produces a single trailing-newline JSON line shaped like:
//
//   {"verb":"tap","ok":true,"result":{...}}
//   {"verb":"tap","ok":false,"error":{"code":"NOT_FOUND","detail":"..."}}

use std::cell::RefCell;
use std::io::{self, Write};

use crate::errors::{ErrCode, ErrInfo};

// ---------- capturable stdout / stderr ----------
//
// Verbs write through `stdout()` / `stderr()` (and the `outln!` / `errln!`
// macros) instead of `io::stdout()`. Normally that is the process's own
// stream; inside `capture` (the MCP server runs verbs in-process, where
// stdout is the JSON-RPC channel) both land in one per-thread buffer.

thread_local! {
    static CAPTURE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// Runs `f` with this thread's verb output (stdout and stderr) collected;
/// returns `f`'s result and the text.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, String) {
    let prev = CAPTURE.with(|c| c.borrow_mut().replace(Vec::new()));
    let r = f();
    let buf = CAPTURE.with(|c| std::mem::replace(&mut *c.borrow_mut(), prev)).unwrap_or_default();
    (r, String::from_utf8_lossy(&buf).into_owned())
}

fn captured(b: &[u8]) -> bool {
    CAPTURE.with(|c| match c.borrow_mut().as_mut() {
        Some(v) => { v.extend_from_slice(b); true }
        None => false,
    })
}

fn capturing() -> bool {
    CAPTURE.with(|c| c.borrow().is_some())
}

pub struct Stdout;
pub struct Stderr;

/// The verb's stdout: the process's own, or the capture buffer.
pub fn stdout() -> Stdout { Stdout }
/// The verb's stderr: the process's own, or the capture buffer.
pub fn stderr() -> Stderr { Stderr }

impl Write for Stdout {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if captured(b) { Ok(b.len()) } else { io::stdout().write(b) }
    }
    fn flush(&mut self) -> io::Result<()> {
        if capturing() { Ok(()) } else { io::stdout().flush() }
    }
}

impl Write for Stderr {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if captured(b) { Ok(b.len()) } else { io::stderr().write(b) }
    }
    fn flush(&mut self) -> io::Result<()> {
        if capturing() { Ok(()) } else { io::stderr().flush() }
    }
}

/// `println!` through [`stdout()`].
#[macro_export]
macro_rules! outln {
    () => {{ use std::io::Write as _; let _ = writeln!($crate::output::stdout()); }};
    ($($t:tt)*) => {{ use std::io::Write as _; let _ = writeln!($crate::output::stdout(), $($t)*); }};
}

/// `eprintln!` through [`stderr()`].
#[macro_export]
macro_rules! errln {
    () => {{ use std::io::Write as _; let _ = writeln!($crate::output::stderr()); }};
    ($($t:tt)*) => {{ use std::io::Write as _; let _ = writeln!($crate::output::stderr(), $($t)*); }};
}
use crate::flags::OutFmt;
use crate::json_out::Obj;

pub struct Reporter {
    pub fmt: OutFmt,
}

impl Reporter {
    pub fn new(fmt: OutFmt) -> Self { Self { fmt } }

    /// Emit a success record. `human` is the historical text line (printed
    /// verbatim with a trailing newline); `json_result` is the inner
    /// `result` object for the `--json` envelope.
    pub fn ok(&self, verb: &str, human: &str, json_result: Obj) -> io::Result<()> {
        let mut out = stdout();
        match self.fmt {
            OutFmt::Human => {
                writeln!(out, "{human}")?;
            }
            OutFmt::Json => {
                let line = Obj::new()
                    .s("verb", verb)
                    .b("ok", true)
                    .raw("result", &json_result.finish())
                    .finish();
                writeln!(out, "{line}")?;
            }
        }
        out.flush()
    }

    /// Convert an `ErrInfo` into both an output record (Human or JSON) and
    /// an `io::Error` whose `Other` payload carries the structured code via
    /// `ReportedError`. `main` downcasts that to choose the exit status.
    pub fn fail(&self, verb: &str, err: ErrInfo) -> io::Error {
        let _ = self.write_fail(verb, &err);
        io::Error::other(ReportedError { verb: verb.to_string(), info: err })
    }

    fn write_fail(&self, verb: &str, err: &ErrInfo) -> io::Result<()> {
        match self.fmt {
            OutFmt::Human => {
                // Errors go to stderr in human mode so the stdout pipe
                // stays a clean success-only channel.
                let mut e = stderr();
                writeln!(e, "ERR:{}:{}", err.code.as_short(), err.detail)?;
                e.flush()
            }
            OutFmt::Json => {
                let inner = Obj::new()
                    .s("code", err.code.as_short())
                    .s("detail", &err.detail);
                let line = Obj::new()
                    .s("verb", verb)
                    .b("ok", false)
                    .raw("error", &inner.finish())
                    .finish();
                let mut out = stdout();
                writeln!(out, "{line}")?;
                out.flush()
            }
        }
    }
}

/// `io::Error::other` payload that survives the round-trip back to `main`,
/// where it's downcast to set the process exit code.
#[derive(Debug)]
pub struct ReportedError {
    pub verb: String,
    pub info: ErrInfo,
}

impl std::fmt::Display for ReportedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.verb, self.info.message())
    }
}

impl std::error::Error for ReportedError {}

/// Best-effort: if `err` is one of ours, return the structured info.
pub fn err_code_of(err: &io::Error) -> Option<ErrCode> {
    err.get_ref()
        .and_then(|inner| inner.downcast_ref::<ReportedError>())
        .map(|r| r.info.code)
}
