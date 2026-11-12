//! SPIKE: capabilities as separate processes, for docs/extensibility.md.
//!
//! With `--split`, this process is the core: it listens, authenticates and
//! routes, and starts one provider per capability (`peek --provider screen`,
//! `peek --provider vitals`), each on a pair of pipes (its stdin and
//! stdout). A client socket becomes one channel on each provider, and
//! whatever the providers send on that channel goes out on the socket.
//!
//! Messages on a pipe: u32 payload length, u8 kind, u64 channel, u64 stamp
//! (little-endian), then the payload. To a provider: OPEN (the client's
//! query, as JSON), TEXT (a client message, untouched), CLOSE. From one:
//! TEXT and BINARY, where the stamp is when the frame was handed over.
//!
//! A screen provider belongs to one desktop session. In the Windows service
//! (`--service --split`) the core is the service itself, where there is no
//! display, and it starts a screen provider in whichever session is at the
//! console, again when that changes. Client sockets stay open across it.
//!
//! Not here: bounding what a provider may queue, and telling the browser
//! which capability a message belongs to.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;

use crate::engine::Out;
use crate::screen::{self, Sink};
use crate::vitals::Sampler;

const OPEN: u8 = 1;
const CLOSE: u8 = 2;
const TEXT: u8 = 3;
const BINARY: u8 = 4;
const HEADER: usize = 21;
/// Larger than any frame; a length above it means the stream is out of step.
const MAX_PAYLOAD: usize = 256 << 20;

fn write_frame(w: &mut impl Write, kind: u8, channel: u64, stamp: u64, payload: &[u8]) -> io::Result<()> {
    let mut head = [0u8; HEADER];
    head[0..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    head[4] = kind;
    head[5..13].copy_from_slice(&channel.to_le_bytes());
    head[13..21].copy_from_slice(&stamp.to_le_bytes());
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

fn read_frame(r: &mut impl Read) -> io::Result<(u8, u64, u64, Vec<u8>)> {
    let mut head = [0u8; HEADER];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes(head[0..4].try_into().unwrap()) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let channel = u64::from_le_bytes(head[5..13].try_into().unwrap());
    let stamp = u64::from_le_bytes(head[13..21].try_into().unwrap());
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok((head[4], channel, stamp, payload))
}

// ---- the core's end ----

/// One open channel: where it goes, and how to open it again.
struct Channel {
    /// What the client connected with, kept up to date with what it has
    /// asked for since, so a new provider can pick the channel up as it was.
    query: HashMap<String, String>,
    tx: UnboundedSender<Out>,
}

/// A capability's channels, and the provider serving them at the moment.
/// The provider can go and another be attached (a screen provider belongs
/// to one desktop session); the channels and the sockets behind them stay.
pub struct Link {
    name: &'static str,
    /// The pipe to the provider, with a count of the providers there have
    /// been. Locked before `channels`, always.
    to: Mutex<(u64, Option<Box<dyn Write + Send>>)>,
    channels: Mutex<HashMap<u64, Channel>>,
}

impl Link {
    /// A link with no provider yet. Channels opened on it wait for one.
    pub fn detached(name: &'static str) -> Arc<Link> {
        Arc::new(Link { name, to: Mutex::new((0, None)), channels: Mutex::default() })
    }

    /// Start `peek --provider <name>` with `flags`, in this session.
    pub fn spawn(name: &'static str, flags: &[String]) -> io::Result<Arc<Link>> {
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("--provider").arg(name).args(flags);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        let mut child = command.spawn()?;
        let link = Link::detached(name);
        crate::log!("provider {name}: started (pid {})", child.id());
        link.attach(child.stdin.take().expect("piped stdin"), child.stdout.take().expect("piped stdout"));
        Ok(link)
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
                let _ = write_frame(&mut to, OPEN, *id, 0, &serde_json::to_vec(&channel.query).unwrap_or_default());
            }
            if !channels.is_empty() {
                crate::log!("provider {}: took over {} open channel(s)", self.name, channels.len());
            }
            pipe.1 = Some(to);
            pipe.0
        };
        let link = self.clone();
        let reader = std::thread::Builder::new().name(format!("from-{}", self.name)).spawn(move || {
            let mut from = from;
            while let Ok((kind, channel, stamp, payload)) = read_frame(&mut from) {
                let out = match kind {
                    TEXT => match String::from_utf8(payload) {
                        Ok(text) => Out::Text(text),
                        Err(_) => continue,
                    },
                    BINARY => Out::Binary(payload, stamp),
                    _ => continue,
                };
                if let Some(c) = link.channels.lock().unwrap().get(&channel) {
                    let _ = c.tx.send(out);
                }
            }
            let mut pipe = link.to.lock().unwrap();
            // Unless another provider has taken over since.
            if pipe.0 == generation {
                pipe.1 = None;
                crate::log!("provider {}: stopped; its channels wait for the next one", link.name);
            }
        });
        if let Err(e) = reader {
            crate::log!("provider {}: cannot read from it ({e})", self.name);
        }
    }

    fn send(pipe: &mut (u64, Option<Box<dyn Write + Send>>), kind: u8, channel: u64, payload: &[u8]) {
        if let Some(to) = &mut pipe.1 {
            let _ = write_frame(to, kind, channel, 0, payload);
        }
    }

    /// Open `channel`, with the query the client connected with. What the
    /// provider sends on it goes to `tx`.
    pub fn open(&self, channel: u64, query: &HashMap<String, String>, tx: UnboundedSender<Out>) {
        let mut pipe = self.to.lock().unwrap();
        self.channels.lock().unwrap().insert(channel, Channel { query: query.clone(), tx });
        Self::send(&mut pipe, OPEN, channel, &serde_json::to_vec(query).unwrap_or_default());
    }

    /// Pass a client's message on as it arrived.
    pub fn text(&self, channel: u64, text: &str) {
        Self::send(&mut self.to.lock().unwrap(), TEXT, channel, text.as_bytes());
    }

    /// Note something the client has asked for since it connected, for
    /// when its channel is opened again on another provider.
    pub fn remember(&self, channel: u64, key: &str, value: String) {
        if let Some(c) = self.channels.lock().unwrap().get_mut(&channel) {
            c.query.insert(key.into(), value);
        }
    }

    pub fn close(&self, channel: u64) {
        let mut pipe = self.to.lock().unwrap();
        self.channels.lock().unwrap().remove(&channel);
        Self::send(&mut pipe, CLOSE, channel, &[]);
    }
}

