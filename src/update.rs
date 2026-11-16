//! Updating peek from a release channel: published builds on GitHub (stable
//! releases, and the rolling `nightly` prerelease for `next`), each with
//! SHA256SUMS and version.json beside the binaries.
//!
//! The peek that is running updates itself, when it runs the installed file:
//! the service runs as SYSTEM and the logon task as administrator, so either
//! may replace its own file, and a browser can ask for it (POST /api/update)
//! without anyone at an administrator prompt. `peek update` asks the running
//! peek to do it; only with none running does it replace the file itself.
use std::{
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

const MISMATCH: &str = "the download does not match SHA256SUMS; nothing was changed";
const NOT_INSTALLED: &str = "peek is not installed here; peek update updates an installed peek (see peek install)";
const NOT_SELF: &str = "this peek does not run from its installed file, so it cannot update itself; \
    run `peek install` from the new version instead";
/// How often a running peek looks for a new version, and how soon after it starts.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const FIRST_CHECK: Duration = Duration::from_secs(30);

fn asset(os: &str, arch: &str) -> Result<&'static str, String> {
    match (os, arch) {
        ("windows", "x86_64") => Ok("peek-win32-x64.exe"),
        ("linux", "x86_64") => Ok("peek-linux-x64"),
        ("linux", "aarch64") => Ok("peek-linux-arm64"),
        _ => Err("there is no published build for this platform".into()),
    }
}
fn base_url(s: &str) -> Result<String, String> {
    let valid = if let Some(rest) = s.strip_prefix("https://") {
        !rest.is_empty() && !rest.starts_with('/') && !rest.contains(['?', '#', '@', '\\', ' '])
    } else if let Some(rest) = s.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or("");
        let mut parts = authority.split(':');
        matches!(parts.next(), Some("localhost" | "127.0.0.1")) && parts.next().is_some_and(|p| p.parse::<u16>().is_ok_and(|n| n > 0)) && parts.next().is_none() && !rest.contains(['?', '#', '@', '\\', ' '])
    } else { false };
    if !valid { return Err("the update address must use https or a local test server".into()); }
    Ok(format!("{}/", s.trim_end_matches('/')))
}
fn sum_for(s: &str, name: &str) -> Option<String> {
    s.lines().find_map(|line| {
        let (hash, file) = line.split_once("  ")?;
        if file == name && hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()) { Some(hash.to_ascii_lowercase()) } else { None }
    })
}
fn hash(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new(); let mut buf = [0; 65536];
    loop { let n = f.read(&mut buf).map_err(|e| e.to_string())?; if n == 0 { break; } h.update(&buf[..n]); }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Result<Self, String> {
        for n in 0..1000 { let p = std::env::temp_dir().join(format!("peek-update-{}-{n}", std::process::id())); match fs::create_dir(&p) { Ok(()) => return Ok(Self(p)), Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue, Err(e) => return Err(e.to_string()) } }
        Err("cannot create a temporary folder".into())
    }
}
impl Drop for Temp { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn download(url: &str, dest: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let curl = std::env::var_os("SystemRoot").map(PathBuf::from).map(|p| p.join("System32").join("curl.exe")).filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("curl"));
    #[cfg(not(windows))]
    let curl = PathBuf::from("curl");
    let mut command = Command::new(curl);
    command.args(["-fsSL", "--max-time", "60", "-o"]).arg(dest).arg(url).stdin(Stdio::null());
    hide(&mut command);
    let out = command.output().map_err(|e| format!("cannot download {url}: {e}"))?;
    if !out.status.success() { return Err(format!("cannot download {url}: curl exit code {}; {}", out.status.code().map_or("unknown".into(), |c| c.to_string()), String::from_utf8_lossy(&out.stderr).trim())); }
    Ok(())
}
fn version(path: &Path) -> Option<String> {
    let mut command = Command::new(path);
    command.arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    hide(&mut command);
    let mut child = command.spawn().ok()?; let start = Instant::now();
    loop {
        match child.try_wait() { Ok(Some(status)) => { let mut s = String::new(); child.stdout.take()?.read_to_string(&mut s).ok()?; return if status.success() { Some(s.trim().to_string()) } else { None }; }, Ok(None) if start.elapsed() < Duration::from_secs(10) => std::thread::sleep(Duration::from_millis(50)), _ => { let _ = child.kill(); let _ = child.wait(); return None; } }
    }
}
/// No console window for the programs a hidden peek starts.
fn hide(_command: &mut Command) {
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; _command.creation_flags(0x0800_0000); }   // CREATE_NO_WINDOW
}
fn sibling(target: &Path, suffix: &str) -> PathBuf {
    let name = target.file_stem().unwrap_or_default().to_string_lossy();
    let ext = target.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    target.with_file_name(format!("{name}.{suffix}{ext}"))
}
fn io_error(target: &Path, e: std::io::Error) -> String {
    #[cfg(windows)]
    if e.kind() == std::io::ErrorKind::PermissionDenied { return format!("replacing {} needs administrator rights: run this from an administrator terminal", target.display()); }
    format!("cannot replace {}: {e}", target.display())
}
fn replace(target: &Path, download: &Path) -> Result<(), String> {
    let new = sibling(target, "new"); let prev = sibling(target, "prev");
    let result = (|| {
        fs::copy(download, &new).map_err(|e| io_error(target, e))?;
        if prev.exists() { fs::remove_file(&prev).map_err(|e| io_error(target, e))?; }
        // A running program's file can be renamed, on Windows too, though
        // not deleted: so the running peek's own file can be swapped out.
        fs::rename(target, &prev).map_err(|e| io_error(target, e))?;
        if let Err(e) = fs::rename(&new, target) { return match fs::rename(&prev, target) { Ok(()) => Err(io_error(target, e)), Err(restore) => Err(format!("{}; cannot restore previous version: {restore}", io_error(target, e))) }; }
        Ok(())
    })();
    let _ = fs::remove_file(new); result
}
fn rollback(target: &Path) -> Result<(), String> {
    let prev = sibling(target, "prev"); let tmp = sibling(target, "swap");
    if !prev.exists() { return Err("there is no previous version to go back to".into()); }
    if tmp.exists() { return Err(format!("cannot swap versions: {} already exists", tmp.display())); }
    fs::rename(target, &tmp).map_err(|e| io_error(target, e))?;
    if let Err(e) = fs::rename(&prev, target) { let _ = fs::rename(&tmp, target); return Err(io_error(target, e)); }
    if let Err(e) = fs::rename(&tmp, &prev) { let _ = fs::rename(target, &prev); let _ = fs::rename(&tmp, target); return Err(io_error(target, e)); }
    Ok(())
}
#[cfg(windows)]
fn service_exists() -> bool {
    let mut command = Command::new("sc.exe");
    command.args(["query", "peek"]);
    hide(&mut command);
    command.output().is_ok_and(|o| o.status.success())
}
/// The installed file: the service's, the logon task's, or the systemd
/// unit's. PEEK_UPDATE_TARGET stands in for it in tests.
fn target() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("PEEK_UPDATE_TARGET") { return Ok(p.into()); }
    #[cfg(windows)]
    let path = if service_exists() { crate::install::service_exe() } else { crate::install::installed_exe()? };
    #[cfg(target_os = "linux")]
    let path = crate::install::installed_exe()?;
    if !path.is_file() { return Err(NOT_INSTALLED.into()); }
    Ok(path)
}

