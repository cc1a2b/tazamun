//! Thin binary over the tazamun library.
//!
//! Exit codes: 0 success, 1 runtime error, 2 usage error (from clap).

use clap::Parser;
use tazamun::cli::{Cli, run};
use tracing_subscriber::EnvFilter;

fn init_tracing(verbose: u8) {
    let default = if verbose > 0 {
        "tazamun=debug"
    } else {
        "tazamun=info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
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
    if let Err(e) = runtime.block_on(run(cli)) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