// ---- how long a frame takes from the engine to the socket ----

static HOPS: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

/// A frame handed over at `stamp` has just been written to its socket.
pub fn record_hop(stamp: u64, bytes: usize) {
    let us = screen::now_us().saturating_sub(stamp).min(u32::MAX as u64) as u32;
    let mut hops = HOPS.lock().unwrap();
    if hops.len() < 1_000_000 {
        hops.push((us, bytes as u32));
    }
}

/// Summarise the frames since the last call, and append them to the file
/// `PEEK_HOPS` names (microseconds, bytes) for a benchmark to read.
pub fn report_hops(verbose: bool) {
    let mut hops = std::mem::take(&mut *HOPS.lock().unwrap());
    if hops.is_empty() {
        return;
    }
    if let Some(path) = std::env::var_os("PEEK_HOPS")
        && let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let lines: String = hops.iter().map(|(us, bytes)| format!("{us},{bytes}\n")).collect();
        let _ = file.write_all(lines.as_bytes());
    }
    if verbose {
        hops.sort_unstable();
        let at = |p: usize| hops[(hops.len() * p / 100).min(hops.len() - 1)].0;
        crate::log!(
            "hops: {} frames from engine to socket: median {} us, p99 {} us, longest {} us",
            hops.len(),
            at(50),
            at(99),
            hops[hops.len() - 1].0
        );
    }
}

// ---- a provider's end ----

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

