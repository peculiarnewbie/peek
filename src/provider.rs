//! Providers: what peek does beyond listening, each in a process of its own.
//!
//! The server is the core: it listens, authenticates and routes. A provider
//! is another peek (`peek --provider <name>`) that the core talks to over a
//! pair of pipes (the provider's stdin and stdout). A client's socket, or
//! one request, is a channel on a provider, and what the provider sends on
//! that channel goes back to it.
//!
//! - `screen`: the engine, input and the clipboard. In the Windows service
//!   it runs in the session at the console, since the service has no
//!   display; elsewhere it is in the core's own process.
//! - `vitals`: processor, memory and storage, on every client's socket.
//! - `power`: sleep, and waking another machine on the same network.
//!
//! A provider can end (the screen's goes with its session, at a sign-out or
//! a switch of user). The core and its sockets stay, and each open channel
//! is opened again on the next provider, as the client last had it.
//!
//! Messages on a pipe: u32 payload length, u8 kind, u64 channel
//! (little-endian), then the payload.
//!
//! - To the provider: `OPEN` (how to open the channel: the client's query,
//!   as JSON), `TEXT` (a client's message, untouched), `CLOSE`.
//! - From it: `TEXT` and `BINARY` for the client, `STATE` (how to open this
//!   channel again as it is now, which the core keeps and does not read),
//!   and `LOG` (a line for the core's log).

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;

use crate::engine::Out;
use crate::screen::{self, Sink};

const OPEN: u8 = 1;
const CLOSE: u8 = 2;
const TEXT: u8 = 3;
const BINARY: u8 = 4;
const STATE: u8 = 5;
const LOG: u8 = 6;
const HEADER: usize = 13;
/// Larger than any frame; a length above it means the stream is out of step.
const MAX_PAYLOAD: usize = 256 << 20;
/// How long before a provider that ended is started again: at first, and
/// at most, while it keeps ending.
const RESTART_FIRST: Duration = Duration::from_secs(1);
const RESTART_MAX: Duration = Duration::from_secs(60);
/// How long a request waits for a provider's answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

