//! The power provider (`peek --provider power`): putting this machine to
//! sleep, and waking another one on the same network.
//!
//! A machine asleep runs nothing, peek included, so it cannot be asked to
//! wake. Another machine on its network can wake it, with a Wake-on-LAN
//! packet to its network card; a browser cannot send one. And no machine
//! knows of the others: only the browser is paired with them all.
//!
//! So each machine's provider says what the same provider on the others
//! may know of it (its `record`: the card to wake it through, its
//! networks), the browser carries those between them, and each then says
//! what to show (docs/providers.md). The browser draws that and knows
//! nothing of sleep or wake:
//!
//! - On its own card: Sleep, or why not, when another machine on its
//!   network is there to wake it.
//! - On the card of a machine it can wake, while that one is away: Wake.
//!
//! Whether waking works also depends on the firmware, which no program can
//! read. So it is tried once, as a test, and neither half of the test needs
//! the browser to stay open:
//!
//! - The machine that will send the packet is asked first, and counts the
//!   seconds itself (`wake` with `after`).
//! - The machine going to sleep sets a wake timer before it does, where
//!   Windows allows them, so a packet that wakes nothing costs two minutes
//!   and not a walk to the power button. When it is back it knows how the
//!   test went from when it woke: soon after the packets, or only at the
//!   timer. It keeps that itself.
//!
//! Requests, one per channel, each with one answer:
//! - `{"t":"view","place":"card","peers":[..]}`: what to show on the cards.
//! - `{"t":"do","do":{..},"peers":[..]}`: a button of this machine's was
//!   pressed; `do` is one of the requests below. Answers with the view.
//! - `{"t":"peer","do":{..}}`: the same, on another machine's behalf: only
//!   `wake`. Answers `{"t":"ok"}`.
//! - `{"t":"info"}`: what this machine can do; see `info`.
//! - `{"t":"sleep"}`: answers `{"t":"sleeping"}`, then sleeps. With
//!   `"test":true` it is the test above.
//! - `{"t":"wake","mac":"..","addr":"192.168.1.20/24"}`: sends the packet
//!   onto that network, if this machine is on it. Answers `{"t":"wake-sent"}`.
//!   With `"after":15` it answers `{"t":"wake-armed"}` at once and sends the
//!   packet that many seconds later, and three more times after that.
//!
//! Anything refused answers `{"t":"error","why":".."}`.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::provider::{self, Event};

/// Wake-on-LAN's customary port.
const WOL_PORT: u16 = 9;
/// Long enough for the answer to reach the browser before the network goes.
const SLEEP_AFTER: Duration = Duration::from_secs(1);
const TESTED_FILE: &str = "wake-tested.json";

// The test. The other machine sends its packets 15, 25, 35 and 45 seconds
// after it is asked (`TEST_WAKE_AFTER`, and `WAKE_AGAIN`), in case this one
// is slow to fall asleep. Its own timer is well after the last.
/// How long after it is asked the other machine sends a test's first packet.
const TEST_WAKE_AFTER: u64 = 15;
/// When a machine under test wakes itself, if nothing else has.
const TEST_TIMER: Duration = Duration::from_secs(120);
/// Awake again within this of the test starting: a packet woke it. Later:
/// the timer did, or somebody at the machine.
const WOKEN_BY_PACKET_WITHIN: Duration = Duration::from_secs(75);
/// Woken with nobody at it, Windows goes back to sleep within minutes. Long
/// enough to connect and read how the test went.
const STAY_AWAKE: Duration = Duration::from_secs(600);
/// A delayed wake is sent this many more times, this far apart.
const WAKE_AGAIN: (u32, Duration) = (3, Duration::from_secs(10));
/// The longest a wake can be put off.
const MAX_WAKE_AFTER: u64 = 300;

// What the cards say, and for how long.
/// A machine told to sleep says so for this long, while it still answers.
const GOING_TO_SLEEP: Duration = Duration::from_secs(30);
/// From a wake's first packet: a machine not back by now did not wake.
const AWAKE_WITHIN: Duration = Duration::from_secs(90);
/// The same for one that wakes itself on a timer if no packet does, two
/// minutes into its test, and then has to reconnect.
const AWAKE_BY_TIMER_WITHIN: Duration = Duration::from_secs(225);
/// How a test went is said on the card for this long after it.
const SAY_TEST_FOR: Duration = Duration::from_secs(600);
/// A wake that woke nothing is told to the others for this long.
const TELL_UNWOKEN_FOR: u64 = 7 * 86400;

/// What to do once a request's answer is sent.
#[derive(Debug, PartialEq)]
enum Then {
    Nothing,
    Sleep { test: bool },
    /// Send the packet for `mac` from `local` to `broadcast`, `after` from now.
    WakeLater { mac: [u8; 6], local: Ipv4Addr, broadcast: Ipv4Addr, after: Duration },
}

/// What this provider remembers between requests.
#[derive(Default)]
struct State {
    /// The wakes it has sent or is about to, by the card they are for.
    wakes: HashMap<String, Wake>,
    /// Wakes that woke nothing: the card, and when (seconds since 1970).
    /// Part of this machine's record, so the machine it is about hears.
    unwoken: BTreeMap<String, u64>,
    /// How this machine's last test went, to say so for a while after.
    last_test: Option<LastTest>,
    /// When this machine was last told to sleep.
    sleeping: Option<SystemTime>,
    /// It had a card to be woken through, the last time it was asked.
    had_card: bool,
}

struct Wake {
    /// When its first packet goes, or went.
    at: SystemTime,
    /// The machine wakes itself on a timer if no packet does.
    timer: bool,
}

struct LastTest {
    ok: bool,
    /// Seconds from the test beginning to the machine being awake again.
    took: u64,
    at: SystemTime,
}

