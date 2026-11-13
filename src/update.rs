use std::{fs, io::Read, path::{Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant}};
use sha2::{Digest, Sha256};

const MISMATCH: &str = "the download does not match SHA256SUMS; nothing was changed";
const NOT_INSTALLED: &str = "peek is not installed here; peek update updates an installed peek (see peek install)";

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
    let out = Command::new(curl).args(["-fsSL", "--max-time", "60", "-o"]).arg(dest).arg(url).output().map_err(|e| format!("cannot download {url}: {e}"))?;
    if !out.status.success() { return Err(format!("cannot download {url}: curl exit code {}; {}", out.status.code().map_or("unknown".into(), |c| c.to_string()), String::from_utf8_lossy(&out.stderr).trim())); }
    Ok(())
}
fn version(path: &Path) -> Option<String> {
    let out = Command::new(path).arg("--version").stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut child = out; let start = Instant::now();
    loop {
        match child.try_wait() { Ok(Some(status)) => { let mut s = String::new(); child.stdout.take()?.read_to_string(&mut s).ok()?; return if status.success() { Some(s.trim().to_string()) } else { None }; }, Ok(None) if start.elapsed() < Duration::from_secs(10) => std::thread::sleep(Duration::from_millis(50)), _ => { let _ = child.kill(); let _ = child.wait(); return None; } }
    }
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
fn service_exists() -> bool { Command::new("sc.exe").args(["query", "peek"]).output().is_ok_and(|o| o.status.success()) }
fn target() -> Result<(PathBuf, bool, bool), String> {
    if let Some(p) = std::env::var_os("PEEK_UPDATE_TARGET") { return Ok((p.into(), false, true)); }
    #[cfg(windows)]
    let service = service_exists();
    #[cfg(not(windows))]
    let service = false;
    #[cfg(windows)]
    let path = if service { crate::install::service_exe() } else { crate::install::installed_exe()? };
    #[cfg(target_os = "linux")]
    let path = crate::install::installed_exe()?;
    if !path.is_file() { return Err(NOT_INSTALLED.into()); }
    Ok((path, service, false))
}
fn command(program: &str, args: &[&str]) -> Result<(), String> {
    let ok = Command::new(program).args(args).status().is_ok_and(|s| s.success());
    if ok { Ok(()) } else { Err(format!("`{program} {}` failed; the new version is installed and takes effect at the next start", args.join(" "))) }
}
fn restart(service: bool, _version: &str) -> Result<(), String> {
    #[cfg(windows)]
    if service {
        command("sc.exe", &["stop", "peek"])?;
        let start = Instant::now();
        loop {
            let stopped = Command::new("sc.exe").args(["query", "peek"]).output().is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("STOPPED"));
            if stopped { break; }
            if start.elapsed() >= Duration::from_secs(30) { return Err("`sc.exe query peek` did not report stopped; the new version is installed and takes effect at the next start".into()); }
            std::thread::sleep(Duration::from_millis(500));
        }
        return command("sc.exe", &["start", "peek"]);
    } else { command("schtasks", &["/End", "/TN", "peek"])?; return command("schtasks", &["/Run", "/TN", "peek"]); }
    #[cfg(target_os = "linux")]
    { let _ = service; if Command::new("systemctl").args(["--user", "is-enabled", "peek"]).output().is_ok_and(|o| o.status.success()) { return command("systemctl", &["--user", "restart", "peek"]); } println!("restart peek to use {_version}"); Ok(()) }
}
pub fn run(channel: Option<&str>, check: bool, no_restart: bool, back: bool) -> Result<(), String> {
    let (target, service, overridden) = target()?;
    if back {
        let old = version(&target).unwrap_or_else(|| "unknown".into());
        rollback(&target)?;
        let new = version(&target).unwrap_or_else(|| "unknown".into());
        println!("rolled back {} from {old} to {new}", target.display());
        if !no_restart && !overridden { restart(service, &new)?; }
        return Ok(());
    }
    let name = asset(std::env::consts::OS, std::env::consts::ARCH)?;
    let channel = channel.unwrap_or(env!("PEEK_CHANNEL"));
    let default = if channel == "next" { "https://github.com/peculiarnewbie/peek/releases/download/nightly/" } else { "https://github.com/peculiarnewbie/peek/releases/latest/download/" };
    let url = base_url(&std::env::var("PEEK_UPDATE_URL").unwrap_or_else(|_| default.into()))?;
    let temp = Temp::new()?;
    println!("checking {channel} releases");
    let verfile = temp.0.join("version.json");
    if let Err(e) = download(&format!("{url}version.json"), &verfile) { if channel == "stable" { return Err("there is no stable release to update from yet; update with npm: npm install -g @peculiarnewbie/peek, then peek install".into()); } return Err(e); }
    download(&format!("{url}SHA256SUMS"), &temp.0.join("SHA256SUMS"))?;
    let data: serde_json::Value = serde_json::from_slice(&fs::read(verfile).map_err(|e| e.to_string())?).map_err(|e| format!("invalid version.json: {e}"))?;
    let published = data["version"].as_str().filter(|v| !v.is_empty()).ok_or("invalid version.json: missing version")?;
    let sums = fs::read_to_string(temp.0.join("SHA256SUMS")).map_err(|e| e.to_string())?;
    let sum = sum_for(&sums, name);
    let old = version(&target).and_then(|s| s.strip_prefix("peek ").map(str::to_owned)).unwrap_or_else(|| "unknown".into());
    if check { println!("installed: {old}; {channel}: {published}"); return Ok(()); }
    if published == env!("PEEK_VERSION") && sum.as_ref().is_some_and(|s| hash(&target).is_ok_and(|h| h == *s)) { println!("peek {published} is already the latest on {channel}"); return Ok(()); }
    println!("downloading {name}");
    let file = temp.0.join(name); download(&format!("{url}{name}"), &file)?;
    println!("checking the download");
    if !sum.is_some_and(|s| hash(&file).is_ok_and(|h| h == s)) { let _ = fs::remove_file(&file); return Err(MISMATCH.into()); }
    #[cfg(target_os = "linux")]
    { use std::os::unix::fs::PermissionsExt; fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?; }
    let output = version(&file).unwrap_or_else(|| "unknown".into());
    if output != format!("peek {published}") { return Err(format!("the downloaded peek says \"{output}\", not {published}; nothing was changed")); }
    println!("replacing {}", target.display()); replace(&target, &file)?;
    println!("updated {} from {old} to {published}; peek update --rollback goes back", target.display());
    if !no_restart && !overridden { restart(service, published)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn sums() { let h = "AB".repeat(32); let s = format!("{h}  file\nnot-a-hash  bad\n{h}  \n"); assert_eq!(sum_for(&s, "file"), Some(h.to_lowercase())); assert_eq!(sum_for(&s, "missing"), None); assert_eq!(sum_for(&s, "bad"), None); }
    #[test] fn assets() { assert_eq!(asset("windows", "x86_64").unwrap(), "peek-win32-x64.exe"); assert_eq!(asset("linux", "x86_64").unwrap(), "peek-linux-x64"); assert_eq!(asset("linux", "aarch64").unwrap(), "peek-linux-arm64"); assert!(asset("macos", "aarch64").is_err()); }
    #[test] fn urls() { for s in ["https://example.com/", "http://localhost:1234/", "http://127.0.0.1:3"] { assert!(base_url(s).is_ok()); } for s in ["http://example.com/", "http://localhost/", "http://localhost:3@evil/", "file:///x"] { assert!(base_url(s).is_err()); } }
    #[test] fn swaps() { let t = Temp::new().unwrap(); let p = t.0.join("peek"); let d = t.0.join("download"); fs::write(&p, "old").unwrap(); fs::write(&d, "new").unwrap(); replace(&p, &d).unwrap(); assert_eq!(fs::read(&p).unwrap(), b"new"); rollback(&p).unwrap(); assert_eq!(fs::read(&p).unwrap(), b"old"); rollback(&p).unwrap(); assert_eq!(fs::read(&p).unwrap(), b"new"); }
}
