//! The screen as one capability: the capture engine, injected input and the
//! clipboard, behind a small surface. Open a session, pass it what the
//! client says, close it.
//!
//! The server calls this in its own process. Where the server cannot see a
//! screen (a Windows service), a provider process holds it instead and the
//! server only forwards: see `provider`.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::Deserialize;

use crate::engine::{self, Cmd, Out, Quality, View, ViewChange};
use crate::platform::{self, Monitor, input};
use crate::server::{MAX_CLIPBOARD, MAX_CLIPBOARD_IMAGE};

/// The first byte of a binary message says what it is. From a client: an
/// image pasted there, as PNG, for the host's clipboard.
const BIN_CLIPBOARD_PNG: u8 = 1;
/// To a client: an image copied on the host, as PNG. (Frames are 1.)
const BIN_OUT_CLIPBOARD_PNG: u8 = 2;
/// How many times the watcher looks again at a clipboard change it could
/// not read: another program may still have the clipboard open.
const CLIP_READ_TRIES: u32 = 4;

/// Where a session's output goes: a socket's queue, or a provider's pipe.
pub type Sink = Arc<dyn Fn(Out) + Send + Sync>;

pub struct Shared {
    pub view_only: bool,
    pub clipboard: bool,
    pub verbose: bool,
    pub engine: Sender<Cmd>,
    pub monitors: RwLock<Vec<Monitor>>,
    /// Every open session, for clipboard broadcasts.
    pub peers: Mutex<HashMap<u64, Sink>>,
    /// The text most recently synchronised in either direction, so a change
    /// we caused ourselves is not echoed back.
    pub clip_last: Mutex<String>,
    /// The same for images, as a hash of the PNG.
    pub clip_last_image: Mutex<Option<u64>>,
    /// The sessions sent images copied here: those that asked, while their
    /// screen is showing. A browser can only take one into its clipboard
    /// while it has focus, and each is megabytes.
    pub images_to: Mutex<HashSet<u64>>,
}

/// Start the engine and the clipboard watcher. Both stay idle until a
/// session opens.
pub fn start(view_only: bool, clipboard: bool, verbose: bool, fps: u32) -> Arc<Shared> {
    let (engine_tx, engine_rx) = std::sync::mpsc::channel();
    let shared = Arc::new(Shared {
        view_only,
        clipboard,
        verbose,
        engine: engine_tx,
        monitors: RwLock::new(Vec::new()),
        peers: Mutex::new(HashMap::new()),
        clip_last: Mutex::new(String::new()),
        clip_last_image: Mutex::new(None),
        images_to: Mutex::default(),
    });
    {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || engine::run(engine_rx, shared, fps))
            .expect("spawn engine thread");
    }
    if clipboard {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || watch_clipboard(shared))
            .expect("spawn clipboard thread");
    }
    shared
}

/// What a client says to the screen. The built-in server handles `ping`;
/// provider-owned pages send their timing probe here. Pairing stays in the host.
#[derive(Deserialize)]
#[serde(tag = "t")]
enum ClientMsg {
    #[serde(rename = "ping")]
    Ping { n: u32 },
    #[serde(rename = "ack")]
    Ack { s: u32 },
    #[serde(rename = "vis")]
    Visible { v: bool },
    #[serde(rename = "cfg")]
    Configure {
        mon: Option<usize>,
        q: Option<String>,
        scale: Option<usize>,
        fps: Option<u32>,
        soft: Option<bool>,
        lossless: Option<bool>,
    },
    #[serde(rename = "m")]
    Move { x: f64, y: f64 },
    #[serde(rename = "b")]
    Button { b: u8, d: u8, x: f64, y: f64 },
    #[serde(rename = "w")]
    Wheel {
        #[serde(default)]
        dx: i32,
        #[serde(default)]
        dy: i32,
    },
    #[serde(rename = "k")]
    Key { c: String, d: u8 },
    #[serde(rename = "rel")]
    Release,
    /// Text to type as characters (from a phone's keyboard, which reports
    /// text rather than keys).
    #[serde(rename = "type")]
    Type { s: String },
    #[serde(rename = "clip")]
    Clipboard { s: String },
    /// Whether to send this client images copied on the host. Off until it
    /// says, as a client from before this does not know the message.
    #[serde(rename = "clipimg")]
    ClipboardImages { on: bool },
}