// ---- what is published ----

/// The newest build on a channel, as its release says.
struct Published {
    version: String,
    channel: String,
    /// Its SHA-256 for this platform, if SHA256SUMS lists one.
    sum: Option<String>,
    url: String,
    name: &'static str,
    /// Holds the downloads until they are done with.
    temp: Temp,
}

fn published(channel: &str) -> Result<Published, String> {
    let name = asset(std::env::consts::OS, std::env::consts::ARCH)?;
    let default = if channel == "next" { "https://github.com/peculiarnewbie/peek/releases/download/nightly/" } else { "https://github.com/peculiarnewbie/peek/releases/latest/download/" };
    let url = base_url(&std::env::var("PEEK_UPDATE_URL").unwrap_or_else(|_| default.into()))?;
    let temp = Temp::new()?;
    let verfile = temp.0.join("version.json");
    if let Err(e) = download(&format!("{url}version.json"), &verfile) {
        if channel == "stable" { return Err("there is no stable release to update from yet; update with npm: npm install -g @peculiarnewbie/peek, then peek install".into()); }
        return Err(e);
    }
    download(&format!("{url}SHA256SUMS"), &temp.0.join("SHA256SUMS"))?;
    let data: serde_json::Value = serde_json::from_slice(&fs::read(&verfile).map_err(|e| e.to_string())?).map_err(|e| format!("invalid version.json: {e}"))?;
    let version = data["version"].as_str().filter(|v| !v.is_empty()).ok_or("invalid version.json: missing version")?.to_string();
    let sums = fs::read_to_string(temp.0.join("SHA256SUMS")).map_err(|e| e.to_string())?;
    Ok(Published { version, channel: channel.into(), sum: sum_for(&sums, name), url, name, temp })
}

