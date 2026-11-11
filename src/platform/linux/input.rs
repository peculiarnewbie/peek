//! Mouse and keyboard injection.
//!
//! - The pointer is a wlr virtual pointer. There is one per monitor, each
//!   bound to its output, so an absolute position is simply a fraction of
//!   that monitor whatever its scale.
//! - Keys go through a uinput keyboard: the compositor treats it like any
//!   other keyboard, so its layout decides what is typed and its own
//!   shortcuts work.
//! - Text goes through a Wayland virtual keyboard whose keymap holds exactly
//!   the characters being typed, so any character can be typed whatever the
//!   layout. Afterwards the seat's own keymap is put back: some compositors
//!   (niri up to 26.04) leave the last virtual keymap active for programs,
//!   which would garble the next key from a real keyboard.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_keyboard::{self, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_pointer::{Axis, AxisSource, ButtonState};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

/// Section name marking the keymaps made for typing text, so they are not
/// mistaken for the seat's own when the compositor echoes them back.
const TEXT_KEYMAP: &str = "peek-text";
/// Characters per text keymap: keycodes 9 to 255 are usable.
const TEXT_CHUNK: usize = 240;

#[derive(Default)]
struct State {
    globals: Vec<(u32, String, u32)>,
    /// The seat's keymap as last announced (not one of ours).
    seat_keymap: Option<Vec<u8>>,
    /// The last keymap the compositor announced was one of ours.
    #[cfg(test)]
    last_was_text: bool,
}

struct Inner {
    queue: EventQueue<State>,
    state: State,
    seat: Option<WlSeat>,
    pointer_manager: Option<ZwlrVirtualPointerManagerV1>,
    pointers: HashMap<ObjectId, ZwlrVirtualPointerV1>,
    layout_generation: u64,
    /// The pointer last moved; buttons and the wheel go through it.
    current: Option<ZwlrVirtualPointerV1>,
    keyboard: Option<ZwpVirtualKeyboardV1>,
    uinput: Result<Uinput, String>,
    start: Instant,
    /// Wheel movement not yet amounting to a whole notch, per axis.
    wheel_rest: [i32; 2],
}

static INPUT: OnceLock<Result<Mutex<Inner>, String>> = OnceLock::new();

fn inner() -> Result<std::sync::MutexGuard<'static, Inner>, &'static String> {
    INPUT.get_or_init(|| setup().map(Mutex::new)).as_ref().map(|m| m.lock().unwrap())
}

fn setup() -> Result<Inner, String> {
    let conn = super::connection()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    let find = |name: &str| state.globals.iter().find(|g| g.1 == name).map(|g| (g.0, g.2));

    let seat: Option<WlSeat> = find("wl_seat").map(|(n, v)| registry.bind(n, v.min(7), &qh, ()));
    let pointer_manager: Option<ZwlrVirtualPointerManagerV1> = find("zwlr_virtual_pointer_manager_v1")
        .map(|(n, v)| registry.bind(n, v.min(2), &qh, ()));
    let keyboard = match (&seat, find("zwp_virtual_keyboard_manager_v1")) {
        (Some(seat), Some((n, v))) => {
            let manager: ZwpVirtualKeyboardManagerV1 = registry.bind(n, v.min(1), &qh, ());
            // Its keymap events tell us the seat's own layout.
            seat.get_keyboard(&qh, ());
            Some(manager.create_virtual_keyboard(seat, &qh, ()))
        }
        _ => None,
    };
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;

    Ok(Inner {
        queue,
        state,
        seat,
        pointer_manager,
        pointers: HashMap::new(),
        layout_generation: 0,
        current: None,
        keyboard,
        uinput: Uinput::open().map_err(|e| e.to_string()),
        start: Instant::now(),
        wheel_rest: [0, 0],
    })
}

pub(super) fn init() {
    if let Err(e) = inner() {
        crate::log!("remote input unavailable: {e}");
    }
}

