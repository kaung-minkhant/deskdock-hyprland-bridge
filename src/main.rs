mod android;
mod bridge;
mod config;
mod control;
mod deskdock;
mod keyboard;
mod state;
mod supervisor;
mod synergy;
mod tray;

use anyhow::{Context, Result};
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
    /// Start everything: DeskDock, the bridge, and a tray icon. Ctrl+C or the tray's Quit stops all.
    Run {
        /// Configuration file path. Defaults to $XDG_CONFIG_HOME/hypr-android-bridge/config.toml.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Do not show a tray icon.
        #[arg(long)]
        no_tray: bool,
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
            bridge::run(config, state::Shared::default())
        }
        Command::Run { config, no_tray } => run(config, no_tray),
        Command::Toggle => control::toggle(),
    }
}

fn run(config: Option<PathBuf>, no_tray: bool) -> Result<()> {
    let path = config.map(Ok).unwrap_or_else(config::default_path)?;
    let config = config::Config::load(&path)?;
    let shared = state::Shared::default();

    {
        let shared = shared.clone();
        ctrlc::set_handler(move || shared.request_shutdown())
            .context("Could not install the Ctrl+C handler")?;
    }

    // Best effort: DeskDock's preferences only exist after its first launch.
    if config.deskdock.launch
        && let Err(error) = deskdock::configure(&config.synergy.address)
    {
        eprintln!("Skipping DeskDock Synergy setup: {error:#}");
    }

    let tray = (!no_tray).then(|| tray::spawn(shared.clone()));
    let supervisor = if config.deskdock.launch {
        Some(supervisor::spawn(
            config.deskdock.command.clone(),
            shared.clone(),
        )?)
    } else {
        None
    };

    let result = bridge::run(config, shared.clone());

    shared.request_shutdown();
    if let Some(supervisor) = supervisor {
        let _ = supervisor.join();
    }
    if let Some(tray) = tray {
        let _ = tray.join();
    }
    result
}