/// What one client is currently holding down, so it can be let go of.
#[derive(Default)]
struct Held {
    keys: HashSet<String>,
    buttons: HashSet<u8>,
}

impl Held {
    fn release_all(&mut self, inject: bool) {
        for code in self.keys.drain() {
            if inject {
                input::key(&code, false);
            }
        }
        for b in self.buttons.drain() {
            if inject {
                input::button(b, false);
            }
        }
    }
}

/// One client's view of the screen and hold on its input.
pub struct Session {
    id: u64,
    mon: usize,
    held: Held,
    /// The query that would open this session as it is now: the one it was
    /// opened with, brought up to date with what the client has asked for
    /// since.
    query: HashMap<String, String>,
    changed: bool,
}

fn scale(v: Option<usize>) -> Option<usize> {
    v.filter(|s| matches!(s, 1 | 2))
}

fn fps(v: Option<u32>) -> Option<u32> {
    v.map(|f| f.min(1000))
}

/// A switch, as the query spells it.
fn flag(on: bool) -> String {
    if on { "1" } else { "0" }.into()
}

/// Open a session from the query a client connected with.
pub fn open(shared: &Shared, id: u64, query: &HashMap<String, String>, sink: Sink) -> Session {
    fn param<T: std::str::FromStr>(query: &HashMap<String, String>, key: &str) -> Option<T> {
        query.get(key).and_then(|v| v.parse().ok())
    }
    let mon = param(query, "mon").unwrap_or(0usize);
    let _ = shared.engine.send(Cmd::Add {
        id,
        tx: sink.clone(),
        view: View {
            mon,
            quality: query.get("q").and_then(|q| Quality::parse(q)).unwrap_or(Quality::Med),
            scale: scale(param(query, "scale")).unwrap_or(1),
            fps: fps(param(query, "fps")).unwrap_or(0),
            soft: query.get("soft").is_some_and(|v| v == "1"),
            lossless: query.get("lossless").is_some_and(|v| v == "1"),
        },
        visible: query.get("vis").is_none_or(|v| v != "0"),
    });
    shared.peers.lock().unwrap().insert(id, sink);
    let session = Session { id, mon, held: Held::default(), query: query.clone(), changed: false };
    session.want_images(shared);
    session
}

impl Session {
    /// Whether this session is sent images copied on the host, as its
    /// query now says: asked for (`img`), and showing (`vis`).
    fn want_images(&self, shared: &Shared) {
        let on = self.query.get("img").is_some_and(|v| v == "1") && self.query.get("vis").is_none_or(|v| v != "0");
        let mut to = shared.images_to.lock().unwrap();
        if on {
            to.insert(self.id);
        } else {
            to.remove(&self.id);
        }
    }

    /// The query to open this session again as it is now, if that has
    /// changed since this was last asked.
    pub fn changed(&mut self) -> Option<&HashMap<String, String>> {
        std::mem::take(&mut self.changed).then_some(&self.query)
    }

    fn remember(&mut self, key: &str, value: String) {
        self.query.insert(key.into(), value);
        self.changed = true;
    }

