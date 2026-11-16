//! What is running on a machine, live, for peek: a light task manager, as a
//! plugin.
//!
//! Peek builds it in as its own `processes` app, off by default, and serves
//! `PAGE` itself. It has nothing of peek's in it, so it also builds alone as
//! a plugin (`src/main.rs`), as `apps/files` does.
//!
//! Nothing is read while nobody looks:
//!
//! - **The page** sends `{"t":"watch"}` while it is visible and
//!   `{"t":"pause"}` when it is hidden. While at least one page watches, the
//!   processes are read once a second, and each page gets only what changed
//!   since what it was sent last (`tick`, below).
//! - **The card** asks `{"t":"view","place":"card"}` every so often, and the
//!   answer is empty unless something stands out: a process group using a
//!   good share of the processor, or memory nearly full.
//!
//! Ending a process is `{"t":"do","do":{"t":"end","pid":..,"start":"..","force":false}}`,
//! so a view-only host refuses it before it gets here. `start` is when the
//! process started, as the page was told: a process ended since, whose
//! number has gone to another, is not the one ended. Peek itself, the
//! machine's first process and processes Windows cannot run without are
//! never ended; the system's and other users' only with the `end_any`
//! setting.
//!
//! `tick`, to a page:
//!
//! ```json
//! { "t": "tick", "full": true, "cpu": 123, "mem": [used, total], "cores": 8, "force": true,
//!   "add": [[pid, "start", ppid, "name", "user", "cmd", sys, cpu, mem]],
//!   "set": [[pid, cpu, mem]], "gone": [pid] }
//! ```
//!
//! `cpu` is in tenths of a percent of the whole machine (1000 is every core
//! busy), or null until there are two readings. `mem` is in bytes: the
//! resident set on Linux, the private working set on Windows (as Task
//! Manager's Memory column). `sys` marks the system's own processes.

use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(windows, path = "windows.rs")]
mod sys;

// What a message on the pipe is: u32 length, u8 kind, u64 channel
// (little-endian), then the payload.
const CLOSE: u8 = 2;
const TEXT: u8 = 3;
const LOG: u8 = 6;
const ABOUT: u8 = 7;
const HEADER: usize = 13;
/// More than a page has to ask.
const MAX_ASK: usize = 64 << 10;

/// How often a watching page hears.
const EVERY: Duration = Duration::from_secs(1);
/// The first reading has no processor figures; the second comes this soon.
const SOON: Duration = Duration::from_millis(400);
/// A reading this recent is used as it is, by the card.
const FRESH: Duration = Duration::from_millis(1500);
/// Older than this, a reading is no base for the card's figures.
const STALE: Duration = Duration::from_secs(60);
/// The card names a group using this much of the machine…
const CARD_BUSY: u32 = 250;
/// …and, beside it, others using this much.
const CARD_ALSO: u32 = 100;
/// Memory this full is on the card.
const CARD_MEMORY: f64 = 0.9;
/// A change of memory smaller than this (and than 1%) is not sent.
const MEM_STEP: u64 = 256 << 10;

pub const PAGE: &str = include_str!("../page/index.html");

/// One process as the platform reads it, every time.
pub(crate) struct Raw {
    pub pid: u32,
    pub ppid: u32,
    /// When it started, in the platform's units: with the pid, which process this is.
    pub start: u64,
    /// Processor time so far, in `sys::cpu_units()` a second.
    pub cpu: u64,
    pub mem: u64,
    pub name: String,
    /// Windows' session; 0 elsewhere.
    #[allow(dead_code)]
    pub session: u32,
}

/// What the platform reads of the machine at once.
pub(crate) struct Snapshot {
    pub procs: Vec<Raw>,
    /// The machine's processor time so far: (busy, total), in any unit.
    pub times: Option<(u64, u64)>,
    /// Physical memory: (used, total), in bytes.
    pub mem: (u64, u64),
}

/// What is read of a process once, when it is first seen.
#[derive(Clone)]
pub(crate) struct Meta {
    pub name: String,
    pub user: String,
    pub cmd: String,
    pub sys: bool,
}

