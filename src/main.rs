mod android;
mod bridge;
mod config;
mod control;
mod deskdock;
mod synergy;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

const SYNERGY_PORT: u16 = 24800;

#[derive(Debug, Parser)]
#[command(about = "Hyprland mouse bridge for DeskDock's Synergy/Barrier integration")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List evdev devices accessible to the current user.
    ListDevices,
    /// Configure DeskDock to connect to this bridge over localhost.
    EnableSynergy {
        /// Synergy server address saved in DeskDock's preferences.
        #[arg(long, default_value = "127.0.0.1")]
        address: String,
    },
    /// Create the default configuration template without overwriting an existing file.
    InitConfig {
        /// Optional configuration file path. Defaults to $XDG_CONFIG_HOME/hypr-android-bridge/config.toml.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Run the configured bridge and wait for DeskDock to connect.
    Forward {
        /// Configuration file path. Defaults to $XDG_CONFIG_HOME/hypr-android-bridge/config.toml.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Toggle a running bridge between Linux and exclusive tablet pointer mode.
    Toggle,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::ListDevices => bridge::list_devices(),
        Command::EnableSynergy { address } => deskdock::enable_synergy(&address, SYNERGY_PORT),
        Command::InitConfig { config } => {
            let path = config.map(Ok).unwrap_or_else(config::default_path)?;
            config::initialize(&path)
        }
        Command::Forward { config } => {
            let path = config.map(Ok).unwrap_or_else(config::default_path)?;
            let config = config::Config::load(&path)?;
            bridge::run(config)
        }
        Command::Toggle => control::toggle(),
    }
}
