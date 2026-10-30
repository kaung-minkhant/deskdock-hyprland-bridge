use crate::state::Shared;
use anyhow::{Context, Result};
use std::{
    os::unix::process::CommandExt,
    process::{Child, Command},
    sync::atomic::Ordering,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(100);
const STOP_GRACE: Duration = Duration::from_secs(3);
const MIN_RESTART_DELAY: Duration = Duration::from_secs(2);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);
const HEALTHY_RUNTIME: Duration = Duration::from_secs(20);

/// Start DeskDock and keep it running until shutdown. The child is stopped when
/// the bridge exits, and also receives SIGTERM if the bridge is killed.
pub fn spawn(command: String, display: String, shared: Shared) -> Result<JoinHandle<()>> {
    // Fail early when the executable cannot be launched at all.
    let first = launch(&command, &display)?;
    Ok(thread::spawn(move || {
        supervise(command, display, first, shared)
    }))
}

fn launch(command: &str, display: &str) -> Result<Child> {
    let mut process = Command::new(command);
    // Keep this identical to the documented DeskDock invocation. Its bundled
    // Java code needs Xwayland even though DeskDock shows no ordinary window.
    process
        .env_remove("JAVA_TOOL_OPTIONS")
        .env_remove("_JAVA_OPTIONS")
        .env("DISPLAY", display);
    // SAFETY: prctl is async-signal-safe and allocates nothing.
    unsafe {
        process.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    process
        .spawn()
        .with_context(|| format!("Could not start DeskDock using `{command}`"))
}

fn supervise(command: String, display: String, first: Child, shared: Shared) {
    let mut child = Some(first);
    let mut delay = MIN_RESTART_DELAY;
    let mut started = Instant::now();
    shared.deskdock_running.store(true, Ordering::SeqCst);
    eprintln!("Started DeskDock (`{command}`).");

    loop {
        if shared.is_shutdown() {
            if let Some(child) = child.as_mut() {
                stop(child);
            }
            shared.deskdock_running.store(false, Ordering::SeqCst);
            return;
        }

        if shared.restart_deskdock.swap(false, Ordering::SeqCst)
            && let Some(running) = child.as_mut()
        {
            eprintln!("Restarting DeskDock...");
            stop(running);
        }

        match child.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => {}
            Some(Ok(Some(status))) => {
                eprintln!("DeskDock exited ({status}).");
                child = None;
                shared.deskdock_running.store(false, Ordering::SeqCst);
                if started.elapsed() >= HEALTHY_RUNTIME {
                    delay = MIN_RESTART_DELAY;
                }
                started = Instant::now();
                wait_interruptible(&shared, delay);
                delay = (delay * 2).min(MAX_RESTART_DELAY);
            }
            Some(Err(error)) => {
                eprintln!("Could not poll DeskDock: {error}");
                child = None;
                shared.deskdock_running.store(false, Ordering::SeqCst);
            }
            None => {
                if shared.is_shutdown() {
                    continue;
                }
                match launch(&command, &display) {
                    Ok(new_child) => {
                        eprintln!("Restarted DeskDock.");
                        child = Some(new_child);
                        started = Instant::now();
                        shared.deskdock_running.store(true, Ordering::SeqCst);
                    }
                    Err(error) => {
                        eprintln!("{error:#}");
                        wait_interruptible(&shared, delay);
                        delay = (delay * 2).min(MAX_RESTART_DELAY);
                    }
                }
            }
        }
        thread::sleep(POLL);
    }
}

fn wait_interruptible(shared: &Shared, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline && !shared.is_shutdown() {
        thread::sleep(POLL);
    }
}

fn stop(child: &mut Child) {
    // SAFETY: signalling a child process this module owns.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}
