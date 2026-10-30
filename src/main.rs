use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use evdev::{Device, EventSummary, KeyCode};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Parser)]
#[command(about = "Experimental Hyprland-to-Android input bridge")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// List input devices that can be opened by this user.
    ListDevices,
    /// Verify that an ADB target accepts input commands.
    CheckAdb {
        /// Device serial from `adb devices`.
        #[arg(long)]
        serial: Option<String>,
    },
    /// Forward events from one evdev device to Android until Ctrl+C.
    ///
    /// This is deliberately a single-device, no-grab diagnostic mode. It does
    /// not yet take exclusive control of your mouse or keyboard.
    Forward {
        /// /dev/input/eventN device to monitor.
        #[arg(long)]
        device: PathBuf,
        /// Device serial from `adb devices`.
        #[arg(long)]
        serial: Option<String>,
        /// Grab the device exclusively. Only use this after confirming the
        /// non-grabbing mode works and keep a separate recovery path ready.
        #[arg(long)]
        grab: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        CommandKind::ListDevices => list_devices(),
        CommandKind::CheckAdb { serial } => check_adb(serial.as_deref()),
        CommandKind::Forward {
            device,
            serial,
            grab,
        } => forward(&device, serial.as_deref(), grab),
    }
}

fn list_devices() -> Result<()> {
    let mut devices: Vec<_> = evdev::enumerate().collect();
    devices.sort_by(|(left, _), (right, _)| left.cmp(right));

    if devices.is_empty() {
        bail!("No readable evdev devices found. Check access to /dev/input/event*.");
    }

    for (path, device) in devices {
        let keys = device
            .supported_keys()
            .map(|keys| format!(" keys={}", keys.iter().count()))
            .unwrap_or_default();
        let relative_axes = device
            .supported_relative_axes()
            .map(|axes| format!(" rel_axes={axes:?}"))
            .unwrap_or_default();
        println!(
            "{}\t{}{}{}",
            path.display(),
            device.name().unwrap_or("<unnamed>"),
            keys,
            relative_axes
        );
    }
    Ok(())
}

fn check_adb(serial: Option<&str>) -> Result<()> {
    let mut args = adb_args(serial);
    args.extend(["shell", "input", "keyevent", "KEYCODE_WAKEUP"]);
    let status = Command::new("adb").args(&args).status().context(
        "Could not run adb. Ensure android-tools is installed and the tablet is connected.",
    )?;
    if !status.success() {
        bail!("ADB input test failed. Run `adb devices` and supply --serial if necessary.");
    }
    println!("ADB input test sent successfully.");
    Ok(())
}

fn forward(path: &Path, serial: Option<&str>, grab: bool) -> Result<()> {
    let mut device =
        Device::open(path).with_context(|| format!("Could not open {}", path.display()))?;
    println!(
        "Listening to {} ({})",
        path.display(),
        device.name().unwrap_or("unnamed")
    );

    if grab {
        device.grab().with_context(|| {
            format!(
                "Could not exclusively grab {}. Do not retry with sudo; configure device permissions instead.",
                path.display()
            )
        })?;
        eprintln!("WARNING: device is exclusively grabbed. Press Ctrl+C to release it.");
    }

    let tablet_size = tablet_size(serial)?;
    eprintln!("Android display: {}x{}", tablet_size.0, tablet_size.1);
    eprintln!("Diagnostic mapping only: relative movement becomes a short touchscreen swipe.");

    for event in device
        .fetch_events()
        .context("Could not read input events")?
    {
        match event.destructure() {
            EventSummary::RelativeAxis(_, axis, value) => {
                eprintln!("relative {axis:?}: {value}");
            }
            EventSummary::Key(_, key, value) if value == 1 => {
                eprintln!("key pressed: {key:?}");
                if key == KeyCode::KEY_ESC {
                    send_key(serial, "KEYCODE_BACK")?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn tablet_size(serial: Option<&str>) -> Result<(u32, u32)> {
    let mut args = adb_args(serial);
    args.extend(["shell", "wm", "size"]);
    let output = Command::new("adb")
        .args(&args)
        .output()
        .context("Could not query Android display size with adb")?;
    if !output.status.success() {
        bail!("`adb shell wm size` failed");
    }
    let output = String::from_utf8(output.stdout).context("ADB returned invalid UTF-8")?;
    let dimensions = output
        .split_whitespace()
        .find(|word| word.contains('x'))
        .context("Could not find display dimensions in `adb shell wm size` output")?;
    let (width, height) = dimensions
        .split_once('x')
        .context("Malformed Android display dimensions")?;
    Ok((width.parse()?, height.parse()?))
}

fn send_key(serial: Option<&str>, key: &str) -> Result<()> {
    let mut args = adb_args(serial);
    args.extend(["shell", "input", "keyevent", key]);
    let status = Command::new("adb")
        .args(&args)
        .stdout(Stdio::null())
        .status()
        .context("Could not send Android key event")?;
    if !status.success() {
        bail!("ADB rejected Android key event");
    }
    Ok(())
}

fn adb_args(serial: Option<&str>) -> Vec<&str> {
    serial.map(|serial| vec!["-s", serial]).unwrap_or_default()
}