struct Proc {
    start: u64,
    ppid: u32,
    meta: Meta,
    cpu_time: u64,
    /// Tenths of a percent of the machine, since the reading before.
    cpu: Option<u32>,
    mem: u64,
}

struct Sample {
    at: Instant,
    procs: HashMap<u32, Proc>,
    times: Option<(u64, u64)>,
    cpu: Option<u32>,
    mem: (u64, u64),
}

#[derive(Default)]
struct Sampler {
    last: Option<Sample>,
    cores: u32,
}

impl Sampler {
    fn sample(&mut self) -> &Sample {
        let snap = sys::snapshot();
        let at = Instant::now();
        let prev = self.last.take();
        let elapsed = prev.as_ref().map(|p| at.duration_since(p.at).as_secs_f64()).filter(|e| *e > 0.05);
        let cores = self.cores.max(1) as f64;
        let mut procs = HashMap::with_capacity(snap.procs.len());
        for raw in snap.procs {
            let before = prev.as_ref().and_then(|p| p.procs.get(&raw.pid)).filter(|p| p.start == raw.start);
            let cpu = match (before, elapsed) {
                (Some(b), Some(e)) => {
                    let share = raw.cpu.saturating_sub(b.cpu_time) as f64 / (e * sys::cpu_units() * cores);
                    Some((share.clamp(0.0, 1.0) * 1000.0).round() as u32)
                }
                _ => None,
            };
            let meta = match before {
                Some(b) => b.meta.clone(),
                None => sys::meta(&raw),
            };
            procs.insert(raw.pid, Proc { start: raw.start, ppid: raw.ppid, meta, cpu_time: raw.cpu, cpu, mem: raw.mem });
        }
        let cpu = match (prev.as_ref().and_then(|p| p.times), snap.times) {
            (Some((b0, t0)), Some((b1, t1))) if t1 > t0 => Some(((b1.saturating_sub(b0)) as f64 / (t1 - t0) as f64 * 1000.0).round().min(1000.0) as u32),
            _ => None,
        };
        self.last.insert(Sample { at, procs, times: snap.times, cpu, mem: snap.mem })
    }

    /// A reading with processor figures, for the card: the last one if it
    /// is recent, or a new one compared with it.
    fn for_card(&mut self) -> &Sample {
        let age = self.last.as_ref().map(|s| s.at.elapsed());
        if age.is_none_or(|a| a > STALE) {
            self.sample();
            std::thread::sleep(SOON);
        } else if age.is_some_and(|a| a <= FRESH) && self.last.as_ref().is_some_and(|s| s.cpu.is_some()) {
            return self.last.as_ref().unwrap();
        }
        self.sample()
    }
}

/// A page that watches, and what it was last sent of each process.
#[derive(Default)]
struct Watcher {
    on: bool,
    sent: HashMap<u32, (u64, Option<u32>, u64)>,
}

impl Watcher {
    fn tick(&mut self, s: &Sample, extra: &Value) -> Value {
        let full = self.sent.is_empty();
        let (mut add, mut set, mut gone) = (Vec::new(), Vec::new(), Vec::new());
        for (pid, p) in &s.procs {
            match self.sent.get(pid) {
                Some(&(start, cpu, mem)) if start == p.start => {
                    let moved = p.mem.abs_diff(mem);
                    let mem_changed = moved >= MEM_STEP && moved * 100 >= mem;
                    if cpu != p.cpu || mem_changed {
                        set.push(json!([pid, p.cpu, p.mem]));
                        self.sent.insert(*pid, (p.start, p.cpu, if mem_changed { p.mem } else { mem }));
                    }
                }
                // New, or its number went to another process: the page replaces it.
                _ => {
                    let m = &p.meta;
                    add.push(json!([pid, p.start.to_string(), p.ppid, m.name, m.user, m.cmd, m.sys, p.cpu, p.mem]));
                    self.sent.insert(*pid, (p.start, p.cpu, p.mem));
                }
            }
        }
        self.sent.retain(|pid, _| {
            let here = s.procs.contains_key(pid);
            if !here { gone.push(*pid); }
            here
        });
        let mut tick = json!({ "t": "tick", "full": full, "cpu": s.cpu, "mem": [s.mem.0, s.mem.1], "add": add, "set": set, "gone": gone });
        if full {
            for (k, v) in extra.as_object().into_iter().flatten() { tick[k] = v.clone(); }
        }
        tick
    }
}

