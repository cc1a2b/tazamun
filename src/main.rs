//! Thin binary over the tazamun library.
//!
//! Exit codes: 0 success, 1 runtime error, 2 usage error (from clap).

use clap::Parser;
use tazamun::cli::{Cli, Cmd, run};
use tazamun::ui::progress::Ui;
use tracing_subscriber::EnvFilter;

fn init_tracing(verbose: u8, ui: &Ui) {
    use std::io::IsTerminal;
    let default = if verbose > 0 {
        "tazamun=debug"
    } else {
        "tazamun=info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let ansi = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(ansi)
        .with_writer(ui.tracing_writer())
        .init();
}

fn main() {
    let cli = Cli::parse();
    // Progress bars exist only for the foreground daemon; every other command
    // (and every non-TTY invocation) runs with presentation disabled.
    let ui = match &cli.cmd {
        Cmd::Start { .. } => Ui::detect(),
        _ => Ui::disabled(),
    };
    init_tracing(cli.verbose, &ui);
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
