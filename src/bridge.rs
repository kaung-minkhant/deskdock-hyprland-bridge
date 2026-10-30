use crate::{
    android::{self, DisplaySize},
    config::{Config, Device as ConfiguredDevice, DeviceKind},
    control::ControlSocket,
    keyboard::{KeyAction, KeyboardState},
    state::Shared,
    synergy,
};
use anyhow::{Context, Result, bail};
use evdev::{
    AbsoluteAxisCode, Device, EventSummary, KeyCode, RelativeAxisCode, SynchronizationCode,
};
use std::{
    fs,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

// Input and protocol polling must stay short: the previous single-device loop
// delivered a completed evdev frame promptly, while a 10 ms worker poll plus
// socket and channel waits made the multi-device path visibly laggy.
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const ROTATION_CHECK_INTERVAL: Duration = Duration::from_millis(500);

pub fn list_devices() -> Result<()> {
    let mut devices: Vec<_> = evdev::enumerate()
        .filter(|(_, device)| is_supported_device(device))
        .collect();
    devices.sort_by(|(left, _), (right, _)| left.cmp(right));
    if devices.is_empty() {
        bail!(
            "No readable input devices found. Check access to /dev/input/event* and try `newgrp input` if needed."
        );
    }

    println!("Supported evdev devices (kind = \"mouse\", \"touchpad\", or \"keyboard\"):");
    for (path, device) in devices {
        println!(
            "\n{} ({})",
            device.name().unwrap_or("<unnamed>"),
            device_kind_label(&device)
        );
        println!("  current: {}", path.display());
        match stable_device_path(&path) {
            Some(stable_path) => println!("  stable:  {}", stable_path.display()),
            None => println!("  stable:  no /dev/input/by-id or /dev/input/by-path alias found"),
        }
    }
    println!("\nAdd only the devices you want captured to config.toml.");
    Ok(())
}

pub fn run(config: Config, shared: Shared) -> Result<()> {
    let tablet_mode = Arc::clone(&shared.tablet_mode);
    let (events, event_receiver) = mpsc::channel();
    let configured_paths = config
        .devices
        .iter()
        .map(|device| device.path.clone())
        .collect::<Vec<_>>();

    for device in config.devices.iter().cloned() {
        spawn_input_worker(device, Arc::clone(&tablet_mode), events.clone())?;
    }
    drop(events);

    eprintln!("Linux mode: configured devices remain with Hyprland until tablet mode is toggled.");
    for path in &configured_paths {
        eprintln!("Configured device: {}", path.display());
    }
    if config
        .devices
        .iter()
        .any(|device| matches!(device.kind, DeviceKind::Keyboard))
    {
        eprintln!("Keyboard forwarding enabled: press Super+Shift+T to return to Linux mode.");
    }

    let control_socket = ControlSocket::bind(Arc::clone(&tablet_mode))?;
    eprintln!("Control socket: {}", control_socket.path().display());

    let listener = TcpListener::bind((config.synergy.address.as_str(), config.synergy.port))
        .with_context(|| {
            format!(
                "Could not listen on {}:{}; another Synergy server may be running",
                config.synergy.address, config.synergy.port
            )
        })?;
    eprintln!(
        "Waiting for DeskDock's Synergy/Barrier connection on {}:{}.",
        config.synergy.address, config.synergy.port
    );

    listener
        .set_nonblocking(true)
        .context("Could not make the Synergy listener nonblocking")?;
    while !shared.is_shutdown() {
        let (stream, peer) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) => return Err(error).context("Could not accept DeskDock connection"),
        };
        eprintln!("DeskDock connected from {peer}. Sending Synergy handshake...");
        let result = run_session(&config, stream, &event_receiver, &shared);
        shared.connected.store(false, Ordering::SeqCst);
        tablet_mode.store(false, Ordering::SeqCst);
        if let Err(error) = result {
            eprintln!("DeskDock session ended: {error:#}");
            eprintln!("Linux mode restored; waiting for DeskDock to reconnect...");
        }
    }
    eprintln!("Bridge stopped; devices released to Hyprland.");
    Ok(())
}

