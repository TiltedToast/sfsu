#![warn(clippy::all, clippy::pedantic, rust_2018_idioms)]

// TODO: Replace regex with glob
// TODO: Global custom hook fn

mod buckets;
mod commands;
mod config;
mod packages;

use std::path::PathBuf;

use clap::Parser;

use commands::Commands;

#[must_use]
/// Gets the user's scoop path, via either the default path or as provided by the SCOOP env variable
///
/// Will ignore the global scoop path
///
/// # Panics
/// - There is no home folder
/// - The discovered scoop path does not exist
fn get_scoop_path() -> PathBuf {
    use std::env::var_os;

    // TODO: Add support for both global and non-global scoop installs

    let scoop_path = {
        if let Some(path) = var_os("SCOOP") {
            path.into()
        } else if let Some(path) = config::Scoop::load()
            .expect("scoop config loaded correctly")
            .root_path
        {
            path.into()
        } else {
            dirs::home_dir().expect("user home directory").join("scoop")
        }
    };

    if scoop_path.exists() {
        dunce::canonicalize(scoop_path).expect("failed to find real path to scoop")
    } else {
        panic!("Scoop path does not exist");
    }
}

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