/// Run the provider until the core goes away.
pub fn run(input: File, core: Arc<provider::ToCore>, view_only: bool) {
    // It shows on the cards, and wants to hear of the other machines.
    core.about(r#"{"places":["card"],"peers":true}"#);
    let state: Arc<Mutex<State>> = Arc::default();
    provider::serve(input, |event| {
        if let Event::Text(channel, text) = event {
            let (answer, then) = answer(&text, view_only, &state);
            core.text(channel, &answer.to_string());
            match then {
                Then::Nothing => {}
                Then::Sleep { test } => {
                    let state = state.clone();
                    std::thread::spawn(move || sleep(test, &state));
                }
                Then::WakeLater { mac, local, broadcast, after } => drop(std::thread::spawn(move || {
                    std::thread::sleep(after);
                    for again in 0..=WAKE_AGAIN.0 {
                        if again > 0 {
                            std::thread::sleep(WAKE_AGAIN.1);
                        }
                        match send_wake(mac, local, broadcast) {
                            Ok(()) => crate::log!("power: sent a wake packet to {} on {broadcast}", format_mac(&mac)),
                            Err(e) => crate::log!("power: could not send the wake packet ({e})"),
                        }
                    }
                })),
            }
        }
    });
}

/// Go to sleep. As a test: with a wake timer set first, and keeping how it
/// went once this machine is awake again.
fn sleep(test: bool, state: &Mutex<State>) {
    let started = SystemTime::now();
    // The card now: just after waking, its link may not be up yet.
    let card = test.then(target).and_then(Result::ok).map(|t| t.mac);
    let timer = test.then(|| platform::wake_timer(started + TEST_TIMER)).flatten();
    if test {
        crate::log!(
            "power: testing sleep and wake, {}",
            if timer.is_some() { "with a wake timer in case no packet wakes this machine" } else { "with no wake timer" }
        );
    }
    std::thread::sleep(SLEEP_AFTER);
    crate::log!("power: going to sleep, as a client asked");
    // Whether or not the call returns before the machine is asleep, a
    // thread that sleeps a second at a time and finds much more has gone by
    // was stopped with the machine, and it has just woken.
    let asleep = std::thread::spawn(platform::sleep);
    if !test {
        if let Ok(Err(e)) = asleep.join() {
            crate::log!("power: could not sleep ({e})");
        }
        state.lock().unwrap().sleeping = None;
        return;
    }
    let mut asleep = Some(asleep);
    let mut last = SystemTime::now();
    let woke = loop {
        std::thread::sleep(Duration::from_secs(1));
        let now = SystemTime::now();
        let since_start = now.duration_since(started).unwrap_or_default();
        if now.duration_since(last).unwrap_or_default() > Duration::from_secs(5) {
            break Some(since_start);
        }
        // Refused: there is nothing to wait for. Otherwise keep the timer
        // set until it is due: a machine still awake then is staying awake.
        if asleep.as_ref().is_some_and(|a| a.is_finished())
            && let Some(Ok(Err(e))) = asleep.take().map(|a| a.join())
        {
            crate::log!("power: could not sleep ({e})");
            break None;
        }
        if since_start > TEST_TIMER {
            break None;
        }
        last = now;
    };
    drop(timer);
    state.lock().unwrap().sleeping = None;
    let Some(woke) = woke else {
        crate::log!("power: this machine did not go to sleep, so the test showed nothing");
        return;
    };
    let ok = by_packet(woke);
    crate::log!(
        "power: awake {} s after the test began: {}",
        woke.as_secs(),
        if ok { "a packet woke this machine" } else { "no packet woke it" }
    );
    match card.map(|mac| keep_tested(&mac, ok)) {
        Some(Ok(())) => {}
        Some(Err(why)) => crate::log!("power: could not keep how the test went ({why})"),
        None => crate::log!("power: there was no card to wake this machine through, so the test is not kept"),
    }
    state.lock().unwrap().last_test = Some(LastTest { ok, took: woke.as_secs(), at: SystemTime::now() });
    platform::stay_awake(STAY_AWAKE);
}

/// Whether a machine awake again this long after its test began was woken
/// by a packet.
fn by_packet(woke: Duration) -> bool {
    woke < WOKEN_BY_PACKET_WITHIN
}

/// Keep how the last try at waking this machine went, with its card.
fn keep_tested(mac: &[u8; 6], ok: bool) -> Result<(), String> {
    let record = json!({ "mac": format_mac(mac), "ok": ok, "at": now() });
    std::fs::write(crate::config_dir().join(TESTED_FILE), record.to_string()).map_err(|e| e.to_string())
}

/// How the last try at waking this machine through this card went, and
/// when (seconds since 1970).
fn tested(mac: &str) -> Option<(bool, u64)> {
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(crate::config_dir().join(TESTED_FILE)).ok()?).ok()?;
    (kept["mac"].as_str() == Some(mac)).then_some(())?;
    Some((kept["ok"].as_bool()?, kept["at"].as_u64()?))
}

fn error(why: impl Into<String>) -> Value {
    json!({ "t": "error", "why": why.into() })
}

fn is_error(answer: &Value) -> bool {
    answer["t"] == "error"
}

/// The answer to one request, and what to do once it is sent.
fn answer(text: &str, view_only: bool, state: &Mutex<State>) -> (Value, Then) {
    let Ok(req) = serde_json::from_str::<Value>(text) else {
        return (error("not JSON"), Then::Nothing);
    };
    match req["t"].as_str().unwrap_or("") {
        "view" if req["place"] == "card" => (card_now(&req, view_only, state), Then::Nothing),
        "view" => (error("power shows nothing there"), Then::Nothing),
        // A button on one of this machine's views.
        "do" => {
            let ask = &req["do"];
            let (done, then) = act(ask, view_only);
            if is_error(&done) {
                return (done, Then::Nothing);
            }
            {
                let mut state = state.lock().unwrap();
                if matches!(then, Then::Sleep { .. }) {
                    state.sleeping = Some(SystemTime::now());
                    // How an earlier test went is not about this sleep.
                    state.last_test = None;
                }
                if let Some(mac) = ask["mac"].as_str().and_then(parse_mac).filter(|_| ask["t"] == "wake") {
                    state.unwoken.remove(&format_mac(&mac));
                    state.wakes.insert(format_mac(&mac), Wake { at: SystemTime::now(), timer: false });
                }
            }
            (card_now(&req, view_only, state), then)
        }
        // The same provider on another machine, through the browser: it
        // may ask for a wake, and nothing else.
        "peer" => {
            let ask = &req["do"];
            if ask["t"] != "wake" {
                return (error("another machine may only ask this one to wake it"), Then::Nothing);
            }
            let (done, then) = act(ask, view_only);
            if is_error(&done) {
                return (done, Then::Nothing);
            }
            if let Some(mac) = ask["mac"].as_str().and_then(parse_mac) {
                let at = SystemTime::now() + Duration::from_secs(ask["after"].as_u64().unwrap_or(0));
                let mut state = state.lock().unwrap();
                state.unwoken.remove(&format_mac(&mac));
                state.wakes.insert(format_mac(&mac), Wake { at, timer: ask["timer"].as_bool().unwrap_or(false) });
            }
            (json!({ "t": "ok" }), then)
        }
        _ => act(&req, view_only),
    }
}

/// The requests that do something, or say what could be done.
fn act(req: &Value, view_only: bool) -> (Value, Then) {
    match req["t"].as_str().unwrap_or("") {
        "info" => (info(), Then::Nothing),
        "sleep" if view_only => (error("this machine is view-only"), Then::Nothing),
        "sleep" => match platform::can_sleep() {
            Some(_) => (json!({ "t": "sleeping" }), Then::Sleep { test: req["test"].as_bool().unwrap_or(false) }),
            None => (error("this machine cannot sleep"), Then::Nothing),
        },
        "wake" => {
            let (Some(mac), Some(addr)) = (req["mac"].as_str().and_then(parse_mac), req["addr"].as_str().and_then(parse_net))
            else {
                return (error("a wake needs mac and addr"), Then::Nothing);
            };
            wake(mac, addr, req["after"].as_u64(), &lan())
        }
        other => (error(format!("no request is called {other:?}")), Then::Nothing),
    }
}