struct Pipe(BufWriter<io::Stdout>);

impl Pipe {
    fn send(&mut self, kind: u8, channel: u64, payload: &[u8]) {
        let mut head = [0u8; HEADER];
        head[..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        head[4] = kind;
        head[5..].copy_from_slice(&channel.to_le_bytes());
        // Peek has gone if this fails, and stdin ends next.
        let _ = self.0.write_all(&head).and_then(|_| self.0.write_all(payload)).and_then(|_| self.0.flush());
    }
    fn json(&mut self, kind: u8, channel: u64, value: Value) {
        self.send(kind, channel, value.to_string().as_bytes());
    }
    fn log(&mut self, line: &str) {
        self.send(LOG, 0, format!("processes: {line}").as_bytes());
    }
}

enum Event {
    Text(u64, Value),
    Close(u64),
}

/// What may be ended, and by whom.
pub(crate) struct Policy {
    /// The system's and other users' processes too.
    pub any: bool,
    /// Never these: this program and peek, which started it.
    pub keep: [u32; 2],
}

pub fn run(page: Option<std::path::PathBuf>) {
    let mut pipe = Pipe(BufWriter::new(io::stdout()));
    let settings: Value = std::env::var("PEEK_SETTINGS").ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    let view_only = std::env::var("PEEK_VIEW_ONLY").is_ok_and(|v| v == "1");
    let policy = Policy { any: settings["end_any"].as_bool().unwrap_or(false), keep: [std::process::id(), sys::parent()] };

    let mut about = json!({ "places": ["card"] });
    if let Some(page) = page { about["page"] = page.display().to_string().into(); }
    pipe.json(ABOUT, 0, about);

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || read(tx));

    let mut sampler = Sampler { last: None, cores: sys::cores() };
    // What a page hears once, with its first tick.
    let extra = json!({ "cores": sampler.cores, "force": sys::FORCE, "ending": !view_only, "any": policy.any });
    let mut watchers: HashMap<u64, Watcher> = HashMap::new();
    let mut next: Option<Instant> = None;

