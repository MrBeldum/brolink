//! Keyboard / mouse injection via SendInput, plus optional ViGEm Xbox 360 pad.

use brolink_core::proto::InputEvent;
use parking_lot::Mutex;
use std::collections::HashSet;

#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::*;

/// Tracks what the remote client is currently holding down.
///
/// Without this, a client that disconnects (or releases mouse capture) while a
/// key is held leaves that key stuck down on the host — walking forever in a
/// game, or repeating a character in an editor. Every held key and button is
/// released when the session ends.
#[derive(Default)]
struct HeldInput {
    keys: HashSet<(u16, u16)>,
    buttons: HashSet<u8>,
}

pub struct InputInjector {
    gamepad: Mutex<Option<VigemPad>>,
    relative: Mutex<bool>,
    held: Mutex<HeldInput>,
}

impl InputInjector {
    pub fn new(enable_gamepad: bool) -> Self {
        let gamepad = if enable_gamepad {
            match VigemPad::connect() {
                Ok(p) => {
                    tracing::info!("ViGEm virtual Xbox 360 pad ready");
                    Some(p)
                }
                Err(e) => {
                    tracing::warn!(
                        "ViGEm not available ({e}). Gamepad streaming disabled. \
                         Install ViGEmBus from https://github.com/nefarius/ViGEmBus/releases"
                    );
                    None
                }
            }
        } else {
            None
        };
        Self {
            gamepad: Mutex::new(gamepad),
            relative: Mutex::new(true),
            held: Mutex::new(HeldInput::default()),
        }
    }

    pub fn set_relative(&self, rel: bool) {
        *self.relative.lock() = rel;
    }

    pub fn apply(&self, events: &[InputEvent]) {
        for ev in events {
            match ev {
                InputEvent::MouseMoveRel { dx, dy } => inject_mouse_move(*dx, *dy),
                InputEvent::MouseMoveAbs { x, y } => inject_mouse_abs(*x, *y),
                InputEvent::MouseButton { button, down } => {
                    let mut held = self.held.lock();
                    if *down {
                        held.buttons.insert(*button);
                    } else {
                        held.buttons.remove(button);
                    }
                    drop(held);
                    inject_mouse_button(*button, *down);
                }
                InputEvent::MouseWheel { dx, dy } => inject_wheel(*dx, *dy),
                InputEvent::Key { scancode, vk, down } => {
                    let mut held = self.held.lock();
                    if *down {
                        held.keys.insert((*scancode, *vk));
                    } else {
                        held.keys.remove(&(*scancode, *vk));
                    }
                    drop(held);
                    inject_key(*scancode, *vk, *down);
                }
                InputEvent::Gamepad {
                    buttons,
                    lt,
                    rt,
                    lx,
                    ly,
                    rx,
                    ry,
                } => {
                    if let Some(pad) = self.gamepad.lock().as_mut() {
                        pad.update(&XusbReport {
                            w_buttons: *buttons,
                            b_left_trigger: *lt,
                            b_right_trigger: *rt,
                            s_thumb_lx: *lx,
                            s_thumb_ly: *ly,
                            s_thumb_rx: *rx,
                            s_thumb_ry: *ry,
                        });
                    }
                }
            }
        }
    }

    /// Release every key and button we believe the client is holding, and
    /// centre the virtual pad.
    pub fn release_all(&self) {
        let (keys, buttons) = {
            let mut held = self.held.lock();
            (
                std::mem::take(&mut held.keys),
                std::mem::take(&mut held.buttons),
            )
        };
        for (scancode, vk) in keys {
            inject_key(scancode, vk, false);
        }
        for button in buttons {
            inject_mouse_button(button, false);
        }
        if let Some(pad) = self.gamepad.lock().as_mut() {
            pad.update(&XusbReport::default());
        }
    }

    #[cfg(test)]
    fn held_counts(&self) -> (usize, usize) {
        let held = self.held.lock();
        (held.keys.len(), held.buttons.len())
    }
}

impl Drop for InputInjector {
    fn drop(&mut self) {
        self.release_all();
    }
}

#[cfg(not(windows))]
fn inject_mouse_move(_dx: i32, _dy: i32) {}
#[cfg(not(windows))]
fn inject_mouse_abs(_x: u16, _y: u16) {}
#[cfg(not(windows))]
fn inject_mouse_button(_button: u8, _down: bool) {}
#[cfg(not(windows))]
fn inject_wheel(_dx: i32, _dy: i32) {}
#[cfg(not(windows))]
fn inject_key(_scancode: u16, _vk: u16, _down: bool) {}

#[cfg(windows)]
fn send(inputs: &[INPUT]) {
    unsafe {
        let sent = SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
        if sent as usize != inputs.len() {
            // Usually UIPI: a process running at higher integrity (an elevated
            // window, or the secure desktop) has focus and refuses our input.
            tracing::debug!("SendInput delivered {sent}/{} events", inputs.len());
        }
    }
}

