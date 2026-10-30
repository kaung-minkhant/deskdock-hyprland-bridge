use anyhow::{Context, Result, bail};
use std::{fs, path::PathBuf};

pub fn enable_synergy(address: &str, port: u16) -> Result<()> {
    if address.is_empty() || address.contains(['\n', '\r', '"', '&', '<', '>']) {
        bail!("--address must be a plain hostname or IP address");
    }

    let path = preferences_path()?;
    let original = fs::read_to_string(&path)
        .with_context(|| format!("Could not read DeskDock preferences at {}", path.display()))?;
    let updated = replace_preference(&original, "PREF_KEY_SYNERGY_ENABLED", "true")?;
    let updated = replace_preference(&updated, "PREF_KEY_SYNERGY_SERVER_IP", address)?;
    let updated = replace_preference(&updated, "PREF_KEY_SYNERGY_ENCRYPTED", "false")?;
    fs::write(&path, updated)
        .with_context(|| format!("Could not write DeskDock preferences at {}", path.display()))?;

    println!("Enabled DeskDock Synergy/Barrier integration for {address}:{port}.");
    println!("Restart DeskDock after the bridge is listening.");
    println!("Encryption is disabled only because this is a localhost connection.");
    Ok(())
}

fn preferences_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let path = PathBuf::from(home).join(".java/.userPrefs/<unnamed>/prefs.xml");
    if !path.exists() {
        bail!(
            "DeskDock preferences were not found at {}. Start DeskDock once first.",
            path.display()
        );
    }
    Ok(path)
}

fn replace_preference(xml: &str, key: &str, value: &str) -> Result<String> {
    let prefix = format!("<entry key=\"{key}\" value=\"");
    let start = xml
        .find(&prefix)
        .with_context(|| format!("DeskDock preference {key} was not found"))?;
    let value_start = start + prefix.len();
    let value_end = xml[value_start..]
        .find('"')
        .map(|offset| value_start + offset)
        .context("Malformed DeskDock preferences file")?;
    Ok(format!(
        "{}{}{}",
        &xml[..value_start],
        value,
        &xml[value_end..]
    ))
}
