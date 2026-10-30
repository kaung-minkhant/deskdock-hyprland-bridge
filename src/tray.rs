use crate::state::Shared;
use ksni::{
    Category, Icon, MenuItem, Status, ToolTip,
    blocking::TrayMethods,
    menu::{StandardItem, SubMenu},
};
use std::{
    sync::atomic::Ordering,
    thread::{self, JoinHandle},
    time::Duration,
};

const SIZE: i32 = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// DeskDock has not connected yet.
    Waiting,
    /// Connected; devices stay with Linux.
    Linux,
    /// Connected; devices are forwarded to Android.
    Tablet,
}

struct BridgeTray {
    shared: Shared,
    mode: Mode,
    deskdock_running: bool,
}

impl BridgeTray {
    fn read(shared: &Shared) -> (Mode, bool) {
        let mode = if !shared.connected.load(Ordering::SeqCst) {
            Mode::Waiting
        } else if shared.tablet_mode.load(Ordering::SeqCst) {
            Mode::Tablet
        } else {
            Mode::Linux
        };
        (mode, shared.deskdock_running.load(Ordering::SeqCst))
    }

    fn summary(&self) -> &'static str {
        match self.mode {
            Mode::Waiting if !self.deskdock_running => "DeskDock is not running",
            Mode::Waiting => "Waiting for the Android tablet",
            Mode::Linux => "Linux mode: input stays on this computer",
            Mode::Tablet => "Tablet mode: input goes to Android",
        }
    }
}

impl ksni::Tray for BridgeTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "hypr-android-bridge".into()
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn title(&self) -> String {
        "DeskDock bridge".into()
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![icon(self.mode)]
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "DeskDock bridge".into(),
            description: self.summary().into(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let connected = self.mode != Mode::Waiting;
        let tablet = self.mode == Mode::Tablet;
        vec![
            StandardItem {
                label: self.summary().into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: if tablet {
                    "Return input to Linux"
                } else {
                    "Send input to tablet"
                }
                .into(),
                enabled: connected,
                activate: Box::new(|this: &mut Self| {
                    let _ = this.shared.toggle_tablet_mode();
                }),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "DeskDock".into(),
                submenu: vec![
                    StandardItem {
                        label: "Restart DeskDock".into(),
                        activate: Box::new(|this: &mut Self| {
                            this.shared.restart_deskdock.store(true, Ordering::SeqCst);
                        }),
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| this.shared.request_shutdown()),
                ..Default::default()
            }
            .into(),
        ]
    }

    // Waybar may start after the bridge; keep running and register when it appears.
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        true
    }
}

/// Start the tray icon and keep it in sync with the shared state.
pub fn spawn(shared: Shared) -> JoinHandle<()> {
    thread::spawn(move || {
        let (mode, deskdock_running) = BridgeTray::read(&shared);
        let tray = BridgeTray {
            shared: shared.clone(),
            mode,
            deskdock_running,
        };
        let handle = match tray.assume_sni_available(true).spawn() {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("Tray icon unavailable: {error}");
                return;
            }
        };
        while !shared.is_shutdown() && !handle.is_closed() {
            let (mode, deskdock_running) = BridgeTray::read(&shared);
            handle.update(|tray| {
                tray.mode = mode;
                tray.deskdock_running = deskdock_running;
            });
            thread::sleep(Duration::from_millis(250));
        }
        let _ = handle.shutdown();
    })
}

/// A tablet silhouette, tinted by state: grey = waiting, blue = Linux, green = tablet.
fn icon(mode: Mode) -> Icon {
    let (red, green, blue) = match mode {
        Mode::Waiting => (0x9a, 0x9a, 0x9a),
        Mode::Linux => (0x4c, 0x9b, 0xf0),
        Mode::Tablet => (0x3f, 0xcf, 0x6e),
    };
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            // Outer body 20x28, rounded; inner screen cut-out leaves a bezel.
            let inside = |left: i32, top: i32, right: i32, bottom: i32, radius: i32| {
                if x < left || x >= right || y < top || y >= bottom {
                    return false;
                }
                let cx = x.clamp(left + radius, right - 1 - radius);
                let cy = y.clamp(top + radius, bottom - 1 - radius);
                (x - cx).pow(2) + (y - cy).pow(2) <= radius * radius
            };
            let body = inside(6, 2, 26, 30, 4);
            let screen = inside(9, 6, 23, 25, 1);
            let alpha = if body && !screen {
                0xff
            } else if screen {
                0x55
            } else {
                0
            };
            // StatusNotifierItem pixmaps are ARGB32 in network byte order.
            data.extend_from_slice(&[alpha, red, green, blue]);
        }
    }
    Icon {
        width: SIZE,
        height: SIZE,
        data,
    }
}
