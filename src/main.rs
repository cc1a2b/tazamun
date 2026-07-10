//! Thin binary over the tazamun library.
//!
//! Exit codes: 0 success, 1 runtime error, 2 usage error (from clap).

use clap::Parser;
use tazamun::cli::{Cli, Cmd, run};
use tazamun::service::RotatingLog;
use tazamun::ui::progress::Ui;
use tracing_subscriber::EnvFilter;

/// A daemon without a terminal (service mode) also logs to
/// `.tazamun/logs/daemon.log`, size-rotated at 5 MiB keeping 3 generations.
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;
const LOG_KEEP: usize = 3;

/// Tee: every trace line goes to the normal writer and, when present, the
/// rotating service log.
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

fn init_tracing(verbose: u8, ui: &Ui, file_log: Option<RotatingLog>) {
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

fn main() {
    let cli = Cli::parse();
    // Progress bars exist only for the foreground daemon; every other command
    // (and every non-TTY invocation) runs with presentation disabled.
    let ui = match &cli.cmd {
        Cmd::Start => Ui::detect(),
        _ => Ui::disabled(),
    };
    // Service mode (a daemon with no terminal) also writes the rotated
    // .tazamun/logs/daemon.log; interactive daemons and one-shot commands
    // don't touch the log.
    let file_log = {
        use std::io::IsTerminal;
        if matches!(&cli.cmd, Cmd::Start) && !std::io::stdout().is_terminal() {
            RotatingLog::open(&cli.dir, LOG_ROTATE_BYTES, LOG_KEEP).ok()
        } else {
            None
        }
    };
    init_tracing(cli.verbose, &ui, file_log);
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