fn now() -> u64 {
    secs(SystemTime::now())
}

fn secs(at: SystemTime) -> u64 {
    at.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---- what to show ----

/// What this machine's provider says of it to the same provider on the
/// other paired machines. The browser carries it, and keeps it for while
/// this machine is asleep.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
struct Record {
    name: String,
    /// The card to wake it through, and its address on that network
    /// ("192.168.1.20/24"); neither if nothing can wake it.
    mac: Option<String>,
    addr: Option<String>,
    adapter: Option<String>,
    /// "s3", or "modern" for Modern Standby.
    standby: Option<String>,
    /// The networks it is on, and so can wake others on.
    lan: Vec<String>,
    /// It sends wakes for others (it is not view-only).
    wakes: bool,
    /// Wakes it sent that woke nothing: the card, and when.
    unwoken: BTreeMap<String, u64>,
    /// When it last learnt whether it can be woken.
    tested_at: Option<u64>,
}

/// Another paired machine, as the browser tells of it.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Peer {
    id: String,
    name: String,
    /// It answers the browser now.
    up: bool,
    record: Option<Record>,
}

/// What a card is made from, on this machine's side.
struct Me {
    name: String,
    /// How it sleeps ("s3", "modern"), if it can.
    standby: Option<&'static str>,
    target: Result<Target, String>,
    lan: Vec<(Ipv4Addr, u8)>,
    /// How the last try at waking it went, and when.
    tested: Option<(bool, u64)>,
    /// A test that fails to wake it ends by itself, on a wake timer.
    timer: bool,
    view_only: bool,
}

/// The card view for a request, from how this machine is now.
fn card_now(req: &Value, view_only: bool, state: &Mutex<State>) -> Value {
    let peers: Vec<Peer> = serde_json::from_value(req["peers"].clone()).unwrap_or_default();
    let mut target = target();
    // A card that was there a moment ago: the machine is just awake, or its
    // cable just moved, and the link is not up yet. Give it a moment before
    // saying there is nothing to wake it through.
    if target.is_err() && state.lock().unwrap().had_card {
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(500));
            target = self::target();
            if target.is_ok() {
                break;
            }
        }
    }
    state.lock().unwrap().had_card = target.is_ok();
    let mut me = Me {
        name: crate::pairing::machine_name(),
        standby: platform::can_sleep(),
        tested: target.as_ref().ok().and_then(|t| tested(&format_mac(&t.mac))),
        target,
        lan: lan(),
        timer: platform::wake_timers_allowed(),
        view_only,
    };
    // Another machine sent this one a wake that woke nothing: that is how
    // its last try went, whatever went before.
    if let Ok(t) = &me.target
        && unwoken_since(&peers, &format_mac(&t.mac), me.tested)
        && keep_tested(&t.mac, false).is_ok()
    {
        crate::log!("power: another machine's wake did not wake this one");
        me.tested = Some((false, now()));
    }
    card(&me, &peers, &mut state.lock().unwrap(), SystemTime::now())
}

/// Whether one of the others sent this machine's card a wake that woke
/// nothing, since it last learnt whether it can be woken.
fn unwoken_since(peers: &[Peer], mac: &str, tested: Option<(bool, u64)>) -> bool {
    peers
        .iter()
        .filter_map(|p| p.record.as_ref()?.unwoken.get(mac))
        .any(|at| tested.is_none_or(|(_, learnt)| *at > learnt))
}

fn line(text: impl Into<String>, tone: &str, busy: bool) -> Value {
    let mut item = json!({ "kind": "text", "text": text.into() });
    if !tone.is_empty() {
        item["tone"] = tone.into();
    }
    if busy {
        item["busy"] = true.into();
    }
    item
}

/// What to check on a machine that no packet woke.
fn what_to_check(adapter: Option<&str>, standby: Option<&str>) -> String {
    format!(
        "Most often Wake-on-LAN is off in its BIOS or UEFI settings (\"Wake on LAN\", \"Power on by PCI-E\"), \
         or \"Wake on Magic Packet\" is off for {} in Windows' Device Manager.{}",
        adapter.unwrap_or("its network card"),
        if standby == Some("modern") {
            " It also uses Modern Standby, which can turn the network card off while asleep."
        } else {
            ""
        }
    )
}

fn on_network(a: (Ipv4Addr, u8), b: (Ipv4Addr, u8)) -> bool {
    a.1 == b.1 && u32::from(a.0) & mask(a.1) == u32::from(b.0) & mask(b.1)
}

/// What to show on the cards: this machine's own, and the cards of the
/// machines it can wake (those items say whose with `about`). With it goes
/// this machine's record, and how soon to ask again while something is
/// under way.
fn card(me: &Me, peers: &[Peer], state: &mut State, now: SystemTime) -> Value {
    let mut items = Vec::new();
    let mut busy = false;
    if state.sleeping.is_some_and(|at| now.duration_since(at).is_ok_and(|d| d < GOING_TO_SLEEP)) {
        items.push(line("Going to sleep…", "", true));
        busy = true;
    } else if me.standby.is_some() {
        own(me, peers, state, now, &mut items);
    }
    if !me.view_only {
        busy |= others(me, peers, state, now, &mut items);
    }
    let target = me.target.as_ref().ok();
    let old = secs(now).saturating_sub(TELL_UNWOKEN_FOR);
    state.unwoken.retain(|_, at| *at > old);
    let record = Record {
        name: me.name.clone(),
        mac: target.map(|t| format_mac(&t.mac)),
        addr: target.map(|t| format!("{}/{}", t.addr.0, t.addr.1)),
        adapter: target.map(|t| t.adapter.clone()),
        standby: me.standby.map(str::to_string),
        lan: me.lan.iter().map(|(ip, prefix)| format!("{ip}/{prefix}")).collect(),
        wakes: !me.view_only,
        unwoken: state.unwoken.clone(),
        tested_at: me.tested.map(|(_, at)| at),
    };
    json!({
        "t": "view",
        "place": "card",
        "view": { "items": items },
        "record": record,
        "again": busy.then_some(2),
    })
}

