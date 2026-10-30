use crate::{control::ControlSocket, synergy};
use anyhow::{Context, Result, bail};
use evdev::{Device, EventSummary, KeyCode, RelativeAxisCode};
use std::{
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const TABLET_WIDTH: i16 = 1800;
const TABLET_HEIGHT: i16 = 2880;

pub fn list_devices() -> Result<()> {
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

pub fn run(path: &Path, port: u16, sensitivity: i32) -> Result<()> {
    let mut device =
        Device::open(path).with_context(|| format!("Could not open {}", path.display()))?;
    device
        .set_nonblocking(true)
        .context("Could not make the evdev device nonblocking")?;

    println!(
        "Listening to {} ({})",
        path.display(),
        device.name().unwrap_or("unnamed")
    );
    eprintln!(
        "Linux mode: Hyprland receives this mouse. Run `hypr-android-bridge toggle` to enter tablet mode."
    );

    let tablet_mode = Arc::new(AtomicBool::new(false));
    let control_socket = ControlSocket::bind(Arc::clone(&tablet_mode))?;
    eprintln!("Control socket: {}", control_socket.path().display());

    let listener = TcpListener::bind(("127.0.0.1", port)).with_context(|| {
        format!("Could not listen on 127.0.0.1:{port}; another Synergy server may be running")
    })?;
    eprintln!("Waiting for DeskDock's Synergy/Barrier connection on 127.0.0.1:{port}.");

    loop {
        let (stream, peer) = listener
            .accept()
            .context("Could not accept DeskDock connection")?;
        eprintln!("DeskDock connected from {peer}. Sending Synergy handshake...");
        if let Err(error) = run_session(&mut device, path, stream, sensitivity, &tablet_mode) {
            // A disconnected DeskDock session must never retain the mouse.
            tablet_mode.store(false, Ordering::SeqCst);
            let _ = device.ungrab();
            eprintln!("DeskDock session ended: {error:#}");
            eprintln!("Linux mode restored; waiting for DeskDock to reconnect...");
        }
    }
}

fn run_session(
    device: &mut Device,
    path: &Path,
    mut stream: TcpStream,
    sensitivity: i32,
    tablet_mode: &AtomicBool,
) -> Result<()> {
    synergy::handshake(&mut stream)?;
    synergy::send_frame(&mut stream, &synergy::server_options())?;
    synergy::send_frame(
        &mut stream,
        &synergy::screen_info(TABLET_WIDTH, TABLET_HEIGHT),
    )?;
    synergy::send_frame(
        &mut stream,
        &synergy::client_enter(TABLET_WIDTH, TABLET_HEIGHT),
    )?;
    stream.set_read_timeout(Some(Duration::from_millis(10)))?;
    eprintln!("DeskDock Synergy session active. Press Ctrl+C to stop.");

    let mut pointer = Pointer::centered(TABLET_WIDTH, TABLET_HEIGHT);
    let mut pending_x = 0;
    let mut pending_y = 0;
    let mut device_grabbed = false;

    loop {
        let should_grab = tablet_mode.load(Ordering::SeqCst);
        if should_grab != device_grabbed {
            if should_grab {
                device.grab().with_context(|| {
                    format!("Could not grab {} for tablet mode", path.display())
                })?;
                pointer.center();
                synergy::send_frame(
                    &mut stream,
                    &synergy::mouse_move(pointer.x as i16, pointer.y as i16),
                )?;
                eprintln!(
                    "Tablet mode: {} is captured; Android pointer reset to center.",
                    path.display()
                );
            } else {
                device.ungrab().with_context(|| {
                    format!("Could not release {} back to Linux", path.display())
                })?;
                eprintln!(
                    "Linux mode: {} was released back to Hyprland.",
                    path.display()
                );
            }
            device_grabbed = should_grab;
        }

        synergy::drain_client_messages(&mut stream, TABLET_WIDTH, TABLET_HEIGHT)?;
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => return Err(error).context("Could not read input events"),
        };

        for event in events {
            if !tablet_mode.load(Ordering::SeqCst) {
                continue;
            }
            match event.destructure() {
                EventSummary::RelativeAxis(_, RelativeAxisCode::REL_X, value) => pending_x += value,
                EventSummary::RelativeAxis(_, RelativeAxisCode::REL_Y, value) => pending_y += value,
                EventSummary::Synchronization(_, _, _) if pending_x != 0 || pending_y != 0 => {
                    pointer.move_by(pending_x * sensitivity, pending_y * sensitivity);
                    synergy::send_frame(
                        &mut stream,
                        &synergy::mouse_move(pointer.x as i16, pointer.y as i16),
                    )?;
                    pending_x = 0;
                    pending_y = 0;
                }
                EventSummary::RelativeAxis(_, RelativeAxisCode::REL_WHEEL, value) if value != 0 => {
                    synergy::send_frame(
                        &mut stream,
                        &synergy::mouse_wheel(0, value.signum() as i16),
                    )?;
                }
                EventSummary::Key(_, KeyCode::BTN_LEFT, value @ (0 | 1)) => {
                    synergy::send_frame(&mut stream, &synergy::mouse_button(value == 1, 1))?;
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
    fn centered(width: i16, height: i16) -> Self {
        Self {
            x: i32::from(width) / 2,
            y: i32::from(height) / 2,
            width: i32::from(width),
            height: i32::from(height),
        }
    }

    fn center(&mut self) {
        self.x = self.width / 2;
        self.y = self.height / 2;
    }

    fn move_by(&mut self, x: i32, y: i32) {
        self.x = (self.x + x).clamp(0, self.width - 1);
        self.y = (self.y + y).clamp(0, self.height - 1);
    }
}
