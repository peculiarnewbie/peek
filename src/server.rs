//! HTTP and WebSocket front end: serves the client page, authenticates
//! sockets, and turns client messages into injected input.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::serve::ListenerExt;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::engine::{Cmd, Out, Quality, View, ViewChange};
use crate::platform::{self, Monitor, input};

/// Close code telling the client its token is wrong (so it stops retrying).
const CLOSE_BAD_TOKEN: u16 = 4403;
/// Close code telling a page loaded from an older host to reload itself.
const CLOSE_OUTDATED: u16 = 4426;
/// Bumped whenever the wire format changes; the client sends it as `v`.
const PROTOCOL: &str = "4";
/// Clipboard text larger than this is not synchronised.
pub const MAX_CLIPBOARD: usize = 1 << 20;
const PING_EVERY: Duration = Duration::from_secs(5);
/// A client silent for this long is gone; its held keys must be released.
const DEAD_AFTER: Duration = Duration::from_secs(15);

pub struct Shared {
    pub token: String,
    pub view_only: bool,
    pub clipboard: bool,
    pub verbose: bool,
    pub engine: Sender<Cmd>,
    pub monitors: RwLock<Vec<Monitor>>,
    /// Every connected client, for clipboard broadcasts.
    pub peers: Mutex<HashMap<u64, UnboundedSender<Out>>>,
    /// The text most recently synchronised in either direction, so a change
    /// we caused ourselves is not echoed back.
    pub clip_last: Mutex<String>,
    pub next_id: AtomicU64,
}

pub async fn serve(shared: Arc<Shared>, addr: SocketAddr) -> std::io::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws))
        .layer(middleware::from_fn(ip_guard))
        .with_state(shared);
    // Input and small screen updates must not sit in Nagle's buffer.
    let listener = tokio::net::TcpListener::bind(addr).await?.tap_io(|stream| {
        let _ = stream.set_nodelay(true);
    });
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await
}

/// Only private, link-local and tailnet addresses may talk to us at all.
pub fn allowed(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || (o[0] == 100 && o[1] & 0xC0 == 64) // 100.64.0.0/10, Tailscale
        }
        IpAddr::V6(a) => {
            let first = a.segments()[0];
            a.is_loopback() || first & 0xFE00 == 0xFC00 || first & 0xFFC0 == 0xFE80
        }
    }
}

async fn ip_guard(ConnectInfo(addr): ConnectInfo<SocketAddr>, req: Request, next: Next) -> Response {
    if allowed(addr.ip()) {
        next.run(req).await
    } else {
        crate::log!("rejected non-private address {}", addr.ip());
        StatusCode::FORBIDDEN.into_response()
    }
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        include_str!("../client/index.html"),
    )
}

