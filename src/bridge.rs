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
    time::{Duration, Instant},
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
    let connected = Arc::clone(&shared.connected);
    let (events, event_receiver) = mpsc::channel();
    let configured_paths = config
        .devices
        .iter()
        .map(|device| device.path.clone())
        .collect::<Vec<_>>();

    for device in config.devices.iter().cloned() {
        spawn_input_worker(
            device,
            Arc::clone(&tablet_mode),
            Arc::clone(&connected),
            config.tablet.touchpad_scroll_percent,
            config.tablet.touchpad_tap_delay_ms,
            events.clone(),
        )?;
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

    let control_socket = ControlSocket::bind(shared.clone())?;
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

/// Poll Android's logical display size without ever blocking the input loop.
/// Errors are logged here because rotation is an optional convenience; the
/// previous valid size remains usable when ADB is briefly unavailable.
struct RotationWorker {
    receiver: Receiver<DisplaySize>,
    thread: thread::JoinHandle<()>,
}

fn spawn_rotation_worker(serial: Option<String>, stop: Arc<AtomicBool>) -> RotationWorker {
    let (sender, receiver) = mpsc::channel();
    let thread = thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match android::logical_display_size(serial.as_deref()) {
                Ok(size) if sender.send(size).is_err() => return,
                Ok(_) => {}
                Err(error) => eprintln!("Could not refresh Android display size: {error:#}"),
            }
            // Do not sleep the whole interval during shutdown.
            let until = std::time::Instant::now() + ROTATION_CHECK_INTERVAL;
            while std::time::Instant::now() < until {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    });
    RotationWorker { receiver, thread }
}

