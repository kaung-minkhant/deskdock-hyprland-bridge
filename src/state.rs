use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

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
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}
