#![warn(clippy::all, clippy::pedantic, rust_2018_idioms)]

// TODO: Replace regex with glob
// TODO: Global custom hook fn

mod commands;

use clap::Parser;

use commands::Commands;

use sfsl::get_scoop_path;

/// Scoop utilities that can replace the slowest parts of Scoop, and run anywhere from 30-100 times faster
#[derive(Debug, Parser)]
#[clap(about, long_about, author, version)]
struct Args {
    #[command(subcommand)]
    command: Commands,

    #[clap(long, global = true, help = "Disable terminal formatting")]
    no_color: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.no_color {
        colored::control::set_override(false);
    }

    args.command.run()
}