pub(super) fn notes() -> Vec<String> {
    let Ok(i) = inner() else {
        return vec!["Remote input is unavailable: no connection to the compositor.".into()];
    };
    let mut notes = Vec::new();
    if let Err(e) = &i.uinput {
        notes.push(format!(
            "Keys cannot be sent: /dev/uinput: {e}. Typed text still works. See the README (Linux, uinput)."
        ));
    }
    if i.pointer_manager.is_none() {
        notes.push("This compositor has no virtual pointer (wlr-virtual-pointer): the mouse will not work.".into());
    }
    if i.keyboard.is_none() {
        notes.push("This compositor has no virtual keyboard: text sent from a phone will not be typed.".into());
    }
    notes
}

impl Inner {
    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn flush(&self) {
        let _ = self.queue.flush();
    }

    /// The virtual pointer bound to `output`, made on first use.
    fn pointer_for(&mut self, output: &wayland_client::protocol::wl_output::WlOutput) -> Option<ZwlrVirtualPointerV1> {
        let manager = self.pointer_manager.as_ref()?;
        if let Some(p) = self.pointers.get(&output.id()) {
            return Some(p.clone());
        }
        let qh = self.queue.handle();
        let p = if manager.version() >= 2 {
            manager.create_virtual_pointer_with_output(self.seat.as_ref(), Some(output), &qh, ())
        } else {
            manager.create_virtual_pointer(self.seat.as_ref(), &qh, ())
        };
        self.pointers.insert(output.id(), p.clone());
        Some(p)
    }

    /// The pointer to send buttons and wheel through.
    fn pointer(&mut self) -> Option<ZwlrVirtualPointerV1> {
        if self.current.is_none() {
            let output = super::LAYOUT.read().unwrap().screens.first().map(|(_, o)| o.clone())?;
            self.current = self.pointer_for(&output);
        }
        self.current.clone()
    }
}

/// Move the pointer to `(x, y)` in the layout capture published (monitors
/// side by side, physical pixels).
pub fn mouse_move(x: i32, y: i32) {
    let Ok(mut i) = inner() else { return };
    let (generation, target) = {
        let layout = super::LAYOUT.read().unwrap();
        let target = layout
            .screens
            .iter()
            .find(|(m, _)| x >= m.x && y >= m.y && x < m.x + m.width as i32 && y < m.y + m.height as i32)
            .map(|(m, o)| (*m, o.clone()));
        (layout.generation, target)
    };
    if generation != i.layout_generation {
        for (_, p) in i.pointers.drain() {
            p.destroy();
        }
        i.current = None;
        i.layout_generation = generation;
    }
    let Some((m, output)) = target else { return };
    let Some(p) = i.pointer_for(&output) else { return };
    let time = i.time();
    p.motion_absolute(time, (x - m.x) as u32, (y - m.y) as u32, m.width, m.height);
    p.frame();
    i.current = Some(p);
    i.flush();
}

/// `button` uses the browser's numbering: 0 left, 1 middle, 2 right,
/// 3 back, 4 forward. Returns false for anything else.
pub fn button(button: u8, down: bool) -> bool {
    const BTN_LEFT: u32 = 0x110;
    let code = match button {
        0 => BTN_LEFT,
        1 => BTN_LEFT + 2, // BTN_MIDDLE
        2 => BTN_LEFT + 1, // BTN_RIGHT
        3 => BTN_LEFT + 3, // BTN_SIDE
        4 => BTN_LEFT + 4, // BTN_EXTRA
        _ => return false,
    };
    let Ok(mut i) = inner() else { return true };
    let Some(p) = i.pointer() else { return true };
    let state = if down { ButtonState::Pressed } else { ButtonState::Released };
    p.button(i.time(), code, state);
    p.frame();
    i.flush();
    true
}