/// This machine's own card: Sleep, the test that comes before it, or why
/// neither.
fn own(me: &Me, peers: &[Peer], state: &State, now: SystemTime, items: &mut Vec<Value>) {
    let mut off = |why: String| items.push(line(format!("Sleep is off: {why}"), "hint", false));
    if me.view_only {
        return off("this machine is view-only.".into());
    }
    let t = match &me.target {
        Ok(t) => t,
        Err(why) => return off(why.clone()),
    };
    if !t.armed {
        return off(format!(
            "Windows does not let {0} wake this machine. To allow it, run in an administrator terminal there: \
             powercfg /deviceenablewake \"{0}\"",
            t.adapter
        ));
    }
    let can_wake = |r: &Record| r.wakes && r.lan.iter().filter_map(|n| parse_net(n)).any(|n| on_network(n, t.addr));
    let Some(waker) = peers.iter().find(|p| p.up && p.record.as_ref().is_some_and(can_wake)) else {
        return off("no other paired machine on its network is online to wake it.".into());
    };
    let said = state.last_test.as_ref().filter(|l| now.duration_since(l.at).is_ok_and(|d| d < SAY_TEST_FOR));
    if me.tested.is_some_and(|(ok, _)| ok) {
        if let Some(last) = said.filter(|l| l.ok) {
            items.push(line(format!("Waking works: it was back after {} s. Sleep is ready.", last.took), "", false));
        }
        items.push(json!({
            "kind": "button",
            "label": "Sleep",
            "confirm": format!("Put {} to sleep? {} can wake it from here.", me.name, waker.name),
            "do": { "t": "sleep" },
        }));
        return;
    }
    let check = what_to_check(Some(&t.adapter), me.standby);
    match (said.filter(|l| !l.ok), me.tested) {
        (Some(last), _) => items.push(line(
            format!("No packet woke {}: it was back only after {} s. {check} Then test again.", me.name, last.took),
            "error",
            false,
        )),
        (None, Some((false, at))) => {
            let on = chrono::DateTime::from_timestamp(at as i64, 0)
                .map(|t| t.with_timezone(&chrono::Local).format("%-d %B").to_string())
                .unwrap_or_default();
            items.push(line(format!("The last test, on {on}, did not wake it. {check}"), "error", false));
        }
        _ => {}
    }
    let if_not = if me.timer {
        "If that does not wake it, it wakes itself after about two minutes and says so."
    } else {
        "If that does not wake it, someone will have to press its power button: wake timers are off in its \
         power settings, so it cannot wake itself."
    };
    items.push(json!({
        "kind": "button",
        "label": "Test sleep and wake",
        "confirm": format!(
            "{} will go to sleep, and {} will try to wake it about {TEST_WAKE_AFTER} seconds later.\n\n{if_not} Go ahead?",
            me.name, waker.name
        ),
        // The wake first: once this machine is asleep it is too late to
        // find that nobody will send one.
        "first": { "peer": waker.id, "do": {
            "t": "wake",
            "mac": format_mac(&t.mac),
            "addr": format!("{}/{}", t.addr.0, t.addr.1),
            "after": TEST_WAKE_AFTER,
            "timer": me.timer,
        } },
        "do": { "t": "sleep", "test": true },
    }));
}

/// The cards of the machines this one can wake: Wake while one is away,
/// and how a wake under way is going. True while something is under way.
fn others(me: &Me, peers: &[Peer], state: &mut State, now: SystemTime, items: &mut Vec<Value>) -> bool {
    let mut busy = false;
    for peer in peers {
        let Some(record) = &peer.record else { continue };
        let (Some(mac), Some(addr)) =
            (record.mac.as_deref().and_then(parse_mac), record.addr.as_deref().and_then(parse_net))
        else {
            continue;
        };
        if same_network(addr, &me.lan).is_none() {
            continue;
        }
        let key = format_mac(&mac);
        let name = if peer.name.is_empty() { &record.name } else { &peer.name };
        let about = |mut item: Value| {
            item["about"] = peer.id.clone().into();
            item
        };
        // It has learnt of a wake that woke nothing: no need to go on saying.
        if let (Some(at), Some(learnt)) = (state.unwoken.get(&key), record.tested_at)
            && learnt >= *at
        {
            state.unwoken.remove(&key);
        }
        if let Some(wake) = state.wakes.get(&key) {
            // None: its first packet is still to go.
            let sent = now.duration_since(wake.at).ok();
            let within = if wake.timer { AWAKE_BY_TIMER_WITHIN } else { AWAKE_WITHIN };
            match (peer.up, sent) {
                // Not asleep yet.
                (true, None) => {}
                // Back.
                (true, Some(_)) => drop(state.wakes.remove(&key)),
                (false, None) => {
                    items.push(about(line(format!("Asleep. {} is about to wake it…", me.name), "", true)));
                    busy = true;
                    continue;
                }
                (false, Some(waited)) if waited < within => {
                    items.push(about(line(format!("Waking, sent from {}…", me.name), "", true)));
                    busy = true;
                    continue;
                }
                (false, Some(_)) => {
                    state.unwoken.insert(key.clone(), secs(wake.at));
                    state.wakes.remove(&key);
                }
            }
        }
        if peer.up {
            continue;
        }
        if state.unwoken.contains_key(&key) {
            items.push(about(line(
                format!(
                    "{name} did not wake. {} Press its power button, then test again. ({} sent the packet.)",
                    what_to_check(record.adapter.as_deref(), record.standby.as_deref()),
                    me.name
                ),
                "error",
                false,
            )));
        }
        items.push(about(json!({
            "kind": "button",
            "label": "Wake",
            "style": "primary",
            "note": format!("from {}", me.name),
            "do": { "t": "wake", "mac": key, "addr": format!("{}/{}", addr.0, addr.1) },
        })));
    }
    busy
}

/// The network card another machine would wake this one through.
pub struct Target {
    mac: [u8; 6],
    /// This machine's address on that network, with its prefix length.
    addr: (Ipv4Addr, u8),
    adapter: String,
    /// Windows lets the card wake the machine.
    armed: bool,
}

/// That card, or why there is none.
fn target() -> Result<Target, String> {
    platform::target()
}

/// What this machine can do, for the browser to keep:
/// - `sleep`: it can be put to sleep (`standby` says how: "s3", or "modern"
///   for Modern Standby, where a sleeping machine may not keep its network
///   card powered).
/// - `wake`: the card to wake it through, or null, with `why`.
/// - `lan`: the networks it is on, so the browser can tell which machines
///   can wake which.
/// - `tested`: how the last try at waking it went, for this card.
/// - `timer`: a test that fails to wake it ends by itself, on a wake timer.
fn info() -> Value {
    let (wake, why, tested) = match target() {
        Ok(t) => {
            let mac = format_mac(&t.mac);
            let tested = tested(&mac).map(|(ok, at)| json!({ "ok": ok, "at": at }));
            let why = (!t.armed).then(|| format!("Windows does not let {} wake this machine", t.adapter));
            let wake = json!({
                "mac": mac,
                "addr": format!("{}/{}", t.addr.0, t.addr.1),
                "adapter": t.adapter,
                "armed": t.armed,
            });
            (wake, why, tested)
        }
        Err(why) => (Value::Null, Some(why), None),
    };
    json!({
        "t": "power",
        "name": crate::pairing::machine_name(),
        "sleep": platform::can_sleep().is_some(),
        "standby": platform::can_sleep(),
        "wake": wake,
        "why": why,
        "lan": lan().iter().map(|(ip, prefix)| format!("{ip}/{prefix}")).collect::<Vec<_>>(),
        "tested": tested,
        "timer": platform::wake_timers_allowed(),
    })
}

