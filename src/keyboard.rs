use crate::synergy;
use anyhow::Result;
use evdev::KeyCode;
use std::{collections::BTreeMap, net::TcpStream};

// Synergy/Barrier modifier mask bits.
const SHIFT: u16 = 0x0001;
const CONTROL: u16 = 0x0002;
const ALT: u16 = 0x0004;
const SUPER: u16 = 0x0010;

// Synergy/Barrier key IDs for non-character keys (X11 keysym - 0x1000).
const KEY_BACKSPACE: u16 = 0xEF08;
const KEY_TAB: u16 = 0xEF09;
const KEY_RETURN: u16 = 0xEF0D;
const KEY_SCROLL_LOCK: u16 = 0xEF14;
const KEY_ESCAPE: u16 = 0xEF1B;
const KEY_DELETE: u16 = 0xEFFF;
const KEY_HOME: u16 = 0xEF50;
const KEY_LEFT: u16 = 0xEF51;
const KEY_UP: u16 = 0xEF52;
const KEY_RIGHT: u16 = 0xEF53;
const KEY_DOWN: u16 = 0xEF54;
const KEY_PAGE_UP: u16 = 0xEF55;
const KEY_PAGE_DOWN: u16 = 0xEF56;
const KEY_END: u16 = 0xEF57;
const KEY_INSERT: u16 = 0xEF63;
const KEY_MENU: u16 = 0xEF67;
const KEY_NUM_LOCK: u16 = 0xEF7F;
const KEY_KP_ENTER: u16 = 0xEF8D;
const KEY_KP_MULTIPLY: u16 = 0xEFAA;
const KEY_KP_ADD: u16 = 0xEFAB;
const KEY_KP_SUBTRACT: u16 = 0xEFAD;
const KEY_KP_DECIMAL: u16 = 0xEFAE;
const KEY_KP_DIVIDE: u16 = 0xEFAF;
const KEY_KP_0: u16 = 0xEFB0;
const KEY_F1: u16 = 0xEFBE;
const KEY_SHIFT_L: u16 = 0xEFE1;
const KEY_SHIFT_R: u16 = 0xEFE2;
const KEY_CONTROL_L: u16 = 0xEFE3;
const KEY_CONTROL_R: u16 = 0xEFE4;
const KEY_CAPS_LOCK: u16 = 0xEFE5;
const KEY_ALT_L: u16 = 0xEFE9;
const KEY_ALT_R: u16 = 0xEFEA;
const KEY_SUPER_L: u16 = 0xEFEB;
const KEY_SUPER_R: u16 = 0xEFEC;
const KEY_LEFT_TAB: u16 = 0xEE20;

/// US-layout printable keys: (evdev key, unshifted, shifted). Barrier key IDs
/// for printable keys are the Unicode code point of the produced character.
const PRINTABLE: &[(KeyCode, char, char)] = &[
    (KeyCode::KEY_1, '1', '!'),
    (KeyCode::KEY_2, '2', '@'),
    (KeyCode::KEY_3, '3', '#'),
    (KeyCode::KEY_4, '4', '$'),
    (KeyCode::KEY_5, '5', '%'),
    (KeyCode::KEY_6, '6', '^'),
    (KeyCode::KEY_7, '7', '&'),
    (KeyCode::KEY_8, '8', '*'),
    (KeyCode::KEY_9, '9', '('),
    (KeyCode::KEY_0, '0', ')'),
    (KeyCode::KEY_MINUS, '-', '_'),
    (KeyCode::KEY_EQUAL, '=', '+'),
    (KeyCode::KEY_Q, 'q', 'Q'),
    (KeyCode::KEY_W, 'w', 'W'),
    (KeyCode::KEY_E, 'e', 'E'),
    (KeyCode::KEY_R, 'r', 'R'),
    (KeyCode::KEY_T, 't', 'T'),
    (KeyCode::KEY_Y, 'y', 'Y'),
    (KeyCode::KEY_U, 'u', 'U'),
    (KeyCode::KEY_I, 'i', 'I'),
    (KeyCode::KEY_O, 'o', 'O'),
    (KeyCode::KEY_P, 'p', 'P'),
    (KeyCode::KEY_LEFTBRACE, '[', '{'),
    (KeyCode::KEY_RIGHTBRACE, ']', '}'),
    (KeyCode::KEY_A, 'a', 'A'),
    (KeyCode::KEY_S, 's', 'S'),
    (KeyCode::KEY_D, 'd', 'D'),
    (KeyCode::KEY_F, 'f', 'F'),
    (KeyCode::KEY_G, 'g', 'G'),
    (KeyCode::KEY_H, 'h', 'H'),
    (KeyCode::KEY_J, 'j', 'J'),
    (KeyCode::KEY_K, 'k', 'K'),
    (KeyCode::KEY_L, 'l', 'L'),
    (KeyCode::KEY_SEMICOLON, ';', ':'),
    (KeyCode::KEY_APOSTROPHE, '\'', '"'),
    (KeyCode::KEY_GRAVE, '`', '~'),
    (KeyCode::KEY_BACKSLASH, '\\', '|'),
    (KeyCode::KEY_Z, 'z', 'Z'),
    (KeyCode::KEY_X, 'x', 'X'),
    (KeyCode::KEY_C, 'c', 'C'),
    (KeyCode::KEY_V, 'v', 'V'),
    (KeyCode::KEY_B, 'b', 'B'),
    (KeyCode::KEY_N, 'n', 'N'),
    (KeyCode::KEY_M, 'm', 'M'),
    (KeyCode::KEY_COMMA, ',', '<'),
    (KeyCode::KEY_DOT, '.', '>'),
    (KeyCode::KEY_SLASH, '/', '?'),
    (KeyCode::KEY_SPACE, ' ', ' '),
];

