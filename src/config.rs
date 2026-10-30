use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const CONFIG_DIRECTORY: &str = "hypr-android-bridge";
pub const CONFIG_FILE: &str = "config.toml";
const DEFAULT_PORT: u16 = 24800;
const DEFAULT_WIDTH: i16 = 1800;
const DEFAULT_HEIGHT: i16 = 2880;
const DEFAULT_SENSITIVITY: i32 = 4;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub synergy: Synergy,
    #[serde(default)]
    pub tablet: Tablet,
    pub devices: Vec<Device>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Synergy {
    #[serde(default = "default_address")]
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tablet {
    #[serde(default = "default_width")]
    pub width: i16,
    #[serde(default = "default_height")]
    pub height: i16,
    #[serde(default = "default_sensitivity")]
    pub sensitivity: i32,
    /// Refresh Android's current logical size while the bridge is running.
    #[serde(default)]
    pub follow_rotation: bool,
    /// Optional USB ADB serial; omit it when exactly one authorized device is attached.
    pub adb_serial: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub path: PathBuf,
    pub kind: DeviceKind,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Mouse,
    Touchpad,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Could not read configuration {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("Could not parse configuration {}", path.display()))?;
        config.validate(path)?;
        Ok(config)
    }

    fn validate(&self, path: &Path) -> Result<()> {
        if self.synergy.address != "127.0.0.1" && self.synergy.address != "::1" {
            bail!(
                "{} must keep synergy.address on loopback (127.0.0.1 or ::1)",
                path.display()
            );
        }
        if self.synergy.port == 0 {
            bail!("{} has an invalid synergy.port", path.display());
        }
        if self.tablet.width <= 0 || self.tablet.height <= 0 {
            bail!(
                "{} must use positive tablet width and height",
                path.display()
            );
        }
        if self.tablet.sensitivity <= 0 {
            bail!("{} must use a positive tablet sensitivity", path.display());
        }
        if let Some(serial) = &self.tablet.adb_serial
            && serial.trim().is_empty()
        {
            bail!("{} has an empty tablet.adb_serial", path.display());
        }
        if self.devices.is_empty() {
            bail!("{} must configure at least one device", path.display());
        }
        if self.devices.iter().any(|device| !device.path.is_absolute()) {
            bail!(
                "{} contains a device path that is not absolute",
                path.display()
            );
        }
        for (index, device) in self.devices.iter().enumerate() {
            if self.devices[..index]
                .iter()
                .any(|other| other.path == device.path)
            {
                bail!(
                    "{} configures {} more than once",
                    path.display(),
                    device.path.display()
                );
            }
        }
        Ok(())
    }
}

impl Default for Synergy {
    fn default() -> Self {
        Self {
            address: default_address(),
            port: default_port(),
        }
    }
}

impl Default for Tablet {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: default_height(),
            sensitivity: default_sensitivity(),
            follow_rotation: false,
            adb_serial: None,
        }
    }
}

pub fn default_path() -> Result<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("Neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(config_home.join(CONFIG_DIRECTORY).join(CONFIG_FILE))
}

pub fn initialize(path: &Path) -> Result<()> {
    if path.exists() {
        bail!(
            "Configuration already exists at {}; refusing to overwrite it",
            path.display()
        );
    }
    let parent = path
        .parent()
        .context("Configuration path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "Could not create configuration directory {}",
            parent.display()
        )
    })?;
    fs::write(path, template())
        .with_context(|| format!("Could not write configuration {}", path.display()))?;
    println!("Created {}", path.display());
    println!("Review the device paths before starting the bridge.");
    Ok(())
}

pub fn template() -> &'static str {
    r#"# Hyprland → DeskDock mouse bridge configuration.
# Only devices listed below are captured in tablet mode.

[synergy]
# Kept on loopback so DeskDock can connect locally without exposing the bridge.
address = "127.0.0.1"
port = 24800

[tablet]
# Set these to the current logical orientation when follow_rotation is false.
width = 1800
height = 2880
sensitivity = 4
# Use USB ADB to update the pointer bounds after an Android rotation.
follow_rotation = true
# adb_serial = "your-usb-device-serial"

[[devices]]
path = "/dev/input/by-id/usb-YICHIP_Wireless_Device_b120300001-if01-event-mouse"
kind = "mouse"

[[devices]]
path = "/dev/input/by-path/pci-0000:00:15.0-platform-i2c_designware.0-event-mouse"
kind = "touchpad"
"#
}

fn default_address() -> String {
    "127.0.0.1".to_owned()
}
fn default_port() -> u16 {
    DEFAULT_PORT
}
fn default_width() -> i16 {
    DEFAULT_WIDTH
}
fn default_height() -> i16 {
    DEFAULT_HEIGHT
}
fn default_sensitivity() -> i32 {
    DEFAULT_SENSITIVITY
}