#[cfg(windows)]
fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(windows)]
fn inject_mouse_move(dx: i32, dy: i32) {
    send(&[mouse_input(dx, dy, 0, MOUSEEVENTF_MOVE)]);
}

#[cfg(windows)]
fn inject_mouse_abs(x: u16, y: u16) {
    // Absolute coordinates are 0..=65535 across the whole virtual desktop.
    send(&[mouse_input(
        x as i32,
        y as i32,
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )]);
}

#[cfg(windows)]
fn inject_mouse_button(button: u8, down: bool) {
    let (flags, data) = match (button, down) {
        (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (0, false) => (MOUSEEVENTF_LEFTUP, 0),
        (1, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (1, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (3, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
        (3, false) => (MOUSEEVENTF_XUP, XBUTTON1),
        (4, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
        (4, false) => (MOUSEEVENTF_XUP, XBUTTON2),
        _ => return,
    };
    send(&[mouse_input(0, 0, data, flags)]);
}

#[cfg(windows)]
const XBUTTON1: u32 = 0x0001;
#[cfg(windows)]
const XBUTTON2: u32 = 0x0002;

#[cfg(windows)]
fn inject_wheel(dx: i32, dy: i32) {
    // mouseData is a signed wheel delta reinterpreted as DWORD.
    if dy != 0 {
        send(&[mouse_input(0, 0, (dy * 120) as u32, MOUSEEVENTF_WHEEL)]);
    }
    if dx != 0 {
        send(&[mouse_input(0, 0, (dx * 120) as u32, MOUSEEVENTF_HWHEEL)]);
    }
}

/// Scancodes above 0xFF, or with 0xE0 set, are the extended set.
pub(crate) fn is_extended_scancode(scancode: u16) -> bool {
    scancode & 0xFF00 == 0xE000 || scancode & 0x0100 != 0
}

#[cfg(windows)]
fn inject_key(scancode: u16, vk: u16, down: bool) {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    // Scancode injection is what games read, so prefer it. Some keys reach us
    // with only a virtual-key code, and injecting scancode 0 would be a no-op —
    // fall back to virtual-key injection for those.
    let use_scancode = scancode & 0xFF != 0;
    if use_scancode {
        flags |= KEYEVENTF_SCANCODE;
        if is_extended_scancode(scancode) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
    } else if vk == 0 {
        return;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                // With KEYEVENTF_SCANCODE the virtual key is ignored and must
                // be zero.
                wVk: if use_scancode {
                    VIRTUAL_KEY(0)
                } else {
                    VIRTUAL_KEY(vk)
                },
                wScan: scancode & 0xFF,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send(&[input]);
}

/// Runtime-loaded ViGEmClient. Compiles without the SDK; works if ViGEmBus is installed.
pub struct VigemPad {
    client: *mut std::ffi::c_void,
    target: *mut std::ffi::c_void,
    update: unsafe extern "C" fn(*mut std::ffi::c_void, *const XusbReport) -> i32,
    target_remove: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> i32,
    target_free: unsafe extern "C" fn(*mut std::ffi::c_void),
    disconnect: unsafe extern "C" fn(*mut std::ffi::c_void),
    free: unsafe extern "C" fn(*mut std::ffi::c_void),
    // Must outlive every function pointer above.
    _lib: libloading::Library,
}

#[repr(C)]
#[derive(Default)]
struct XusbReport {
    w_buttons: u16,
    b_left_trigger: u8,
    b_right_trigger: u8,
    s_thumb_lx: i16,
    s_thumb_ly: i16,
    s_thumb_rx: i16,
    s_thumb_ry: i16,
}

impl VigemPad {
    pub fn connect() -> anyhow::Result<Self> {
        unsafe {
            let lib = load_vigem()?;
            type FnAlloc = unsafe extern "C" fn() -> *mut std::ffi::c_void;
            type FnConnect = unsafe extern "C" fn(*mut std::ffi::c_void) -> i32;
            type FnPair = unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> i32;
            type FnUpdate = unsafe extern "C" fn(*mut std::ffi::c_void, *const XusbReport) -> i32;
            type FnDrop = unsafe extern "C" fn(*mut std::ffi::c_void);

            let alloc: FnAlloc = *lib.get(b"vigem_alloc\0")?;
            let connect: FnConnect = *lib.get(b"vigem_connect\0")?;
            let target_alloc: FnAlloc = *lib.get(b"vigem_target_x360_alloc\0")?;
            let add: FnPair = *lib.get(b"vigem_target_add\0")?;
            let update: FnUpdate = *lib.get(b"vigem_target_x360_update\0")?;
            let target_remove: FnPair = *lib.get(b"vigem_target_remove\0")?;
            let target_free: FnDrop = *lib.get(b"vigem_target_free\0")?;
            let disconnect: FnDrop = *lib.get(b"vigem_disconnect\0")?;
            let free: FnDrop = *lib.get(b"vigem_free\0")?;

            let client = alloc();
            if client.is_null() {
                anyhow::bail!("vigem_alloc failed");
            }
            let rc = connect(client);
            if rc != 0 {
                free(client);
                anyhow::bail!("vigem_connect rc={rc} (is ViGEmBus installed and running?)");
            }
            let target = target_alloc();
            if target.is_null() {
                disconnect(client);
                free(client);
                anyhow::bail!("vigem_target_x360_alloc failed");
            }
            let rc = add(client, target);
            if rc != 0 {
                target_free(target);
                disconnect(client);
                free(client);
                anyhow::bail!("vigem_target_add rc={rc}");
            }
            Ok(Self {
                client,
                target,
                update,
                target_remove,
                target_free,
                disconnect,
                free,
                _lib: lib,
            })
        }
    }

    fn update(&self, report: &XusbReport) {
        unsafe {
            let _ = (self.update)(self.target, report);
        }
    }
}

impl Drop for VigemPad {
    fn drop(&mut self) {
        // Unplug the virtual controller. Skipping this leaves a phantom pad in
        // Device Manager (and in every game's controller list) until the host
        // process exits.
        unsafe {
            let _ = (self.target_remove)(self.client, self.target);
            (self.target_free)(self.target);
            (self.disconnect)(self.client);
            (self.free)(self.client);
        }
    }
}

// The ViGEm handles are only ever touched behind `InputInjector`'s mutex.
unsafe impl Send for VigemPad {}
unsafe impl Sync for VigemPad {}

fn load_vigem() -> anyhow::Result<libloading::Library> {
    let mut errs = Vec::new();
    // Next to our own executable first, so a bundled copy wins.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("ViGEmClient.dll");
            if p.exists() {
                match unsafe { libloading::Library::new(&p) } {
                    Ok(l) => return Ok(l),
                    Err(e) => errs.push(format!("{}: {e}", p.display())),
                }
            }
        }
    }
    for n in ["ViGEmClient.dll", "vigemclient.dll", "ViGEmClient64.dll"] {
        match unsafe { libloading::Library::new(n) } {
            Ok(l) => return Ok(l),
            Err(e) => errs.push(format!("{n}: {e}")),
        }
    }
    anyhow::bail!("{}", errs.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_scancodes_are_recognised() {
        // Arrow keys and friends arrive with the 0xE0 prefix.
        assert!(is_extended_scancode(0xE04B), "left arrow");
        assert!(is_extended_scancode(0xE048), "up arrow");
        assert!(is_extended_scancode(0xE053), "delete");
        // Or with just the high bit set, depending on the sender.
        assert!(is_extended_scancode(0x014B));
        // Ordinary letter and function keys are not extended.
        assert!(!is_extended_scancode(0x11), "W");
        assert!(!is_extended_scancode(0x1E), "A");
        assert!(!is_extended_scancode(0x3B), "F1");
        assert!(!is_extended_scancode(0x00));
    }

    #[test]
    fn injector_tracks_and_clears_held_input() {
        // On non-Windows the injection calls are no-ops, but the bookkeeping
        // that prevents stuck keys is platform-independent and worth testing.
        let inj = InputInjector::new(false);
        assert_eq!(inj.held_counts(), (0, 0));

        inj.apply(&[
            InputEvent::Key {
                scancode: 0x11,
                vk: 0x57,
                down: true,
            },
            InputEvent::Key {
                scancode: 0x1E,
                vk: 0x41,
                down: true,
            },
            InputEvent::MouseButton {
                button: 0,
                down: true,
            },
        ]);
        assert_eq!(inj.held_counts(), (2, 1));

        // A matching key-up removes it from the held set.
        inj.apply(&[InputEvent::Key {
            scancode: 0x11,
            vk: 0x57,
            down: false,
        }]);
        assert_eq!(inj.held_counts(), (1, 1));

        // Repeated presses of the same key do not accumulate.
        inj.apply(&[
            InputEvent::Key {
                scancode: 0x1E,
                vk: 0x41,
                down: true,
            },
            InputEvent::Key {
                scancode: 0x1E,
                vk: 0x41,
                down: true,
            },
        ]);
        assert_eq!(inj.held_counts(), (1, 1));

        inj.release_all();
        assert_eq!(
            inj.held_counts(),
            (0, 0),
            "release_all must clear everything"
        );
        // And it is safe to call again.
        inj.release_all();
        assert_eq!(inj.held_counts(), (0, 0));
    }

    #[test]
    fn moves_and_wheels_are_not_tracked_as_held() {
        let inj = InputInjector::new(false);
        inj.apply(&[
            InputEvent::MouseMoveRel { dx: 5, dy: -5 },
            InputEvent::MouseMoveAbs { x: 100, y: 200 },
            InputEvent::MouseWheel { dx: 0, dy: 1 },
        ]);
        assert_eq!(inj.held_counts(), (0, 0));
    }
}
