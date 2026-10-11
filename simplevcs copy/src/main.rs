use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use clap::Parser;
use simplevcs::cli::Cli;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    if let Err(e) = Cli::parse().execute() {
        eprintln!("Error: {:?}", e);
        std::process::exit(1);
    }
    Ok(())
}
