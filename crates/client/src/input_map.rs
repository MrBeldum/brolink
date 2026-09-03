//! Map egui / gilrs input onto BroLink `InputEvent`s (Windows scancodes + XInput).

use brolink_core::proto::InputEvent;
use egui::{InputState, Key, PointerButton};
use gilrs::{Axis, Button, Gilrs};

/// Keys the client swallows because they drive the client UI itself. Sending
/// them on would mean pressing F8 to release the mouse also pressed F8 in the
/// game you were playing.
fn is_client_hotkey(key: Key, mods: &egui::Modifiers) -> bool {
    matches!(key, Key::F8 | Key::F11 | Key::F7) || (mods.ctrl && mods.shift && key == Key::Q)
}

/// Modifier scancode/virtual-key pairs, in the order they are reported.
const MODIFIERS: [(&str, u16, u16); 4] = [
    ("shift", 0x2A, 0x10),     // Left Shift / VK_SHIFT
    ("ctrl", 0x1D, 0x11),      // Left Ctrl / VK_CONTROL
    ("alt", 0x38, 0x12),       // Left Alt / VK_MENU
    ("command", 0xE05B, 0x5B), // Left Win / VK_LWIN
];

#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct ModState {
    shift: bool,
    ctrl: bool,
    alt: bool,
    command: bool,
}

impl ModState {
    fn from_egui(m: &egui::Modifiers) -> Self {
        Self {
            shift: m.shift,
            ctrl: m.ctrl,
            alt: m.alt,
            command: m.command && !m.ctrl,
        }
    }

    fn get(&self, name: &str) -> bool {
        match name {
            "shift" => self.shift,
            "ctrl" => self.ctrl,
            "alt" => self.alt,
            _ => self.command,
        }
    }
}

pub struct InputCollector {
    pub gilrs: Option<Gilrs>,
    last_pad: Option<InputEvent>,
    /// Modifiers we have told the host are held. egui reports modifiers as a
    /// state bitmask rather than as key events, so without tracking edges here
    /// the host never sees Shift, Ctrl or Alt at all.
    mods: ModState,
    /// Non-modifier keys and mouse buttons we have sent a press for. Needed so
    /// releasing capture can release them too, instead of leaving the host with
    /// W held down forever.
    keys_down: Vec<(u16, u16)>,
    buttons_down: Vec<u8>,
    /// Last absolute position sent, to avoid flooding identical positions.
    last_abs: Option<(u16, u16)>,
}

impl Default for InputCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl InputCollector {
    pub fn new() -> Self {
        let gilrs = Gilrs::new().ok();
        if gilrs.is_none() {
            tracing::warn!("gilrs failed to init — keyboard/mouse only");
        }
        Self {
            gilrs,
            last_pad: None,
            mods: ModState::default(),
            keys_down: Vec::new(),
            buttons_down: Vec::new(),
            last_abs: None,
        }
    }