    /// One message from the client, as it arrived.
    pub fn message(&mut self, shared: &Shared, text: &str) {
        let Ok(msg) = serde_json::from_str::<ClientMsg>(text) else {
            if shared.verbose {
                crate::log!("bad message {text}");
            }
            return;
        };
        let id = self.id;
        let inject = !shared.view_only;
        match msg {
            ClientMsg::Ping { n } => {
                if let Some(sink) = shared.peers.lock().unwrap().get(&self.id) {
                    sink(Out::Text(serde_json::json!({"t":"pong","n":n}).to_string()));
                }
            }
            ClientMsg::Ack { s } => {
                let _ = shared.engine.send(Cmd::Ack { id, seq: s });
            }
            ClientMsg::Visible { v } => {
                self.remember("vis", flag(v));
                self.want_images(shared);
                let _ = shared.engine.send(Cmd::Visible { id, visible: v });
            }
            ClientMsg::ClipboardImages { on } => {
                self.remember("img", flag(on));
                self.want_images(shared);
            }
            ClientMsg::Configure { mon: m, q, scale: s, fps: f, soft, lossless } => {
                self.mon = m.unwrap_or(self.mon);
                // As `open` reads them: only values it would accept.
                if let Some(m) = m {
                    self.remember("mon", m.to_string());
                }
                if let Some(q) = q.as_deref().filter(|q| Quality::parse(q).is_some()) {
                    self.remember("q", q.into());
                }
                if let Some(s) = scale(s) {
                    self.remember("scale", s.to_string());
                }
                if let Some(f) = fps(f) {
                    self.remember("fps", f.to_string());
                }
                if let Some(soft) = soft {
                    self.remember("soft", flag(soft));
                }
                if let Some(lossless) = lossless {
                    self.remember("lossless", flag(lossless));
                }
                let change = ViewChange {
                    mon: m,
                    quality: q.as_deref().and_then(Quality::parse),
                    scale: scale(s),
                    fps: fps(f),
                    soft,
                    lossless,
                };
                let _ = shared.engine.send(Cmd::Configure { id, change });
            }
            ClientMsg::Move { x, y } => {
                if let Some((px, py)) = to_pixel(shared, self.mon, x, y) {
                    if shared.verbose {
                        crate::log!("move {px},{py}");
                    }
                    if inject {
                        input::mouse_move(px, py);
                    }
                }
            }
            ClientMsg::Button { b, d, x, y } => {
                if b > 4 {
                    return;
                }
                if let Some((px, py)) = to_pixel(shared, self.mon, x, y) {
                    let down = d != 0;
                    if shared.verbose {
                        crate::log!("move {px},{py}");
                        crate::log!("button {b} {}", if down { "down" } else { "up" });
                    }
                    if down {
                        self.held.buttons.insert(b);
                    } else {
                        self.held.buttons.remove(&b);
                    }
                    if inject {
                        input::mouse_move(px, py);
                        input::button(b, down);
                    }
                }
            }
            ClientMsg::Wheel { dx, dy } => {
                if shared.verbose {
                    crate::log!("wheel dx={dx} dy={dy}");
                }
                if inject {
                    input::wheel(dx, dy);
                }
            }
            ClientMsg::Key { c, d } => {
                let down = d != 0;
                if input::lookup(&c).is_none() {
                    if shared.verbose {
                        crate::log!("key {c} ignored (unmapped)");
                    }
                    return;
                }
                if shared.verbose {
                    crate::log!("key {c} {}", if down { "down" } else { "up" });
                }
                if inject {
                    input::key(&c, down);
                }
                if down {
                    self.held.keys.insert(c);
                } else {
                    self.held.keys.remove(&c);
                }
            }
            ClientMsg::Type { s } => {
                if shared.verbose {
                    crate::log!("type {} chars", s.chars().count());
                }
                if inject && s.len() <= MAX_CLIPBOARD {
                    input::text(&s);
                }
            }
            ClientMsg::Release => {
                if shared.verbose {
                    crate::log!("release all");
                }
                self.held.release_all(inject);
            }
            ClientMsg::Clipboard { s } => {
                if !shared.clipboard || s.is_empty() || s.len() > MAX_CLIPBOARD {
                    return;
                }
                if !inject {
                    return;
                }
                // Already there, perhaps with formatting that the text alone
                // would lose (it was copied here and came back on a paste).
                if platform::clipboard::get_text().is_some_and(|now| same_text(&now, &s)) {
                    return;
                }
                if shared.verbose {
                    crate::log!("clipboard from client ({} chars)", s.chars().count());
                }
                // Recorded first so the watcher does not send it straight
                // back, and put back if the clipboard could not be set, so
                // the next paste of the same text is not taken as done.
                let before = std::mem::replace(&mut *shared.clip_last.lock().unwrap(), s.clone());
                if platform::clipboard::set_text(&s) {
                    // The image last synchronised is gone: copying it again here must be sent.
                    *shared.clip_last_image.lock().unwrap() = None;
                } else {
                    *shared.clip_last.lock().unwrap() = before;
                    crate::log!("could not set the clipboard to the client's text (another program has it open?)");
                }
            }
        }
    }

