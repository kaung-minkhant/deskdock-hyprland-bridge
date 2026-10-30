# Hyprland → Android input bridge (experimental)

A Wayland/Hyprland-oriented experiment for sending Linux input to an Android
tablet through its already-paired ADB connection. It is intentionally built in
small, safe stages instead of grabbing your input devices immediately.

## Current scope

- Lists readable `/dev/input/event*` devices.
- Verifies that the paired Android device accepts an ADB input command.
- Monitors one selected evdev device in diagnostic mode.
- **Does not yet enable a global Hyprland toggle or forward pointer events.**

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

Move the mouse or press keys. The program should print its events. This tests
permissions and identifies the correct hardware before it can take exclusive
control. Press `Ctrl+C` to stop.

`--grab` is deliberately present only for a later controlled test. An evdev
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