/// This machine's IPv4 networks, other than loopback, link-local and
/// Tailscale's: (its address, prefix length).
fn lan() -> Vec<(Ipv4Addr, u8)> {
    let Ok(addrs) = if_addrs::get_if_addrs() else { return Vec::new() };
    addrs
        .iter()
        .filter_map(|a| match &a.addr {
            if_addrs::IfAddr::V4(v4) => Some((v4.ip, v4.prefixlen)),
            _ => None,
        })
        .filter(|(ip, prefix)| {
            !ip.is_loopback() && !ip.is_link_local() && !crate::server::tailnet((*ip).into()) && (8..=30).contains(prefix)
        })
        .collect()
}

fn mask(prefix: u8) -> u32 {
    if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32)) }
}

/// The local address on the same network as `addr`, and that network's
/// broadcast address.
fn same_network(addr: (Ipv4Addr, u8), lan: &[(Ipv4Addr, u8)]) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let m = mask(addr.1);
    let net = u32::from(addr.0) & m;
    lan.iter()
        .find(|(ip, prefix)| *prefix == addr.1 && u32::from(*ip) & m == net)
        .map(|(ip, _)| (*ip, Ipv4Addr::from(net | !m)))
}

/// Six bytes of 0xFF, then the card's address sixteen times.
fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

/// Send the packet for `mac` onto the network `addr` is on, now or `after`
/// seconds from now. Only onto a network this machine (on `lan`) is on
/// itself: nobody else's.
fn wake(mac: [u8; 6], addr: (Ipv4Addr, u8), after: Option<u64>, lan: &[(Ipv4Addr, u8)]) -> (Value, Then) {
    let from = crate::pairing::machine_name();
    let Some((local, broadcast)) = same_network(addr, lan) else {
        return (error(format!("{from} is not on the network of {}/{}", addr.0, addr.1)), Then::Nothing);
    };
    if let Some(after) = after {
        if after > MAX_WAKE_AFTER {
            return (error(format!("a wake can be put off {MAX_WAKE_AFTER} seconds at most")), Then::Nothing);
        }
        crate::log!("power: will send a wake packet to {} in {after} s", format_mac(&mac));
        let then = Then::WakeLater { mac, local, broadcast, after: Duration::from_secs(after) };
        return (json!({ "t": "wake-armed", "from": from, "after": after }), then);
    }
    match send_wake(mac, local, broadcast) {
        Ok(()) => {
            crate::log!("power: sent a wake packet to {} on {broadcast}", format_mac(&mac));
            (json!({ "t": "wake-sent", "from": from }), Then::Nothing)
        }
        Err(e) => (error(format!("could not send the wake packet: {e}")), Then::Nothing),
    }
}