/// `peek --provider screen`: sessions of the screen, one per channel.
/// Returns when the core goes away.
pub fn run_screen(shared: Arc<screen::Shared>) {
    let (input, output) = stdio();
    let output = Arc::new(Mutex::new(output));
    let mut input = BufReader::new(input);
    let mut sessions: HashMap<u64, screen::Session> = HashMap::new();
    while let Ok((kind, channel, _, payload)) = read_frame(&mut input) {
        match kind {
            OPEN => {
                let query: HashMap<String, String> = serde_json::from_slice(&payload).unwrap_or_default();
                let output = output.clone();
                let sink: Sink = Arc::new(move |out| {
                    let mut w = output.lock().unwrap();
                    let _ = match out {
                        Out::Text(text) => write_frame(&mut *w, TEXT, channel, 0, text.as_bytes()),
                        Out::Binary(bytes, stamp) => write_frame(&mut *w, BINARY, channel, stamp, &bytes),
                    };
                });
                // Opened twice (the core says so again to a provider that has
                // just started): the later one stands.
                if let Some(old) = sessions.insert(channel, screen::open(&shared, channel, &query, sink)) {
                    old.close(&shared);
                }
            }
            TEXT => {
                if let (Some(session), Ok(text)) = (sessions.get_mut(&channel), std::str::from_utf8(&payload)) {
                    session.message(&shared, text);
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

/// `peek --provider vitals`: a reading a second on every open channel, and
/// one straight away on a channel that has just opened. Reads nothing while
/// no channel is open. Returns when the core goes away.
pub fn run_vitals() {
    #[derive(Default)]
    struct Channels {
        all: Vec<u64>,
        new: Vec<u64>,
        gone: bool,
    }
    let (input, mut output) = stdio();
    let state: Arc<(Mutex<Channels>, Condvar)> = Arc::default();
    {
        let state = state.clone();
        std::thread::spawn(move || {
            let (lock, wake) = &*state;
            let mut sampler = Sampler::default();
            let mut ticked = Instant::now() - Duration::from_secs(1);
            loop {
                let mut channels = lock.lock().unwrap();
                while channels.all.is_empty() && !channels.gone {
                    channels = wake.wait(channels).unwrap();
                }
                let due = ticked + Duration::from_secs(1);
                while channels.new.is_empty() && !channels.gone && Instant::now() < due {
                    channels = wake.wait_timeout(channels, due - Instant::now()).unwrap().0;
                }
                if channels.gone {
                    return;
                }
                let to = if Instant::now() >= due {
                    ticked = Instant::now();
                    channels.new.clear();
                    channels.all.clone()
                } else {
                    std::mem::take(&mut channels.new)
                };
                drop(channels);
                if to.is_empty() {
                    continue;
                }
                let text = sampler.snapshot();
                for channel in to {
                    if write_frame(&mut output, TEXT, channel, 0, text.as_bytes()).is_err() {
                        return;
                    }
                }
            }
        });
    }
    let (lock, wake) = &*state;
    let mut input = BufReader::new(input);
    while let Ok((kind, channel, _, _)) = read_frame(&mut input) {
        let mut channels = lock.lock().unwrap();
        match kind {
            OPEN => {
                channels.all.push(channel);
                channels.new.push(channel);
            }
            CLOSE => {
                channels.all.retain(|&c| c != channel);
                channels.new.retain(|&c| c != channel);
            }
            _ => {}
        }
        wake.notify_all();
    }
    lock.lock().unwrap().gone = true;
    wake.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut pipe = Vec::new();
        let big = vec![7u8; 300_000];
        write_frame(&mut pipe, TEXT, 3, 0, b"{\"t\":\"ack\",\"s\":1}").unwrap();
        write_frame(&mut pipe, BINARY, u64::MAX, 123_456_789_012, &big).unwrap();
        write_frame(&mut pipe, CLOSE, 3, 0, &[]).unwrap();
        let mut r = &pipe[..];
        assert_eq!(read_frame(&mut r).unwrap(), (TEXT, 3, 0, b"{\"t\":\"ack\",\"s\":1}".to_vec()));
        assert_eq!(read_frame(&mut r).unwrap(), (BINARY, u64::MAX, 123_456_789_012, big));
        assert_eq!(read_frame(&mut r).unwrap(), (CLOSE, 3, 0, Vec::new()));
        assert!(read_frame(&mut r).is_err(), "end of stream");
    }
}