/// Constant-time comparison, so response timing says nothing about the token.
fn token_matches(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn ws(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(query): Query<HashMap<String, String>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade
        .max_message_size(MAX_CLIPBOARD * 4 + 4096)
        .on_upgrade(move |socket| client(socket, shared, query, addr))
}

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
    #[serde(rename = "clip")]
    Clipboard { s: String },
    /// Answered straight away, for the client's round-trip measurement.
    #[serde(rename = "ping")]
    Ping { n: u32 },
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

async fn client(mut socket: WebSocket, shared: Arc<Shared>, query: HashMap<String, String>, addr: SocketAddr) {
    if !token_matches(query.get("token").map_or("", String::as_str), &shared.token) {
        crate::log!("bad token from {}", addr.ip());
        let _ = socket
            .send(Message::Close(Some(CloseFrame { code: CLOSE_BAD_TOKEN, reason: "bad token".into() })))
            .await;
        return;
    }

    if query.get("v").map(String::as_str) != Some(PROTOCOL) {
        // A tab left open across a host upgrade: its script predates this protocol.
        let _ = socket
            .send(Message::Close(Some(CloseFrame { code: CLOSE_OUTDATED, reason: "client out of date".into() })))
            .await;
        return;
    }

    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let scale = |v: Option<usize>| v.filter(|s| matches!(s, 1 | 2));
    let fps = |v: Option<u32>| v.map(|f| f.min(1000));
    fn param<T: std::str::FromStr>(query: &HashMap<String, String>, key: &str) -> Option<T> {
        query.get(key).and_then(|v| v.parse().ok())
    }
    let mut mon = param(&query, "mon").unwrap_or(0usize);
    let (tx, mut rx) = unbounded_channel::<Out>();
    let _ = shared.engine.send(Cmd::Add {
        id,
        tx: tx.clone(),
        view: View {
            mon,
            quality: query.get("q").and_then(|q| Quality::parse(q)).unwrap_or(Quality::Med),
            scale: scale(param(&query, "scale")).unwrap_or(1),
            fps: fps(param(&query, "fps")).unwrap_or(0),
            soft: query.get("soft").is_some_and(|v| v == "1"),
        },
        visible: query.get("vis").is_none_or(|v| v != "0"),
    });
    let reply = tx.clone();
    shared.peers.lock().unwrap().insert(id, tx);
    crate::log!("client connected: {}", addr.ip());

    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_EVERY);
        loop {
            let msg = tokio::select! {
                out = rx.recv() => match out {
                    Some(Out::Text(text)) => Message::Text(text.into()),
                    Some(Out::Binary(bytes)) => Message::Binary(bytes.into()),
                    None => break,
                },
                _ = ping.tick() => Message::Ping(Default::default()),
            };
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    let inject = !shared.view_only;
    let mut held = Held::default();
    // Browsers answer pings on their own, so silence means the client is gone.
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(DEAD_AFTER, stream.next()).await {
        let text = match msg {
            Message::Text(text) => text,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(msg) = serde_json::from_str::<ClientMsg>(&text) else {
            if shared.verbose {
                crate::log!("bad message {}", text.as_str());
            }
            continue;
        };
        match msg {
            ClientMsg::Ack { s } => {
                let _ = shared.engine.send(Cmd::Ack { id, seq: s });
            }
            ClientMsg::Visible { v } => {
                let _ = shared.engine.send(Cmd::Visible { id, visible: v });
            }
            ClientMsg::Configure { mon: m, q, scale: s, fps: f, soft } => {
                mon = m.unwrap_or(mon);
                let change = ViewChange {
                    mon: m,
                    quality: q.as_deref().and_then(Quality::parse),
                    scale: scale(s),
                    fps: fps(f),
                    soft,
                };
                let _ = shared.engine.send(Cmd::Configure { id, change });
            }
            ClientMsg::Move { x, y } => {
                if let Some((px, py)) = to_pixel(&shared, mon, x, y) {
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
                    continue;
                }
                if let Some((px, py)) = to_pixel(&shared, mon, x, y) {
                    let down = d != 0;
                    if shared.verbose {
                        crate::log!("move {px},{py}");
                        crate::log!("button {b} {}", if down { "down" } else { "up" });
                    }
                    if down {
                        held.buttons.insert(b);
                    } else {
                        held.buttons.remove(&b);
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
                    continue;
                }
                if shared.verbose {
                    crate::log!("key {c} {}", if down { "down" } else { "up" });
                }
                if inject {
                    input::key(&c, down);
                }
                if down {
                    held.keys.insert(c);
                } else {
                    held.keys.remove(&c);
                }
            }
            ClientMsg::Release => {
                if shared.verbose {
                    crate::log!("release all");
                }
                held.release_all(inject);
            }
            ClientMsg::Clipboard { s } => {
                if !shared.clipboard || s.is_empty() || s.len() > MAX_CLIPBOARD {
                    continue;
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
            ClientMsg::Ping { n } => {
                // Queued behind any frames already waiting, which is the
                // delay a fresh frame would see too.
                let _ = reply.send(Out::Text(serde_json::json!({ "t": "pong", "n": n }).to_string()));
            }
        }
    }

    // Never leave a key or button held down on the host.
    held.release_all(inject);
    shared.peers.lock().unwrap().remove(&id);
    let _ = shared.engine.send(Cmd::Remove { id });
    writer.abort();
    crate::log!("client disconnected: {}", addr.ip());
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
pub fn watch_clipboard(shared: Arc<Shared>) {
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
        for tx in shared.peers.lock().unwrap().values() {
            let _ = tx.send(Out::Text(msg.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_private_addresses_are_allowed() {
        for ok in [
            "127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.1.20",
            "100.64.0.1", "100.101.102.103", "100.127.255.255", "169.254.1.1", "::1",
            "fd7a:115c:a1e0::1", "fe80::1", "::ffff:192.168.1.5",
        ] {
            assert!(allowed(ok.parse().unwrap()), "{ok} should be allowed");
        }
        for bad in [
            "8.8.8.8", "172.32.0.1", "100.63.255.255", "100.128.0.1", "192.169.0.1",
            "2001:4860:4860::8888", "::ffff:8.8.8.8",
        ] {
            assert!(!allowed(bad.parse().unwrap()), "{bad} should be rejected");
        }
    }

    #[test]
    fn token_comparison() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abd", "abc"));
        assert!(!token_matches("ab", "abc"));
        assert!(!token_matches("", "abc"));
    }
}