fn spawn_input_worker(
    configured: ConfiguredDevice,
    tablet_mode: Arc<AtomicBool>,
    sender: mpsc::Sender<InputEvent>,
) -> Result<()> {
    let mut device = Device::open(&configured.path).with_context(|| {
        format!(
            "Could not open configured device {}",
            configured.path.display()
        )
    })?;
    validate_device(&device, &configured)?;
    device
        .set_nonblocking(true)
        .with_context(|| format!("Could not make {} nonblocking", configured.path.display()))?;

    thread::spawn(move || {
        let path = configured.path;
        let kind = configured.kind;
        let mut grabbed = false;
        let mut mouse = MouseState::default();
        let mut touchpad = TouchpadState::new(&device);
        loop {
            let mut should_grab = tablet_mode.load(Ordering::SeqCst);
            // The tablet-mode shortcut (Super+Shift+T) is still held when the
            // toggle arrives. Grabbing now would hide the key releases from
            // Hyprland and leave its modifiers stuck, so a keyboard waits until
            // every key is up before it is captured.
            if should_grab
                && !grabbed
                && kind == DeviceKind::Keyboard
                && device
                    .get_key_state()
                    .map(|keys| keys.iter().next().is_some())
                    .unwrap_or(false)
            {
                should_grab = false;
            }
            if should_grab != grabbed {
                let result = if should_grab {
                    device.grab()
                } else {
                    device.ungrab()
                };
                if let Err(error) = result {
                    let _ = sender.send(InputEvent::Error {
                        path: path.clone(),
                        error: format!(
                            "Could not {} {}: {error}",
                            if should_grab { "grab" } else { "release" },
                            path.display()
                        ),
                    });
                    tablet_mode.store(false, Ordering::SeqCst);
                    return;
                }
                grabbed = should_grab;
                // Events were deliberately ignored in Linux mode, so their
                // absolute coordinates cannot be compared with the first
                // Android-mode contact. Start that contact with a fresh
                // baseline to prevent a one-frame cursor jump.
                if grabbed && kind == DeviceKind::Touchpad {
                    touchpad.reset_motion();
                }
            }

            let events = match device.fetch_events() {
                Ok(events) => events,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_INTERVAL);
                    continue;
                }
                Err(error) => {
                    let _ = sender.send(InputEvent::Error {
                        path: path.clone(),
                        error: format!("Could not read {}: {error}", path.display()),
                    });
                    tablet_mode.store(false, Ordering::SeqCst);
                    return;
                }
            };
            for event in events {
                if !tablet_mode.load(Ordering::SeqCst) {
                    continue;
                }
                // A keyboard must never reach both machines: forward only
                // while it is actually grabbed away from Hyprland.
                if kind == DeviceKind::Keyboard && !grabbed {
                    continue;
                }
                if let Some(event) =
                    translate_event(kind, &mut mouse, &mut touchpad, event.destructure())
                    && sender.send(event).is_err()
                {
                    return;
                }
            }
        }
    });
    Ok(())
}