    loop {
        let wait = next.map_or(Duration::from_secs(3600), |n| n.saturating_duration_since(Instant::now()));
        match rx.recv_timeout(wait) {
            Ok(Event::Close(ch)) => { watchers.remove(&ch); }
            Ok(Event::Text(ch, ask)) => match ask["t"].as_str() {
                Some("watch") => {
                    let w = watchers.entry(ch).or_default();
                    if !w.on {
                        w.on = true;
                        w.sent.clear();
                        let fresh = sampler.last.as_ref().is_some_and(|s| s.at.elapsed() <= FRESH);
                        if !fresh { sampler.sample(); }
                        let s = sampler.last.as_ref().unwrap();
                        let tick = w.tick(s, &extra);
                        pipe.json(TEXT, ch, tick);
                        let soon = Instant::now() + if s.cpu.is_none() { SOON } else { EVERY };
                        next = Some(next.map_or(soon, |n| n.min(soon)));
                    }
                }
                Some("pause") => {
                    if let Some(w) = watchers.get_mut(&ch) { w.on = false; w.sent.clear(); }
                }
                // The page's heartbeat: peek closes a page socket that says nothing for 15 s.
                Some("alive") => {}
                Some("view") => {
                    let view = card(sampler.for_card());
                    pipe.json(TEXT, ch, json!({ "t": "view", "place": ask["place"], "view": view }));
                }
                Some("do") => {
                    let reply = if view_only {
                        Err("this machine is view-only".to_string())
                    } else {
                        end(&ask["do"], &policy)
                    };
                    let reply = match reply {
                        Ok(pid) => json!({ "t": "ended", "pid": pid, "ref": ask["do"]["ref"] }),
                        Err(why) => json!({ "t": "error", "why": why, "ref": ask["do"]["ref"] }),
                    };
                    pipe.json(TEXT, ch, reply);
                }
                _ => pipe.json(TEXT, ch, json!({ "t": "error", "why": "processes does not know that" })),
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if next.is_some_and(|n| Instant::now() >= n) {
            if watchers.values().any(|w| w.on) {
                let s = sampler.sample();
                for (ch, w) in watchers.iter_mut().filter(|(_, w)| w.on) {
                    let tick = w.tick(s, &extra);
                    pipe.json(TEXT, *ch, tick);
                }
                next = Some(Instant::now() + EVERY);
            } else {
                next = None;
            }
        }
    }
    pipe.log("stdin ended");
}

/// Peek's messages, until it closes the pipe.
fn read(tx: mpsc::Sender<Event>) {
    let mut input = BufReader::new(io::stdin().lock());
    let mut head = [0u8; HEADER];
    while input.read_exact(&mut head).is_ok() {
        let length = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
        let kind = head[4];
        let channel = u64::from_le_bytes(head[5..].try_into().unwrap());
        if length > MAX_ASK {
            if io::copy(&mut (&mut input).take(length as u64), &mut io::sink()).is_err() { break; }
            continue;
        }
        let mut payload = vec![0; length];
        if input.read_exact(&mut payload).is_err() { break; }
        let event = match kind {
            TEXT => match serde_json::from_slice::<Value>(&payload) {
                Ok(ask) => Event::Text(channel, ask),
                Err(_) => continue,
            },
            CLOSE => Event::Close(channel),
            _ => continue,
        };
        if tx.send(event).is_err() { break; }
    }
}

fn end(what: &Value, policy: &Policy) -> Result<u32, String> {
    if what["t"].as_str() != Some("end") {
        return Err("processes does not know that".into());
    }
    let pid = what["pid"].as_u64().and_then(|p| u32::try_from(p).ok()).ok_or("which process?")?;
    let start = what["start"].as_str().and_then(|s| s.parse::<u64>().ok()).ok_or("which process?")?;
    if policy.keep.contains(&pid) {
        return Err("that is peek itself".into());
    }
    sys::end(pid, start, what["force"].as_bool().unwrap_or(false), policy)?;
    Ok(pid)
}

/// The card: nothing, unless something stands out.
fn card(s: &Sample) -> Value {
    let mut items = Vec::new();
    // By name: a browser is many processes, and one thing to the person.
    let mut groups: HashMap<&str, (u32, u64)> = HashMap::new();
    for p in s.procs.values() {
        let g = groups.entry(p.meta.name.as_str()).or_default();
        g.0 += p.cpu.unwrap_or(0);
        g.1 += p.mem;
    }
    let mut by_cpu: Vec<_> = groups.iter().collect();
    by_cpu.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    if by_cpu.first().is_some_and(|(_, g)| g.0 >= CARD_BUSY) {
        let named: Vec<String> = by_cpu.iter().take(3).filter(|(_, g)| g.0 >= CARD_ALSO).map(|(n, g)| format!("{n} {}", percent(g.0))).collect();
        items.push(json!({ "kind": "text", "text": format!("Busy: {}", named.join(", ")) }));
    }
    let (used, total) = s.mem;
    if total > 0 && used as f64 / total as f64 >= CARD_MEMORY {
        let top = groups.iter().max_by_key(|(_, g)| g.1);
        let most = top.map(|(n, g)| format!(", most by {n} ({})", size(g.1))).unwrap_or_default();
        items.push(json!({ "kind": "text", "tone": "error", "text": format!("Memory {} used{most}", percent((used as f64 / total as f64 * 1000.0) as u32)) }));
    }
    json!({ "items": items })
}

fn percent(tenths: u32) -> String {
    format!("{} %", tenths.div_ceil(10))
}

fn size(bytes: u64) -> String {
    let gb = bytes as f64 / (1u64 << 30) as f64;
    if gb >= 1.0 { format!("{gb:.1} GB") } else { format!("{} MB", bytes >> 20) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(name: &str, cpu: u32, mem: u64) -> Proc {
        Proc { start: 1, ppid: 0, meta: Meta { name: name.into(), user: String::new(), cmd: String::new(), sys: false }, cpu_time: 0, cpu: Some(cpu), mem }
    }

    fn sample(procs: Vec<(u32, Proc)>, mem: (u64, u64)) -> Sample {
        Sample { at: Instant::now(), procs: procs.into_iter().collect(), times: None, cpu: Some(0), mem }
    }

    #[test]
    fn the_card_is_quiet_unless_something_stands_out() {
        let calm = sample(vec![(1, proc("a", 50, 10)), (2, proc("b", 20, 10))], (1, 10));
        assert_eq!(card(&calm)["items"], json!([]));
        // Three processes of one name are one group, at 30 %.
        let busy = sample(vec![(1, proc("chrome", 100, 1 << 30)), (2, proc("chrome", 100, 1 << 30)), (3, proc("chrome", 100, 0)), (4, proc("rustc", 120, 0)), (5, proc("x", 5, 0))], (95, 100));
        let items = card(&busy)["items"].clone();
        assert_eq!(items[0]["text"], "Busy: chrome 30 %, rustc 12 %");
        assert_eq!(items[1]["text"], "Memory 95 % used, most by chrome (2.0 GB)");
    }

    #[test]
    fn a_page_hears_only_what_changed() {
        let mut w = Watcher { on: true, ..Default::default() };
        let first = w.tick(&sample(vec![(1, proc("a", 10, 10 << 20)), (2, proc("b", 0, 10 << 20))], (1, 2)), &json!({"cores": 4}));
        assert_eq!(first["full"], true);
        assert_eq!(first["cores"], 4);
        assert_eq!(first["add"].as_array().unwrap().len(), 2);

        // b's memory moved by less than a step; a's processor changed; c is new; nothing went.
        let mut b = proc("b", 0, (10 << 20) + 4096);
        b.start = 1;
        let next = w.tick(&sample(vec![(1, proc("a", 30, 10 << 20)), (2, b), (3, proc("c", 0, 0))], (1, 2)), &json!({}));
        assert_eq!(next["full"], false);
        assert!(next.get("cores").is_none());
        assert_eq!(next["set"], json!([[1, 30, 10 << 20]]));
        assert_eq!(next["add"].as_array().unwrap().len(), 1);
        assert_eq!(next["gone"], json!([]));

        // 2 ended, and its number went to another process.
        let mut reused = proc("d", 0, 0);
        reused.start = 99;
        let last = w.tick(&sample(vec![(2, reused)], (1, 2)), &json!({}));
        assert_eq!(last["add"][0][3], "d");
        let mut gone: Vec<u64> = last["gone"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
        gone.sort();
        assert_eq!(gone, [1, 3]);
    }

    #[test]
    fn this_machine_reads_as_a_list_of_processes() {
        let mut s = Sampler { last: None, cores: sys::cores() };
        let first = s.sample();
        let me = first.procs.get(&std::process::id()).expect("this test's own process");
        assert!(!me.meta.name.is_empty());
        assert!(first.mem.1 > 0);
        std::thread::sleep(Duration::from_millis(100));
        let second = s.sample();
        assert!(second.cpu.is_some());
        assert!(second.procs.get(&std::process::id()).unwrap().cpu.is_some());
    }

    #[test]
    fn peek_and_strangers_are_not_ended() {
        let policy = Policy { any: false, keep: [std::process::id(), sys::parent()] };
        let me = json!({ "t": "end", "pid": std::process::id(), "start": "0" });
        assert_eq!(end(&me, &policy).unwrap_err(), "that is peek itself");
        // The machine's first process.
        let init = json!({ "t": "end", "pid": 1, "start": "0" });
        assert!(end(&init, &policy).is_err());
    }
}
