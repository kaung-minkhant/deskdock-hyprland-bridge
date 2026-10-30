use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

const SOCKET_NAME: &str = "hypr-android-bridge.sock";

pub struct ControlSocket {
    path: PathBuf,
}

impl ControlSocket {
    pub fn bind(tablet_mode: Arc<AtomicBool>) -> Result<Self> {
        let path = socket_path()?;
        if path.exists() {
            match UnixStream::connect(&path) {
                Ok(_) => bail!(
                    "A bridge is already running (control socket: {}). Stop it before starting another.",
                    path.display()
                ),
                Err(_) => fs::remove_file(&path).with_context(|| {
                    format!("Could not remove stale control socket {}", path.display())
                })?,
            }
        }

        let listener = UnixListener::bind(&path)
            .with_context(|| format!("Could not bind control socket {}", path.display()))?;
        thread::spawn(move || {
            for connection in listener.incoming() {
                let Ok(mut connection) = connection else {
                    continue;
                };
                let mut command = String::new();
                let response = if connection.read_to_string(&mut command).is_ok()
                    && command.trim() == "toggle"
                {
                    if !tablet_mode.fetch_xor(true, Ordering::SeqCst) {
                        "tablet\n"
                    } else {
                        "linux\n"
                    }
                } else {
                    "error\n"
                };
                let _ = connection.write_all(response.as_bytes());
            }
        });

        Ok(Self { path })
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn toggle() -> Result<()> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "Could not connect to {}. Start `hypr-android-bridge run` first.",
            path.display()
        )
    })?;
    stream.write_all(b"toggle\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    match response.trim() {
        "tablet" => println!("Tablet mode requested."),
        "linux" => println!("Linux mode requested."),
        _ => bail!("Bridge rejected the toggle request: {}", response.trim()),
    }
    Ok(())
}

fn socket_path() -> Result<PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR is not set; is this running in a graphical session?")?;
    Ok(PathBuf::from(runtime_dir).join(SOCKET_NAME))
}
