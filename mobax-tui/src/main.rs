// mobax-tui — keyboard-driven Android UI inspector backed by the
// mobileax daemon. Standalone binary; `mobax tui` spawns us with the right
// --device/--host/--port. The connection and wire protocol are
// mobax-core's Conn, the same one the CLI uses.

mod app;
mod conn;
mod json;
mod model;
mod ui;

use std::io::{self, stdout, Stdout};
use std::process::ExitCode;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;

const USAGE: &str = "\
Usage: mobax-tui [options]

Options:
  --device S   the device (default: the one `mobax use` picked last, or the
               only one attached)
  --host H     a remote daemon host; local devices go through the adb server
  --port P     daemon instance on the device (default 9008); with --host,
               the TCP port there
  --active     list only the focused window (default: every app window,
               including popups and bottom sheets in their own window)
  -h, --help   show this message
";

struct Args {
    host: String,
    port: u16,
    active: bool,
}

impl Default for Args {
    fn default() -> Self { Self { host: "127.0.0.1".into(), port: 9008, active: false } }
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => { print!("{USAGE}"); std::process::exit(0); }
            "--host" => a.host = it.next().ok_or("--host needs a value")?,
            "--port" => a.port = it.next().ok_or("--port needs a value")?
                .parse().map_err(|_| "bad --port".to_string())?,
            "--active" => a.active = true,
            "--device" | "-s" => mobax_core::device::set_target(Some(it.next().ok_or("--device needs a SERIAL")?)),
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => { eprintln!("error: {e}\n\n{USAGE}"); return ExitCode::from(2); }
    };

    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("mobax-tui: {e}"); ExitCode::from(1) }
    }
}

fn run(args: Args) -> io::Result<()> {
    let mut app = App::new(args.host, args.port, args.active)?;

    let mut terminal = setup_terminal()?;
    let result = app.run(&mut terminal);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(out))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}