const FUNCTION_KEYS: &[KeyCode] = &[
    KeyCode::KEY_F1,
    KeyCode::KEY_F2,
    KeyCode::KEY_F3,
    KeyCode::KEY_F4,
    KeyCode::KEY_F5,
    KeyCode::KEY_F6,
    KeyCode::KEY_F7,
    KeyCode::KEY_F8,
    KeyCode::KEY_F9,
    KeyCode::KEY_F10,
    KeyCode::KEY_F11,
    KeyCode::KEY_F12,
];

const KEYPAD_DIGITS: &[KeyCode] = &[
    KeyCode::KEY_KP0,
    KeyCode::KEY_KP1,
    KeyCode::KEY_KP2,
    KeyCode::KEY_KP3,
    KeyCode::KEY_KP4,
    KeyCode::KEY_KP5,
    KeyCode::KEY_KP6,
    KeyCode::KEY_KP7,
    KeyCode::KEY_KP8,
    KeyCode::KEY_KP9,
];

/// Translate a Linux key into a Barrier key ID, assuming a US layout.
/// Returns `None` for keys that are intentionally not forwarded.
pub fn key_id(code: KeyCode, shift: bool) -> Option<u16> {
    if let Some((_, plain, shifted)) = PRINTABLE.iter().find(|(key, _, _)| *key == code) {
        return Some(u32::from(if shift { *shifted } else { *plain }) as u16);
    }
    if let Some(index) = FUNCTION_KEYS.iter().position(|key| *key == code) {
        return Some(KEY_F1 + index as u16);
    }
    if let Some(index) = KEYPAD_DIGITS.iter().position(|key| *key == code) {
        return Some(KEY_KP_0 + index as u16);
    }
    Some(match code {
        KeyCode::KEY_ESC => KEY_ESCAPE,
        KeyCode::KEY_BACKSPACE => KEY_BACKSPACE,
        KeyCode::KEY_TAB if shift => KEY_LEFT_TAB,
        KeyCode::KEY_TAB => KEY_TAB,
        KeyCode::KEY_ENTER => KEY_RETURN,
        KeyCode::KEY_DELETE => KEY_DELETE,
        KeyCode::KEY_INSERT => KEY_INSERT,
        KeyCode::KEY_HOME => KEY_HOME,
        KeyCode::KEY_END => KEY_END,
        KeyCode::KEY_PAGEUP => KEY_PAGE_UP,
        KeyCode::KEY_PAGEDOWN => KEY_PAGE_DOWN,
        KeyCode::KEY_LEFT => KEY_LEFT,
        KeyCode::KEY_RIGHT => KEY_RIGHT,
        KeyCode::KEY_UP => KEY_UP,
        KeyCode::KEY_DOWN => KEY_DOWN,
        KeyCode::KEY_COMPOSE => KEY_MENU,
        KeyCode::KEY_KPENTER => KEY_KP_ENTER,
        KeyCode::KEY_KPASTERISK => KEY_KP_MULTIPLY,
        KeyCode::KEY_KPPLUS => KEY_KP_ADD,
        KeyCode::KEY_KPMINUS => KEY_KP_SUBTRACT,
        KeyCode::KEY_KPDOT => KEY_KP_DECIMAL,
        KeyCode::KEY_KPSLASH => KEY_KP_DIVIDE,
        KeyCode::KEY_NUMLOCK => KEY_NUM_LOCK,
        KeyCode::KEY_SCROLLLOCK => KEY_SCROLL_LOCK,
        KeyCode::KEY_CAPSLOCK => KEY_CAPS_LOCK,
        KeyCode::KEY_LEFTSHIFT => KEY_SHIFT_L,
        KeyCode::KEY_RIGHTSHIFT => KEY_SHIFT_R,
        KeyCode::KEY_LEFTCTRL => KEY_CONTROL_L,
        KeyCode::KEY_RIGHTCTRL => KEY_CONTROL_R,
        KeyCode::KEY_LEFTALT => KEY_ALT_L,
        KeyCode::KEY_RIGHTALT => KEY_ALT_R,
        KeyCode::KEY_LEFTMETA => KEY_SUPER_L,
        KeyCode::KEY_RIGHTMETA => KEY_SUPER_R,
        _ => return None,
    })
}

