//! Pairing browsers with this host, for the hosted client: a page served
//! from another origin (by default https://peek.peculiarnewbie.com) that
//! connects straight to each paired machine at its Tailscale HTTPS name.
//!
//! The host makes a one-time code (`peek pair`, or "Pair a device" in a
//! connected client). The browser trades it for a token of its own, which
//! only that browser holds. Here only a hash of each token is kept, with a
//! label, so a browser can be unpaired on its own.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Codes use letters and digits that cannot be mistaken for one another.
const CODE_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
/// 12 characters of 32 kinds: 60 bits, far beyond guessing in `CODE_TTL`.
const CODE_LEN: usize = 12;
pub const CODE_TTL: Duration = Duration::from_secs(5 * 60);
/// At most this many codes wait to be used at once; older ones lapse.
const MAX_CODES: usize = 8;
/// Wrong codes allowed per minute before every attempt is refused for a while.
const MAX_FAILURES: usize = 20;

#[derive(Clone, Serialize, Deserialize)]
pub struct Client {
    pub id: String,
    pub label: String,
    /// SHA-256 of the token, hex.
    hash: String,
    /// Unix seconds.
    pub created: u64,
    pub last_seen: u64,
}

struct Code {
    code: String,
    expires: Instant,
}

pub enum RedeemError {
    /// Unknown, used or expired.
    Invalid,
    /// Too many wrong codes lately.
    Busy,
}

pub struct Pairing {
    dir: PathBuf,
    /// Tells this machine apart from others in a browser's list, whatever
    /// address it is reached at.
    pub host_id: String,
    /// Lets `peek pair` on this machine ask the running host for codes.
    /// Only readable by this user.
    pub local_secret: String,
    clients: Mutex<Vec<Client>>,
    codes: Mutex<Vec<Code>>,
    failures: Mutex<VecDeque<Instant>>,
}

fn random_hex(bytes: usize) -> std::io::Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(std::io::Error::other)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Write a file other users cannot read, replacing it in one step.
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        use std::io::Write;
        options.open(&tmp)?.write_all(contents.as_bytes())?;
    }
    fs::rename(tmp, path)
}

/// A file's trimmed contents, or `make()` written there first.
fn read_or_create(path: &Path, make: impl FnOnce() -> std::io::Result<String>) -> std::io::Result<String> {
    if let Ok(existing) = fs::read_to_string(path) {
        let existing = existing.trim();
        if !existing.is_empty() {
            return Ok(existing.to_string());
        }
    }
    let value = make()?;
    write_private(path, &format!("{value}\n"))?;
    Ok(value)
}

/// What a person typed or pasted as a code, as the code itself.
pub fn normalize_code(input: &str) -> String {
    input.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

/// A code as shown: XXXX-XXXX-XXXX.
pub fn format_code(code: &str) -> String {
    code.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("-")
}

/// Constant-time comparison, so timing says nothing about how much matched.
pub fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Pairing {
    pub fn load(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let host_id = read_or_create(&dir.join("host-id"), || random_hex(8))?;
        let local_secret = read_or_create(&dir.join("local-secret"), || random_hex(24))?;
        let clients = match fs::read_to_string(dir.join("clients.json")) {
            Ok(text) => serde_json::from_str(&text).map_err(std::io::Error::other)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Pairing {
            dir: dir.to_path_buf(),
            host_id,
            local_secret,
            clients: Mutex::new(clients),
            codes: Mutex::new(Vec::new()),
            failures: Mutex::new(VecDeque::new()),
        })
    }

    fn save(&self, clients: &[Client]) {
        let text = serde_json::to_string_pretty(clients).unwrap_or_default();
        if let Err(e) = write_private(&self.dir.join("clients.json"), &text) {
            crate::log!("cannot save paired browsers: {e}");
        }
    }

    /// A new one-time code, valid for `CODE_TTL`.
    pub fn new_code(&self) -> std::io::Result<String> {
        let mut bytes = [0u8; CODE_LEN];
        getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
        // 256 is a multiple of 32, so masking keeps every symbol equally likely.
        let code: String = bytes.iter().map(|b| CODE_ALPHABET[(b & 31) as usize] as char).collect();
        let mut codes = self.codes.lock().unwrap();
        let now = Instant::now();
        codes.retain(|c| c.expires > now);
        if codes.len() >= MAX_CODES {
            codes.remove(0);
        }
        codes.push(Code { code: code.clone(), expires: now + CODE_TTL });
        Ok(code)
    }

    /// Spend a code: the browser gets a token of its own, labelled `label`.
    pub fn redeem(&self, code: &str, label: &str) -> Result<(Client, String), RedeemError> {
        let now = Instant::now();
        {
            let mut failures = self.failures.lock().unwrap();
            while failures.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
                failures.pop_front();
            }
            if failures.len() >= MAX_FAILURES {
                return Err(RedeemError::Busy);
            }
        }
        let code = normalize_code(code);
        let found = {
            let mut codes = self.codes.lock().unwrap();
            codes.retain(|c| c.expires > now);
            let position = codes.iter().position(|c| same(&c.code, &code));
            position.map(|i| codes.remove(i))
        };
        if found.is_none() {
            self.failures.lock().unwrap().push_back(now);
            return Err(RedeemError::Invalid);
        }
        let token = random_hex(32).map_err(|_| RedeemError::Invalid)?;
        let label: String = label.chars().filter(|c| !c.is_control()).take(80).collect();
        let client = Client {
            id: random_hex(4).map_err(|_| RedeemError::Invalid)?,
            label: if label.trim().is_empty() { "browser".into() } else { label.trim().to_string() },
            hash: hash(&token),
            created: now_secs(),
            last_seen: now_secs(),
        };
        let mut clients = self.clients.lock().unwrap();
        clients.push(client.clone());
        self.save(&clients);
        Ok((client, token))
    }

    /// The paired browser this token belongs to, if any.
    pub fn check(&self, token: &str) -> Option<Client> {
        if token.is_empty() {
            return None;
        }
        let hashed = hash(token);
        let mut clients = self.clients.lock().unwrap();
        let client = clients.iter_mut().find(|c| same(&c.hash, &hashed))?;
        client.last_seen = now_secs();
        let found = client.clone();
        self.save(&clients);
        Some(found)
    }

    pub fn list(&self) -> Vec<Client> {
        self.clients.lock().unwrap().clone()
    }

    /// Unpair a browser. Its open sessions carry on until they disconnect.
    pub fn revoke(&self, id: &str) -> bool {
        let mut clients = self.clients.lock().unwrap();
        let before = clients.len();
        clients.retain(|c| c.id != id);
        let removed = clients.len() != before;
        if removed {
            self.save(&clients);
        }
        removed
    }
}

