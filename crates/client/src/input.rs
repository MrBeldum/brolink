//! Keyboard and mouse translation: egui events in, Moonlight input calls out.

use brolink_stream::ffi::{
    BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, BUTTON_X1, BUTTON_X2, MODIFIER_ALT, MODIFIER_CTRL,
    MODIFIER_META, MODIFIER_SHIFT,
};
use brolink_stream::Session;
use egui::{Key, Modifiers, PointerButton};
use std::collections::BTreeSet;

pub const VK_SHIFT: i16 = 0x10;
pub const VK_CONTROL: i16 = 0x11;
pub const VK_MENU: i16 = 0x12;
pub const VK_LWIN: i16 = 0x5B;
pub const VK_DELETE: i16 = 0x2E;
pub const VK_ESCAPE: i16 = 0x1B;
pub const VK_TAB: i16 = 0x09;
pub const VK_SNAPSHOT: i16 = 0x2C;
pub const VK_C: i16 = 0x43;
pub const VK_V: i16 = 0x56;
pub const VK_X: i16 = 0x58;

/// Windows virtual-key code for an egui key, on a US layout.
pub fn vk(key: Key) -> Option<i16> {
    use Key::*;
    Some(match key {
        ArrowDown => 0x28,
        ArrowLeft => 0x25,
        ArrowRight => 0x27,
        ArrowUp => 0x26,
        Escape => VK_ESCAPE,
        Tab => VK_TAB,
        Backspace => 0x08,
        Enter => 0x0D,
        Space => 0x20,
        Insert => 0x2D,
        Delete => VK_DELETE,
        Home => 0x24,
        End => 0x23,
        PageUp => 0x21,
        PageDown => 0x22,
        Colon | Semicolon => 0xBA,
        Comma => 0xBC,
        Backslash | Pipe => 0xDC,
        Slash | Questionmark => 0xBF,
        Exclamationmark => 0x31,
        OpenBracket | OpenCurlyBracket => 0xDB,
        CloseBracket | CloseCurlyBracket => 0xDD,
        Backtick => 0xC0,
        Minus => 0xBD,
        Period => 0xBE,
        Plus | Equals => 0xBB,
        Quote => 0xDE,
        Num0 => 0x30,
        Num1 => 0x31,
        Num2 => 0x32,
        Num3 => 0x33,
        Num4 => 0x34,
        Num5 => 0x35,
        Num6 => 0x36,
        Num7 => 0x37,
        Num8 => 0x38,
        Num9 => 0x39,
        A => 0x41,
        B => 0x42,
        C => 0x43,
        D => 0x44,
        E => 0x45,
        F => 0x46,
        G => 0x47,
        H => 0x48,
        I => 0x49,
        J => 0x4A,
        K => 0x4B,
        L => 0x4C,
        M => 0x4D,
        N => 0x4E,
        O => 0x4F,
        P => 0x50,
        Q => 0x51,
        R => 0x52,
        S => 0x53,
        T => 0x54,
        U => 0x55,
        V => 0x56,
        W => 0x57,
        X => 0x58,
        Y => 0x59,
        Z => 0x5A,
        F1 => 0x70,
        F2 => 0x71,
        F3 => 0x72,
        F4 => 0x73,
        F5 => 0x74,
        F6 => 0x75,
        F7 => 0x76,
        F8 => 0x77,
        F9 => 0x78,
        F10 => 0x79,
        F11 => 0x7A,
        F12 => 0x7B,
        F13 => 0x7C,
        F14 => 0x7D,
        F15 => 0x7E,
        F16 => 0x7F,
        F17 => 0x80,
        F18 => 0x81,
        F19 => 0x82,
        F20 => 0x83,
        F21 => 0x84,
        F22 => 0x85,
        F23 => 0x86,
        F24 => 0x87,
        _ => return None,
    })
}

pub fn button(b: PointerButton) -> Option<i32> {
    Some(match b {
        PointerButton::Primary => BUTTON_LEFT,
        PointerButton::Secondary => BUTTON_RIGHT,
        PointerButton::Middle => BUTTON_MIDDLE,
        PointerButton::Extra1 => BUTTON_X1,
        PointerButton::Extra2 => BUTTON_X2,
    })
}

/// Which keys and buttons the PC currently believes are down, so they can all
/// be released when focus or capture is lost.
#[derive(Default)]
pub struct Held {
    keys: BTreeSet<i16>,
    buttons: BTreeSet<i32>,
    modifiers: Modifiers,
}

impl Held {
    pub fn key(&mut self, session: &Session, vk: i16, down: bool) {
        if down {
            self.keys.insert(vk);
        } else {
            self.keys.remove(&vk);
        }
        session.key(vk, down, self.mask());
    }

    pub fn button(&mut self, session: &Session, b: i32, down: bool) {
        if down {
            self.buttons.insert(b);
        } else {
            self.buttons.remove(&b);
        }
        session.mouse_button(b, down);
    }

    /// Turn modifier changes into key presses. Cmd maps to Ctrl or the
    /// Windows key.
    pub fn modifiers(&mut self, session: &Session, now: Modifiers, cmd_is_ctrl: bool) {
        let was = self.modifiers;
        self.modifiers = now;
        let cmd_vk = if cmd_is_ctrl { VK_CONTROL } else { VK_LWIN };
        for (before, after, vk) in [
            (was.shift, now.shift, VK_SHIFT),
            (was.ctrl, now.ctrl, VK_CONTROL),
            (was.alt, now.alt, VK_MENU),
            (was.mac_cmd, now.mac_cmd, cmd_vk),
        ] {
            if before != after {
                self.key(session, vk, after);
            }
        }
    }

    /// The modifier byte Moonlight wants alongside every key event.
    fn mask(&self) -> i8 {
        let mut m = 0;
        if self.keys.contains(&VK_SHIFT) {
            m |= MODIFIER_SHIFT;
        }
        if self.keys.contains(&VK_CONTROL) {
            m |= MODIFIER_CTRL;
        }
        if self.keys.contains(&VK_MENU) {
            m |= MODIFIER_ALT;
        }
        if self.keys.contains(&VK_LWIN) {
            m |= MODIFIER_META;
        }
        m
    }

    pub fn release_all(&mut self, session: &Session) {
        for k in std::mem::take(&mut self.keys) {
            session.key(k, false, 0);
        }
        for b in std::mem::take(&mut self.buttons) {
            session.mouse_button(b, false);
        }
        self.modifiers = Modifiers::NONE;
    }

    /// Press a chord and release it, e.g. Ctrl+Alt+Del.
    pub fn chord(&mut self, session: &Session, keys: &[i16]) {
        for &k in keys {
            self.key(session, k, true);
        }
        for &k in keys.iter().rev() {
            self.key(session, k, false);
        }
    }

    pub fn any_down(&self) -> bool {
        !self.keys.is_empty() || !self.buttons.is_empty()
    }

    pub fn modifiers_now(&self) -> Modifiers {
        self.modifiers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_symbols_map_to_us_layout_codes() {
        assert_eq!(vk(Key::A), Some(0x41));
        assert_eq!(vk(Key::Z), Some(0x5A));
        assert_eq!(vk(Key::Num0), Some(0x30));
        assert_eq!(vk(Key::F12), Some(0x7B));
        assert_eq!(vk(Key::Colon), vk(Key::Semicolon));
        assert_eq!(vk(Key::Copy), None, "clipboard keys are handled as events");
    }
}