    /// Gather everything that happened this frame.
    ///
    /// `captured` means the pointer is locked to the video and keyboard/mouse
    /// go to the host; when it is false only the gamepad is forwarded, so the
    /// client's own UI stays usable.
    pub fn collect(
        &mut self,
        raw: &InputState,
        captured: bool,
        video_rect: egui::Rect,
    ) -> Vec<InputEvent> {
        let mut out = Vec::new();
        if !captured {
            // Anything still held belongs to the host, and it cannot see that
            // we stopped forwarding — release it explicitly.
            self.release_all(&mut out);
            self.poll_pad(&mut out);
            return out;
        }

        self.collect_modifiers(&raw.modifiers, &mut out);

        for ev in &raw.events {
            match ev {
                egui::Event::PointerButton {
                    button, pressed, ..
                } => {
                    let b = match button {
                        PointerButton::Primary => 0,
                        PointerButton::Secondary => 1,
                        PointerButton::Middle => 2,
                        PointerButton::Extra1 => 3,
                        PointerButton::Extra2 => 4,
                    };
                    if *pressed {
                        if !self.buttons_down.contains(&b) {
                            self.buttons_down.push(b);
                        }
                    } else {
                        self.buttons_down.retain(|x| *x != b);
                    }
                    out.push(InputEvent::MouseButton {
                        button: b,
                        down: *pressed,
                    });
                }
                egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } if !repeat => {
                    if is_client_hotkey(*key, modifiers) {
                        continue;
                    }
                    if let Some((sc, vk)) = map_key(*key) {
                        if *pressed {
                            if !self.keys_down.contains(&(sc, vk)) {
                                self.keys_down.push((sc, vk));
                            }
                        } else {
                            self.keys_down.retain(|k| *k != (sc, vk));
                        }
                        out.push(InputEvent::Key {
                            scancode: sc,
                            vk,
                            down: *pressed,
                        });
                    }
                }
                // Raw device motion, unaffected by the OS pointer sitting at
                // the edge of the screen. This is what makes mouselook work;
                // `PointerMoved` deltas stop as soon as the cursor is pinned.
                egui::Event::MouseMoved(delta) => {
                    if delta.x != 0.0 || delta.y != 0.0 {
                        out.push(InputEvent::MouseMoveRel {
                            dx: delta.x.round() as i32,
                            dy: delta.y.round() as i32,
                        });
                    }
                }
                egui::Event::MouseWheel { delta, unit, .. } => {
                    // The host multiplies by WHEEL_DELTA (120) per notch.
                    let scale = match unit {
                        egui::MouseWheelUnit::Point => 1.0 / 40.0,
                        egui::MouseWheelUnit::Line => 1.0,
                        egui::MouseWheelUnit::Page => 8.0,
                    };
                    let dx = (delta.x * scale).round() as i32;
                    let dy = (delta.y * scale).round() as i32;
                    if dx != 0 || dy != 0 {
                        out.push(InputEvent::MouseWheel { dx, dy });
                    }
                }
                _ => {}
            }
        }

        // Absolute positioning for pointer-driven apps (and any platform that
        // does not deliver raw motion): only used when the cursor is not
        // locked, since a locked cursor never moves.
        if !raw.events.iter().any(is_raw_motion) {
            if let Some(pos) = raw.pointer.latest_pos() {
                if let Some(abs) = to_absolute(pos, video_rect) {
                    if self.last_abs != Some(abs) {
                        self.last_abs = Some(abs);
                        out.push(InputEvent::MouseMoveAbs { x: abs.0, y: abs.1 });
                    }
                }
            }
        }