fn run_session(
    config: &Config,
    mut stream: TcpStream,
    events: &Receiver<InputEvent>,
    shared: &Shared,
) -> Result<()> {
    let tablet_mode: &AtomicBool = &shared.tablet_mode;
    synergy::handshake(&mut stream)?;
    synergy::send_frame(&mut stream, &synergy::server_options())?;
    let display_size = if config.tablet.follow_rotation {
        android::logical_display_size(config.tablet.adb_serial.as_deref()).with_context(|| {
            "Could not read Android's initial logical display size; disable tablet.follow_rotation to use configured dimensions"
        })?
    } else {
        DisplaySize {
            width: config.tablet.width,
            height: config.tablet.height,
        }
    };
    synergy::send_frame(
        &mut stream,
        &synergy::screen_info(display_size.width, display_size.height),
    )?;
    synergy::send_frame(
        &mut stream,
        &synergy::client_enter(display_size.width, display_size.height),
    )?;
    stream.set_nonblocking(true)?;
    shared.connected.store(true, Ordering::SeqCst);
    eprintln!("DeskDock Synergy session active.");

    let mut display_size = display_size;
    let mut pointer = Pointer::centered(display_size.width, display_size.height);
    let mut tablet_active = false;
    let mut keyboard = KeyboardState::default();
    let mut last_rotation_check = std::time::Instant::now() - ROTATION_CHECK_INTERVAL;
    while !shared.is_shutdown() {
        if config.tablet.follow_rotation && last_rotation_check.elapsed() >= ROTATION_CHECK_INTERVAL
        {
            last_rotation_check = std::time::Instant::now();
            match android::logical_display_size(config.tablet.adb_serial.as_deref()) {
                Ok(size) if size != display_size => {
                    display_size = size;
                    pointer.resize(size.width, size.height);
                    synergy::send_frame(
                        &mut stream,
                        &synergy::screen_info(display_size.width, display_size.height),
                    )?;
                    if tablet_active {
                        synergy::send_frame(
                            &mut stream,
                            &synergy::mouse_move(pointer.x as i16, pointer.y as i16),
                        )?;
                    }
                    eprintln!(
                        "Android rotation detected; pointer bounds updated to {}×{}.",
                        size.width, size.height
                    );
                }
                Ok(_) => {}
                Err(error) => eprintln!("Could not refresh Android display size: {error:#}"),
            }
        }
        let active = tablet_mode.load(Ordering::SeqCst);
        if active != tablet_active {
            if active {
                pointer.center();
                synergy::send_frame(
                    &mut stream,
                    &synergy::mouse_move(pointer.x as i16, pointer.y as i16),
                )?;
                eprintln!(
                    "Tablet mode: all configured pointers are captured; Android pointer reset to center."
                );
            } else {
                // Never leave a key or modifier stuck down on Android.
                keyboard.release_all(&mut stream)?;
                synergy::send_frame(&mut stream, synergy::client_leave())?;
                eprintln!("Linux mode: all configured devices were released to Hyprland.");
            }
            tablet_active = active;
        }

        synergy::drain_client_messages(&mut stream, display_size.width, display_size.height)?;
        match events.recv_timeout(POLL_INTERVAL) {
            Ok(event) => {
                handle_input_event(
                    &mut stream,
                    &mut pointer,
                    &mut keyboard,
                    config.tablet.sensitivity,
                    tablet_mode,
                    event,
                )?;
                // Drain any already queued input frames. This prevents a busy
                // pointer from building a delayed Synergy packet backlog.
                while let Ok(event) = events.try_recv() {
                    handle_input_event(
                        &mut stream,
                        &mut pointer,
                        &mut keyboard,
                        config.tablet.sensitivity,
                        tablet_mode,
                        event,
                    )?;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("All configured input workers stopped"),
        }
    }

    // Shutting down: never leave keys stuck or the pointer on Android.
    if tablet_active {
        keyboard.release_all(&mut stream)?;
        synergy::send_frame(&mut stream, synergy::client_leave())?;
    }
    Ok(())
}

fn handle_input_event(
    stream: &mut TcpStream,
    pointer: &mut Pointer,
    keyboard: &mut KeyboardState,
    sensitivity: i32,
    tablet_mode: &AtomicBool,
    event: InputEvent,
) -> Result<()> {
    let tablet_active = tablet_mode.load(Ordering::SeqCst);
    match event {
        InputEvent::Key { code, down } if tablet_active => {
            if keyboard.handle(stream, code, down)? == KeyAction::ReturnToLinux {
                // The session loop sees the flag change and sends COUT.
                tablet_mode.store(false, Ordering::SeqCst);
            }
        }
        InputEvent::Motion { x, y } if tablet_active => {
            pointer.move_by(x * sensitivity, y * sensitivity);
            synergy::send_frame(
                stream,
                &synergy::mouse_move(pointer.x as i16, pointer.y as i16),
            )?;
        }
        InputEvent::Button { down, button } if tablet_active => {
            synergy::send_frame(stream, &synergy::mouse_button(down, button.synergy_code()))?;
        }
        InputEvent::Wheel { y } if tablet_active => {
            synergy::send_frame(stream, &synergy::mouse_wheel(0, y.signum() as i16))?;
        }
        InputEvent::Error { path, error } => {
            bail!("Input worker for {} stopped: {error}", path.display());
        }
        _ => {}
    }
    Ok(())
}

enum InputEvent {
    Motion { x: i32, y: i32 },
    Button { down: bool, button: MouseButton },
    Wheel { y: i32 },
    Key { code: KeyCode, down: bool },
    Error { path: PathBuf, error: String },
}

#[derive(Clone, Copy)]
enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    const fn synergy_code(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Middle => 2,
            Self::Right => 3,
        }
    }
}