impl Published {
    /// Whether `target` is this build already.
    fn installed_at(&self, target: &Path) -> bool {
        self.sum.as_ref().is_some_and(|s| hash(target).is_ok_and(|h| h == *s))
    }

    /// Download it, check it against SHA256SUMS and that it runs and says
    /// its version, then put it in place of `target`, keeping the old one.
    fn install(&self, target: &Path, say: impl Fn(&str)) -> Result<(), String> {
        say(&format!("downloading {}", self.name));
        let file = self.temp.0.join(self.name);
        download(&format!("{}{}", self.url, self.name), &file)?;
        say("checking the download");
        if !self.sum.as_ref().is_some_and(|s| hash(&file).is_ok_and(|h| h == *s)) { let _ = fs::remove_file(&file); return Err(MISMATCH.into()); }
        #[cfg(target_os = "linux")]
        { use std::os::unix::fs::PermissionsExt; fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?; }
        let output = version(&file).unwrap_or_else(|| "unknown".into());
        if output != format!("peek {}", self.version) { return Err(format!("the downloaded peek says \"{output}\", not {}; nothing was changed", self.version)); }
        say(&format!("replacing {}", target.display()));
        replace(target, &file)
    }
}

// ---- the running peek, updating itself ----

/// How this peek was started, so that it can start again on a new version.
struct Running {
    /// The Windows service: Windows has to start it again.
    service: bool,
    /// Its arguments, to start the same way.
    args: Vec<OsString>,
}
static RUNNING: OnceLock<Running> = OnceLock::new();
/// The newest version on this build's channel, from the last check.
static LATEST: Mutex<Option<String>> = Mutex::new(None);
/// One update at a time.
static BUSY: Mutex<()> = Mutex::new(());

/// Record how this peek runs. Call once, in the process that listens.
pub fn started(service: bool) {
    let _ = RUNNING.set(Running { service, args: std::env::args_os().skip(1).collect() });
}

/// The installed file, if this process runs it: only then can it replace
/// and restart itself.
fn own_install() -> Option<PathBuf> {
    let target = target().ok()?;
    let exe = std::env::current_exe().ok()?;
    let same = |a: &Path, b: &Path| fs::canonicalize(a).ok().zip(fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b);
    same(&exe, &target).then_some(target)
}

/// A newer version this peek can update itself to, as of the last check.
pub fn available() -> Option<String> {
    LATEST.lock().unwrap().clone().filter(|v| v != env!("PEEK_VERSION"))
}

