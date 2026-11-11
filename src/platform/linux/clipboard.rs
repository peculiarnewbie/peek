//! Clipboard text through wlr-data-control, which lets a program without a
//! window read and set the clipboard. A thread of its own keeps up with
//! selection changes and serves the text we set.

use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::{
    self, ZwlrDataControlDeviceV1,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::{
    self, ZwlrDataControlOfferV1,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_source_v1::{
    self, ZwlrDataControlSourceV1,
};

/// Text types, most preferred first.
const TEXT_TYPES: &[&str] = &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];
/// How long a program may take to hand over the clipboard.
const READ_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_READ: usize = 16 << 20;

type Mimes = Arc<Mutex<Vec<String>>>;

struct Shared {
    sequence: AtomicU64,
    /// The current selection, with the types it is offered in.
    offer: Mutex<Option<(ZwlrDataControlOfferV1, Mimes)>>,
    device: ZwlrDataControlDeviceV1,
    manager: ZwlrDataControlManagerV1,
    qh: QueueHandle<State>,
}

#[derive(Default)]
struct State {
    globals: Vec<(u32, String, u32)>,
    shared: Option<Arc<Shared>>,
}

fn shared() -> Option<&'static Arc<Shared>> {
    static SHARED: OnceLock<Option<Arc<Shared>>> = OnceLock::new();
    SHARED
        .get_or_init(|| match start() {
            Ok(s) => Some(s),
            Err(e) => {
                crate::log!("clipboard unavailable: {e}");
                None
            }
        })
        .as_ref()
}

fn start() -> Result<Arc<Shared>, String> {
    let conn = super::connection()?;
    let mut queue: EventQueue<State> = conn.new_event_queue();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    let find = |name: &str| state.globals.iter().find(|g| g.1 == name).map(|g| (g.0, g.2));
    let (seat_name, seat_version) = find("wl_seat").ok_or("no seat")?;
    let (manager_name, manager_version) =
        find("zwlr_data_control_manager_v1").ok_or("this compositor does not offer wlr-data-control")?;
    let seat: WlSeat = registry.bind(seat_name, seat_version.min(7), &qh, ());
    let manager: ZwlrDataControlManagerV1 = registry.bind(manager_name, manager_version.min(2), &qh, ());
    let device = manager.get_data_device(&seat, &qh, ());
    let shared = Arc::new(Shared {
        sequence: AtomicU64::new(0),
        offer: Mutex::new(None),
        device,
        manager,
        qh,
    });
    state.shared = Some(shared.clone());
    // The current selection arrives straight away.
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    std::thread::Builder::new()
        .name("wayland-clipboard".into())
        .spawn(move || {
            loop {
                if let Err(e) = super::dispatch_timeout(&mut queue, &mut state, Duration::from_secs(3600)) {
                    crate::log!("clipboard stopped: {e}");
                    return;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(shared)
}

/// A number that changes every time the clipboard's content does.
pub fn sequence() -> u64 {
    shared().map_or(0, |s| s.sequence.load(Ordering::Relaxed))
}

/// The clipboard as text, or `None` when it holds none (or did not answer).
pub fn get_text() -> Option<String> {
    let s = shared()?;
    let (offer, mimes) = s.offer.lock().unwrap().clone()?;
    let mime = {
        let mimes = mimes.lock().unwrap();
        TEXT_TYPES.iter().find(|t| mimes.iter().any(|m| m == *t))?.to_string()
    };
    let (read, write) = super::pipe().ok()?;
    offer.receive(mime, write.as_fd());
    flush();
    drop(write);
    let bytes = read_all(read)?;
    let text = String::from_utf8(bytes).ok()?;
    (!text.is_empty()).then_some(text)
}

pub fn set_text(text: &str) -> bool {
    let Some(s) = shared() else { return false };
    let source = s.manager.create_data_source(&s.qh, Arc::new(text.to_string()));
    for mime in TEXT_TYPES {
        source.offer(mime.to_string());
    }
    s.device.set_selection(Some(&source));
    flush();
    true
}

fn flush() {
    if let Ok(conn) = super::connection() {
        let _ = conn.flush();
    }
}

/// Everything from a pipe, giving up after `READ_TIMEOUT`.
fn read_all(fd: OwnedFd) -> Option<Vec<u8>> {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut file = std::fs::File::from(fd);
    let mut out = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !super::wait_readable(file.as_fd(), left) {
            return None;
        }
        match file.read(&mut chunk) {
            Ok(0) => return Some(out),
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.len() > MAX_READ {
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
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

impl Dispatch<ZwlrDataControlDeviceV1, ()> for State {
    fn event(state: &mut Self, _: &ZwlrDataControlDeviceV1, event: zwlr_data_control_device_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(shared) = &state.shared else { return };
        match event {
            zwlr_data_control_device_v1::Event::Selection { id } => {
                let mut current = shared.offer.lock().unwrap();
                if let Some((old, _)) = current.take() {
                    old.destroy();
                }
                *current = id.map(|offer| {
                    let mimes = offer.data::<Mimes>().cloned().unwrap_or_default();
                    (offer, mimes)
                });
                shared.sequence.fetch_add(1, Ordering::Relaxed);
            }
            zwlr_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
            zwlr_data_control_device_v1::Event::Finished => {
                crate::log!("clipboard access was withdrawn by the compositor");
            }
            _ => {}
        }
    }

    event_created_child!(State, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, Mimes::default()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, Mimes> for State {
    fn event(_: &mut Self, _: &ZwlrDataControlOfferV1, event: zwlr_data_control_offer_v1::Event, mimes: &Mimes, _: &Connection, _: &QueueHandle<Self>) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            mimes.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, Arc<String>> for State {
    fn event(_: &mut Self, source: &ZwlrDataControlSourceV1, event: zwlr_data_control_source_v1::Event, text: &Arc<String>, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_data_control_source_v1::Event::Send { fd, .. } => {
                // A slow reader must not hold up this thread.
                let text = text.clone();
                std::thread::spawn(move || {
                    let _ = std::fs::File::from(fd).write_all(text.as_bytes());
                });
            }
            zwlr_data_control_source_v1::Event::Cancelled => source.destroy(),
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(_: &mut Self, _: &WlSeat, _: <WlSeat as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ZwlrDataControlManagerV1, _: <ZwlrDataControlManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
