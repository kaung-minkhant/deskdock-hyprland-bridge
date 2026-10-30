# Hyprland → Android input bridge (experimental)

A Wayland/Hyprland-oriented experiment for sending Linux input to an Android
tablet through its already-paired ADB connection. It is intentionally built in
small, safe stages instead of grabbing your input devices immediately.

## Current scope

- Lists readable `/dev/input/event*` devices.
- Verifies that the paired Android device accepts an ADB input command.
- Monitors one selected relative-pointer device and forwards mouse movement,
  left-clicks, and vertical scroll to Android with ADB.
- **Does not yet enable a global Hyprland toggle, mouse-button hold/drag state,
  keyboard forwarding, or a safe exclusive input grab.**

The Android device needs Wireless debugging enabled and visible in:

```bash
adb devices
```

## Development environment

```bash
direnv allow
nix develop
cargo run -- list-devices
cargo run -- check-adb
```

If `direnv` is not available, enter the shell manually with `nix develop`.

## Finding mouse and keyboard event nodes

Use the stable device symlinks when available:

```bash
ls -l /dev/input/by-id /dev/input/by-path
```

Then run diagnostic mode **without** `--grab`:

```bash
cargo run -- forward --device /dev/input/eventN
```

The first pointer-forwarding test uses read-only mode, so both Hyprland and
Android receive the selected mouse's activity:

```bash
cargo run -- forward --device /dev/input/eventN
```

A virtual Android pointer starts at the tablet display center. Move the selected
mouse to move it; its left click sends a tap and its vertical wheel sends a
swipe. Use `Ctrl+C` to stop. You can verify mouse injection independently:

```bash
cargo run -- test-swipe
cargo run -- test-tap
```

This milestone invokes `adb shell input` per event, so it is intentionally a
functional proof—not low-latency production input. `--grab` is deliberately
present only for a later controlled test. An evdev
grab prevents Hyprland and other programs from receiving events from that one
device. Do **not** use it until the bridge implements its local emergency
escape chord and connection watchdog.

## Planned integration

The eventual Hyprland binding will call a daemon-owned toggle, for example:

```ini
bind = SUPER SHIFT, M, exec, hypr-android-bridge toggle
```

The daemon—not Hyprland—will detect the same chord while input is captured,
release all evdev grabs, and return control to Linux. We will add that only
after safe pointer forwarding works.

## Security and safety

- Wireless ADB grants powerful control of the tablet. Keep it on a trusted LAN.
- Accessing `/dev/input/event*` requires explicit local permissions. Do not run
  this program with `sudo`; use a narrow NixOS/udev permission rule instead.
- Never deploy exclusive grabs without a tested local escape chord and timeout.
