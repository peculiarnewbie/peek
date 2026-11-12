//! HTTP and WebSocket front end: serves the client page, authenticates
//! sockets, and passes what clients say to the screen: in this process
//! (`screen`), or in a provider's (`provider`).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::connect_info::Connected;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::serve::{IncomingStream, Listener};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::unbounded_channel;

use crate::engine::Out;
use crate::pairing::{self, Pairing, RedeemError};
use crate::provider::Link;
use crate::screen;

/// Close code telling the client its token is wrong (so it stops retrying).
const CLOSE_BAD_TOKEN: u16 = 4403;
/// Close code for a socket opened from a page we did not serve, or through a
/// name we do not trust (so it stops retrying).
const CLOSE_BAD_ORIGIN: u16 = 4401;
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
    /// None: no token, and only Tailscale connections are accepted.
    pub token: Option<String>,
    /// Origin of the hosted client (e.g. https://peek.peculiarnewbie.com),
    /// whose pages may connect with a paired token.
    pub web: Option<String>,
    pub pairing: Pairing,
    pub screen: Screen,
    pub next_id: AtomicU64,
}

/// Where the engine, input and the clipboard are.
pub enum Screen {
    /// In this process.
    Here(Arc<screen::Shared>),
    /// In a provider process, in a session that has a display.
    #[cfg_attr(not(windows), allow(dead_code))]
    Provider(Arc<Link>),
}

pub async fn serve(shared: Arc<Shared>, listener: TcpListener) -> std::io::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/manifest.webmanifest", get(|| asset("application/manifest+json", MANIFEST)))
        .route("/sw.js", get(|| asset("text/javascript; charset=utf-8", SERVICE_WORKER)))
        .route("/icon.svg", get(|| asset("image/svg+xml", ICON_SVG)))
        .route("/icon-192.png", get(|| asset("image/png", ICON_192)))
        .route("/icon-512.png", get(|| asset("image/png", ICON_512)))
        .route("/ws", get(ws))
        .route("/.well-known/peek", get(about).options(preflight))
        .route("/api/pair", post(pair).options(preflight))
        .route("/api/pair-code", post(local_pair_code))
        .route("/api/clients", get(local_clients))
        .route("/api/clients/revoke", post(local_revoke))
        .route("/api/logs", get(local_logs))
        .layer(middleware::from_fn_with_state(shared.clone(), ip_guard))
        .with_state(shared);
    // Input and small screen updates must not sit in Nagle's buffer.
    let listener = NoDelay(listener);
    axum::serve(listener, app.into_make_service_with_connect_info::<Peer>()).await
}

/// Both ends of a connection: who it is from, and which of our addresses it
/// reached.
#[derive(Clone, Copy)]
pub struct Peer {
    remote: SocketAddr,
    local: Option<SocketAddr>,
}

/// A TCP listener whose connections skip Nagle's buffer.
struct NoDelay(TcpListener);

impl Listener for NoDelay {
    type Io = TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (TcpStream, SocketAddr) {
        let (stream, addr) = Listener::accept(&mut self.0).await; // retries errors itself
        let _ = stream.set_nodelay(true);
        (stream, addr)
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Listener::local_addr(&self.0)
    }
}

impl Connected<IncomingStream<'_, NoDelay>> for Peer {
    fn connect_info(stream: IncomingStream<'_, NoDelay>) -> Self {
        Peer { remote: *stream.remote_addr(), local: stream.io().local_addr().ok() }
    }
}

/// An IPv4 address carried in IPv6 counts as the IPv4 address.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// Tailscale's address ranges: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn tailnet(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(a) => {
            let o = a.octets();
            o[0] == 100 && o[1] & 0xC0 == 64
        }
        IpAddr::V6(a) => a.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// Without a token, the connection must come in over Tailscale: from a
