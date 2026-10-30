use anyhow::{Context, Result, bail};
use std::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplaySize {
    pub width: i16,
    pub height: i16,
}

/// Read Android's current *logical* display bounds over USB ADB. Logical bounds
/// rotate with the UI, unlike the panel's fixed physical dimensions.
pub fn logical_display_size(serial: Option<&str>) -> Result<DisplaySize> {
    let mut command = Command::new("adb");
    if let Some(serial) = serial {
        command.args(["-s", serial]);
    }
    let output = command
        .args(["shell", "dumpsys", "input"])
        .output()
        .context("Could not run adb. Ensure Android platform-tools is installed and the USB device is authorized")?;
    if !output.status.success() {
        bail!(
            "adb could not inspect Android's display: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_logical_display_size(&String::from_utf8_lossy(&output.stdout))
}

fn parse_logical_display_size(output: &str) -> Result<DisplaySize> {
    let line = output
        .lines()
        .find(|line| line.contains("Viewport INTERNAL:") && line.contains("logicalFrame=[0, 0,"))
        .context("Android did not report an active internal display viewport")?;
    let bounds = line
        .split("logicalFrame=[0, 0,")
        .nth(1)
        .and_then(|value| value.split(']').next())
        .context("Android reported a malformed logical display viewport")?;
    let mut values = bounds.split(',').map(str::trim);
    let width = values
        .next()
        .context("Android viewport is missing its width")?
        .parse::<i16>()
        .context("Android viewport has an invalid width")?;
    let height = values
        .next()
        .context("Android viewport is missing its height")?
        .parse::<i16>()
        .context("Android viewport has an invalid height")?;
    if width <= 0 || height <= 0 {
        bail!("Android viewport dimensions must be positive");
    }
    Ok(DisplaySize { width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rotated_logical_viewport() {
        let output = "Viewport INTERNAL: displayId=0, orientation=1, logicalFrame=[0, 0, 2880, 1800], physicalFrame=[0, 0, 2880, 1800]";
        assert_eq!(
            parse_logical_display_size(output).unwrap(),
            DisplaySize {
                width: 2880,
                height: 1800
            }
        );
    }
}
