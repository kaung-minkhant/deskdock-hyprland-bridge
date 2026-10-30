# DeskDock Wayland bridge

A **mouse and touchpad** bridge for the existing [DeskDock](https://desk-dock.com/) Android client. It reads only the Linux pointer devices you explicitly configure and uses DeskDock Server's supported **Synergy/Barrier** integration over localhost.

```text
configured Linux pointers → hypr-android-bridge → DeskDock Server → DeskDock Android app
```

It does **not** use DeskDock's legacy X11 input capture, emulate its private Android protocol, or replace the Android client.

> **Experimental project.** The current milestone supports configured relative mice plus single-finger touchpad movement, left click, and vertical mouse-wheel events. Keyboard forwarding is not implemented.

## What works

- Native Android mouse cursor through DeskDock's Synergy/Barrier integration.
- A TOML configuration file at `~/.config/hypr-android-bridge/config.toml`.
- One or more explicitly configured pointer devices; no automatic capture of every mouse or touchpad.
- Safe Linux/tablet handoff through a local control socket.
- Android pointer is centered on tablet-mode entry and hidden when returning to Linux mode.
- DeskDock disconnect/crash releases configured pointers and restores Linux mode.
- Restarting DeskDock reconnects without restarting the bridge.
- Synergy listens only on loopback.

## Current limitations

- Touchpad support is initially single-finger relative movement only. Multi-finger gestures, scrolling, tapping, and pinch/zoom are not forwarded.
- Only movement, left button, and vertical **mouse** wheel are forwarded.
- Tablet resolution defaults to `1800×2880`; configure it when necessary.
- Keyboard input always remains local.
- The current Hyprland binding still invokes the development binary; installing the Nix package and updating the bind is the next operational step.

## Prerequisites

- Nix with flakes enabled, or a Rust toolchain compatible with `Cargo.toml`.
- DeskDock Server installed and the DeskDock Android app connected over **USB ADB**.
- Read access to `/dev/input/event*`. On this NixOS machine, the user belongs to the `input` group. Until the next graphical login, start a development shell with:

  ```bash
  newgrp input
  ```

Never run the bridge with `sudo`.

## Configuration

Create the initial configuration once:

```bash
cd /home/james/Projects/control
nix run . -- init-config
```

It creates, without overwriting an existing file:

```text
~/.config/hypr-android-bridge/config.toml
```

The generated configuration is ready for this laptop:

```toml
[synergy]
address = "127.0.0.1"
port = 24800

[tablet]
width = 1800
height = 2880
sensitivity = 4

[[devices]]
path = "/dev/input/by-id/usb-YICHIP_Wireless_Device_b120300001-if01-event-mouse"
kind = "mouse"

[[devices]]
path = "/dev/input/by-path/pci-0000:00:15.0-platform-i2c_designware.0-event-mouse"
kind = "touchpad"
```

Only devices under `[[devices]]` are captured in tablet mode. The wireless mouse and touchpad above both remain under Hyprland in Linux mode, then both feed the same Android pointer in tablet mode.

### Discover stable device paths

Use:

```bash
nix run . -- list-devices
```

The command lists pointer-capable evdev devices with their temporary event node and a stable udev alias when present. Prefer `/dev/input/by-id` for USB peripherals and `/dev/input/by-path` for integrated hardware. Do not use `/dev/input/eventN` in the configuration: event numbers can change after reconnects or reboots.

## Configure DeskDock

With **DeskDock stopped**, configure its localhost Synergy settings:

```bash
nix run . -- enable-synergy
```

This edits DeskDock's local Java preferences at:

```text
~/.java/.userPrefs/<unnamed>/prefs.xml
```

It enables Synergy/Barrier, configures `127.0.0.1:24800`, and disables Synergy encryption **only for this same-machine loopback connection**.

## Start the bridge

In a terminal with `input`-group access:

```bash
cd /home/james/Projects/control
newgrp input       # only until the next graphical login, if needed
nix run . -- forward
```

The bridge reads `~/.config/hypr-android-bridge/config.toml`, begins in **Linux mode**, and creates a local control socket at:

```text
$XDG_RUNTIME_DIR/hypr-android-bridge.sock
```

To test a different configuration without changing the default, use:

```bash
nix run . -- forward --config /path/to/config.toml
```

## Start DeskDock

In a normal graphical terminal, after the bridge is listening:

```bash
env -u JAVA_TOOL_OPTIONS -u _JAVA_OPTIONS \
  DISPLAY=:1 WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 \
  deskdock
```

The explicit graphical environment is useful when DeskDock otherwise starts headlessly. Wait for the bridge to print:

```text
DeskDock Synergy session active.
```

## Switch pointer control

From another terminal in the same graphical session:

```bash
nix run . -- toggle
```

- First invocation requests **tablet mode**. Every configured pointer is exclusively captured, and the Android pointer resets to the center.
- Invoke it again for **Linux mode**. Every configured pointer is released to Hyprland and the Android cursor is hidden.

The active development shortcut is **Super + Shift + T**. The keyboard remains local, so it is a reliable way to return configured pointers to Linux.

## Recovery behavior

- **DeskDock stops, crashes, or loses its Synergy connection:** configured pointers are released and Linux mode is restored.
- **DeskDock is restarted:** it reconnects without restarting the bridge process.
- **Bridge is stopped with `Ctrl+C`:** input devices are released when the process exits.
- **A stale control socket remains after an unclean exit:** the next bridge removes it after confirming no daemon is listening.

## Restore normal DeskDock operation

With DeskDock stopped, set this entry in `~/.java/.userPrefs/<unnamed>/prefs.xml` back to `false`:

```xml
<entry key="PREF_KEY_SYNERGY_ENABLED" value="false"/>
```

Then start DeskDock normally.

## Build and development checks

```bash
nix flake check
nix build
./result/bin/hypr-android-bridge --help
nix develop --command cargo fmt --check
nix develop --command cargo clippy -- -D warnings
```

## Project layout

- `src/main.rs` — command-line interface.
- `src/config.rs` — TOML configuration, validation, and template generation.
- `src/bridge.rs` — configured input workers, exclusive capture, touchpad translation, reconnect handling, and pointer state.
- `src/synergy.rs` — minimal Synergy/Barrier framing used by DeskDock.
- `src/control.rs` — local Unix-socket toggle interface.
- `src/deskdock.rs` — DeskDock localhost Synergy preference configuration.

## Security notes

The bridge accepts Synergy connections only on loopback and requires access to raw Linux input events. Treat membership in the `input` group as privileged. Keep the DeskDock USB/ADB connection trusted, and do not expose port `24800` beyond localhost.
