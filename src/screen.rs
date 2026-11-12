//! The screen as one capability: the capture engine, injected input and the
//! clipboard, behind a small surface. Open a session, pass it what the
//! client says, close it.
//!
//! The server is its only caller today. It is kept apart from the server,
//! and free of sockets, so that something else could hold it instead: see
//! docs/extensibility.md.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde::Deserialize;

use crate::engine::{self, Cmd, Out, Quality, View, ViewChange};
use crate::platform::{self, Monitor, input};
use crate::server::MAX_CLIPBOARD;

/// Where a session's output goes. For the server, a socket's queue.
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

/// What a client says to the screen. (`ping` and `paircode` are answered by
/// the server itself and never get here.)
#[derive(Deserialize)]
#[serde(tag = "t")]
enum ClientMsg {
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
}

fn scale(v: Option<usize>) -> Option<usize> {
    v.filter(|s| matches!(s, 1 | 2))
}

fn fps(v: Option<u32>) -> Option<u32> {
    v.map(|f| f.min(1000))
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
    Session { id, mon, held: Held::default() }
}

impl Session {
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
            ClientMsg::Ack { s } => {
                let _ = shared.engine.send(Cmd::Ack { id, seq: s });
            }
            ClientMsg::Visible { v } => {
                let _ = shared.engine.send(Cmd::Visible { id, visible: v });
            }
            ClientMsg::Configure { mon: m, q, scale: s, fps: f, soft, lossless } => {
                self.mon = m.unwrap_or(self.mon);
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
                if shared.verbose {
                    crate::log!("clipboard from client ({} chars)", s.chars().count());
                }
                if inject {
                    // Record it first so the watcher does not send it straight back.
                    *shared.clip_last.lock().unwrap() = s.clone();
                    platform::clipboard::set_text(&s);
                }
            }
        }
    }

    /// The client is gone. Never leave a key or button held down on the host.
    pub fn close(mut self, shared: &Shared) {
        self.held.release_all(!shared.view_only);
        shared.peers.lock().unwrap().remove(&self.id);
        let _ = shared.engine.send(Cmd::Remove { id: self.id });
    }
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
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let seq = platform::clipboard::sequence();
        if seq == seen {
            continue;
        }
        seen = seq;
        if shared.peers.lock().unwrap().is_empty() {
            continue;
        }
        let Some(text) = platform::clipboard::get_text() else { continue };
        if text.len() > MAX_CLIPBOARD {
            continue;
        }
        {
            let mut last = shared.clip_last.lock().unwrap();
            if *last == text {
                continue;
            }
            last.clone_from(&text);
        }
        if shared.verbose {
            crate::log!("clipboard to clients ({} chars)", text.chars().count());
        }
        let msg = serde_json::json!({ "t": "clip", "s": text }).to_string();
        for sink in shared.peers.lock().unwrap().values() {
            sink(Out::Text(msg.clone()));
        }
    }
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
        }
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
    fn the_screen_takes_every_message_but_the_servers() {
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
        ] {
            assert!(screen(ours), "{ours}");
        }
        assert!(!screen(r#"{"t":"ping","n":3}"#));
        assert!(!screen(r#"{"t":"paircode"}"#));
    }
}