fn send_wake(mac: [u8; 6], local: Ipv4Addr, broadcast: Ipv4Addr) -> std::io::Result<()> {
    let socket = UdpSocket::bind(SocketAddrV4::new(local, 0))?;
    socket.set_broadcast(true)?;
    let packet = magic_packet(mac);
    // A few times over: it is one datagram, with nothing to say it arrived.
    for _ in 0..3 {
        socket.send_to(&packet, SocketAddrV4::new(broadcast, WOL_PORT))?;
        let _ = socket.send_to(&packet, SocketAddrV4::new(Ipv4Addr::BROADCAST, WOL_PORT));
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let bytes: Vec<u8> = text.split([':', '-']).map(|b| u8::from_str_radix(b, 16)).collect::<Result<_, _>>().ok()?;
    bytes.try_into().ok()
}

fn format_mac(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

fn parse_net(text: &str) -> Option<(Ipv4Addr, u8)> {
    let (ip, prefix) = text.split_once('/')?;
    let prefix: u8 = prefix.parse().ok()?;
    (8..=30).contains(&prefix).then_some(())?;
    Some((ip.parse().ok()?, prefix))
}

#[cfg(windows)]
mod platform {
    use super::Target;
    use std::net::Ipv4Addr;

    /// Whether this machine can sleep, and how: "s3", or "modern".
    pub fn can_sleep() -> Option<&'static str> {
        use windows::Win32::System::Power::{CallNtPowerInformation, SYSTEM_POWER_CAPABILITIES, SystemPowerCapabilities};
        let mut caps = SYSTEM_POWER_CAPABILITIES::default();
        let status = unsafe {
            CallNtPowerInformation(
                SystemPowerCapabilities,
                None,
                0,
                Some(&mut caps as *mut _ as *mut _),
                size_of::<SYSTEM_POWER_CAPABILITIES>() as u32,
            )
        };
        if status.is_err() {
            return None;
        }
        if caps.AoAc {
            Some("modern")
        } else if caps.SystemS3 || caps.SystemS1 || caps.SystemS2 {
            Some("s3")
        } else {
            None
        }
    }

    pub fn sleep() -> Result<(), String> {
        use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
        use windows::Win32::Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
            TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
        };
        use windows::Win32::System::Power::SetSuspendState;
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        unsafe {
            // Held by users and SYSTEM alike, but off until asked for.
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)
                .map_err(|e| e.to_string())?;
            let mut luid = LUID::default();
            let looked = LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid);
            if looked.is_ok() {
                let privileges = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
                };
                let _ = AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None);
            }
            let _ = CloseHandle(token);
            if SetSuspendState(false, false, false) {
                Ok(())
            } else {
                Err(windows::core::Error::from_thread().to_string())
            }
        }
    }

    /// Whether a timer may wake this machine from sleep: the power plan's
    /// "Allow wake timers", for the power it is on now. "Important wake
    /// timers only" leaves ours out.
    pub fn wake_timers_allowed() -> bool {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::System::Power::{
            GetSystemPowerStatus, PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, SYSTEM_POWER_STATUS,
        };
        use windows::core::GUID;
        const SLEEP: GUID = GUID::from_u128(0x238c9fa8_0aad_41ed_83f4_97be242c8f20);
        const ALLOW_WAKE_TIMERS: GUID = GUID::from_u128(0xbd3b718a_0680_4d9d_8ab2_e1d2b4ac806d);
        unsafe {
            let mut scheme: *mut GUID = std::ptr::null_mut();
            if PowerGetActiveScheme(None, &mut scheme).is_err() || scheme.is_null() {
                return false;
            }
            let mut status = SYSTEM_POWER_STATUS::default();
            let on_battery = GetSystemPowerStatus(&mut status).is_ok() && status.ACLineStatus == 0;
            let mut value = 0u32;
            // Both return 0 for success; the bindings type the two differently.
            let failed = if on_battery {
                PowerReadDCValueIndex(None, Some(scheme), Some(&SLEEP), Some(&ALLOW_WAKE_TIMERS), &mut value)
            } else {
                PowerReadACValueIndex(None, Some(scheme), Some(&SLEEP), Some(&ALLOW_WAKE_TIMERS), &mut value).0
            };
            let _ = LocalFree(Some(HLOCAL(scheme as *mut _)));
            failed == 0 && value == 1
        }
    }

    /// A timer that wakes this machine from sleep, until it is dropped.
    pub struct WakeTimer(windows::Win32::Foundation::HANDLE);

    // The handle is only ever cancelled and closed, from whichever thread.
    unsafe impl Send for WakeTimer {}

    impl Drop for WakeTimer {
        fn drop(&mut self) {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::Threading::CancelWaitableTimer;
            unsafe {
                let _ = CancelWaitableTimer(self.0);
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Have this machine wake itself at `at`, if Windows allows that.
    pub fn wake_timer(at: std::time::SystemTime) -> Option<WakeTimer> {
        use windows::Win32::System::Threading::{CreateWaitableTimerW, SetWaitableTimer};
        use windows::core::PCWSTR;
        if !wake_timers_allowed() {
            return None;
        }
        // An absolute time, as a FILETIME: 100 ns steps since 1601. (A
        // relative one may not count the time spent asleep.)
        let unix = at.duration_since(std::time::UNIX_EPOCH).ok()?;
        let due = (unix.as_secs() as i64 + 11_644_473_600) * 10_000_000 + i64::from(unix.subsec_nanos() / 100);
        unsafe {
            let timer = WakeTimer(CreateWaitableTimerW(None, true, PCWSTR::null()).ok()?);
            match SetWaitableTimer(timer.0, &due, 0, None, None, true) {
                Ok(()) => Some(timer),
                Err(e) => {
                    crate::log!("power: could not set a wake timer ({e})");
                    None
                }
            }
        }
    }

    /// Keep this machine from going back to sleep by itself for a while.
    pub fn stay_awake(how_long: std::time::Duration) {
        use windows::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
        // The request lasts as long as the thread that made it.
        std::thread::spawn(move || unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
            std::thread::sleep(how_long);
            SetThreadExecutionState(ES_CONTINUOUS);
        });
    }

    struct Adapter {
        description: String,
        mac: [u8; 6],
        ethernet: bool,
        wifi: bool,
        addrs: Vec<(Ipv4Addr, u8)>,
        gateway: bool,
    }

    fn adapters() -> Vec<Adapter> {
        use windows::Win32::NetworkManagement::IpHelper::{
            GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
            GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
        };
        use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
        use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
        const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
        const IF_TYPE_IEEE80211: u32 = 71;

        let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 16 * 1024u32;
        let mut buffer: Vec<u64>;
        loop {
            // u64s, for the alignment the structures need.
            buffer = vec![0u64; (size as usize).div_ceil(8)];
            let first = buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
            let status = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(first), &mut size) };
            match status {
                0 => break,
                111 => continue, // ERROR_BUFFER_OVERFLOW: `size` now says how much
                _ => return Vec::new(),
            }
        }
        let mut list = Vec::new();
        let mut next = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while let Some(a) = unsafe { next.as_ref() } {
            next = a.Next;
            if a.OperStatus != IfOperStatusUp || a.PhysicalAddressLength != 6 {
                continue;
            }
            let mut addrs = Vec::new();
            let mut unicast = a.FirstUnicastAddress;
            while let Some(u) = unsafe { unicast.as_ref() } {
                unicast = u.Next;
                if let Some(sa) = unsafe { (u.Address.lpSockaddr as *const SOCKADDR_IN).as_ref() }
                    && sa.sin_family == AF_INET
                {
                    let ip = Ipv4Addr::from(u32::from_be(unsafe { sa.sin_addr.S_un.S_addr }));
                    addrs.push((ip, u.OnLinkPrefixLength));
                }
            }
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&a.PhysicalAddress[..6]);
            list.push(Adapter {
                description: unsafe { a.Description.to_string() }.unwrap_or_default(),
                mac,
                ethernet: a.IfType == IF_TYPE_ETHERNET_CSMACD,
                wifi: a.IfType == IF_TYPE_IEEE80211,
                addrs,
                gateway: !a.FirstGatewayAddress.is_null(),
            });
        }
        list
    }

    /// Software switches and tunnels that look like Ethernet: a packet to
    /// their address wakes nothing.
    fn virtual_card(description: &str) -> bool {
        let d = description.to_ascii_lowercase();
        ["hyper-v", "virtual", "vmware", "virtualbox", "tap-", "wintun", "tailscale", "wireguard", "vpn", "loopback"]
            .iter()
            .any(|v| d.contains(v))
    }

    /// The devices Windows lets wake the machine, by name.
    fn wake_armed() -> Vec<String> {
        let Ok(out) = std::process::Command::new("powercfg").args(["/devicequery", "wake_armed"]).output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
    }

    pub fn target() -> Result<Target, String> {
        let all = adapters();
        let routed = |a: &&Adapter| a.gateway && !a.addrs.is_empty() && !virtual_card(&a.description);
        let Some(card) = all.iter().filter(routed).find(|a| a.ethernet) else {
            return Err(if all.iter().filter(routed).any(|a| a.wifi) {
                "It is on Wi-Fi, which can rarely wake a machine. A network cable can.".into()
            } else {
                "It has no wired network connection to wake it through.".into()
            });
        };
        let armed = wake_armed().iter().any(|d| d.eq_ignore_ascii_case(&card.description));
        Ok(Target { mac: card.mac, addr: card.addrs[0], adapter: card.description.clone(), armed })
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Target;

    pub fn can_sleep() -> Option<&'static str> {
        None
    }

    pub fn sleep() -> Result<(), String> {
        Err("not on this system yet".into())
    }

    pub fn target() -> Result<Target, String> {
        Err("Waking is only set up on Windows so far.".into())
    }

    pub fn wake_timers_allowed() -> bool {
        false
    }

    pub struct WakeTimer;

    pub fn wake_timer(_at: std::time::SystemTime) -> Option<WakeTimer> {
        None
    }

    pub fn stay_awake(_how_long: std::time::Duration) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_magic_packet_is_ff_then_the_address_sixteen_times() {
        let mac = parse_mac("0a:1b:2c:3d:4e:5f").unwrap();
        let p = magic_packet(mac);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        assert!(p[6..].chunks(6).all(|c| c == mac));
        assert_eq!(format_mac(&mac), "0a:1b:2c:3d:4e:5f");
        assert_eq!(parse_mac("0A-1B-2C-3D-4E-5F"), Some(mac));
        assert_eq!(parse_mac("0a:1b:2c:3d:4e"), None);
        assert_eq!(parse_mac("0a:1b:2c:3d:4e:zz"), None);
    }

    #[test]
    fn a_wake_goes_only_onto_a_network_this_machine_is_on() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        let lan = [(ip("10.0.0.5"), 8), (ip("192.168.1.23"), 24)];
        assert_eq!(same_network(parse_net("192.168.1.40/24").unwrap(), &lan), Some((ip("192.168.1.23"), ip("192.168.1.255"))));
        assert_eq!(same_network(parse_net("10.200.3.4/8").unwrap(), &lan), Some((ip("10.0.0.5"), ip("10.255.255.255"))));
        // Another network, and the same addresses with another prefix.
        assert_eq!(same_network(parse_net("192.168.2.40/24").unwrap(), &lan), None);
        assert_eq!(same_network(parse_net("192.168.1.40/16").unwrap(), &lan), None);
        assert_eq!(parse_net("192.168.1.40/32"), None);
        assert_eq!(parse_net("192.168.1.40"), None);
    }

    #[test]
    fn a_view_only_machine_will_not_sleep() {
        let state = Mutex::default();
        for request in [r#"{"t":"sleep"}"#, r#"{"t":"sleep","test":true}"#, r#"{"t":"do","do":{"t":"sleep"}}"#] {
            let (answer, then) = super::answer(request, true, &state);
            assert_eq!(answer["t"], "error");
            assert_eq!(then, Then::Nothing);
        }
        let (answer, _) = super::answer(r#"{"t":"wake","mac":"0a:1b:2c:3d:4e:5f"}"#, false, &state);
        assert_eq!(answer["t"], "error");
        let (answer, _) = super::answer("{", false, &state);
        assert_eq!(answer["t"], "error");
        // Another machine may ask for a wake, and for nothing else.
        let (answer, then) = super::answer(r#"{"t":"peer","do":{"t":"sleep"}}"#, false, &state);
        assert_eq!((answer["t"].as_str(), then), (Some("error"), Then::Nothing));
    }

    const MAC: &str = "0a:1b:2c:3d:4e:5f";

    /// A wired machine at 192.168.1.20 that Windows lets its card wake.
    fn me(tested: Option<(bool, u64)>) -> Me {
        let ip: Ipv4Addr = "192.168.1.20".parse().unwrap();
        Me {
            name: "DESK".into(),
            standby: Some("s3"),
            target: Ok(Target { mac: parse_mac(MAC).unwrap(), addr: (ip, 24), adapter: "Some Ethernet".into(), armed: true }),
            lan: vec![(ip, 24)],
            tested,
            timer: true,
            view_only: false,
        }
    }

    /// Another machine on that network, or on another one.
    fn peer(id: &str, up: bool, addr: &str) -> Peer {
        let record = Record {
            name: id.to_uppercase(),
            mac: Some("aa:bb:cc:dd:ee:ff".into()),
            addr: Some(addr.into()),
            lan: vec![addr.into()],
            wakes: true,
            ..Record::default()
        };
        Peer { id: id.into(), name: id.to_uppercase(), up, record: Some(record) }
    }

    fn items(view: &Value) -> Vec<Value> {
        view["view"]["items"].as_array().unwrap().clone()
    }

    #[test]
    fn a_card_offers_sleep_only_with_a_machine_there_to_wake_it() {
        let now = SystemTime::now();
        let off = |view: &Value| items(view)[0]["text"].as_str().unwrap().to_string();
        // Nobody, somebody on another network, and somebody who is away.
        for peers in [vec![], vec![peer("far", true, "10.0.0.9/24")], vec![peer("near", false, "192.168.1.30/24")]] {
            let view = card(&me(None), &peers, &mut State::default(), now);
            assert!(off(&view).starts_with("Sleep is off: no other paired machine"), "{}", off(&view));
            assert!(items(&view).iter().all(|i| i["label"] != "Sleep" && i["label"] != "Test sleep and wake"));
        }
        // Why not, when it is the machine itself.
        let mut unarmed = me(None);
        if let Ok(t) = &mut unarmed.target {
            t.armed = false;
        }
        assert!(off(&card(&unarmed, &[], &mut State::default(), now)).contains("powercfg /deviceenablewake \"Some Ethernet\""));
        let mut wifi = me(None);
        wifi.target = Err("It is on Wi-Fi.".into());
        assert_eq!(off(&card(&wifi, &[], &mut State::default(), now)), "Sleep is off: It is on Wi-Fi.");
        // And its record tells the others how to wake it.
        let view = card(&me(None), &[], &mut State::default(), now);
        assert_eq!(view["record"]["mac"], MAC);
        assert_eq!(view["record"]["addr"], "192.168.1.20/24");
        assert_eq!(view["record"]["wakes"], true);
    }

    #[test]
    fn the_first_sleep_is_a_test_that_asks_for_its_wake_first() {
        let now = SystemTime::now();
        let peers = [peer("near", true, "192.168.1.30/24")];
        let view = card(&me(None), &peers, &mut State::default(), now);
        let button = &items(&view)[0];
        assert_eq!(button["label"], "Test sleep and wake");
        assert_eq!(button["first"]["peer"], "near");
        assert_eq!(button["first"]["do"], json!({ "t": "wake", "mac": MAC, "addr": "192.168.1.20/24", "after": 15, "timer": true }));
        assert_eq!(button["do"], json!({ "t": "sleep", "test": true }));
        assert!(button["confirm"].as_str().unwrap().contains("NEAR will try to wake it about 15 seconds later"));
        assert!(button["confirm"].as_str().unwrap().contains("wakes itself after about two minutes"));
        // With no wake timer, the warning is the power button.
        let mut no_timer = me(None);
        no_timer.timer = false;
        let view = card(&no_timer, &peers, &mut State::default(), now);
        assert!(items(&view)[0]["confirm"].as_str().unwrap().contains("press its power button"));

        // A test that failed is said, with the test still offered.
        let view = card(&me(Some((false, 1_700_000_000))), &peers, &mut State::default(), now);
        assert_eq!(items(&view)[0]["tone"], "error");
        assert!(items(&view)[0]["text"].as_str().unwrap().starts_with("The last test, on "));
        assert_eq!(items(&view)[1]["label"], "Test sleep and wake");

        // One that worked: plain Sleep, and for a while how it went.
        let mut state = State { last_test: Some(LastTest { ok: true, took: 18, at: now }), ..State::default() };
        let view = card(&me(Some((true, 1_700_000_000))), &peers, &mut state, now);
        assert_eq!(items(&view)[0]["text"], "Waking works: it was back after 18 s. Sleep is ready.");
        assert_eq!(items(&view)[1]["label"], "Sleep");
        assert_eq!(items(&view)[1]["do"], json!({ "t": "sleep" }));
        assert!(items(&view)[1].get("first").is_none());
        let view = card(&me(Some((true, 1_700_000_000))), &peers, &mut state, now + SAY_TEST_FOR);
        assert_eq!(items(&view)[0]["label"], "Sleep");
    }

    #[test]
    fn a_machine_that_is_away_gets_a_wake_on_its_card_from_one_that_can_send_it() {
        let now = SystemTime::now();
        let key = "aa:bb:cc:dd:ee:ff";
        let away = [peer("near", false, "192.168.1.30/24")];
        let mut state = State::default();
        let view = card(&me(None), &away, &mut state, now);
        let wake = items(&view).into_iter().find(|i| i["label"] == "Wake").unwrap();
        assert_eq!(wake["about"], "near");
        assert_eq!(wake["note"], "from DESK");
        assert_eq!(wake["do"], json!({ "t": "wake", "mac": key, "addr": "192.168.1.30/24" }));
        assert!(view["again"].is_null());
        // Not for one on another network, one that is here, or from a
        // machine that is view-only.
        for peers in [[peer("far", false, "10.0.0.9/24")], [peer("near", true, "192.168.1.30/24")]] {
            assert!(items(&card(&me(None), &peers, &mut State::default(), now)).iter().all(|i| i["label"] != "Wake"));
        }
        let mut looking = me(None);
        looking.view_only = true;
        let view = card(&looking, &away, &mut State::default(), now);
        assert!(items(&view).iter().all(|i| i["label"] != "Wake"));
        assert_eq!(view["record"]["wakes"], false);

        // A wake under way is said on that card, and asked about again soon.
        state.wakes.insert(key.into(), Wake { at: now + Duration::from_secs(15), timer: false });
        let view = card(&me(None), &away, &mut state, now);
        let under_way: Vec<Value> = items(&view).into_iter().filter(|i| i["about"] == "near").collect();
        assert_eq!(under_way.len(), 1);
        assert_eq!((under_way[0]["text"].as_str(), under_way[0]["busy"].as_bool()), (Some("Asleep. DESK is about to wake it…"), Some(true)));
        assert_eq!(view["again"], 2);
        let view = card(&me(None), &away, &mut state, now + Duration::from_secs(40));
        assert!(items(&view).iter().any(|i| i["text"] == "Waking, sent from DESK…"));

        // Back: nothing more to say.
        let mut back = State::default();
        back.wakes.insert(key.into(), Wake { at: now, timer: false });
        let view = card(&me(None), &[peer("near", true, "192.168.1.30/24")], &mut back, now + Duration::from_secs(20));
        assert!(items(&view).iter().all(|i| i["about"] != "near"));
        assert!(back.wakes.is_empty() && back.unwoken.is_empty());

        // Not back in time: it did not wake, which goes in the record for
        // that machine to hear of, until it has.
        let late = now + Duration::from_secs(15) + AWAKE_WITHIN;
        let view = card(&me(None), &away, &mut state, late);
        let about: Vec<Value> = items(&view).into_iter().filter(|i| i["about"] == "near").collect();
        assert!(about[0]["text"].as_str().unwrap().starts_with("NEAR did not wake."));
        assert_eq!(about[1]["label"], "Wake");
        assert_eq!(view["record"]["unwoken"][key], secs(now + Duration::from_secs(15)));
        let mut heard = peer("near", true, "192.168.1.30/24");
        heard.record.as_mut().unwrap().tested_at = Some(secs(late) + 60);
        let view = card(&me(None), &[heard], &mut state, late + Duration::from_secs(120));
        assert_eq!(view["record"]["unwoken"], json!({}));
        // A machine that wakes itself on a timer gets longer.
        let mut timed = State::default();
        timed.wakes.insert(key.into(), Wake { at: now, timer: true });
        let view = card(&me(None), &away, &mut timed, now + AWAKE_WITHIN + Duration::from_secs(5));
        assert!(items(&view).iter().any(|i| i["text"] == "Waking, sent from DESK…"));
    }

    #[test]
    fn a_machine_hears_of_a_wake_that_did_not_wake_it() {
        let mut told = peer("near", true, "192.168.1.30/24");
        told.record.as_mut().unwrap().unwoken.insert(MAC.into(), 2000);
        let peers = [peer("other", true, "192.168.1.31/24"), told];
        assert!(unwoken_since(&peers, MAC, None));
        assert!(unwoken_since(&peers, MAC, Some((true, 1999))));
        // Already known, or about another card.
        assert!(!unwoken_since(&peers, MAC, Some((false, 2000))));
        assert!(!unwoken_since(&peers, "00:00:00:00:00:01", None));
    }

    #[test]
    fn a_wake_put_off_is_answered_at_once_and_sent_later() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        let lan = [(ip("192.168.1.23"), 24)];
        let mac = parse_mac("0a:1b:2c:3d:4e:5f").unwrap();
        let (answer, then) = wake(mac, (ip("192.168.1.40"), 24), Some(15), &lan);
        assert_eq!(answer["t"], "wake-armed");
        assert_eq!(answer["after"], 15);
        assert_eq!(
            then,
            Then::WakeLater { mac, local: ip("192.168.1.23"), broadcast: ip("192.168.1.255"), after: Duration::from_secs(15) }
        );
        // Not onto another network, and not put off for long.
        let (answer, then) = wake(mac, (ip("192.168.2.40"), 24), Some(15), &lan);
        assert_eq!((answer["t"].as_str(), then), (Some("error"), Then::Nothing));
        let (answer, then) = wake(mac, (ip("192.168.1.40"), 24), Some(MAX_WAKE_AFTER + 1), &lan);
        assert_eq!((answer["t"].as_str(), then), (Some("error"), Then::Nothing));
    }

    /// Where the power plan allows wake timers, one can be set (and is
    /// cancelled when dropped, long before it is due).
    #[cfg(windows)]
    #[test]
    fn a_wake_timer_can_be_set_where_they_are_allowed() {
        let timer = platform::wake_timer(SystemTime::now() + Duration::from_secs(3600));
        assert_eq!(timer.is_some(), platform::wake_timers_allowed());
    }

    #[test]
    fn a_test_is_told_by_when_the_machine_woke() {
        // The last packet goes out 45 s after the test began, and the timer
        // is at 120 s: Windows may wake the machine half a minute early.
        assert!(by_packet(Duration::from_secs(20)));
        assert!(by_packet(Duration::from_secs(60)));
        assert!(!by_packet(Duration::from_secs(90)));
        assert!(!by_packet(Duration::from_secs(125)));
        assert!(WOKEN_BY_PACKET_WITHIN < TEST_TIMER - Duration::from_secs(30));
    }
}