/// tailnet address to one of ours. Checking our end as well means a device
/// on the LAN that gives itself a 100.x address still cannot reach us, and
/// it keeps working when Tailscale comes up after peek does.
pub fn tailnet_connection(remote: IpAddr, local: Option<IpAddr>) -> bool {
    let local = local.map(canonical);
    (tailnet(remote) && local.is_some_and(tailnet))
        || (canonical(remote).is_loopback() && local.is_some_and(|l| l.is_loopback()))
}

/// Only private, link-local and tailnet addresses may talk to us at all.
pub fn allowed(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(a) => a.is_loopback() || a.is_private() || a.is_link_local() || tailnet(ip),
        IpAddr::V6(a) => {
            let first = a.segments()[0];
            a.is_loopback() || first & 0xFE00 == 0xFC00 || first & 0xFFC0 == 0xFE80
        }
    }
}

async fn ip_guard(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    req: Request,
    next: Next,
) -> Response {
    let ip = peer.remote.ip();
    let ok = match shared.token {
        Some(_) => allowed(ip),
        None => tailnet_connection(ip, peer.local.map(|l| l.ip())),
    };
    if ok {
        next.run(req).await
    } else {
        let why = if shared.token.is_some() { "not a private address" } else { "not over Tailscale" };
        crate::log!("rejected {ip}: {why}");
        StatusCode::FORBIDDEN.into_response()
    }
}

/// A browser lets any page open a WebSocket to any address, and the request
/// then comes from a device we trust. So a socket must come from our own
/// page: its Origin must name the very host it connected to.
///
/// Without a token, that host must also be a name no stranger can point at
/// us (DNS rebinding: their domain resolving to our address would make the
/// Origin match): an IP address, localhost, a bare machine name, or a
/// Tailscale MagicDNS name.
fn origin_ok(headers: &HeaderMap, need_trusted_name: bool) -> bool {
    let get = |name| headers.get(name).and_then(|v| v.to_str().ok());
    let (Some(host), Some(origin)) = (get(header::HOST), get(header::ORIGIN)) else {
        return false;
    };
    let Some(origin_host) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else {
        return false;
    };
    origin_host.eq_ignore_ascii_case(host) && (!need_trusted_name || trusted_name(host))
}

fn trusted_name(host: &str) -> bool {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split_once(']').is_some_and(|(ip, _)| ip.parse::<Ipv6Addr>().is_ok());
    }
    let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    !name.is_empty()
        && (name.parse::<Ipv4Addr>().is_ok()
            || name == "localhost"
            || !name.contains('.')
            || name.ends_with(".ts.net"))
}

// What makes the client installable as an app: a manifest, icons, and a
// service worker (see client/public/).
const MANIFEST: &[u8] = include_bytes!("../client/dist/manifest.webmanifest");
const SERVICE_WORKER: &[u8] = include_bytes!("../client/dist/sw.js");
const ICON_SVG: &[u8] = include_bytes!("../client/dist/icon.svg");
const ICON_192: &[u8] = include_bytes!("../client/dist/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../client/dist/icon-512.png");

async fn asset(content_type: &'static str, body: &'static [u8]) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-cache")], body)
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        include_str!("../client/dist/index.html"),
    )
}

/// Constant-time comparison, so response timing says nothing about the token.
fn token_matches(given: &str, expected: &str) -> bool {
    pairing::same(given, expected)
}

fn header_str<'a>(headers: &'a HeaderMap, name: header::HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The request comes from a page of the hosted client.
fn from_web(shared: &Shared, headers: &HeaderMap) -> bool {
    shared.web.as_deref().is_some_and(|web| header_str(headers, header::ORIGIN) == Some(web))
}

/// CORS headers letting the hosted client, and only it, read a response.
/// Chrome also asks before a public page may reach a private address.
fn cors(shared: &Shared, req: &HeaderMap) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::VARY, HeaderValue::from_static("Origin"));
    if from_web(shared, req) {
        if let Some(origin) = req.get(header::ORIGIN) {
            h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        }
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST"));
        h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
        if req.contains_key("access-control-request-private-network") {
            h.insert("access-control-allow-private-network", HeaderValue::from_static("true"));
        }
    }
    h
}