fn translate_event(
    kind: DeviceKind,
    mouse: &mut MouseState,
    touchpad: &mut TouchpadState,
    event: EventSummary,
) -> Option<InputEvent> {
    match kind {
        DeviceKind::Mouse => mouse.translate(event),
        DeviceKind::Keyboard => match event {
            // Ignore auto-repeat (value 2); Android generates its own.
            EventSummary::Key(_, code, value @ (0 | 1)) => Some(InputEvent::Key {
                code,
                down: value == 1,
            }),
            _ => None,
        },
        DeviceKind::Touchpad => match event {
            // Preserve the touchpad's physical left click while using its
            // contact coordinates only for relative pointer movement.
            EventSummary::Key(_, KeyCode::BTN_LEFT, value @ (0 | 1)) => Some(InputEvent::Button {
                down: value == 1,
                button: MouseButton::Left,
            }),
            _ => touchpad.translate(event),
        },
    }
}

#[derive(Default)]
struct MouseState {
    pending_x: i32,
    pending_y: i32,
}

impl MouseState {
    fn translate(&mut self, event: EventSummary) -> Option<InputEvent> {
        match event {
            EventSummary::RelativeAxis(_, RelativeAxisCode::REL_X, x) => {
                self.pending_x += x;
                None
            }
            EventSummary::RelativeAxis(_, RelativeAxisCode::REL_Y, y) => {
                self.pending_y += y;
                None
            }
            // A dropped event means relative movement is incomplete; discard
            // it rather than turning an invalid frame boundary into a jump.
            EventSummary::Synchronization(_, SynchronizationCode::SYN_DROPPED, _) => {
                self.pending_x = 0;
                self.pending_y = 0;
                None
            }
            EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _)
                if self.pending_x != 0 || self.pending_y != 0 =>
            {
                let event = InputEvent::Motion {
                    x: std::mem::take(&mut self.pending_x),
                    y: std::mem::take(&mut self.pending_y),
                };
                Some(event)
            }
            EventSummary::RelativeAxis(_, RelativeAxisCode::REL_WHEEL, y) if y != 0 => {
                Some(InputEvent::Wheel { y })
            }
            EventSummary::Key(_, KeyCode::BTN_LEFT, value @ (0 | 1)) => Some(InputEvent::Button {
                down: value == 1,
                button: MouseButton::Left,
            }),
            EventSummary::Key(_, KeyCode::BTN_RIGHT, value @ (0 | 1)) => Some(InputEvent::Button {
                down: value == 1,
                button: MouseButton::Right,
            }),
            EventSummary::Key(_, KeyCode::BTN_MIDDLE, value @ (0 | 1)) => {
                Some(InputEvent::Button {
                    down: value == 1,
                    button: MouseButton::Middle,
                })
            }
            _ => None,
        }
    }
}