/// Look for a new version now and then, if this peek can update itself.
pub fn watch() {
    if own_install().is_none() { return; }
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(|| {
        std::thread::sleep(FIRST_CHECK);
        let mut said = String::new();
        loop {
            match published(env!("PEEK_CHANNEL")) {
                Ok(p) => {
                    if p.version != env!("PEEK_VERSION") && p.version != said { crate::log!("update: peek {} is available", p.version); }
                    said = p.version.clone();
                    *LATEST.lock().unwrap() = Some(p.version);
                }
                Err(e) if e != said => { crate::log!("update: cannot check for a new version: {e}"); said = e; }
                Err(_) => {}
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// What asking this peek to update came to.
pub enum Outcome {
    /// It is the newest version already.
    Current(String),
    /// The installed file is now `to`, and this peek restarts on it.
    Changed { from: String, to: String },
}

/// Update the installed peek that this process runs, from `channel` (this
/// build's own if `None`). On `Changed`, call `restart_soon`.
pub fn update_self(channel: Option<&str>) -> Result<Outcome, String> {
    let _busy = BUSY.try_lock().map_err(|_| "an update is under way already")?;
    let target = own_install().ok_or(NOT_SELF)?;
    let p = published(channel.unwrap_or(env!("PEEK_CHANNEL")))?;
    let running = env!("PEEK_VERSION").to_string();
    if p.installed_at(&target) {
        // Installed but not yet running (a restart that did not happen):
        // restarting is all there is to do.
        if p.version == running { return Ok(Outcome::Current(running)); }
        return Ok(Outcome::Changed { from: running, to: p.version });
    }
    p.install(&target, |step| crate::log!("update: {step}"))?;
    *LATEST.lock().unwrap() = Some(p.version.clone()).filter(|_| p.channel == env!("PEEK_CHANNEL"));
    Ok(Outcome::Changed { from: running, to: p.version })
}

/// Go back to the version the last update replaced. Call `restart_soon` after.
pub fn rollback_self() -> Result<Outcome, String> {
    let _busy = BUSY.try_lock().map_err(|_| "an update is under way already")?;
    let target = own_install().ok_or(NOT_SELF)?;
    rollback(&target)?;
    let to = version(&target).and_then(|v| v.strip_prefix("peek ").map(str::to_owned)).unwrap_or_else(|| "unknown".into());
    Ok(Outcome::Changed { from: env!("PEEK_VERSION").into(), to })
}

/// Start again on the installed file, in a moment: after the answer to
/// whoever asked for the update has gone out.
pub fn restart_soon() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(800));
        if let Err(e) = restart_self() { crate::log!("update: cannot restart: {e}; the new version takes effect at the next start"); }
    });
}

fn restart_self() -> Result<(), String> {
    let running = RUNNING.get().ok_or("peek did not record how it was started")?;
    let exe = own_install().ok_or(NOT_SELF)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = Command::new(&exe);
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        if running.service {
            // Only Windows can start the service again, and only once it has
            // stopped: a peek of its own asks it to, from outside it.
            command.args(["update", "--restart-service"]).spawn().map_err(|e| e.to_string())?;
            crate::log!("update: the service restarts on the new version");
            return Ok(());
        }
        // The new version, started as this one was, takes over the port once
        // this one has exited.
        command.args(&running.args).env("PEEK_REPLACES", std::process::id().to_string()).spawn().map_err(|e| e.to_string())?;
        crate::log!("update: restarting on the new version");
        std::process::exit(0);
    }
    #[cfg(unix)]
    {
        // The new file in place of this process, same process ID, so systemd
        // sees nothing change. Every descriptor closes, the listening socket
        // and the providers' pipes among them, and the new peek opens its own.
        use std::os::unix::process::CommandExt;
        let _ = running.service;
        crate::log!("update: restarting on the new version");
        Err(Command::new(&exe).args(&running.args).exec().to_string())
    }
}

/// When this peek was started by one it replaces, wait for that one to
/// exit, so the port is free.
pub fn wait_for_predecessor() {
    let Some(pid) = std::env::var("PEEK_REPLACES").ok().and_then(|p| p.parse::<u32>().ok()) else { return };
    // SAFETY: before any other thread could read the environment.
    unsafe { std::env::remove_var("PEEK_REPLACES") };
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
        if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(process, 30_000);
            let _ = CloseHandle(process);
        }
    }
    #[cfg(unix)]
    let _ = pid;
}

// ---- `peek update` ----