fn modifier_bit(code: KeyCode) -> u16 {
    match code {
        KeyCode::KEY_LEFTSHIFT | KeyCode::KEY_RIGHTSHIFT => SHIFT,
        KeyCode::KEY_LEFTCTRL | KeyCode::KEY_RIGHTCTRL => CONTROL,
        KeyCode::KEY_LEFTALT | KeyCode::KEY_RIGHTALT => ALT,
        KeyCode::KEY_LEFTMETA | KeyCode::KEY_RIGHTMETA => SUPER,
        _ => 0,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// The key was forwarded (or deliberately ignored).
    Handled,
    /// The Super+Shift+T escape chord was pressed; Android keys were released.
    ReturnToLinux,
}

/// Tracks keys currently held on Android so they can always be released, and
/// watches for the escape chord that returns control to Linux. The chord
/// matches the Hyprland binding, which can no longer see a grabbed keyboard.
#[derive(Default)]
pub struct KeyboardState {
    /// evdev key code → Barrier key ID that was sent on press.
    held: BTreeMap<u16, u16>,
}

impl KeyboardState {
    pub fn handle(
        &mut self,
        stream: &mut TcpStream,
        code: KeyCode,
        down: bool,
    ) -> Result<KeyAction> {
        if down {
            if code == KeyCode::KEY_T && self.escape_chord_held() {
                self.release_all(stream)?;
                return Ok(KeyAction::ReturnToLinux);
            }
            if self.held.contains_key(&code.code()) {
                return Ok(KeyAction::Handled);
            }
            let Some(id) = key_id(code, self.shift_held()) else {
                return Ok(KeyAction::Handled);
            };
            self.held.insert(code.code(), id);
            synergy::send_frame(
                stream,
                &synergy::key_down(id, self.modifier_mask(), key_button(code)),
            )?;
        } else if let Some(id) = self.held.remove(&code.code()) {
            synergy::send_frame(
                stream,
                &synergy::key_up(id, self.modifier_mask(), key_button(code)),
            )?;
        }
        Ok(KeyAction::Handled)
    }

    /// Release everything Android believes is held. Called when leaving tablet
    /// mode so no modifier or key stays stuck on the tablet.
    pub fn release_all(&mut self, stream: &mut TcpStream) -> Result<()> {
        for (code, id) in std::mem::take(&mut self.held) {
            synergy::send_frame(
                stream,
                &synergy::key_up(id, 0, key_button(KeyCode::new(code))),
            )?;
        }
        Ok(())
    }

    fn escape_chord_held(&self) -> bool {
        let mask = self.modifier_mask();
        mask & SUPER != 0 && mask & SHIFT != 0
    }

    fn shift_held(&self) -> bool {
        self.modifier_mask() & SHIFT != 0
    }

    fn modifier_mask(&self) -> u16 {
        self.held
            .keys()
            .fold(0, |mask, code| mask | modifier_bit(KeyCode::new(*code)))
    }
}

/// Barrier's KeyButton only needs to be a stable, non-zero per-key value.
/// Use the X11 keycode convention (evdev code + 8).
fn key_button(code: KeyCode) -> u16 {
    code.code() + 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_keys_follow_shift() {
        assert_eq!(key_id(KeyCode::KEY_A, false), Some(u16::from(b'a')));
        assert_eq!(key_id(KeyCode::KEY_A, true), Some(u16::from(b'A')));
        assert_eq!(key_id(KeyCode::KEY_1, true), Some(u16::from(b'!')));
        assert_eq!(key_id(KeyCode::KEY_SPACE, false), Some(u16::from(b' ')));
    }

    #[test]
    fn special_keys_use_barrier_ids() {
        assert_eq!(key_id(KeyCode::KEY_ENTER, false), Some(0xEF0D));
        assert_eq!(key_id(KeyCode::KEY_ESC, false), Some(0xEF1B));
        assert_eq!(key_id(KeyCode::KEY_F1, false), Some(0xEFBE));
        assert_eq!(key_id(KeyCode::KEY_F12, false), Some(0xEFC9));
        assert_eq!(key_id(KeyCode::KEY_TAB, true), Some(0xEE20));
        assert_eq!(key_id(KeyCode::KEY_LEFTMETA, false), Some(0xEFEB));
        assert_eq!(key_id(KeyCode::KEY_KP7, false), Some(0xEFB7));
    }

    #[test]
    fn unmapped_keys_are_not_forwarded() {
        assert_eq!(key_id(KeyCode::KEY_VOLUMEUP, false), None);
    }

    #[test]
    fn modifier_mask_tracks_held_modifiers() {
        let mut state = KeyboardState::default();
        assert!(!state.escape_chord_held());
        state.held.insert(KeyCode::KEY_LEFTMETA.code(), KEY_SUPER_L);
        state
            .held
            .insert(KeyCode::KEY_RIGHTSHIFT.code(), KEY_SHIFT_R);
        assert_eq!(state.modifier_mask(), SUPER | SHIFT);
        assert!(state.escape_chord_held());
        state.held.remove(&KeyCode::KEY_LEFTMETA.code());
        assert!(!state.escape_chord_held());
    }
}