struct TouchpadState {
    active_slot: i32,
    primary_tracking: bool,
    finger_count: u8,
    last_x: Option<i32>,
    last_y: Option<i32>,
    current_x: Option<i32>,
    current_y: Option<i32>,
    x_range: i32,
    y_range: i32,
    // Preserve sub-pixel movement between frames. Without these remainders,
    // slow diagonal contact motion is rounded down independently per axis and
    // becomes a horizontal/vertical stair-step.
    x_remainder: f64,
    y_remainder: f64,
}

impl TouchpadState {
    fn new(device: &Device) -> Self {
        let info = device.get_abs_state().ok();
        let x_range = info
            .as_ref()
            .map(|state| {
                state[AbsoluteAxisCode::ABS_MT_POSITION_X.0 as usize].maximum
                    - state[AbsoluteAxisCode::ABS_MT_POSITION_X.0 as usize].minimum
            })
            .unwrap_or(1)
            .max(1);
        let y_range = info
            .as_ref()
            .map(|state| {
                state[AbsoluteAxisCode::ABS_MT_POSITION_Y.0 as usize].maximum
                    - state[AbsoluteAxisCode::ABS_MT_POSITION_Y.0 as usize].minimum
            })
            .unwrap_or(1)
            .max(1);
        Self {
            active_slot: 0,
            primary_tracking: false,
            finger_count: 0,
            last_x: None,
            last_y: None,
            current_x: None,
            current_y: None,
            x_range,
            y_range,
            x_remainder: 0.0,
            y_remainder: 0.0,
        }
    }

    fn translate(&mut self, event: EventSummary) -> Option<InputEvent> {
        match event {
            // Multi-touch position events are scoped to the most recently
            // selected slot. Follow only slot 0, the primary contact.
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_SLOT, slot) => {
                self.active_slot = slot;
                None
            }
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_TRACKING_ID, tracking_id)
                if self.active_slot == 0 =>
            {
                self.primary_tracking = tracking_id >= 0;
                if !self.primary_tracking {
                    self.reset_motion();
                }
                None
            }
            EventSummary::Key(_, KeyCode::BTN_TOOL_FINGER, value @ (0 | 1)) => {
                self.finger_count = value as u8;
                if self.finger_count != 1 {
                    self.reset_motion();
                }
                None
            }
            EventSummary::Key(_, KeyCode::BTN_TOOL_DOUBLETAP, value @ (0 | 1)) => {
                if value == 1 {
                    self.finger_count = 2;
                    self.reset_motion();
                }
                None
            }
            EventSummary::Key(_, KeyCode::BTN_TOOL_TRIPLETAP, value @ (0 | 1)) => {
                if value == 1 {
                    self.finger_count = 3;
                    self.reset_motion();
                }
                None
            }
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_POSITION_X, value)
                if self.active_slot == 0 =>
            {
                self.current_x = Some(value);
                None
            }
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_POSITION_Y, value)
                if self.active_slot == 0 =>
            {
                self.current_y = Some(value);
                None
            }
            // `fetch_events` does not repair state after SYN_DROPPED. Forget
            // the old baseline so its next complete report only establishes a
            // position; never infer motion across lost input events.
            EventSummary::Synchronization(_, SynchronizationCode::SYN_DROPPED, _) => {
                self.reset_motion();
                None
            }
            EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _)
                if self.primary_tracking && self.finger_count == 1 =>
            {
                let (x, y) = (self.current_x?, self.current_y?);
                let previous = (self.last_x.replace(x), self.last_y.replace(y));
                let (Some(last_x), Some(last_y)) = previous else {
                    return None;
                };
                // Keep fractional movement across frames. Integer-dividing
                // each tiny absolute delta independently makes slow diagonal
                // movement collapse into horizontal/vertical stair-steps.
                self.x_remainder += f64::from(x - last_x) * 1000.0 / f64::from(self.x_range);
                self.y_remainder += f64::from(y - last_y) * 1000.0 / f64::from(self.y_range);
                let delta_x = self.x_remainder.trunc() as i32;
                let delta_y = self.y_remainder.trunc() as i32;
                self.x_remainder -= f64::from(delta_x);
                self.y_remainder -= f64::from(delta_y);
                (delta_x != 0 || delta_y != 0).then_some(InputEvent::Motion {
                    x: delta_x,
                    y: delta_y,
                })
            }
            _ => None,
        }
    }

    fn reset_motion(&mut self) {
        self.last_x = None;
        self.last_y = None;
        self.current_x = None;
        self.current_y = None;
        self.x_remainder = 0.0;
        self.y_remainder = 0.0;
    }
}

