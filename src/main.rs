use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use evdev::{Device, EventSummary, KeyCode, RelativeAxisCode};
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
    /// Forward one relative-pointer device to Android until Ctrl+C.
    ///
    /// This is deliberately a single-device prototype. It does not grab the
    /// mouse unless --grab is explicitly provided.
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
        /// Android pixels to move per one relative mouse unit.
        #[arg(long, default_value_t = 4)]
        sensitivity: i32,
    },
    /// Send one Android touchscreen swipe, for verifying pointer delivery.
    TestSwipe {
        /// Device serial from `adb devices`.
        #[arg(long)]
        serial: Option<String>,
    },
    /// Send an Android left-click/tap at the center of the display.
    TestTap {
        /// Device serial from `adb devices`.
        #[arg(long)]
        serial: Option<String>,
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
            sensitivity,
        } => forward(&device, serial.as_deref(), grab, sensitivity),
        CommandKind::TestSwipe { serial } => test_swipe(serial.as_deref()),
        CommandKind::TestTap { serial } => test_tap(serial.as_deref()),
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

fn forward(path: &Path, serial: Option<&str>, grab: bool, sensitivity: i32) -> Result<()> {
    if sensitivity <= 0 {
        bail!("--sensitivity must be a positive number");
    }

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
    } else {
        eprintln!("Read-only mode: Hyprland will still receive this mouse's input.");
    }

    let (width, height) = tablet_size(serial)?;
    let mut pointer = Pointer::centered(width, height);
    let mut pending_x = 0;
    let mut pending_y = 0;
    eprintln!("Android display: {width}x{height}; virtual pointer begins at its center.");
    eprintln!("Press Ctrl+C to stop. Use --grab only after validating forwarding.");

    loop {
        // `fetch_events()` yields the currently buffered frame, then its
        // iterator ends. Keep fetching frames instead of exiting after the
        // first one.
        for event in device
            .fetch_events()
            .context("Could not read input events")?
        {
            match event.destructure() {
                EventSummary::RelativeAxis(_, RelativeAxisCode::REL_X, value) => pending_x += value,
                EventSummary::RelativeAxis(_, RelativeAxisCode::REL_Y, value) => pending_y += value,
                EventSummary::Synchronization(_, _, _) if pending_x != 0 || pending_y != 0 => {
                    pointer.move_by(pending_x * sensitivity, pending_y * sensitivity);
                    send_move(serial, pointer.x, pointer.y)?;
                    pending_x = 0;
                    pending_y = 0;
                }
                EventSummary::RelativeAxis(_, RelativeAxisCode::REL_WHEEL, value) => {
                    send_scroll(serial, value)?;
                }
                EventSummary::Key(_, KeyCode::BTN_LEFT, 1) => {
                    send_tap(serial, pointer.x, pointer.y)?
                }
                EventSummary::Key(_, key, value) if value == 1 => {
                    eprintln!("ignored button/key press: {key:?}");
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug)]
struct Pointer {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Pointer {
    fn centered(width: u32, height: u32) -> Self {
        Self {
            x: (width / 2) as i32,
            y: (height / 2) as i32,
            width: width as i32,
            height: height as i32,
        }
    }

    fn move_by(&mut self, x: i32, y: i32) {
        self.x = (self.x + x).clamp(0, self.width - 1);
        self.y = (self.y + y).clamp(0, self.height - 1);
    }
}

fn test_swipe(serial: Option<&str>) -> Result<()> {
    let (width, height) = tablet_size(serial)?;
    send_swipe(
        serial,
        width as i32 / 4,
        height as i32 / 2,
        width as i32 * 3 / 4,
        height as i32 / 2,
        300,
    )
}

fn test_tap(serial: Option<&str>) -> Result<()> {
    let (width, height) = tablet_size(serial)?;
    send_tap(serial, width as i32 / 2, height as i32 / 2)
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

fn send_move(serial: Option<&str>, x: i32, y: i32) -> Result<()> {
    // Ask Android to treat this as a mouse motion event, so Android can render
    // its native pointer instead of interpreting movement as a touch gesture.
    send_input(
        serial,
        [
            "mouse",
            "motionevent",
            "MOVE",
            &x.to_string(),
            &y.to_string(),
        ],
    )
}

fn send_tap(serial: Option<&str>, x: i32, y: i32) -> Result<()> {
    let x = x.to_string();
    let y = y.to_string();
    send_input(serial, ["mouse", "motionevent", "DOWN", &x, &y])?;
    send_input(serial, ["mouse", "motionevent", "UP", &x, &y])
}

fn send_scroll(serial: Option<&str>, amount: i32) -> Result<()> {
    let (width, height) = tablet_size(serial)?;
    let x = (width / 2).to_string();
    let start_y = (height / 2).to_string();
    let end_y = ((height as i32 / 2) - amount.signum() * 300)
        .clamp(0, height as i32 - 1)
        .to_string();
    send_input(
        serial,
        ["touchscreen", "swipe", &x, &start_y, &x, &end_y, "150"],
    )
}

fn send_swipe(
    serial: Option<&str>,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    duration_ms: i32,
) -> Result<()> {
    send_input(
        serial,
        [
            "touchscreen",
            "swipe",
            &x1.to_string(),
            &y1.to_string(),
            &x2.to_string(),
            &y2.to_string(),
            &duration_ms.to_string(),
        ],
    )
}

fn send_input<const N: usize>(serial: Option<&str>, input_args: [&str; N]) -> Result<()> {
    let mut args = adb_args(serial);
    args.extend(["shell", "input"]);
    args.extend(input_args);
    let status = Command::new("adb")
        .args(&args)
        .stdout(Stdio::null())
        .status()
        .context("Could not send Android input event")?;
    if !status.success() {
        bail!("ADB rejected Android input event");
    }
    Ok(())
}

fn adb_args(serial: Option<&str>) -> Vec<&str> {
    serial.map(|serial| vec!["-s", serial]).unwrap_or_default()
}