/// Restart the service, from outside it: stop it, wait until it has, start it.
#[cfg(windows)]
pub fn restart_service() -> Result<(), String> {
    let sc = |args: &[&str]| { let mut c = Command::new("sc.exe"); c.args(args); hide(&mut c); c.output() };
    let ok = |args: &[&str]| sc(args).is_ok_and(|o| o.status.success());
    if !ok(&["stop", "peek"]) { return Err("`sc.exe stop peek` failed".into()); }
    let start = Instant::now();
    loop {
        let stopped = sc(&["query", "peek"]).is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("STOPPED"));
        if stopped { break; }
        if start.elapsed() >= Duration::from_secs(30) { return Err("the peek service did not stop".into()); }
        std::thread::sleep(Duration::from_millis(500));
    }
    if ok(&["start", "peek"]) { Ok(()) } else { Err("`sc.exe start peek` failed".into()) }
}

/// `peek update --check`: what is installed, and what the channel has.
pub fn check(channel: Option<&str>) -> Result<(), String> {
    let target = target()?;
    let channel = channel.unwrap_or(env!("PEEK_CHANNEL"));
    println!("checking {channel} releases");
    let p = published(channel)?;
    let old = version(&target).and_then(|s| s.strip_prefix("peek ").map(str::to_owned)).unwrap_or_else(|| "unknown".into());
    println!("installed: {old}; {channel}: {}", p.version);
    Ok(())
}

/// `peek update` with no peek running to do it: replace the installed file
/// here. It takes effect when peek next starts.
pub fn run_here(channel: Option<&str>, back: bool) -> Result<(), String> {
    let target = target()?;
    if back {
        let old = version(&target).unwrap_or_else(|| "unknown".into());
        rollback(&target)?;
        let new = version(&target).unwrap_or_else(|| "unknown".into());
        println!("rolled back {} from {old} to {new}; it takes effect when peek next starts", target.display());
        return Ok(());
    }
    let channel = channel.unwrap_or(env!("PEEK_CHANNEL"));
    println!("checking {channel} releases");
    let p = published(channel)?;
    if p.installed_at(&target) { println!("peek {} is already the latest on {channel}", p.version); return Ok(()); }
    let old = version(&target).and_then(|s| s.strip_prefix("peek ").map(str::to_owned)).unwrap_or_else(|| "unknown".into());
    p.install(&target, |step| println!("{step}"))?;
    println!("updated {} from {old} to {}; it takes effect when peek next starts. peek update --rollback goes back", target.display(), p.version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn sums() { let h = "AB".repeat(32); let s = format!("{h}  file\nnot-a-hash  bad\n{h}  \n"); assert_eq!(sum_for(&s, "file"), Some(h.to_lowercase())); assert_eq!(sum_for(&s, "missing"), None); assert_eq!(sum_for(&s, "bad"), None); }
    #[test] fn assets() { assert_eq!(asset("windows", "x86_64").unwrap(), "peek-win32-x64.exe"); assert_eq!(asset("linux", "x86_64").unwrap(), "peek-linux-x64"); assert_eq!(asset("linux", "aarch64").unwrap(), "peek-linux-arm64"); assert!(asset("macos", "aarch64").is_err()); }
    #[test] fn urls() { for s in ["https://example.com/", "http://localhost:1234/", "http://127.0.0.1:3"] { assert!(base_url(s).is_ok()); } for s in ["http://example.com/", "http://localhost/", "http://localhost:3@evil/", "file:///x"] { assert!(base_url(s).is_err()); } }
    #[test] fn swaps() { let t = Temp::new().unwrap(); let p = t.0.join("peek"); let d = t.0.join("download"); fs::write(&p, "old").unwrap(); fs::write(&d, "new").unwrap(); replace(&p, &d).unwrap(); assert_eq!(fs::read(&p).unwrap(), b"new"); rollback(&p).unwrap(); assert_eq!(fs::read(&p).unwrap(), b"old"); rollback(&p).unwrap(); assert_eq!(fs::read(&p).unwrap(), b"new"); }
    #[test] fn no_newer_version_is_offered_until_a_check_finds_one() {
        *LATEST.lock().unwrap() = None; assert_eq!(available(), None);
        *LATEST.lock().unwrap() = Some(env!("PEEK_VERSION").into()); assert_eq!(available(), None);
        *LATEST.lock().unwrap() = Some("99.0.0".into()); assert_eq!(available().as_deref(), Some("99.0.0"));
        *LATEST.lock().unwrap() = None;
    }
}