fn validate_device(device: &Device, configured: &ConfiguredDevice) -> Result<()> {
    let matches = match configured.kind {
        DeviceKind::Mouse => is_relative_mouse(device),
        DeviceKind::Touchpad => is_touchpad(device),
        DeviceKind::Keyboard => is_keyboard(device),
    };
    if matches {
        Ok(())
    } else {
        bail!(
            "{} is not a supported {} device",
            configured.path.display(),
            match configured.kind {
                DeviceKind::Mouse => "relative mouse",
                DeviceKind::Touchpad => "touchpad",
                DeviceKind::Keyboard => "keyboard",
            }
        )
    }
}

fn is_supported_device(device: &Device) -> bool {
    is_relative_mouse(device) || is_touchpad(device) || is_keyboard(device)
}
fn is_keyboard(device: &Device) -> bool {
    // Require ordinary typing keys so power buttons, lid switches, and
    // media-only HID interfaces are not mistaken for keyboards.
    !is_relative_mouse(device)
        && device.supported_keys().is_some_and(|keys| {
            [
                KeyCode::KEY_A,
                KeyCode::KEY_Z,
                KeyCode::KEY_ENTER,
                KeyCode::KEY_SPACE,
            ]
            .iter()
            .all(|key| keys.contains(*key))
        })
}
fn is_relative_mouse(device: &Device) -> bool {
    device.supported_relative_axes().is_some_and(|axes| {
        axes.contains(RelativeAxisCode::REL_X) && axes.contains(RelativeAxisCode::REL_Y)
    }) && device
        .supported_keys()
        .is_some_and(|keys| keys.contains(KeyCode::BTN_LEFT))
}
fn is_touchpad(device: &Device) -> bool {
    device.supported_absolute_axes().is_some_and(|axes| {
        axes.contains(AbsoluteAxisCode::ABS_MT_POSITION_X)
            && axes.contains(AbsoluteAxisCode::ABS_MT_POSITION_Y)
    }) && device
        .supported_keys()
        .is_some_and(|keys| keys.contains(KeyCode::BTN_TOUCH))
}
fn device_kind_label(device: &Device) -> &'static str {
    if is_relative_mouse(device) {
        "mouse"
    } else if is_touchpad(device) {
        "touchpad"
    } else {
        "keyboard"
    }
}

fn stable_device_path(event_path: &Path) -> Option<PathBuf> {
    let event_target = fs::canonicalize(event_path).ok()?;
    for directory in ["/dev/input/by-id", "/dev/input/by-path"] {
        let mut aliases = fs::read_dir(directory)
            .ok()?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                let name = path.to_string_lossy();
                name.ends_with("-event-mouse") || name.ends_with("-event-kbd")
            })
            .filter(|path| fs::canonicalize(path).ok().as_ref() == Some(&event_target))
            .collect::<Vec<_>>();
        aliases.sort();
        if let Some(alias) = aliases.into_iter().next() {
            return Some(alias);
        }
    }
    None
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
    fn resize(&mut self, width: i16, height: i16) {
        self.width = i32::from(width);
        self.height = i32::from(height);
        self.x = self.x.clamp(0, self.width - 1);
        self.y = self.y.clamp(0, self.height - 1);
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