/// Wheel movement in Windows units (120 per notch); positive `dy` scrolls up,
/// positive `dx` scrolls right.
pub fn wheel(dx: i32, dy: i32) {
    let Ok(mut i) = inner() else { return };
    let Some(p) = i.pointer() else { return };
    let time = i.time();
    p.axis_source(AxisSource::Wheel);
    // Wayland scrolls down for positive vertical values.
    for (n, (axis, amount)) in [(Axis::HorizontalScroll, dx), (Axis::VerticalScroll, -dy)].into_iter().enumerate() {
        if amount == 0 {
            continue;
        }
        // 15 units a notch is what libinput reports for a mouse wheel.
        let value = amount as f64 * 15.0 / 120.0;
        // Programs that count notches see one each time a whole one adds up.
        i.wheel_rest[n] += amount;
        let notches = i.wheel_rest[n] / 120;
        i.wheel_rest[n] -= notches * 120;
        if notches != 0 {
            p.axis_discrete(time, axis, value, notches);
        } else {
            p.axis(time, axis, value);
        }
    }
    p.frame();
    i.flush();
}

/// `KeyboardEvent.code` to Linux key code (input-event-codes.h). Physical
/// keys map to physical keys, so the host's layout decides what comes out.
const KEYS: &[(&str, u16)] = &[
    ("Escape", 1), ("Digit1", 2), ("Digit2", 3), ("Digit3", 4), ("Digit4", 5), ("Digit5", 6),
    ("Digit6", 7), ("Digit7", 8), ("Digit8", 9), ("Digit9", 10), ("Digit0", 11), ("Minus", 12),
    ("Equal", 13), ("Backspace", 14), ("Tab", 15), ("KeyQ", 16), ("KeyW", 17), ("KeyE", 18),
    ("KeyR", 19), ("KeyT", 20), ("KeyY", 21), ("KeyU", 22), ("KeyI", 23), ("KeyO", 24),
    ("KeyP", 25), ("BracketLeft", 26), ("BracketRight", 27), ("Enter", 28), ("ControlLeft", 29),
    ("KeyA", 30), ("KeyS", 31), ("KeyD", 32), ("KeyF", 33), ("KeyG", 34), ("KeyH", 35),
    ("KeyJ", 36), ("KeyK", 37), ("KeyL", 38), ("Semicolon", 39), ("Quote", 40), ("Backquote", 41),
    ("ShiftLeft", 42), ("Backslash", 43), ("KeyZ", 44), ("KeyX", 45), ("KeyC", 46), ("KeyV", 47),
    ("KeyB", 48), ("KeyN", 49), ("KeyM", 50), ("Comma", 51), ("Period", 52), ("Slash", 53),
    ("ShiftRight", 54), ("NumpadMultiply", 55), ("AltLeft", 56), ("Space", 57), ("CapsLock", 58),
    ("F1", 59), ("F2", 60), ("F3", 61), ("F4", 62), ("F5", 63), ("F6", 64), ("F7", 65),
    ("F8", 66), ("F9", 67), ("F10", 68), ("NumLock", 69), ("ScrollLock", 70), ("Numpad7", 71),
    ("Numpad8", 72), ("Numpad9", 73), ("NumpadSubtract", 74), ("Numpad4", 75), ("Numpad5", 76),
    ("Numpad6", 77), ("NumpadAdd", 78), ("Numpad1", 79), ("Numpad2", 80), ("Numpad3", 81),
    ("Numpad0", 82), ("NumpadDecimal", 83), ("IntlBackslash", 86), ("F11", 87), ("F12", 88),
    ("IntlRo", 89), ("NumpadEnter", 96), ("ControlRight", 97), ("NumpadDivide", 98),
    ("PrintScreen", 99), ("AltRight", 100), ("Home", 102), ("ArrowUp", 103), ("PageUp", 104),
    ("ArrowLeft", 105), ("ArrowRight", 106), ("End", 107), ("ArrowDown", 108), ("PageDown", 109),
    ("Insert", 110), ("Delete", 111), ("Pause", 119), ("IntlYen", 124), ("MetaLeft", 125),
    ("MetaRight", 126), ("ContextMenu", 127),
];

pub fn lookup(code: &str) -> Option<u16> {
    KEYS.iter().find(|(name, _)| *name == code).map(|&(_, key)| key)
}

