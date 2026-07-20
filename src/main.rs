//! Thin binary over the tazamun library.
//!
//! Exit codes: 0 success, 1 runtime error, 2 usage error (from clap).

use clap::{CommandFactory, Parser};
use tazamun::cli::{Cli, Cmd, run};
use tazamun::service::LineCappedLog;
use tazamun::ui::progress::Ui;
use tracing_subscriber::EnvFilter;

/// Tee: every trace line goes to the normal writer and, when present, the
/// line-capped OS-directory log.
struct Tee<A, B>(A, B);

impl<A: std::io::Write, B: std::io::Write> std::io::Write for Tee<A, B> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.0.write(buf)?;
        let _ = self.1.write_all(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()?;
        let _ = self.1.flush();
        Ok(())
    }
}

fn init_tracing(verbose: u8, ui: &Ui, file_log: Option<LineCappedLog>) {
    use std::io::IsTerminal;
    let default = if verbose > 0 {
        "tazamun=debug"
    } else {
        "tazamun=info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let ansi = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    use tracing_subscriber::fmt::MakeWriter;
    let base = ui.tracing_writer();
    match file_log {
        Some(log) => {
            let make = move || Tee(base.make_writer(), log.clone());
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(make)
                .init();
        }
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(ansi)
                .with_writer(base)
                .init();
        }
    }
}

/// clap's own did-you-mean scores with Jaro above a 0.7 cut, which is too
/// strict for short commands: `gi` against `gui` scores 0.611, so the most
/// likely typo of the most-reached-for command got no hint at all. When clap
/// rejects a subcommand outright, add our own suggestion before its usage text.
fn parse_cli() -> Cli {
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            if err.kind() == clap::error::ErrorKind::InvalidSubcommand
                && let Some(clap::error::ContextValue::String(typed)) =
                    err.get(clap::error::ContextKind::InvalidSubcommand)
            {
                let cmd = Cli::command();
                let names: Vec<String> = cmd
                    .get_subcommands()
                    .filter(|s| !s.is_hide_set())
                    .map(|s| s.get_name().to_string())
                    .collect();
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let hits = tazamun::suggest::closest(typed, &refs, 3);
                if !hits.is_empty() {
                    let list = hits
                        .iter()
                        .map(|h| format!("'{h}'"))
                        .collect::<Vec<_>>()
                        .join(" or ");
                    eprintln!("error: unrecognized subcommand '{typed}'\n");
                    eprintln!("  did you mean {list}?\n");
                    eprintln!("Usage: tazamun [OPTIONS] [COMMAND]");
                    eprintln!("\nFor the full list, try 'tazamun --help'.");
                    std::process::exit(2);
                }
            }
            // Anything else — including --help and --version, which clap
            // reports as "errors" — prints exactly as clap intended.
            err.exit()
        }
    }
}

fn main() {
    let cli = parse_cli();
    // Progress bars exist only for the foreground daemon; every other command
    // (and every non-TTY invocation) runs with presentation disabled.
    let ui = match &cli.cmd {
        Some(Cmd::Start { .. }) => Ui::detect(),
        _ => Ui::disabled(),
    };
    // The daemon always keeps a persistent, line-capped log under the OS log
    // directory (see `state::log_file_path`) — foreground or unattended — so a
    // record survives after the terminal scrolls or the process exits. One-shot
    // commands never touch it. (`--log-file` on the installed service unit is
    // now implied and harmless.)
    let file_log = match &cli.cmd {
        Some(Cmd::Start { .. }) => {
            LineCappedLog::open(&cli.dir, tazamun::consts::LOG_MAX_LINES).ok()
        }
        _ => None,
    };
    init_tracing(cli.verbose, &ui, file_log);
    // The native GUI runs the winit event loop on the true main thread and builds
    // its own async runtime for background I/O, so it must run OUTSIDE the outer
    // Tokio runtime (nesting runtimes panics; winit requires the main thread).
    if matches!(cli.cmd, Some(Cmd::Gui)) {
        if let Err(e) = tazamun::gui_native::run() {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start async runtime: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = runtime.block_on(run(cli, ui)) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