fn spawn_input_worker(
    configured: ConfiguredDevice,
    tablet_mode: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    touchpad_scroll_percent: i32,
    touchpad_tap_delay_ms: u64,
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
        let mut touchpad = TouchpadState::new(
            &device,
            touchpad_scroll_percent,
            Duration::from_millis(touchpad_tap_delay_ms),
        );
        loop {
            let mut should_grab =
                tablet_mode.load(Ordering::SeqCst) && connected.load(Ordering::SeqCst);
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
                {
                    if let InputEvent::DelayedTap { id } = event {
                        let sender = sender.clone();
                        let delay = touchpad.tap_delay;
                        thread::spawn(move || {
                            thread::sleep(delay);
                            let _ = sender.send(InputEvent::Tap { id });
                        });
                    } else if sender.send(event).is_err() {
                        return;
                    }
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
    // Synergy mouse packets are only 12 bytes. Disable Nagle buffering so
    // fast local pointer movement is delivered as fresh positions instead of
    // being held briefly and released as a visible cursor jump.
    stream
        .set_nodelay(true)
        .context("Could not disable TCP buffering for DeskDock")?;
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
    // Every new DeskDock session starts in Linux mode. This also prevents a
    // stale or racing tablet-mode request made while disconnected from taking
    // effect when DeskDock reconnects.
    tablet_mode.store(false, Ordering::SeqCst);
    shared.connected.store(true, Ordering::SeqCst);
    eprintln!("DeskDock Synergy session active.");

    let rotation_stop = Arc::new(AtomicBool::new(false));
    let rotation_updates = config.tablet.follow_rotation.then(|| {
        spawn_rotation_worker(config.tablet.adb_serial.clone(), Arc::clone(&rotation_stop))
    });

    let mut display_size = display_size;
    let mut pointer = Pointer::centered(display_size.width, display_size.height);
    let mut tablet_active = false;
    let mut keyboard = KeyboardState::default();
    while !shared.is_shutdown() {
        // ADB is deliberately queried in a separate worker: `dumpsys input`
        // can take tens of milliseconds, which must never pause forwarding.
        if let Some(updates) = &rotation_updates {
            while let Ok(size) = updates.receiver.try_recv() {
                if size == display_size {
                    continue;
                }
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

    rotation_stop.store(true, Ordering::SeqCst);
    if let Some(worker) = rotation_updates {
        let _ = worker.thread.join();
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
        InputEvent::Tap { id } if tablet_active && pointer.confirm_tap(id) => {
            synergy::send_frame(
                stream,
                &synergy::mouse_button(true, MouseButton::Left.synergy_code()),
            )?;
            synergy::send_frame(
                stream,
                &synergy::mouse_button(false, MouseButton::Left.synergy_code()),
            )?;
        }
        InputEvent::StartTapDrag { cancel_id } if tablet_active => {
            pointer.cancel_tap(cancel_id);
            synergy::send_frame(
                stream,
                &synergy::mouse_button(true, MouseButton::Left.synergy_code()),
            )?;
        }
        InputEvent::Wheel { y } if tablet_active => {
            synergy::send_frame(stream, &synergy::mouse_wheel(0, y.signum() as i16))?;
        }
        // DeskDock can map its right mouse button to Android Back.
        InputEvent::BackGesture if tablet_active => {
            synergy::send_frame(
                stream,
                &synergy::mouse_button(true, MouseButton::Right.synergy_code()),
            )?;
            synergy::send_frame(
                stream,
                &synergy::mouse_button(false, MouseButton::Right.synergy_code()),
            )?;
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
    DelayedTap { id: u64 },
    Tap { id: u64 },
    StartTapDrag { cancel_id: u64 },
    Wheel { y: i32 },
    BackGesture,
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

const TAP_MAX_DURATION: Duration = Duration::from_millis(250);
const TAP_MAX_MOVEMENT_PERCENT: i32 = 1;
const BACK_SWIPE_PERCENT: i32 = 15;

struct TouchpadState {
    scroll_percent: i32,
    tap_delay: Duration,
    active_slot: i32,
    primary_tracking: bool,
    secondary_tracking: bool,
    finger_count: u8,
    last_x: Option<i32>,
    last_y: Option<i32>,
    current_x: Option<i32>,
    current_y: Option<i32>,
    x_range: i32,
    y_range: i32,
    two_finger_last_x: Option<i32>,
    two_finger_last_y: Option<i32>,
    // Accumulate small touchpad deltas so one physical scroll step maps to
    // one wheel event instead of a wheel event for every evdev report.
    two_finger_wheel_remainder: f64,
    two_finger_x_total: i32,
    two_finger_y_total: i32,
    back_swipe_started: bool,
    last_tap: Option<(Instant, u64)>,
    next_tap_id: u64,
    tap_dragging: bool,
    // Tap recognition is implemented here because capturing the raw evdev
    // device prevents the compositor from synthesizing tap-to-click.
    contact_started: Option<Instant>,
    tap_start_x: Option<i32>,
    tap_start_y: Option<i32>,
    tap_cancelled: bool,
    // Preserve sub-pixel movement between frames. Without these remainders,
    // slow diagonal contact motion is rounded down independently per axis and
    // becomes a horizontal/vertical stair-step.
    x_remainder: f64,
    y_remainder: f64,
}

impl TouchpadState {
    fn new(device: &Device, scroll_percent: i32, tap_delay: Duration) -> Self {
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
            scroll_percent,
            tap_delay,
            active_slot: 0,
            primary_tracking: false,
            secondary_tracking: false,
            finger_count: 0,
            last_x: None,
            last_y: None,
            current_x: None,
            current_y: None,
            x_range,
            y_range,
            two_finger_last_x: None,
            two_finger_last_y: None,
            two_finger_wheel_remainder: 0.0,
            two_finger_x_total: 0,
            two_finger_y_total: 0,
            back_swipe_started: false,
            last_tap: None,
            next_tap_id: 0,
            tap_dragging: false,
            contact_started: None,
            tap_start_x: None,
            tap_start_y: None,
            tap_cancelled: false,
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
                if self.primary_tracking {
                    self.contact_started = Some(Instant::now());
                    self.tap_start_x = None;
                    self.tap_start_y = None;
                    self.current_x = None;
                    self.current_y = None;
                    self.tap_cancelled = self.finger_count > 1;
                    let cancel_id = self
                        .last_tap
                        .take()
                        .and_then(|(tap, id)| (tap.elapsed() <= self.tap_delay).then_some(id));
                    self.tap_dragging = cancel_id.is_some();
                    cancel_id.map(|cancel_id| InputEvent::StartTapDrag { cancel_id })
                } else {
                    let was_tap = self.is_tap();
                    let was_dragging = std::mem::take(&mut self.tap_dragging);
                    self.contact_started = None;
                    self.tap_start_x = None;
                    self.tap_start_y = None;
                    self.reset_motion();
                    if was_dragging {
                        Some(InputEvent::Button {
                            down: false,
                            button: MouseButton::Left,
                        })
                    } else if was_tap {
                        self.next_tap_id = self.next_tap_id.wrapping_add(1);
                        let id = self.next_tap_id;
                        self.last_tap = Some((Instant::now(), id));
                        Some(InputEvent::DelayedTap { id })
                    } else {
                        self.last_tap = None;
                        None
                    }
                }
            }
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_TRACKING_ID, tracking_id)
                if self.active_slot == 1 =>
            {
                self.secondary_tracking = tracking_id >= 0;
                None
            }
            EventSummary::Key(_, KeyCode::BTN_TOOL_FINGER, value @ (0 | 1)) => {
                self.finger_count = value as u8;
                if self.finger_count != 1 {
                    if self.finger_count > 1 {
                        self.cancel_tap();
                    }
                    self.reset_motion();
                    self.reset_two_finger_gesture();
                }
                None
            }
            EventSummary::Key(_, KeyCode::BTN_TOOL_DOUBLETAP, value @ (0 | 1)) => {
                if value == 1 {
                    self.finger_count = 2;
                    self.cancel_tap();
                    self.reset_motion();
                    self.reset_two_finger_gesture();
                }
                None
            }
            EventSummary::Key(_, KeyCode::BTN_TOOL_TRIPLETAP, value @ (0 | 1)) => {
                if value == 1 {
                    self.finger_count = 3;
                    self.cancel_tap();
                    self.reset_motion();
                    self.reset_two_finger_gesture();
                }
                None
            }
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_POSITION_X, value)
                if self.active_slot == 0 =>
            {
                self.current_x = Some(value);
                self.tap_start_x.get_or_insert(value);
                self.update_tap_movement();
                None
            }
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_POSITION_Y, value)
                if self.active_slot == 0 =>
            {
                self.current_y = Some(value);
                self.tap_start_y.get_or_insert(value);
                self.update_tap_movement();
                None
            }
            // `fetch_events` does not repair state after SYN_DROPPED. Forget
            // the old baseline so its next complete report only establishes a
            // position; never infer motion across lost input events.
            EventSummary::Synchronization(_, SynchronizationCode::SYN_DROPPED, _) => {
                self.cancel_tap();
                self.last_tap = None;
                self.reset_motion();
                self.reset_two_finger_gesture();
                None
            }
            EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _)
                if self.primary_tracking && self.secondary_tracking && self.finger_count == 2 =>
            {
                let (x, y) = (self.current_x?, self.current_y?);
                let previous = (
                    self.two_finger_last_x.replace(x),
                    self.two_finger_last_y.replace(y),
                );
                let (Some(last_x), Some(last_y)) = previous else {
                    return None;
                };
                let delta_x = x - last_x;
                let delta_y = y - last_y;
                self.two_finger_x_total += delta_x;
                self.two_finger_y_total += delta_y;

                if !self.back_swipe_started
                    && self.two_finger_x_total * 100 >= self.x_range * BACK_SWIPE_PERCENT
                    && self.two_finger_x_total.abs() > self.two_finger_y_total.abs()
                {
                    self.back_swipe_started = true;
                    Some(InputEvent::BackGesture)
                } else if !self.back_swipe_started && delta_y != 0 && delta_y.abs() >= delta_x.abs()
                {
                    // Synergy wheel events are discrete. Accumulate touchpad
                    // movement so scrolling remains responsive without turning
                    // every evdev report into a full Android scroll step.
                    self.two_finger_wheel_remainder += f64::from(delta_y) * 100.0
                        / (f64::from(self.y_range) * f64::from(self.scroll_percent));
                    let steps = self.two_finger_wheel_remainder.trunc() as i32;
                    self.two_finger_wheel_remainder -= f64::from(steps);
                    (steps != 0).then_some(InputEvent::Wheel { y: steps })
                } else {
                    None
                }
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

    fn reset_two_finger_gesture(&mut self) {
        self.two_finger_last_x = None;
        self.two_finger_last_y = None;
        self.two_finger_wheel_remainder = 0.0;
        self.two_finger_x_total = 0;
        self.two_finger_y_total = 0;
        self.back_swipe_started = false;
    }

    fn is_tap(&self) -> bool {
        !self.tap_cancelled
            && self
                .contact_started
                .is_some_and(|started| started.elapsed() <= TAP_MAX_DURATION)
    }

    fn update_tap_movement(&mut self) {
        let (Some(start_x), Some(start_y), Some(x), Some(y)) = (
            self.tap_start_x,
            self.tap_start_y,
            self.current_x,
            self.current_y,
        ) else {
            return;
        };
        let max_x = (self.x_range * TAP_MAX_MOVEMENT_PERCENT / 100).max(1);
        let max_y = (self.y_range * TAP_MAX_MOVEMENT_PERCENT / 100).max(1);
        if (x - start_x).abs() > max_x || (y - start_y).abs() > max_y {
            self.cancel_tap();
        }
    }

    fn cancel_tap(&mut self) {
        self.tap_cancelled = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn touchpad() -> TouchpadState {
        TouchpadState {
            scroll_percent: 5,
            tap_delay: Duration::from_millis(500),
            active_slot: 0,
            primary_tracking: false,
            secondary_tracking: false,
            finger_count: 1,
            last_x: None,
            last_y: None,
            current_x: None,
            current_y: None,
            x_range: 1_000,
            y_range: 1_000,
            two_finger_last_x: None,
            two_finger_last_y: None,
            two_finger_wheel_remainder: 0.0,
            two_finger_x_total: 0,
            two_finger_y_total: 0,
            back_swipe_started: false,
            last_tap: None,
            next_tap_id: 0,
            tap_dragging: false,
            contact_started: None,
            tap_start_x: None,
            tap_start_y: None,
            tap_cancelled: false,
            x_remainder: 0.0,
            y_remainder: 0.0,
        }
    }

    fn tracking(touchpad: &mut TouchpadState, tracking_id: i32) -> Option<InputEvent> {
        touchpad.primary_tracking = tracking_id >= 0;
        if touchpad.primary_tracking {
            touchpad.contact_started = Some(Instant::now());
            None
        } else {
            let was_tap = touchpad.is_tap();
            touchpad.contact_started = None;
            was_tap.then_some(InputEvent::DelayedTap { id: 1 })
        }
    }

    fn position(touchpad: &mut TouchpadState, x: i32, y: i32) {
        touchpad.current_x = Some(x);
        touchpad.current_y = Some(y);
        touchpad.tap_start_x.get_or_insert(x);
        touchpad.tap_start_y.get_or_insert(y);
        touchpad.update_tap_movement();
    }

    #[test]
    fn single_finger_tap_emits_click() {
        let mut touchpad = touchpad();
        assert!(tracking(&mut touchpad, 1).is_none());
        position(&mut touchpad, 500, 500);
        assert!(matches!(
            tracking(&mut touchpad, -1),
            Some(InputEvent::DelayedTap { .. })
        ));
    }

    #[test]
    fn moving_contact_is_not_a_tap() {
        let mut touchpad = touchpad();
        assert!(tracking(&mut touchpad, 1).is_none());
        position(&mut touchpad, 500, 500);
        position(&mut touchpad, 520, 500);
        assert!(tracking(&mut touchpad, -1).is_none());
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
    cancelled_tap: Option<u64>,
}
impl Pointer {
    fn centered(width: i16, height: i16) -> Self {
        Self {
            x: i32::from(width) / 2,
            y: i32::from(height) / 2,
            width: i32::from(width),
            height: i32::from(height),
            cancelled_tap: None,
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
    fn cancel_tap(&mut self, id: u64) {
        self.cancelled_tap = Some(id);
    }
    fn confirm_tap(&mut self, id: u64) -> bool {
        if self.cancelled_tap == Some(id) {
            self.cancelled_tap = None;
            false
        } else {
            true
        }
    }
}
