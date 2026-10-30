# DeskDock Wayland bridge

A **mouse-only**, Hyprland-friendly bridge for the existing [DeskDock](https://desk-dock.com/) Android client. It reads one Linux evdev mouse and speaks DeskDock Server's supported **Synergy/Barrier** integration over localhost.

```text
selected Linux mouse → hypr-android-bridge → DeskDock Server → DeskDock Android app
```

It does **not** use DeskDock's legacy X11 input capture, emulate its private Android protocol, or replace the Android client.

> **Experimental project.** The current milestone is deliberately narrow: one configured relative mouse, movement, left click, and vertical scrolling. Keyboard forwarding is not implemented.

## What works

- Native Android mouse cursor through DeskDock's Synergy/Barrier integration.
- Safe Linux/tablet mouse handoff through a local control socket.
- `Super + Shift + T` toggles the running bridge in the current development setup.
- Android pointer is recentered on every entry to tablet mode.
- DeskDock disconnect/crash releases the captured mouse and restores Linux mode.
- Restarting DeskDock reconnects without restarting the bridge.
- The Synergy listener is bound only to `127.0.0.1`.

## Current limitations

- One mouse only: currently `/dev/input/event6` (`YICHIP Wireless Device Mouse`).
- Only movement, left button, and vertical wheel are forwarded.
- The tablet resolution is currently fixed at `1800×2880`.
- Keyboard input always remains local; this is intentional while mouse capture is the only exclusive input path.
- The Hyprland binding currently points at the development binary. Packaging is the next operational cleanup.

## Prerequisites

- Nix with flakes enabled, or a Rust toolchain compatible with `Cargo.toml`.
- DeskDock Server installed and the DeskDock Android app connected over **USB ADB**.
- Read access to `/dev/input/event*`. On this NixOS machine, the user belongs to the `input` group. Until the next graphical login, start a development shell with:

  ```bash
  newgrp input
  ```

Never run the bridge with `sudo`.

## Setup

### 1. Find the mouse device

```bash
cd /home/james/Projects/control
nix develop --command cargo run -- list-devices
```

Confirm the path for the relative mouse you intend to hand to the tablet. The tested device is:

```text
/dev/input/event6  YICHIP Wireless Device Mouse
```

Event numbers can change after reconnects or reboots; verify this before starting the bridge.

### 2. Configure DeskDock's Synergy integration

With **DeskDock stopped**, configure its localhost Synergy settings:

```bash
nix develop --command cargo run -- enable-synergy
```

This edits DeskDock's local Java preferences at:

```text
~/.java/.userPrefs/<unnamed>/prefs.xml
```

It enables Synergy/Barrier, configures `127.0.0.1:24800`, and disables Synergy encryption **only for this same-machine loopback connection**.

### 3. Start the bridge

In a terminal with `input`-group access:

```bash
cd /home/james/Projects/control
newgrp input       # only until the next graphical login, if needed
nix develop
cargo run -- forward --device /dev/input/event6
```

The bridge begins in **Linux mode**: Hyprland still receives the selected mouse. It creates a local control socket at:

```text
$XDG_RUNTIME_DIR/hypr-android-bridge.sock
```

### 4. Start DeskDock

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

## Switching mouse control

### Terminal toggle

From another terminal in the same graphical session:

```bash
cd /home/james/Projects/control
nix develop --command cargo run -- toggle
```

- First invocation requests **tablet mode**. The bridge exclusively grabs the selected mouse, resets the Android pointer to the center, and forwards the supported mouse events only to DeskDock.
- Invoke it again for **Linux mode**. The bridge releases the mouse to Hyprland.

The transition is processed even when the mouse is stationary.

### Hyprland shortcut

The active development binding is:

```text
Super + Shift + T
```

It invokes the debug binary's `toggle` command. The keyboard remains local, so the same shortcut is a reliable way to return a captured mouse to Linux.

## Recovery behavior

- **DeskDock stops, crashes, or loses its Synergy connection:** the bridge ungrabs the selected mouse, forces Linux mode, and waits for a new DeskDock connection.
- **DeskDock is restarted:** it can reconnect to the bridge without restarting the bridge process.
- **Bridge is stopped with `Ctrl+C`:** the evdev device is released when the process exits.
- **A stale control socket remains after an unclean exit:** a new bridge process removes it after confirming no bridge is listening.

## Restore normal DeskDock operation

With DeskDock stopped, set this entry in `~/.java/.userPrefs/<unnamed>/prefs.xml` back to `false`:

```xml
<entry key="PREF_KEY_SYNERGY_ENABLED" value="false"/>
```

Then start DeskDock normally.

## Development checks

```bash
nix develop --command cargo fmt --check
nix develop --command cargo check
nix develop --command cargo clippy -- -D warnings
```

## Project layout

- `src/main.rs` — command-line interface and option validation.
- `src/bridge.rs` — evdev capture, mode changes, reconnect handling, and pointer state.
- `src/synergy.rs` — minimal Synergy/Barrier framing used by DeskDock.
- `src/control.rs` — local Unix-socket toggle interface.
- `src/deskdock.rs` — DeskDock localhost Synergy preference configuration.

## Security notes

The bridge accepts Synergy connections only on loopback and requires access to raw Linux input events. Treat membership in the `input` group as privileged. Keep the DeskDock USB/ADB connection trusted, and do not expose port `24800` beyond localhost.
