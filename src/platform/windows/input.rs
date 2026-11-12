//! Mouse and keyboard injection through `SendInput`.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

fn send(input: INPUT) {
    send_all(&[input]);
}

/// Inject `inputs`. Windows refuses while this thread is on another desktop
/// than the one taking input (a UAC prompt or the lock screen came up or went
/// away); then move there and try again. (Only peek running as SYSTEM can.)
fn send_all(inputs: &[INPUT]) {
    let size = size_of::<INPUT>() as i32;
    let sent = unsafe { SendInput(inputs, size) } as usize;
    if sent < inputs.len() && super::desktop::follow().is_some() {
        unsafe { SendInput(&inputs[sent..], size) };
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) {
    send(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    });
}

/// Move the pointer to an absolute pixel on the virtual desktop.
pub fn mouse_move(x: i32, y: i32) {
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let norm = |p: i32, origin: i32, size: i32| {
        ((p - origin) as f64 * 65535.0 / (size - 1).max(1) as f64).round() as i32
    };
    mouse(
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        norm(x, vx, vw),
        norm(y, vy, vh),
        0,
    );
}

/// `button` uses the browser's numbering: 0 left, 1 middle, 2 right,
/// 3 back, 4 forward. Returns false for anything else.
pub fn button(button: u8, down: bool) -> bool {
    let (down_flag, up_flag, data) = match button {
        0 => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        1 => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        2 => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        3 => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
        4 => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
        _ => return false,
    };
    mouse(if down { down_flag } else { up_flag }, 0, 0, data);
    true
}

/// Wheel movement in Windows units (120 per notch); positive `dy` scrolls up,
/// positive `dx` scrolls right.
pub fn wheel(dx: i32, dy: i32) {
    if dy != 0 {
        mouse(MOUSEEVENTF_WHEEL, 0, 0, dy);
    }
    if dx != 0 {
        mouse(MOUSEEVENTF_HWHEEL, 0, 0, dx);
    }
}

/// How a `KeyboardEvent.code` is injected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Set-1 scancode, optionally with the E0 prefix. Physical keys map to
    /// physical keys, so the host's layout decides what character comes out.
    Scan(u16, bool),
    /// Keys with no single-scancode form go by virtual-key code.
    Virtual(u16),
}

