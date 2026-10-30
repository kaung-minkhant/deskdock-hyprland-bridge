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
use std::{
    path::PathBuf,
    process::{Command as ProcessCommand, Stdio},
};

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

    /// Start DeskDock, the bridge, and a tray icon. By default this detaches from the terminal.
    Run {
        /// Configuration file path. Defaults to $XDG_CONFIG_HOME/hypr-android-bridge/config.toml.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Do not show a tray icon.
        #[arg(long)]
        no_tray: bool,
        /// Keep the process attached to this terminal and print its diagnostics.
        #[arg(long)]
        foreground: bool,
    },
    /// Internal foreground process used by `run`; not intended for direct use.
    #[command(hide = true)]
    RunForeground {
        #[arg(long)]
        config: Option<PathBuf>,
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
        Command::Run {
            config,
            no_tray,
            foreground,
        } => {
            if foreground {
                run_foreground(config, no_tray)
            } else {
                run_background(config, no_tray)
            }
        }
        Command::RunForeground { config, no_tray } => run_foreground(config, no_tray),
        Command::Toggle => control::toggle(),
    }
}

/// Replace this short-lived launcher with a detached copy of ourselves. Keeping
/// the daemon as a child executable (rather than forking Rust threads) gives it
/// its own Ctrl+C handler and preserves the Wayland/D-Bus session environment.
fn run_background(config: Option<PathBuf>, no_tray: bool) -> Result<()> {
    let executable = std::env::current_exe().context("Could not find the bridge executable")?;
    let mut command = ProcessCommand::new(executable);
    command
        .arg("run-foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = config {
        command.arg("--config").arg(path);
    }
    if no_tray {
        command.arg("--no-tray");
    }
    let child = command
        .spawn()
        .context("Could not start the bridge in the background")?;
    println!(
        "Started DeskDock bridge (PID {}). Use the Waybar tray icon to control it.",
        child.id()
    );
    Ok(())
}

fn run_foreground(config: Option<PathBuf>, no_tray: bool) -> Result<()> {
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
            config.deskdock.display.clone(),
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