fn write_frame(w: &mut impl Write, kind: u8, channel: u64, payload: &[u8]) -> io::Result<()> {
    let mut head = [0u8; HEADER];
    head[0..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    head[4] = kind;
    head[5..13].copy_from_slice(&channel.to_le_bytes());
    if payload.len() <= 4096 {
        // One write for a small message, so the reader wakes once.
        let mut whole = Vec::with_capacity(HEADER + payload.len());
        whole.extend_from_slice(&head);
        whole.extend_from_slice(payload);
        w.write_all(&whole)
    } else {
        w.write_all(&head)?;
        w.write_all(payload)
    }
}

fn read_frame(r: &mut impl Read) -> io::Result<(u8, u64, Vec<u8>)> {
    let mut head = [0u8; HEADER];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes(head[0..4].try_into().unwrap()) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let channel = u64::from_le_bytes(head[5..13].try_into().unwrap());
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok((head[4], channel, payload))
}

// ---- the core's end ----

struct Channel {
    /// The `OPEN` payload that opens this channel as it is now: the client's
    /// query at first, then whatever the provider last sent as `STATE`.
    open: Vec<u8>,
    tx: UnboundedSender<Out>,
}

/// The pipe to the provider, if there is one, and how many there have been.
type Pipe = (u64, Option<Box<dyn Write + Send>>);

/// A provider's channels, and the process serving them at the moment.
pub struct Link {
    name: &'static str,
    /// Locked before `channels`, always.
    to: Mutex<Pipe>,
    channels: Mutex<HashMap<u64, Channel>>,
}

impl Link {
    /// A link with no provider yet. Channels opened on it wait for one.
    pub fn new(name: &'static str) -> Arc<Link> {
        Arc::new(Link { name, to: Mutex::new((0, None)), channels: Mutex::default() })
    }

    /// A link to `peek --provider <name>`, started here as a child of this
    /// process with these flags, and started again whenever it ends.
    pub fn spawn(name: &'static str, flags: Vec<String>) -> Arc<Link> {
        let link = Link::new(name);
        let keep = link.clone();
        let started = std::thread::Builder::new().name(format!("{name}-provider")).spawn(move || {
            let mut wait = RESTART_FIRST;
            loop {
                let since = Instant::now();
                match keep.start_child(&flags) {
                    Ok(mut child) => match child.wait() {
                        Ok(status) => crate::log!("{name}: the provider exited ({status})"),
                        Err(e) => crate::log!("{name}: lost the provider ({e})"),
                    },
                    Err(e) => crate::log!("{name}: cannot start the provider ({e})"),
                }
                // Quickly after a long run; ever more slowly while it keeps failing.
                wait = if since.elapsed() > RESTART_MAX { RESTART_FIRST } else { (wait * 2).min(RESTART_MAX) };
                std::thread::sleep(wait);
            }
        });
        if let Err(e) = started {
            crate::log!("{name}: cannot start the provider ({e})");
        }
        link
    }

    fn start_child(self: &Arc<Self>, flags: &[String]) -> io::Result<std::process::Child> {
        use std::process::{Command, Stdio};
        let mut command = Command::new(std::env::current_exe()?);
        command.args(["--provider", self.name]).args(flags);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn()?;
        let (to, from) = (child.stdin.take(), child.stdout.take());
        if let (Some(to), Some(from)) = (to, from) {
            self.attach(to, from);
        }
        Ok(child)
    }

    /// Make the provider on these pipes the one serving this link, and open
    /// on it every channel that is open already.
    pub fn attach(self: &Arc<Self>, to: impl Write + Send + 'static, from: impl Read + Send + 'static) {
        let generation = {
            let mut pipe = self.to.lock().unwrap();
            pipe.0 += 1;
            let mut to: Box<dyn Write + Send> = Box::new(to);
            let channels = self.channels.lock().unwrap();
            for (id, channel) in channels.iter() {
                let _ = write_frame(&mut to, OPEN, *id, &channel.open);
            }
            if !channels.is_empty() {
                crate::log!("{}: the new provider took over {} open connection(s)", self.name, channels.len());
            }
            pipe.1 = Some(to);
            pipe.0
        };
        let link = self.clone();
        let reader = std::thread::Builder::new().name(format!("from-{}", self.name)).spawn(move || {
            let mut from = from;
            while let Ok((kind, channel, payload)) = read_frame(&mut from) {
                link.receive(kind, channel, payload);
            }
            let mut pipe = link.to.lock().unwrap();
            // Unless another provider has taken over since.
            if pipe.0 == generation {
                pipe.1 = None;
            }
        });
        if let Err(e) = reader {
            crate::log!("{}: cannot read from the provider ({e})", self.name);
        }
    }

    fn receive(&self, kind: u8, channel: u64, payload: Vec<u8>) {
        match kind {
            TEXT | BINARY => {
                let out = match kind {
                    BINARY => Out::Binary(payload),
                    _ => match String::from_utf8(payload) {
                        Ok(text) => Out::Text(text),
                        Err(_) => return,
                    },
                };
                if let Some(c) = self.channels.lock().unwrap().get(&channel) {
                    let _ = c.tx.send(out);
                }
            }
            STATE => {
                if let Some(c) = self.channels.lock().unwrap().get_mut(&channel) {
                    c.open = payload;
                }
            }
            LOG => crate::log!("{}", String::from_utf8_lossy(&payload)),
            _ => {}
        }
    }

    fn send(pipe: &mut Pipe, kind: u8, channel: u64, payload: &[u8]) {
        if let Some(to) = &mut pipe.1 {
            let _ = write_frame(to, kind, channel, payload);
        }
    }

    /// Open `channel`, with the query the client connected with. What the
    /// provider sends on it goes to `tx`.
    pub fn open(&self, channel: u64, query: &HashMap<String, String>, tx: UnboundedSender<Out>) {
        let open = serde_json::to_vec(query).unwrap_or_default();
        let mut pipe = self.to.lock().unwrap();
        Self::send(&mut pipe, OPEN, channel, &open);
        self.channels.lock().unwrap().insert(channel, Channel { open, tx });
    }

    /// Pass a client's message on as it arrived.
    pub fn text(&self, channel: u64, text: &str) {
        Self::send(&mut self.to.lock().unwrap(), TEXT, channel, text.as_bytes());
    }

    pub fn close(&self, channel: u64) {
        let mut pipe = self.to.lock().unwrap();
        self.channels.lock().unwrap().remove(&channel);
        Self::send(&mut pipe, CLOSE, channel, &[]);
    }

    /// One message on a channel of its own, for a request: the provider's
    /// first answer, or None if it gives none in time.
    pub async fn ask(&self, channel: u64, text: &str) -> Option<String> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        self.open(channel, &HashMap::new(), tx);
        self.text(channel, text);
        let answer = tokio::time::timeout(ANSWER_WITHIN, async {
            loop {
                match rx.recv().await {
                    Some(Out::Text(text)) => return Some(text),
                    Some(Out::Binary(_)) => continue,
                    None => return None,
                }
            }
        })
        .await;
        self.close(channel);
        answer.ok().flatten()
    }
}