        self.poll_pad(&mut out);
        out
    }

    fn collect_modifiers(&mut self, m: &egui::Modifiers, out: &mut Vec<InputEvent>) {
        let now = ModState::from_egui(m);
        if now == self.mods {
            return;
        }
        for (name, sc, vk) in MODIFIERS {
            let was = self.mods.get(name);
            let is = now.get(name);
            if was != is {
                out.push(InputEvent::Key {
                    scancode: sc,
                    vk,
                    down: is,
                });
            }
        }
        self.mods = now;
    }

    /// Emit a release for everything we have pressed, and forget it.
    fn release_all(&mut self, out: &mut Vec<InputEvent>) {
        for (sc, vk) in self.keys_down.drain(..) {
            out.push(InputEvent::Key {
                scancode: sc,
                vk,
                down: false,
            });
        }
        for b in self.buttons_down.drain(..) {
            out.push(InputEvent::MouseButton {
                button: b,
                down: false,
            });
        }
        for (name, sc, vk) in MODIFIERS {
            if self.mods.get(name) {
                out.push(InputEvent::Key {
                    scancode: sc,
                    vk,
                    down: false,
                });
            }
        }
        self.mods = ModState::default();
        self.last_abs = None;
    }

    fn poll_pad(&mut self, out: &mut Vec<InputEvent>) {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return;
        };
        // Drain the event queue so `gamepad()` state is current; the events
        // themselves are not needed because we send a full pad snapshot.
        while gilrs.next_event().is_some() {}
        let Some((_id, pad)) = gilrs.gamepads().next() else {
            return;
        };

        let mut buttons = 0u16;
        for (mask, button) in [
            (0x0001u16, Button::DPadUp),
            (0x0002, Button::DPadDown),
            (0x0004, Button::DPadLeft),
            (0x0008, Button::DPadRight),
            (0x0010, Button::Start),
            (0x0020, Button::Select),
            (0x0040, Button::LeftThumb),
            (0x0080, Button::RightThumb),
            // XInput's shoulder bits are the bumpers; gilrs calls those
            // LeftTrigger/RightTrigger and the analog triggers *Trigger2.
            (0x0100, Button::LeftTrigger),
            (0x0200, Button::RightTrigger),
            (0x1000, Button::South),
            (0x2000, Button::East),
            (0x4000, Button::West),
            (0x8000, Button::North),
        ] {
            if pad.is_pressed(button) {
                buttons |= mask;
            }
        }

        // Analog triggers: prefer the button's own analog value, because many
        // pads do not expose LeftZ/RightZ axes at all.
        let lt = trigger_value(&pad, Button::LeftTrigger2, Axis::LeftZ);
        let rt = trigger_value(&pad, Button::RightTrigger2, Axis::RightZ);
        // gilrs and XInput agree that up is positive on the sticks, so these
        // pass straight through — negating Y here inverted look controls.
        let lx = axis_i16(pad.value(Axis::LeftStickX));
        let ly = axis_i16(pad.value(Axis::LeftStickY));
        let rx = axis_i16(pad.value(Axis::RightStickX));
        let ry = axis_i16(pad.value(Axis::RightStickY));

        let ev = InputEvent::Gamepad {
            buttons,
            lt,
            rt,
            lx,
            ly,
            rx,
            ry,
        };
        // Only send on change: a pad at rest would otherwise saturate the link
        // with identical packets at the UI frame rate.
        if self.last_pad.as_ref() != Some(&ev) {
            self.last_pad = Some(ev.clone());
            out.push(ev);
        }
    }
}

fn is_raw_motion(ev: &egui::Event) -> bool {
    matches!(ev, egui::Event::MouseMoved(_))
}

/// Map a screen position onto the host's 0..=65535 virtual-desktop coordinates.
/// Returns `None` when the pointer is outside the video image.
fn to_absolute(pos: egui::Pos2, rect: egui::Rect) -> Option<(u16, u16)> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || !rect.contains(pos) {
        return None;
    }
    let fx = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    let fy = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
    Some(((fx * 65535.0) as u16, (fy * 65535.0) as u16))
}

fn trigger_value(pad: &gilrs::Gamepad<'_>, button: Button, axis: Axis) -> u8 {
    if let Some(data) = pad.button_data(button) {
        return axis_u8(data.value());
    }
    let v = pad.value(axis);
    // Some backends report triggers as -1..=1 rather than 0..=1.
    axis_u8(if v < 0.0 { (v + 1.0) / 2.0 } else { v })
}

fn axis_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0) as i16
}

fn axis_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0) as u8
}