    /// One binary message from the client: its first byte says what it is.
    pub fn binary(&mut self, shared: &Shared, bytes: &[u8]) {
        let Some((&kind, body)) = bytes.split_first() else { return };
        match kind {
            BIN_CLIPBOARD_PNG => {
                if !shared.clipboard || shared.view_only || body.is_empty() || body.len() > MAX_CLIPBOARD_IMAGE {
                    return;
                }
                // The image this host just sent, pasted back: it is still on
                // the clipboard, with formats the PNG alone would lose.
                let hash = hash(body);
                let before = shared.clip_last_image.lock().unwrap().replace(hash);
                if before == Some(hash) {
                    return;
                }
                if shared.verbose {
                    crate::log!("clipboard image from client ({} KB)", body.len().div_ceil(1024));
                }
                if platform::clipboard::set_png(body) {
                    // The text last synchronised is no longer on the clipboard,
                    // so copying it again here must reach the client.
                    shared.clip_last.lock().unwrap().clear();
                } else {
                    *shared.clip_last_image.lock().unwrap() = before;
                    crate::log!("could not put the client's image on the clipboard");
                }
            }
            _ => {
                if shared.verbose {
                    crate::log!("unknown binary message {kind}");
                }
            }
        }
    }

    /// The client is gone. Never leave a key or button held down on the host.
    pub fn close(mut self, shared: &Shared) {
        self.held.release_all(!shared.view_only);
        shared.peers.lock().unwrap().remove(&self.id);
        shared.images_to.lock().unwrap().remove(&self.id);
        let _ = shared.engine.send(Cmd::Remove { id: self.id });
    }
}

fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// The same text, whichever line endings a browser or the OS gave it.
fn same_text(a: &str, b: &str) -> bool {
    a == b || a.replace("\r\n", "\n") == b.replace("\r\n", "\n")
}

/// Map a position given as fractions of a monitor to a desktop pixel.
fn to_pixel(shared: &Shared, mon: usize, x: f64, y: f64) -> Option<(i32, i32)> {
    let monitors = shared.monitors.read().unwrap();
    let m = monitors.get(mon.min(monitors.len().checked_sub(1)?))?;
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let along = |v: f64, size: u32| ((v.clamp(0.0, 1.0) * size as f64) as i32).min(size as i32 - 1);
    Some((m.x + along(x, m.width), m.y + along(y, m.height)))
}

