use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Result of a request to switch the configured devices' destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabletModeChange {
    Tablet,
    Linux,
    Unavailable,
}

/// State shared between the bridge, the DeskDock supervisor, and the tray.
#[derive(Clone, Default)]
pub struct Shared {
    /// True while configured devices are captured and sent to Android.
    pub tablet_mode: Arc<AtomicBool>,
    /// True while DeskDock holds an active Synergy session with the bridge.
    pub connected: Arc<AtomicBool>,
    /// True while the supervised DeskDock process is alive.
    pub deskdock_running: Arc<AtomicBool>,
    /// Set to ask every component to stop.
    pub shutdown: Arc<AtomicBool>,
    /// Set by the tray to ask the supervisor to restart DeskDock.
    pub restart_deskdock: Arc<AtomicBool>,
}

impl Shared {
    /// Switch to tablet mode only while DeskDock has an active Synergy session.
    /// Switching back to Linux is always allowed so a disconnected session can
    /// still release every configured device.
    pub fn toggle_tablet_mode(&self) -> TabletModeChange {
        if self.tablet_mode.load(Ordering::SeqCst) {
            self.tablet_mode.store(false, Ordering::SeqCst);
            TabletModeChange::Linux
        } else if self.connected.load(Ordering::SeqCst) {
            self.tablet_mode.store(true, Ordering::SeqCst);
            TabletModeChange::Tablet
        } else {
            TabletModeChange::Unavailable
        }
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::{Shared, TabletModeChange};
    use std::sync::atomic::Ordering;

    #[test]
    fn does_not_enable_tablet_mode_without_a_connection() {
        let shared = Shared::default();

        assert_eq!(shared.toggle_tablet_mode(), TabletModeChange::Unavailable);
        assert!(!shared.tablet_mode.load(Ordering::SeqCst));
    }

    #[test]
    fn toggles_when_connected() {
        let shared = Shared::default();
        shared.connected.store(true, Ordering::SeqCst);

        assert_eq!(shared.toggle_tablet_mode(), TabletModeChange::Tablet);
        assert_eq!(shared.toggle_tablet_mode(), TabletModeChange::Linux);
    }
}