/// Windows Set 1 scancode + virtual-key for common keys.
pub fn map_key(key: Key) -> Option<(u16, u16)> {
    Some(match key {
        Key::Escape => (0x01, 0x1B),
        Key::Num1 => (0x02, 0x31),
        Key::Num2 => (0x03, 0x32),
        Key::Num3 => (0x04, 0x33),
        Key::Num4 => (0x05, 0x34),
        Key::Num5 => (0x06, 0x35),
        Key::Num6 => (0x07, 0x36),
        Key::Num7 => (0x08, 0x37),
        Key::Num8 => (0x09, 0x38),
        Key::Num9 => (0x0A, 0x39),
        Key::Num0 => (0x0B, 0x30),
        Key::Minus => (0x0C, 0xBD),
        Key::Equals => (0x0D, 0xBB),
        Key::Backspace => (0x0E, 0x08),
        Key::Tab => (0x0F, 0x09),
        Key::Q => (0x10, 0x51),
        Key::W => (0x11, 0x57),
        Key::E => (0x12, 0x45),
        Key::R => (0x13, 0x52),
        Key::T => (0x14, 0x54),
        Key::Y => (0x15, 0x59),
        Key::U => (0x16, 0x55),
        Key::I => (0x17, 0x49),
        Key::O => (0x18, 0x4F),
        Key::P => (0x19, 0x50),
        Key::OpenBracket => (0x1A, 0xDB),
        Key::CloseBracket => (0x1B, 0xDD),
        Key::Enter => (0x1C, 0x0D),
        Key::A => (0x1E, 0x41),
        Key::S => (0x1F, 0x53),
        Key::D => (0x20, 0x44),
        Key::F => (0x21, 0x46),
        Key::G => (0x22, 0x47),
        Key::H => (0x23, 0x48),
        Key::J => (0x24, 0x4A),
        Key::K => (0x25, 0x4B),
        Key::L => (0x26, 0x4C),
        Key::Semicolon => (0x27, 0xBA),
        Key::Quote => (0x28, 0xDE),
        Key::Backtick => (0x29, 0xC0),
        Key::Backslash => (0x2B, 0xDC),
        Key::Z => (0x2C, 0x5A),
        Key::X => (0x2D, 0x58),
        Key::C => (0x2E, 0x43),
        Key::V => (0x2F, 0x56),
        Key::B => (0x30, 0x42),
        Key::N => (0x31, 0x4E),
        Key::M => (0x32, 0x4D),
        Key::Comma => (0x33, 0xBC),
        Key::Period => (0x34, 0xBE),
        Key::Slash => (0x35, 0xBF),
        Key::Space => (0x39, 0x20),
        Key::F1 => (0x3B, 0x70),
        Key::F2 => (0x3C, 0x71),
        Key::F3 => (0x3D, 0x72),
        Key::F4 => (0x3E, 0x73),
        Key::F5 => (0x3F, 0x74),
        Key::F6 => (0x40, 0x75),
        Key::F7 => (0x41, 0x76),
        Key::F8 => (0x42, 0x77),
        Key::F9 => (0x43, 0x78),
        Key::F10 => (0x44, 0x79),
        Key::F11 => (0x57, 0x7A),
        Key::F12 => (0x58, 0x7B),
        Key::Home => (0xE047, 0x24),
        Key::ArrowUp => (0xE048, 0x26),
        Key::PageUp => (0xE049, 0x21),
        Key::ArrowLeft => (0xE04B, 0x25),
        Key::ArrowRight => (0xE04D, 0x27),
        Key::End => (0xE04F, 0x23),
        Key::ArrowDown => (0xE050, 0x28),
        Key::PageDown => (0xE051, 0x22),
        Key::Insert => (0xE052, 0x2D),
        Key::Delete => (0xE053, 0x2E),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A collector with no gamepad, so tests only exercise keyboard and mouse.
    fn collector() -> InputCollector {
        InputCollector {
            gilrs: None,
            last_pad: None,
            mods: ModState::default(),
            keys_down: Vec::new(),
            buttons_down: Vec::new(),
            last_abs: None,
        }
    }

    fn mods(shift: bool, ctrl: bool, alt: bool) -> egui::Modifiers {
        egui::Modifiers {
            shift,
            ctrl,
            alt,
            command: ctrl,
            mac_cmd: false,
        }
    }

    fn state(events: Vec<egui::Event>, modifiers: egui::Modifiers) -> InputState {
        let raw = egui::RawInput {
            events,
            modifiers,
            ..Default::default()
        };
        InputState::default().begin_pass(raw, false, 1.0 / 60.0, &Default::default())
    }

    fn key_event(key: Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    const RECT: egui::Rect = egui::Rect {
        min: egui::Pos2 { x: 0.0, y: 0.0 },
        max: egui::Pos2 { x: 100.0, y: 100.0 },
    };

    #[test]
    fn keys_map_to_scancode_and_vk() {
        let mut c = collector();
        let raw = state(
            vec![key_event(Key::W, true, mods(false, false, false))],
            mods(false, false, false),
        );
        let out = c.collect(&raw, true, RECT);
        assert!(out.contains(&InputEvent::Key {
            scancode: 0x11,
            vk: 0x57,
            down: true
        }));
    }

    #[test]
    fn modifier_edges_are_sent_as_key_presses() {
        let mut c = collector();
        // Shift goes down: egui reports it only in the modifier bitmask.
        let out = c.collect(&state(vec![], mods(true, false, false)), true, RECT);
        assert_eq!(
            out,
            vec![InputEvent::Key {
                scancode: 0x2A,
                vk: 0x10,
                down: true
            }]
        );
        // Holding it produces nothing further.
        let out = c.collect(&state(vec![], mods(true, false, false)), true, RECT);
        assert!(out.is_empty(), "{out:?}");
        // Releasing sends the up edge.
        let out = c.collect(&state(vec![], mods(false, false, false)), true, RECT);
        assert_eq!(
            out,
            vec![InputEvent::Key {
                scancode: 0x2A,
                vk: 0x10,
                down: false
            }]
        );
    }

    #[test]
    fn several_modifiers_can_change_at_once() {
        let mut c = collector();
        let out = c.collect(&state(vec![], mods(true, true, true)), true, RECT);
        let downs: Vec<u16> = out
            .iter()
            .filter_map(|e| match e {
                InputEvent::Key {
                    scancode,
                    down: true,
                    ..
                } => Some(*scancode),
                _ => None,
            })
            .collect();
        assert_eq!(downs, vec![0x2A, 0x1D, 0x38]);
    }

    #[test]
    fn client_hotkeys_are_not_forwarded() {
        let mut c = collector();
        let plain = mods(false, false, false);
        let out = c.collect(
            &state(
                vec![
                    key_event(Key::F8, true, plain),
                    key_event(Key::F11, true, plain),
                ],
                plain,
            ),
            true,
            RECT,
        );
        assert!(
            out.is_empty(),
            "F8/F11 drive the client, not the host: {out:?}"
        );

        // Ctrl+Shift+Q is the disconnect chord, but a bare Q must still work.
        let chord = mods(true, true, false);
        let out = c.collect(
            &state(vec![key_event(Key::Q, true, chord)], chord),
            true,
            RECT,
        );
        assert!(
            !out.iter()
                .any(|e| matches!(e, InputEvent::Key { vk: 0x51, .. })),
            "{out:?}"
        );
        let mut c = collector();
        let out = c.collect(
            &state(vec![key_event(Key::Q, true, plain)], plain),
            true,
            RECT,
        );
        assert!(out.contains(&InputEvent::Key {
            scancode: 0x10,
            vk: 0x51,
            down: true
        }));
    }

    #[test]
    fn f12_still_reaches_the_host() {
        let mut c = collector();
        let plain = mods(false, false, false);
        let out = c.collect(
            &state(vec![key_event(Key::F12, true, plain)], plain),
            true,
            RECT,
        );
        assert!(out.contains(&InputEvent::Key {
            scancode: 0x58,
            vk: 0x7B,
            down: true
        }));
    }

    #[test]
    fn raw_motion_becomes_relative_movement() {
        let mut c = collector();
        let plain = mods(false, false, false);
        let out = c.collect(
            &state(vec![egui::Event::MouseMoved(egui::vec2(12.4, -7.6))], plain),
            true,
            RECT,
        );
        assert_eq!(out, vec![InputEvent::MouseMoveRel { dx: 12, dy: -8 }]);
    }

    #[test]
    fn zero_motion_is_not_sent() {
        let mut c = collector();
        let plain = mods(false, false, false);
        let out = c.collect(
            &state(vec![egui::Event::MouseMoved(egui::vec2(0.0, 0.0))], plain),
            true,
            RECT,
        );
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn releasing_capture_releases_everything_held() {
        let mut c = collector();
        let held = mods(true, false, false);
        c.collect(
            &state(
                vec![
                    key_event(Key::W, true, held),
                    egui::Event::PointerButton {
                        pos: egui::pos2(10.0, 10.0),
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: held,
                    },
                ],
                held,
            ),
            true,
            RECT,
        );

        // Uncapturing must not leave W, Shift and the left button stuck down on
        // the host — it cannot tell that we simply stopped forwarding.
        let out = c.collect(&state(vec![], held), false, RECT);
        assert!(out.contains(&InputEvent::Key {
            scancode: 0x11,
            vk: 0x57,
            down: false
        }));
        assert!(out.contains(&InputEvent::MouseButton {
            button: 0,
            down: false
        }));
        assert!(out.contains(&InputEvent::Key {
            scancode: 0x2A,
            vk: 0x10,
            down: false
        }));
        // And nothing is released twice.
        let out = c.collect(&state(vec![], mods(false, false, false)), false, RECT);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn mouse_buttons_map_to_indices() {
        let mut c = collector();
        let plain = mods(false, false, false);
        let events: Vec<egui::Event> = [
            PointerButton::Primary,
            PointerButton::Secondary,
            PointerButton::Middle,
            PointerButton::Extra1,
            PointerButton::Extra2,
        ]
        .into_iter()
        .map(|button| egui::Event::PointerButton {
            pos: egui::pos2(1.0, 1.0),
            button,
            pressed: true,
            modifiers: plain,
        })
        .collect();
        let out = c.collect(&state(events, plain), true, RECT);
        let buttons: Vec<u8> = out
            .iter()
            .filter_map(|e| match e {
                InputEvent::MouseButton { button, .. } => Some(*button),
                _ => None,
            })
            .collect();
        assert_eq!(buttons, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn wheel_units_are_normalised_to_notches() {
        let mut c = collector();
        let plain = mods(false, false, false);
        let out = c.collect(
            &state(
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 2.0),
                    modifiers: plain,
                }],
                plain,
            ),
            true,
            RECT,
        );
        assert_eq!(out, vec![InputEvent::MouseWheel { dx: 0, dy: 2 }]);

        // 40 points is one line's worth of scrolling.
        let out = c.collect(
            &state(
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 40.0),
                    modifiers: plain,
                }],
                plain,
            ),
            true,
            RECT,
        );
        assert_eq!(out, vec![InputEvent::MouseWheel { dx: 0, dy: 1 }]);
    }

    #[test]
    fn sub_notch_scrolling_is_dropped_rather_than_rounded_up() {
        let mut c = collector();
        let plain = mods(false, false, false);
        let out = c.collect(
            &state(
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 1.0),
                    modifiers: plain,
                }],
                plain,
            ),
            true,
            RECT,
        );
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn absolute_mapping_covers_the_video_rect() {
        let r = egui::Rect::from_min_max(egui::pos2(20.0, 10.0), egui::pos2(120.0, 110.0));
        assert_eq!(to_absolute(egui::pos2(20.0, 10.0), r), Some((0, 0)));
        assert_eq!(
            to_absolute(egui::pos2(120.0, 110.0), r),
            Some((65535, 65535))
        );
        let mid = to_absolute(egui::pos2(70.0, 60.0), r).expect("centre is inside");
        assert!((mid.0 as i32 - 32767).abs() <= 2, "{mid:?}");
        assert!((mid.1 as i32 - 32767).abs() <= 2, "{mid:?}");
        // Outside the image, and degenerate rects, produce nothing.
        assert_eq!(to_absolute(egui::pos2(0.0, 0.0), r), None);
        assert_eq!(to_absolute(egui::pos2(5.0, 5.0), egui::Rect::NOTHING), None);
    }

    #[test]
    fn raw_motion_suppresses_absolute_positioning() {
        let mut c = collector();
        let plain = mods(false, false, false);
        // With raw motion present, the pointer position is meaningless (it is
        // pinned), so only the relative event should be sent.
        let out = c.collect(
            &state(
                vec![
                    egui::Event::PointerMoved(egui::pos2(50.0, 50.0)),
                    egui::Event::MouseMoved(egui::vec2(3.0, 3.0)),
                ],
                plain,
            ),
            true,
            RECT,
        );
        assert_eq!(out, vec![InputEvent::MouseMoveRel { dx: 3, dy: 3 }]);
    }

    #[test]
    fn pointer_position_becomes_absolute_movement_without_raw_motion() {
        let mut c = collector();
        let plain = mods(false, false, false);
        let out = c.collect(
            &state(
                vec![egui::Event::PointerMoved(egui::pos2(50.0, 25.0))],
                plain,
            ),
            true,
            RECT,
        );
        let abs = out
            .iter()
            .find_map(|e| match e {
                InputEvent::MouseMoveAbs { x, y } => Some((*x, *y)),
                _ => None,
            })
            .expect("an absolute move");
        assert!((abs.0 as i32 - 32767).abs() < 400, "{abs:?}");
        assert!((abs.1 as i32 - 16383).abs() < 400, "{abs:?}");

        // Standing still does not repeat the position.
        let out = c.collect(&state(vec![], plain), true, RECT);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn uncaptured_input_is_not_forwarded() {
        let mut c = collector();
        let plain = mods(false, false, false);
        let out = c.collect(
            &state(
                vec![
                    key_event(Key::W, true, plain),
                    egui::Event::MouseMoved(egui::vec2(9.0, 9.0)),
                ],
                plain,
            ),
            false,
            RECT,
        );
        assert!(
            out.is_empty(),
            "typing in the client UI must stay local: {out:?}"
        );
    }

    #[test]
    fn axis_scaling_hits_the_expected_ranges() {
        assert_eq!(axis_i16(1.0), 32767);
        assert_eq!(axis_i16(-1.0), -32767);
        assert_eq!(axis_i16(0.0), 0);
        assert_eq!(axis_i16(2.0), 32767, "out-of-range input is clamped");
        assert_eq!(axis_u8(1.0), 255);
        assert_eq!(axis_u8(0.0), 0);
        assert_eq!(axis_u8(-1.0), 0);
    }

    #[test]
    fn every_mapped_key_has_a_distinct_scancode() {
        let keys = [
            Key::Escape,
            Key::A,
            Key::Z,
            Key::Num0,
            Key::Num9,
            Key::F1,
            Key::F12,
            Key::Home,
            Key::End,
            Key::Insert,
            Key::Delete,
            Key::ArrowUp,
            Key::ArrowDown,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::PageUp,
            Key::PageDown,
            Key::Space,
            Key::Enter,
            Key::Tab,
            Key::Backspace,
        ];
        let mut seen = std::collections::HashSet::new();
        for k in keys {
            let (sc, vk) = map_key(k).unwrap_or_else(|| panic!("{k:?} should map"));
            assert!(seen.insert(sc), "{k:?} reuses scancode {sc:#x}");
            assert_ne!(vk, 0, "{k:?} needs a virtual-key for the fallback path");
        }
    }

    #[test]
    fn extended_keys_keep_their_e0_prefix() {
        // The host relies on the 0xE0 prefix to set KEYEVENTF_EXTENDEDKEY;
        // without it the arrow keys act like the numeric keypad.
        for k in [Key::ArrowUp, Key::ArrowDown, Key::Home, Key::Delete] {
            let (sc, _) = map_key(k).expect("mapped");
            assert_eq!(sc & 0xFF00, 0xE000, "{k:?} -> {sc:#x}");
        }
    }
}