// ---- the provider's end ----

/// This process's stdin and stdout as plain files: unbuffered, and with no
/// line handling in the way of binary data.
fn stdio() -> (File, File) {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        (File::from_raw_handle(io::stdin().as_raw_handle()), File::from_raw_handle(io::stdout().as_raw_handle()))
    }
    #[cfg(unix)]
    unsafe {
        use std::os::fd::FromRawFd;
        (File::from_raw_fd(0), File::from_raw_fd(1))
    }
}

/// The pipe to the core, for whatever in this process has something to
/// send: sessions, and the log.
pub struct ToCore(Mutex<File>);

impl ToCore {
    fn send(&self, kind: u8, channel: u64, payload: &[u8]) {
        let _ = write_frame(&mut *self.0.lock().unwrap(), kind, channel, payload);
    }

    /// A line for the core's log, which is the only one there is: this
    /// process has no console, and one file wants one writer.
    pub fn log(&self, line: &str) {
        self.send(LOG, 0, line.as_bytes());
    }

    /// A message for the client on `channel`.
    pub fn text(&self, channel: u64, text: &str) {
        self.send(TEXT, channel, text.as_bytes());
    }
}

/// Take this process's stdin and stdout as the pipes to and from a core.
pub fn connect() -> (File, Arc<ToCore>) {
    let (input, output) = stdio();
    (input, Arc::new(ToCore(Mutex::new(output))))
}

/// What the core asks of a provider.
pub enum Event {
    /// A channel opened.
    Open(u64),
    /// A client's message.
    Text(u64, String),
    Close(u64),
}

/// Hand each thing the core says to `on`, until the core goes away. For
/// providers simpler than the screen, which need no state kept for the next.
pub fn serve(input: File, mut on: impl FnMut(Event)) {
    let mut input = BufReader::new(input);
    while let Ok((kind, channel, payload)) = read_frame(&mut input) {
        match kind {
            OPEN => on(Event::Open(channel)),
            TEXT => {
                if let Ok(text) = String::from_utf8(payload) {
                    on(Event::Text(channel, text));
                }
            }
            CLOSE => on(Event::Close(channel)),
            _ => {}
        }
    }
}