/// Press or release the key named by a `KeyboardEvent.code`. Returns false
/// for codes that have no mapping.
pub fn key(code: &str, down: bool) -> bool {
    let Some(key) = lookup(code) else { return false };
    if let Ok(i) = inner() {
        if let Ok(u) = &i.uinput {
            u.key(key, down);
        }
    }
    true
}

/// Type text as characters, whatever the host's keyboard layout. Line
/// breaks and tabs are pressed as the Return and Tab keys.
pub fn text(s: &str) {
    let Ok(mut i) = inner() else { return };
    let Some(keyboard) = i.keyboard.clone() else { return };
    // Pick up any change to the seat's layout first.
    let Inner { queue, state, .. } = &mut *i;
    if let Err(e) = queue.roundtrip(state) {
        crate::log!("cannot type text: {e}");
        return;
    }
    let Some(seat_keymap) = i.state.seat_keymap.clone() else {
        crate::log!("cannot type text: the compositor has not told us the keyboard layout");
        return;
    };
    let chars: Vec<char> = s
        .replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect();
    let mut rest = &chars[..];
    while !rest.is_empty() {
        // The longest run that fits one keymap.
        let mut keys: Vec<char> = Vec::new();
        let mut len = 0;
        for &c in rest {
            if !keys.contains(&c) {
                if keys.len() == TEXT_CHUNK {
                    break;
                }
                keys.push(c);
            }
            len += 1;
        }
        let Some(fd) = keymap_fd(text_keymap(&keys).as_bytes()) else { return };
        keyboard.keymap(KeymapFormat::XkbV1.into(), fd.0.as_fd(), fd.1);
        for &c in &rest[..len] {
            let code = keys.iter().position(|&k| k == c).unwrap() as u32 + 1; // xkb keycode - 8
            let time = i.time();
            keyboard.key(time, code, wl_keyboard::KeyState::Pressed.into());
            keyboard.key(time, code, wl_keyboard::KeyState::Released.into());
        }
        rest = &rest[len..];
    }
    // Put the seat's own layout back for programs; the modifiers request
    // makes the compositor send it straight away.
    if let Some(fd) = keymap_fd(&seat_keymap) {
        keyboard.keymap(KeymapFormat::XkbV1.into(), fd.0.as_fd(), fd.1);
        keyboard.modifiers(0, 0, 0, 0);
    }
    i.flush();
}

/// A keymap with one key for each of `chars`, from keycode 9 up.
fn text_keymap(chars: &[char]) -> String {
    let mut s = format!("xkb_keymap {{\nxkb_keycodes \"{TEXT_KEYMAP}\" {{\nminimum = 8;\nmaximum = 255;\n");
    for n in 0..chars.len() {
        let _ = writeln!(s, "<K{n}> = {};", n + 9);
    }
    let _ = write!(
        s,
        "}};\nxkb_types \"{TEXT_KEYMAP}\" {{ include \"complete\" }};\n\
         xkb_compatibility \"{TEXT_KEYMAP}\" {{ include \"complete\" }};\n\
         xkb_symbols \"{TEXT_KEYMAP}\" {{\n"
    );
    for (n, &c) in chars.iter().enumerate() {
        let sym = match c {
            '\n' => "Return".to_string(),
            '\t' => "Tab".to_string(),
            c => format!("U{:04X}", c as u32),
        };
        let _ = writeln!(s, "key <K{n}> {{ [ {sym} ] }};");
    }
    s.push_str("};\n};\n");
    s
}

/// A keymap in a file descriptor, as the protocol wants it: NUL-terminated.
fn keymap_fd(keymap: &[u8]) -> Option<(OwnedFd, u32)> {
    let trimmed = keymap.strip_suffix(&[0]).unwrap_or(keymap);
    let len = trimmed.len() + 1;
    let fd = super::memfd("peek-keymap", len).ok()?;
    let mut file = File::from(fd);
    file.write_all(trimmed).ok()?;
    file.write_all(&[0]).ok()?;
    Some((file.into(), len as u32))
}