/// Push host clipboard changes to connected clients. Runs on its own thread.
///
/// Changes made while no client is connected are deliberately not sent on
/// connect: that would overwrite whatever the user copied on the client just
/// before opening the session.
fn watch_clipboard(shared: Arc<Shared>) {
    let mut seen = platform::clipboard::sequence();
    let mut tries = 0;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let seq = platform::clipboard::sequence();
        if seq == seen {
            continue;
        }
        if shared.peers.lock().unwrap().is_empty() {
            seen = seq;
            continue;
        }
        // Text first: copied cells come with a picture of themselves. An
        // image is only read (and a bitmap encoded) for someone who wants it.
        let read = match platform::clipboard::get_text() {
            Some(text) => Some(Clip::Text(text)),
            None if !shared.images_to.lock().unwrap().is_empty() => platform::clipboard::get_png().map(Clip::Png),
            None => None,
        };
        let Some(clip) = read else {
            // Nothing we send, or the clipboard is busy: the program that
            // just set it may still have it open. Look again before letting
            // go of this change, or a copy is lost.
            tries += 1;
            if tries >= CLIP_READ_TRIES {
                seen = seq;
                tries = 0;
                forget(&shared);
            }
            continue;
        };
        seen = seq;
        tries = 0;
        match clip {
            Clip::Text(text) => {
                if text.len() > MAX_CLIPBOARD {
                    forget(&shared);
                    continue;
                }
                {
                    let mut last = shared.clip_last.lock().unwrap();
                    if *last == text {
                        continue;
                    }
                    last.clone_from(&text);
                }
                *shared.clip_last_image.lock().unwrap() = None;
                if shared.verbose {
                    crate::log!("clipboard to clients ({} chars)", text.chars().count());
                }
                let msg = serde_json::json!({ "t": "clip", "s": text }).to_string();
                for sink in shared.peers.lock().unwrap().values() {
                    sink(Out::Text(msg.clone()));
                }
            }
            Clip::Png(png) => {
                if png.len() > MAX_CLIPBOARD_IMAGE {
                    if shared.verbose {
                        crate::log!("clipboard image too large to send ({} KB)", png.len().div_ceil(1024));
                    }
                    forget(&shared);
                    continue;
                }
                let h = hash(&png);
                if shared.clip_last_image.lock().unwrap().replace(h) == Some(h) {
                    continue;
                }
                shared.clip_last.lock().unwrap().clear();
                if shared.verbose {
                    crate::log!("clipboard image to clients ({} KB)", png.len().div_ceil(1024));
                }
                let mut msg = Vec::with_capacity(1 + png.len());
                msg.push(BIN_OUT_CLIPBOARD_PNG);
                msg.extend_from_slice(&png);
                let to = shared.images_to.lock().unwrap().clone();
                for (id, sink) in shared.peers.lock().unwrap().iter() {
                    if to.contains(id) {
                        sink(Out::Binary(msg.clone()));
                    }
                }
            }
        }
    }
}

/// What the watcher found on the clipboard.
enum Clip {
    Text(String),
    Png(Vec<u8>),
}