pub fn lookup(code: &str) -> Option<Key> {
    use Key::{Scan, Virtual};
    Some(match code {
        "Escape" => Scan(0x01, false),
        "Digit1" => Scan(0x02, false),
        "Digit2" => Scan(0x03, false),
        "Digit3" => Scan(0x04, false),
        "Digit4" => Scan(0x05, false),
        "Digit5" => Scan(0x06, false),
        "Digit6" => Scan(0x07, false),
        "Digit7" => Scan(0x08, false),
        "Digit8" => Scan(0x09, false),
        "Digit9" => Scan(0x0A, false),
        "Digit0" => Scan(0x0B, false),
        "Minus" => Scan(0x0C, false),
        "Equal" => Scan(0x0D, false),
        "Backspace" => Scan(0x0E, false),
        "Tab" => Scan(0x0F, false),
        "KeyQ" => Scan(0x10, false),
        "KeyW" => Scan(0x11, false),
        "KeyE" => Scan(0x12, false),
        "KeyR" => Scan(0x13, false),
        "KeyT" => Scan(0x14, false),
        "KeyY" => Scan(0x15, false),
        "KeyU" => Scan(0x16, false),
        "KeyI" => Scan(0x17, false),
        "KeyO" => Scan(0x18, false),
        "KeyP" => Scan(0x19, false),
        "BracketLeft" => Scan(0x1A, false),
        "BracketRight" => Scan(0x1B, false),
        "Enter" => Scan(0x1C, false),
        "ControlLeft" => Scan(0x1D, false),
        "KeyA" => Scan(0x1E, false),
        "KeyS" => Scan(0x1F, false),
        "KeyD" => Scan(0x20, false),
        "KeyF" => Scan(0x21, false),
        "KeyG" => Scan(0x22, false),
        "KeyH" => Scan(0x23, false),
        "KeyJ" => Scan(0x24, false),
        "KeyK" => Scan(0x25, false),
        "KeyL" => Scan(0x26, false),
        "Semicolon" => Scan(0x27, false),
        "Quote" => Scan(0x28, false),
        "Backquote" => Scan(0x29, false),
        "ShiftLeft" => Scan(0x2A, false),
        "Backslash" => Scan(0x2B, false),
        "KeyZ" => Scan(0x2C, false),
        "KeyX" => Scan(0x2D, false),
        "KeyC" => Scan(0x2E, false),
        "KeyV" => Scan(0x2F, false),
        "KeyB" => Scan(0x30, false),
        "KeyN" => Scan(0x31, false),
        "KeyM" => Scan(0x32, false),
        "Comma" => Scan(0x33, false),
        "Period" => Scan(0x34, false),
        "Slash" => Scan(0x35, false),
        "ShiftRight" => Scan(0x36, false),
        "NumpadMultiply" => Scan(0x37, false),
        "AltLeft" => Scan(0x38, false),
        "Space" => Scan(0x39, false),
        "CapsLock" => Scan(0x3A, false),
        "F1" => Scan(0x3B, false),
        "F2" => Scan(0x3C, false),
        "F3" => Scan(0x3D, false),
        "F4" => Scan(0x3E, false),
        "F5" => Scan(0x3F, false),
        "F6" => Scan(0x40, false),
        "F7" => Scan(0x41, false),
        "F8" => Scan(0x42, false),
        "F9" => Scan(0x43, false),
        "F10" => Scan(0x44, false),
        "NumLock" => Scan(0x45, false),
        "ScrollLock" => Scan(0x46, false),
        "Numpad7" => Scan(0x47, false),
        "Numpad8" => Scan(0x48, false),
        "Numpad9" => Scan(0x49, false),
        "NumpadSubtract" => Scan(0x4A, false),
        "Numpad4" => Scan(0x4B, false),
        "Numpad5" => Scan(0x4C, false),
        "Numpad6" => Scan(0x4D, false),
        "NumpadAdd" => Scan(0x4E, false),
        "Numpad1" => Scan(0x4F, false),
        "Numpad2" => Scan(0x50, false),
        "Numpad3" => Scan(0x51, false),
        "Numpad0" => Scan(0x52, false),
        "NumpadDecimal" => Scan(0x53, false),
        "IntlBackslash" => Scan(0x56, false),
        "F11" => Scan(0x57, false),
        "F12" => Scan(0x58, false),
        "IntlRo" => Scan(0x73, false),
        "IntlYen" => Scan(0x7D, false),
        "NumpadEnter" => Scan(0x1C, true),
        "ControlRight" => Scan(0x1D, true),
        "NumpadDivide" => Scan(0x35, true),
        "PrintScreen" => Scan(0x37, true),
        "AltRight" => Scan(0x38, true),
        "Home" => Scan(0x47, true),
        "ArrowUp" => Scan(0x48, true),
        "PageUp" => Scan(0x49, true),
        "ArrowLeft" => Scan(0x4B, true),
        "ArrowRight" => Scan(0x4D, true),
        "End" => Scan(0x4F, true),
        "ArrowDown" => Scan(0x50, true),
        "PageDown" => Scan(0x51, true),
        "Insert" => Scan(0x52, true),
        "Delete" => Scan(0x53, true),
        "MetaLeft" => Scan(0x5B, true),
        "MetaRight" => Scan(0x5C, true),
        "ContextMenu" => Scan(0x5D, true),
        "Pause" => Virtual(0x13),
        _ => return None,
    })
}

/// Type text as characters, whatever the host's keyboard layout: each UTF-16
/// unit goes in as a Unicode key press. Line breaks and tabs are pressed as
/// the Enter and Tab keys instead, which is what applications expect.
pub fn text(s: &str) {
    let unicode = |unit: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let mut batch = Vec::new();
    let flush = |batch: &mut Vec<INPUT>| {
        if !batch.is_empty() {
            send_all(batch);
            batch.clear();
        }
    };
    for ch in s.replace("\r\n", "\n").chars() {
        match ch {
            '\n' | '\r' | '\t' => {
                flush(&mut batch);
                let code = if ch == '\t' { "Tab" } else { "Enter" };
                key(code, true);
                key(code, false);
            }
            _ => {
                let mut units = [0u16; 2];
                for &unit in ch.encode_utf16(&mut units).iter() {
                    batch.push(unicode(unit, false));
                    batch.push(unicode(unit, true));
                }
            }
        }
    }
    flush(&mut batch);
}

/// Press or release the key named by a `KeyboardEvent.code`. Returns false
/// for codes that have no mapping.
pub fn key(code: &str, down: bool) -> bool {
    let Some(key) = lookup(code) else { return false };
    let up = if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP };
    let ki = match key {
        Key::Scan(scan, extended) => KEYBDINPUT {
            wVk: VIRTUAL_KEY(0),
            wScan: scan,
            dwFlags: KEYEVENTF_SCANCODE
                | up
                | if extended { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) },
            time: 0,
            dwExtraInfo: 0,
        },
        Key::Virtual(vk) => {
            KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: 0, dwFlags: up, time: 0, dwExtraInfo: 0 }
        }
    };
    send(INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } });
    true
}