/// This machine's name, as people know it.
pub fn machine_name() -> String {
    let name = if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()
    } else {
        fs::read_to_string("/proc/sys/kernel/hostname").or_else(|_| fs::read_to_string("/etc/hostname")).ok()
    };
    name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "peek".into())
}

/// The tailscale command, if Tailscale is installed.
pub fn tailscale_cmd() -> Option<&'static str> {
    let candidates: &[&'static str] = if cfg!(windows) {
        &["tailscale", r"C:\Program Files\Tailscale\tailscale.exe"]
    } else {
        &["tailscale"]
    };
    candidates
        .iter()
        .copied()
        .find(|cmd| std::process::Command::new(cmd).arg("version").output().is_ok_and(|o| o.status.success()))
}

/// This machine's Tailscale name (machine.tailnet.ts.net), which the hosted
/// client reaches over HTTPS through Tailscale Serve (`peek pair` sets it
/// up). `None` when Tailscale is not installed or up.
pub fn tailscale_name() -> Option<String> {
    let out = std::process::Command::new(tailscale_cmd()?).args(["status", "--json", "--peers=false"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let name = status.get("Self")?.get("DNSName")?.as_str()?.trim_end_matches('.').to_string();
    (!name.is_empty()).then_some(name)
}

/// The link that pairs a browser through the hosted client.
pub fn pair_link(web: &str, host: &str, name: &str, code: &str) -> String {
    let enc = |s: &str| -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    };
    format!("{web}/pair?host={}&name={}#code={}", enc(host), enc(name), format_code(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peek-pairing-{}", random_hex(6).unwrap()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_code_pairs_once_and_the_token_then_works() {
        let dir = temp_dir();
        let p = Pairing::load(&dir).unwrap();
        let code = p.new_code().unwrap();
        assert_eq!(code.len(), CODE_LEN);
        let (client, token) = p.redeem(&format_code(&code).to_lowercase(), "phone").ok().unwrap();
        assert_eq!(client.label, "phone");
        assert!(matches!(p.redeem(&code, "again"), Err(RedeemError::Invalid)));
        assert_eq!(p.check(&token).unwrap().id, client.id);
        assert!(p.check("nope").is_none());
        // Survives a restart; the token itself is never stored.
        let reloaded = Pairing::load(&dir).unwrap();
        assert_eq!(reloaded.host_id, p.host_id);
        assert!(reloaded.check(&token).is_some());
        assert!(!fs::read_to_string(dir.join("clients.json")).unwrap().contains(&token));
        assert!(reloaded.revoke(&client.id));
        assert!(reloaded.check(&token).is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn guessing_is_cut_off() {
        let dir = temp_dir();
        let p = Pairing::load(&dir).unwrap();
        let code = p.new_code().unwrap();
        for _ in 0..MAX_FAILURES {
            assert!(matches!(p.redeem("AAAAAAAAAAAA", ""), Err(RedeemError::Invalid)));
        }
        assert!(matches!(p.redeem(&code, ""), Err(RedeemError::Busy)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn links_carry_the_code_in_the_fragment() {
        let link = pair_link("https://peek.example.com", "hp-book.tail1.ts.net", "hp book", "ABCDEFGHJKLM");
        assert_eq!(link, "https://peek.example.com/pair?host=hp-book.tail1.ts.net&name=hp%20book#code=ABCD-EFGH-JKLM");
    }
}