/// The clipboard holds something not synchronised: whatever was last is no
/// longer there, so it is sent (or set) again if it comes back.
fn forget(shared: &Shared) {
    shared.clip_last.lock().unwrap().clear();
    *shared.clip_last_image.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(monitors: Vec<Monitor>) -> Shared {
        let (engine, _) = std::sync::mpsc::channel();
        Shared {
            view_only: true,
            clipboard: false,
            verbose: false,
            engine,
            monitors: RwLock::new(monitors),
            peers: Mutex::default(),
            clip_last: Mutex::default(),
            clip_last_image: Mutex::default(),
            images_to: Mutex::default(),
        }
    }

    #[test]
    fn images_go_to_sessions_that_asked_while_they_are_showing() {
        let s = shared(Vec::new());
        let wants = |id| s.images_to.lock().unwrap().contains(&id);
        let mut old = open(&s, 1, &HashMap::new(), Arc::new(|_| {}));
        assert!(!wants(1), "a client that never asks gets none");

        old.message(&s, r#"{"t":"clipimg","on":true}"#);
        assert!(wants(1));
        old.message(&s, r#"{"t":"vis","v":false}"#);
        assert!(!wants(1), "not while hidden or paused");
        old.message(&s, r#"{"t":"vis","v":true}"#);
        assert!(wants(1));
        let query = old.changed().unwrap().clone();
        assert_eq!(query.get("img").map(String::as_str), Some("1"), "kept for the next provider");
        old.close(&s);
        assert!(!wants(1));

        let reopened = open(&s, 2, &query, Arc::new(|_| {}));
        assert!(wants(2), "opened again as it was");
        reopened.close(&s);
    }

    #[test]
    fn positions_land_on_the_monitor_they_are_for() {
        let two = shared(vec![
            Monitor { x: 0, y: 0, width: 1920, height: 1080 },
            Monitor { x: 1920, y: -200, width: 1280, height: 720 },
        ]);
        assert_eq!(to_pixel(&two, 0, 0.5, 0.5), Some((960, 540)));
        assert_eq!(to_pixel(&two, 1, 0.0, 0.0), Some((1920, -200)));
        assert_eq!(to_pixel(&two, 1, 1.0, 1.0), Some((3199, 519)), "the last pixel, not one past it");
        assert_eq!(to_pixel(&two, 0, -3.0, 9.0), Some((0, 1079)), "outside is clamped to the edge");
        assert_eq!(to_pixel(&two, 7, 0.5, 0.5), Some((2560, 160)), "a monitor that is gone falls back to the last");
        assert_eq!(to_pixel(&two, 0, f64::NAN, 0.5), None);
        assert_eq!(to_pixel(&shared(Vec::new()), 0, 0.5, 0.5), None);
    }

    #[test]
    fn a_session_knows_how_to_open_itself_again() {
        let s = shared(vec![Monitor { x: 0, y: 0, width: 1920, height: 1080 }]);
        let query = HashMap::from([("q".to_string(), "med".to_string()), ("vis".to_string(), "0".to_string())]);
        let mut session = open(&s, 1, &query, Arc::new(|_| {}));
        assert!(session.changed().is_none(), "as it was opened");

        session.message(&s, r#"{"t":"m","x":0.5,"y":0.5}"#);
        session.message(&s, r#"{"t":"ack","s":3}"#);
        assert!(session.changed().is_none(), "input and acks change nothing about how it opens");

        session.message(&s, r#"{"t":"vis","v":true}"#);
        session.message(&s, r#"{"t":"cfg","q":"high","scale":2,"fps":30,"soft":true,"lossless":false,"mon":1}"#);
        // Values `open` would refuse are not kept.
        session.message(&s, r#"{"t":"cfg","q":"best","scale":3}"#);
        let now = session.changed().expect("the view changed").clone();
        let has = |key: &str| now.get(key).map(String::as_str);
        assert_eq!((has("vis"), has("q"), has("scale")), (Some("1"), Some("high"), Some("2")));
        assert_eq!((has("fps"), has("soft"), has("lossless"), has("mon")), (Some("30"), Some("1"), Some("0"), Some("1")));
        assert!(session.changed().is_none(), "told once");
        session.close(&s);
    }

    #[test]
    fn the_screen_accepts_input_and_a_page_timing_probe() {
        let screen = |text: &str| serde_json::from_str::<ClientMsg>(text).is_ok();
        for ours in [
            r#"{"t":"ack","s":7}"#,
            r#"{"t":"vis","v":true}"#,
            r#"{"t":"cfg","q":"high","mon":1}"#,
            r#"{"t":"m","x":0.5,"y":0.5}"#,
            r#"{"t":"b","b":0,"d":1,"x":0.5,"y":0.5}"#,
            r#"{"t":"w","dy":-120}"#,
            r#"{"t":"k","c":"KeyA","d":1}"#,
            r#"{"t":"rel"}"#,
            r#"{"t":"type","s":"hello"}"#,
            r#"{"t":"clip","s":"text"}"#,
            r#"{"t":"clipimg","on":true}"#,
        ] {
            assert!(screen(ours), "{ours}");
        }
        assert!(screen(r#"{"t":"ping","n":3}"#));
        assert!(!screen(r#"{"t":"paircode"}"#));
    }
}