/// `peek --provider screen`: sessions of the screen, one for each channel
/// the core opens. Returns when the core goes away.
pub fn run_screen(shared: Arc<screen::Shared>, input: File, core: Arc<ToCore>) {
    let mut input = BufReader::new(input);
    let mut sessions: HashMap<u64, screen::Session> = HashMap::new();
    while let Ok((kind, channel, payload)) = read_frame(&mut input) {
        match kind {
            OPEN => {
                let query: HashMap<String, String> = serde_json::from_slice(&payload).unwrap_or_default();
                let to = core.clone();
                let sink: Sink = Arc::new(move |out| match out {
                    Out::Text(text) => to.send(TEXT, channel, text.as_bytes()),
                    Out::Binary(bytes) => to.send(BINARY, channel, &bytes),
                });
                // Opened twice: the later one stands.
                if let Some(old) = sessions.insert(channel, screen::open(&shared, channel, &query, sink)) {
                    old.close(&shared);
                }
            }
            TEXT => {
                if let (Some(session), Ok(text)) = (sessions.get_mut(&channel), std::str::from_utf8(&payload)) {
                    session.message(&shared, text);
                    // What the client now has, for the provider after this one.
                    if let Some(query) = session.changed() {
                        core.send(STATE, channel, &serde_json::to_vec(query).unwrap_or_default());
                    }
                }
            }
            CLOSE => {
                if let Some(session) = sessions.remove(&channel) {
                    session.close(&shared);
                }
            }
            _ => {}
        }
    }
    // The core is gone, and every client with it: let go of what they held.
    for (_, session) in sessions.drain() {
        session.close(&shared);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn frames_round_trip() {
        let mut pipe = Vec::new();
        let big = vec![7u8; 300_000];
        write_frame(&mut pipe, TEXT, 3, b"{\"t\":\"ack\",\"s\":1}").unwrap();
        write_frame(&mut pipe, BINARY, u64::MAX, &big).unwrap();
        write_frame(&mut pipe, CLOSE, 3, &[]).unwrap();
        let mut r = &pipe[..];
        assert_eq!(read_frame(&mut r).unwrap(), (TEXT, 3, b"{\"t\":\"ack\",\"s\":1}".to_vec()));
        assert_eq!(read_frame(&mut r).unwrap(), (BINARY, u64::MAX, big));
        assert_eq!(read_frame(&mut r).unwrap(), (CLOSE, 3, Vec::new()));
        assert!(read_frame(&mut r).is_err(), "end of stream");
    }

    /// What a provider was sent, readable while the link still writes to it.
    #[derive(Clone, Default)]
    struct Sent(Arc<Mutex<Vec<u8>>>);

    impl Write for Sent {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Sent {
        fn frames(&self) -> Vec<(u8, u64, String)> {
            let bytes = self.0.lock().unwrap().clone();
            let mut r = &bytes[..];
            let mut frames = Vec::new();
            while let Ok((kind, channel, payload)) = read_frame(&mut r) {
                frames.push((kind, channel, String::from_utf8(payload).unwrap()));
            }
            frames
        }
    }

    fn until(what: &str, done: impl Fn() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_channel_outlives_its_provider_and_opens_again_as_it_was() {
        let link = Link::new("screen");
        let query = HashMap::from([("q".to_string(), "med".to_string())]);
        let (tx, mut rx) = unbounded_channel();

        // Opened before any provider: it waits.
        link.open(9, &query, tx);
        link.text(9, "lost: nobody to hear it");

        // The first provider is told of it, answers, says how the channel
        // stands now, and goes.
        let first = Sent::default();
        let mut from_first = Vec::new();
        write_frame(&mut from_first, TEXT, 9, b"hello").unwrap();
        write_frame(&mut from_first, BINARY, 9, &[1, 2, 3]).unwrap();
        write_frame(&mut from_first, TEXT, 4, b"for a channel that is not open").unwrap();
        write_frame(&mut from_first, STATE, 9, br#"{"q":"high"}"#).unwrap();
        link.attach(first.clone(), io::Cursor::new(from_first));
        until("the first provider to go", || link.to.lock().unwrap().1.is_none());
        assert_eq!(first.frames(), vec![(OPEN, 9, r#"{"q":"med"}"#.to_string())]);
        assert!(matches!(rx.try_recv(), Ok(Out::Text(t)) if t == "hello"));
        assert!(matches!(rx.try_recv(), Ok(Out::Binary(b)) if b == [1, 2, 3]));
        assert!(rx.try_recv().is_err(), "nothing from a channel that is not open");

        // With no provider, what the client says goes nowhere, and the
        // socket's end of the channel stays open.
        link.text(9, "lost too");
        assert!(!rx.is_closed());

        // The next provider gets the channel as the first one left it.
        let second = Sent::default();
        let (keep_open, never) = std::sync::mpsc::channel::<u8>();
        link.attach(second.clone(), Blocked(never));
        link.text(9, r#"{"t":"ack","s":1}"#);
        link.close(9);
        assert_eq!(
            second.frames(),
            vec![
                (OPEN, 9, r#"{"q":"high"}"#.to_string()),
                (TEXT, 9, r#"{"t":"ack","s":1}"#.to_string()),
                (CLOSE, 9, String::new()),
            ]
        );
        drop(keep_open);
    }

    /// A pipe that stays open and says nothing, until its sender is dropped.
    struct Blocked(std::sync::mpsc::Receiver<u8>);

    impl Read for Blocked {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            Ok(0)
        }
    }
}
