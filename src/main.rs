mod bridge;
mod control;
mod deskdock;
mod synergy;

use anyhow::{Result, bail};
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
    /// Run the mouse bridge and wait for DeskDock to connect.
    Forward {
        /// Relative-pointer evdev device to control.
        #[arg(long)]
        device: PathBuf,
        /// Local Synergy/Barrier port for DeskDock to connect to.
        #[arg(long, default_value_t = SYNERGY_PORT)]
        port: u16,
        /// Android pixels per relative mouse unit.
        #[arg(long, default_value_t = 4)]
        sensitivity: i32,
    },
    /// Toggle a running bridge between Linux and exclusive tablet mouse mode.
    Toggle,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::ListDevices => bridge::list_devices(),
        Command::EnableSynergy { address } => deskdock::enable_synergy(&address, SYNERGY_PORT),
        Command::Forward {
            device,
            port,
            sensitivity,
        } => {
            if sensitivity <= 0 {
                bail!("--sensitivity must be a positive number");
            }
            bridge::run(&device, port, sensitivity)
        }
        Command::Toggle => control::toggle(),
    }
}