async fn preflight(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    (StatusCode::NO_CONTENT, cors(&shared, &headers)).into_response()
}

/// Who this host is: the hosted client checks it before pairing, and to
/// show which paired machines are up.
async fn about(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let body = serde_json::json!({
        "id": shared.pairing.host_id,
        "name": pairing::machine_name(),
        "protocol": PROTOCOL.parse::<u32>().unwrap_or(0),
    });
    (cors(&shared, &headers), [(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], body.to_string())
        .into_response()
}

/// Where a request really came from: behind `tailscale serve` every
/// connection is from loopback, and the proxy names the client.
fn client_ip(peer: &Peer, headers: &HeaderMap) -> String {
    let ip = peer.remote.ip();
    if canonical(ip).is_loopback() {
        if let Some(fwd) = header_str(headers, header::HeaderName::from_static("x-forwarded-for")) {
            return fwd.split(',').next().unwrap_or(fwd).trim().to_string();
        }
    }
    ip.to_string()
}

#[derive(Deserialize)]
struct PairRequest {
    code: String,
    #[serde(default)]
    label: String,
}

/// Trade a one-time code for a token of this browser's own.
async fn pair(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let cors = cors(&shared, &headers);
    let json = |status: StatusCode, body: serde_json::Value| {
        (status, cors.clone(), [(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], body.to_string())
            .into_response()
    };
    // Other pages must not even try; requests from outside a browser carry no Origin.
    if headers.contains_key(header::ORIGIN) && !from_web(&shared, &headers) {
        return json(StatusCode::FORBIDDEN, serde_json::json!({ "error": "origin" }));
    }
    let Ok(req) = serde_json::from_slice::<PairRequest>(&body) else {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": "request" }));
    };
    match shared.pairing.redeem(&req.code, &req.label) {
        Ok((client, token)) => {
            crate::log!("paired a browser: {} ({}) from {}", client.label, client.id, client_ip(&peer, &headers));
            json(
                StatusCode::OK,
                serde_json::json!({
                    "token": token,
                    "client": client.id,
                    "id": shared.pairing.host_id,
                    "name": pairing::machine_name(),
                }),
            )
        }
        Err(RedeemError::Invalid) => {
            crate::log!("wrong pairing code from {}", client_ip(&peer, &headers));
            json(StatusCode::FORBIDDEN, serde_json::json!({ "error": "code" }))
        }
        Err(RedeemError::Busy) => json(StatusCode::TOO_MANY_REQUESTS, serde_json::json!({ "error": "busy" })),
    }
}

/// A new pairing code, with the link that uses it.
pub fn pairing_offer(shared: &Shared) -> serde_json::Value {
    let code = match shared.pairing.new_code() {
        Ok(code) => code,
        Err(e) => return serde_json::json!({ "error": e.to_string() }),
    };
    let name = pairing::machine_name();
    let host = pairing::tailscale_name();
    let link = match (&shared.web, &host) {
        (Some(web), Some(host)) => Some(pairing::pair_link(web, host, &name, &code)),
        _ => None,
    };
    serde_json::json!({
        "code": pairing::format_code(&code),
        "expires": pairing::CODE_TTL.as_secs(),
        "host": host,
        "name": name,
        "web": shared.web,
        "link": link,
    })
}

/// Requests from `peek` commands on this machine: they hold the secret in
/// the config folder, and browsers (which send an Origin) are refused.
fn local_ok(shared: &Shared, headers: &HeaderMap) -> bool {
    let bearer = header_str(headers, header::AUTHORIZATION).and_then(|v| v.strip_prefix("Bearer "));
    !headers.contains_key(header::ORIGIN) && bearer.is_some_and(|b| token_matches(b, &shared.pairing.local_secret))
}

async fn local_pair_code(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !local_ok(&shared, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let offer = tokio::task::spawn_blocking(move || pairing_offer(&shared)).await.unwrap_or_default();
    ([(header::CONTENT_TYPE, "application/json")], offer.to_string()).into_response()
}

async fn local_clients(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !local_ok(&shared, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let list = serde_json::to_string(&shared.pairing.list()).unwrap_or_default();
    ([(header::CONTENT_TYPE, "application/json")], list).into_response()
}

async fn local_logs(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !local_ok(&shared, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let number = |key: &str| query.get(key).and_then(|v| v.parse::<u64>().ok());
    let (last, lines) = crate::recent_log(number("after").unwrap_or(0), number("limit").unwrap_or(50) as usize);
    let file = crate::LOG_PATH.get().map(|p| p.display().to_string());
    let body = serde_json::json!({ "last": last, "lines": lines, "file": file });
    ([(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

async fn local_revoke(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    if !local_ok(&shared, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let id = String::from_utf8_lossy(&body).trim().to_string();
    if shared.pairing.revoke(&id) {
        crate::log!("unpaired browser {id}");
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn ws(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let addr = peer.remote;
    let web = from_web(&shared, &headers);
    if !web && !origin_ok(&headers, shared.token.is_none()) {
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()).unwrap_or("none");
        crate::log!("rejected socket from {} with origin {origin}", addr.ip());
        // Upgrade only to say why, so our own page stops retrying.
        return upgrade.on_upgrade(|mut socket| async move {
            let _ = socket
                .send(Message::Close(Some(CloseFrame { code: CLOSE_BAD_ORIGIN, reason: "bad origin".into() })))
                .await;
        });
    }
    upgrade
        .max_message_size(MAX_CLIPBOARD * 4 + 4096)
        .on_upgrade(move |socket| client(socket, shared, query, addr, web))
}

/// The messages the server answers itself. Everything else a client says
/// belongs to the screen.
#[derive(Deserialize)]
#[serde(tag = "t")]
enum CoreMsg {
    /// Answered straight away, for the client's round-trip measurement.
    #[serde(rename = "ping")]
    Ping { n: u32 },
    /// A one-time code for pairing another browser through the hosted client.
    #[serde(rename = "paircode")]
    PairCode,
}

/// `web`: opened by the hosted client, which always needs a token (a paired
/// one, or the host's own), even on a host started with `--no-token`.
async fn client(mut socket: WebSocket, shared: Arc<Shared>, query: HashMap<String, String>, addr: SocketAddr, web: bool) {
    let given = query.get("token").map_or("", String::as_str);
    let paired = shared.pairing.check(given);
    let host_token = shared.token.as_deref().is_some_and(|expected| token_matches(given, expected));
    let needs_token = web || shared.token.is_some();
    if needs_token && paired.is_none() && !host_token {
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
    let (tx, mut rx) = unbounded_channel::<Out>();
    let reply = tx.clone();
    let mut session = match &shared.screen {
        Screen::Here(screen) => Some(screen::open(screen, id, &query, Arc::new(move |out| drop(tx.send(out))))),
        Screen::Provider(link) => {
            link.open(id, &query, tx);
            None
        }
    };
    match &paired {
        Some(c) => crate::log!("client connected: {} ({})", addr.ip(), c.label),
        None => crate::log!("client connected: {}", addr.ip()),
    }

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

    // Browsers answer pings on their own, so silence means the client is gone.
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(DEAD_AFTER, stream.next()).await {
        let text = match msg {
            Message::Text(text) => text,
            Message::Close(_) => break,
            _ => continue,
        };
        match serde_json::from_str::<CoreMsg>(&text) {
            Ok(CoreMsg::PairCode) => {
                let shared = shared.clone();
                let reply = reply.clone();
                tokio::task::spawn_blocking(move || {
                    let mut offer = pairing_offer(&shared);
                    offer["t"] = "paircode".into();
                    let _ = reply.send(Out::Text(offer.to_string()));
                });
            }
            Ok(CoreMsg::Ping { n }) => {
                // Queued behind any frames already waiting, which is the
                // delay a fresh frame would see too.
                let _ = reply.send(Out::Text(serde_json::json!({ "t": "pong", "n": n }).to_string()));
            }
            Err(_) => match (&shared.screen, &mut session) {
                (Screen::Here(screen), Some(session)) => session.message(screen, &text),
                (Screen::Provider(link), _) => link.text(id, &text),
                (Screen::Here(_), None) => {}
            },
        }
    }

    match (&shared.screen, session) {
        (Screen::Here(screen), Some(session)) => session.close(screen),
        (Screen::Provider(link), _) => link.close(id),
        (Screen::Here(_), None) => {}
    }
    writer.abort();
    crate::log!("client disconnected: {}", addr.ip());
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
    fn tailnet_connections() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let here = Some(ip("100.97.2.26"));
        assert!(tailnet_connection(ip("100.64.38.83"), here));
        assert!(tailnet_connection(ip("fd7a:115c:a1e0::5"), Some(ip("fd7a:115c:a1e0::1"))));
        assert!(tailnet_connection(ip("::ffff:100.64.38.83"), Some(ip("::ffff:100.97.2.26"))));
        assert!(tailnet_connection(ip("127.0.0.1"), Some(ip("127.0.0.1"))));
        // A LAN device posing as a tailnet address, reaching our LAN address.
        assert!(!tailnet_connection(ip("100.64.38.83"), Some(ip("192.168.1.10"))));
        assert!(!tailnet_connection(ip("192.168.1.20"), here));
        assert!(!tailnet_connection(ip("100.64.38.83"), None));
        assert!(!tailnet_connection(ip("fd00::1"), Some(ip("fd7a:115c:a1e0::1"))));
    }

    #[test]
    fn origin_checks() {
        let headers = |host: &str, origin: Option<&str>| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, host.parse().unwrap());
            if let Some(o) = origin {
                h.insert(header::ORIGIN, o.parse().unwrap());
            }
            h
        };
        for (host, origin) in [
            ("100.97.2.26:9182", "http://100.97.2.26:9182"),
            ("[fd7a:115c:a1e0::1]:9182", "http://[fd7a:115c:a1e0::1]:9182"),
            ("desktop-work-ar:9182", "http://DESKTOP-WORK-AR:9182"),
            ("desktop-work-ar.tail1234.ts.net:9182", "http://desktop-work-ar.tail1234.ts.net:9182"),
            ("localhost:9182", "http://localhost:9182"),
        ] {
            assert!(origin_ok(&headers(host, Some(origin)), true), "{host} from {origin}");
        }
        // Another site, a missing Origin, a different port.
        assert!(!origin_ok(&headers("100.97.2.26:9182", Some("https://evil.example")), false));
        assert!(!origin_ok(&headers("100.97.2.26:9182", None), false));
        assert!(!origin_ok(&headers("100.97.2.26:9182", Some("http://100.97.2.26:8080")), false));
        // DNS rebinding: Origin and Host agree, but on a stranger's domain.
        let rebound = headers("evil.example:9182", Some("http://evil.example:9182"));
        assert!(origin_ok(&rebound, false));
        assert!(!origin_ok(&rebound, true));
    }

    #[test]
    fn the_server_answers_only_its_own_messages() {
        let core = |text: &str| serde_json::from_str::<CoreMsg>(text).is_ok();
        assert!(core(r#"{"t":"ping","n":3}"#));
        assert!(core(r#"{"t":"paircode"}"#));
        for screen in [
            r#"{"t":"ack","s":7}"#,
            r#"{"t":"vis","v":true}"#,
            r#"{"t":"cfg","q":"high"}"#,
            r#"{"t":"m","x":0.5,"y":0.5}"#,
            r#"{"t":"k","c":"KeyA","d":1}"#,
            r#"{"t":"clip","s":"text"}"#,
            "not json",
        ] {
            assert!(!core(screen), "{screen} is for the screen");
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