/// A virtual keyboard made through the kernel's uinput.
struct Uinput(File);

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

// ioctl numbers from linux/uinput.h.
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;

impl Uinput {
    fn open() -> std::io::Result<Self> {
        let file = OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open("/dev/uinput")?;
        let fd = file.as_raw_fd();
        let check = |r: libc::c_int| if r < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) };
        unsafe {
            check(libc::ioctl(fd, UI_SET_EVBIT as _, EV_KEY as libc::c_int))?;
            for &(_, key) in KEYS {
                check(libc::ioctl(fd, UI_SET_KEYBIT as _, key as libc::c_int))?;
            }
            let mut setup = UinputSetup {
                id: InputId { bustype: 0x06 /* BUS_VIRTUAL */, vendor: 0x7065, product: 0x656b, version: 1 },
                name: [0; 80],
                ff_effects_max: 0,
            };
            setup.name[..14].copy_from_slice(b"peek keyboard\0");
            check(libc::ioctl(fd, UI_DEV_SETUP as _, &setup))?;
            check(libc::ioctl(fd, UI_DEV_CREATE as _))?;
        }
        Ok(Uinput(file))
    }

    fn emit(&self, kind: u16, code: u16, value: i32) {
        let event = libc::input_event {
            time: libc::timeval { tv_sec: 0, tv_usec: 0 },
            type_: kind,
            code,
            value,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts((&event as *const libc::input_event).cast::<u8>(), size_of::<libc::input_event>())
        };
        let _ = (&self.0).write_all(bytes);
    }

    fn key(&self, key: u16, down: bool) {
        self.emit(EV_KEY, key, down as i32);
        self.emit(EV_SYN, 0, 0);
    }
}

impl Drop for Uinput {
    fn drop(&mut self) {
        unsafe { libc::ioctl(self.0.as_raw_fd(), UI_DEV_DESTROY as _) };
    }
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(state: &mut Self, _: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global { name, interface, version } => state.globals.push((name, interface, version)),
            wl_registry::Event::GlobalRemove { name } => state.globals.retain(|g| g.0 != name),
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(state: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            if format != WEnum::Value(KeymapFormat::XkbV1) {
                return;
            }
            let Some(keymap) = read_keymap(fd, size as usize) else { return };
            let ours = keymap.windows(TEXT_KEYMAP.len()).any(|w| w == TEXT_KEYMAP.as_bytes());
            #[cfg(test)]
            {
                state.last_was_text = ours;
            }
            if !ours {
                state.seat_keymap = Some(keymap);
            }
        }
    }
}

fn read_keymap(fd: OwnedFd, size: usize) -> Option<Vec<u8>> {
    let map = unsafe {
        libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ, libc::MAP_PRIVATE, fd.as_raw_fd(), 0)
    };
    if map == libc::MAP_FAILED {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(map.cast::<u8>(), size) }.to_vec();
    unsafe { libc::munmap(map, size) };
    Some(bytes)
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlSeat, ZwlrVirtualPointerManagerV1, ZwlrVirtualPointerV1, ZwpVirtualKeyboardManagerV1, ZwpVirtualKeyboardV1);

#[cfg(test)]
pub(super) fn keymap_restored() -> bool {
    let mut i = inner().unwrap();
    let Inner { queue, state, .. } = &mut *i;
    queue.roundtrip(state).unwrap();
    state.seat_keymap.is_some() && !state.last_was_text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_keymap_has_a_key_per_character() {
        let km = text_keymap(&['a', '€', '\n']);
        assert!(km.contains("<K0> = 9;"));
        assert!(km.contains("key <K1> { [ U20AC ] };"));
        assert!(km.contains("key <K2> { [ Return ] };"));
        assert!(km.contains(TEXT_KEYMAP));
    }

    #[test]
    fn every_mapped_key_has_a_distinct_code() {
        let mut codes: Vec<u16> = KEYS.iter().map(|k| k.1).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), KEYS.len());
    }
}
