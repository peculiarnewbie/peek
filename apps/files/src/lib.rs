//! A file explorer for peek, as a plugin: browse the folders shared on this
//! machine, and download their files (a spike: see docs/frames.md).
//!
//! It is a program of its own, with nothing of peek's in it. Peek starts it
//! in the plugin's folder and talks to it over its stdin and stdout; the
//! page in `page/` talks to it through peek.
//!
//! Build it, and put the program where `peek-plugin.json` says it is:
//!
//!   cargo build --release
//!   (Windows)  copy target\release\peek-files.exe bin\
//!   (Linux)    cp target/release/peek-files bin/
//!
//! then `peek plugin add <this folder>`, and choose the folders to share in
//! its settings, on the Plugins page.
//!
//! Only those folders, and what is inside them, can be seen. A path is
//! checked after links and junctions are followed, so one that leads out of
//! a shared folder is refused. Hidden and system files are left out.
//! Read-only.

use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

// What a message on the pipe is: u32 length, u8 kind, u64 channel
// (little-endian), then the payload.
const TEXT: u8 = 3;
const LOG: u8 = 6;
const ABOUT: u8 = 7;
const OFFER: u8 = 8;
const HEADER: usize = 13;
/// More than a browser has to ask of a file explorer.
const MAX_ASK: usize = 64 << 10;
/// The most entries one folder lists.
const MAX_LIST: usize = 5000;
/// Never listed, nor given.
const SKIP: [&str; 7] = ["$recycle.bin", "system volume information", "desktop.ini", "thumbs.db", "pagefile.sys", "hiberfil.sys", "swapfile.sys"];

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
        self.send(LOG, 0, format!("files: {line}").as_bytes());
    }
}

/// A shared folder, as the machine's owner named it.
struct Root {
    id: String,
    label: String,
    real: PathBuf,
}

pub const PAGE: &str = include_str!("../page/index.html");

pub fn run(page: Option<PathBuf>) {
    let mut pipe = Pipe(BufWriter::new(io::stdout()));

    // From the plugin's settings (PEEK_SETTINGS), or the arguments.
    let settings: Value = std::env::var("PEEK_SETTINGS").ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    let mut asked: Vec<String> = settings["folders"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
    if asked.is_empty() && page.is_some() {
        asked = std::env::args().skip(1).collect();
    }
    let mut roots = Vec::new();
    for folder in &asked {
        match real(Path::new(folder)) {
            Ok(path) if path.is_dir() => roots.push(Root {
                id: roots.len().to_string(),
                label: path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
                real: path,
            }),
            Ok(_) => pipe.log(&format!("not sharing {folder}: not a folder")),
            Err(e) => pipe.log(&format!("not sharing {folder}: {e}")),
        }
    }

    let mut about = json!({ "places": [] });
    if let Some(page) = page { about["page"] = page.display().to_string().into(); }
    pipe.json(ABOUT, 0, about);
    let shared: Vec<String> = roots.iter().map(|r| r.real.display().to_string()).collect();
    pipe.log(&format!("sharing {}", if shared.is_empty() { "nothing".into() } else { shared.join(", ") }));

    let mut input = BufReader::new(io::stdin().lock());
    let mut head = [0u8; HEADER];
    // Until peek closes the pipe.
    while input.read_exact(&mut head).is_ok() {
        let length = u32::from_le_bytes(head[..4].try_into().unwrap()) as usize;
        let kind = head[4];
        let channel = u64::from_le_bytes(head[5..].try_into().unwrap());
        if length > MAX_ASK {
            // Not something a page asks: pass over it.
            if io::copy(&mut (&mut input).take(length as u64), &mut io::sink()).is_err() {
                break;
            }
            continue;
        }
        let mut payload = vec![0; length];
        if input.read_exact(&mut payload).is_err() {
            break;
        }
        if kind != TEXT {
            continue;
        }
        let Ok(ask) = serde_json::from_slice::<Value>(&payload) else { continue };
        answer(&mut pipe, &roots, channel, &ask);
    }
}

fn answer(pipe: &mut Pipe, roots: &[Root], channel: u64, ask: &Value) {
    let root = ask["root"].as_str().unwrap_or("");
    let path = ask["path"].as_str().unwrap_or("");
    let done = match ask["t"].as_str() {
        Some("roots") => Ok(json!({ "t": "roots", "roots": roots.iter().map(|r| json!({ "id": r.id, "label": r.label })).collect::<Vec<_>>() })),
        Some("list") => list(roots, root, path).map(|entries| json!({ "t": "list", "root": root, "path": path, "entries": entries })),
        Some("get") => match resolve(roots, root, path) {
            Ok(file) if file.is_file() => {
                // Peek makes a link that downloads it once, and sends it on this channel.
                let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                pipe.json(OFFER, channel, json!({ "path": file.display().to_string(), "name": name, "ref": ask["ref"] }));
                return;
            }
            Ok(_) => Err("not a file".to_string()),
            Err(e) => Err(e),
        },
        _ => Err("files does not know that".to_string()),
    };
    let reply = done.unwrap_or_else(|why| json!({ "t": "error", "ref": ask["ref"], "why": why }));
    pipe.json(TEXT, channel, reply);
}

fn root_of<'a>(roots: &'a [Root], id: &str) -> Result<&'a Root, String> {
    roots.iter().find(|r| r.id == id).ok_or_else(|| "no such shared folder".to_string())
}

/// The real path of `rel` in a shared folder: only plain names, nothing
/// hidden on the way, and still inside it once links are followed.
fn resolve(roots: &[Root], id: &str, rel: &str) -> Result<PathBuf, String> {
    let root = root_of(roots, id)?;
    let mut path = root.real.clone();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        if part == "." || part == ".." || hidden_name(part) || part.contains(['\\', ':', '\0']) {
            return Err("not a path in this folder".into());
        }
        path.push(part);
        if fs::metadata(&path).is_ok_and(|m| hidden_attrs(&m)) {
            return Err("not a path in this folder".into());
        }
    }
    let found = real(&path).map_err(|e| why(&e))?;
    if !inside(&found, &root.real) {
        return Err("that leads out of the shared folder".into());
    }
    Ok(found)
}

fn list(roots: &[Root], id: &str, rel: &str) -> Result<Vec<Value>, String> {
    let dir = resolve(roots, id, rel)?;
    let root = root_of(roots, id)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| why(&e))?.flatten().take(MAX_LIST) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if hidden_name(&name) {
            continue;
        }
        // Where it really is: a link or a junction counts only if that is still shared.
        let Ok(target) = real(&entry.path()) else { continue };
        if !inside(&target, &root.real) {
            continue;
        }
        let Ok(meta) = fs::metadata(&target) else { continue };
        if hidden_attrs(&meta) {
            continue;
        }
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0.0, |d| d.as_millis() as f64);
        entries.push(json!({ "name": name, "dir": meta.is_dir(), "size": if meta.is_dir() { Value::Null } else { json!(meta.len()) }, "mtime": mtime }));
    }
    Ok(entries)
}

fn why(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "it is not there".into(),
        io::ErrorKind::PermissionDenied => "not allowed to read it".into(),
        _ => e.to_string(),
    }
}

fn hidden_name(name: &str) -> bool {
    name.starts_with('.') || SKIP.contains(&name.to_lowercase().as_str())
}

/// Marked hidden or system, as Windows keeps its own files.
#[cfg(windows)]
fn hidden_attrs(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    meta.file_attributes() & (HIDDEN | SYSTEM) != 0
}

#[cfg(not(windows))]
fn hidden_attrs(_: &fs::Metadata) -> bool {
    false
}

/// The path with every link followed, written as people write it
/// (`D:\Shared`, not `\\?\D:\Shared`), so that paths compare as they should.
fn real(path: &Path) -> io::Result<PathBuf> {
    let found = fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let text = found.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        if let Some(plain) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(plain));
        }
    }
    Ok(found)
}

/// `path` is `root`, or somewhere under it.
fn inside(path: &Path, root: &Path) -> bool {
    if cfg!(windows) {
        let (p, r) = (path.to_string_lossy().to_lowercase(), root.to_string_lossy().to_lowercase());
        let r = r.trim_end_matches('\\');
        p == r || p.starts_with(&format!("{r}\\"))
    } else {
        path.starts_with(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (PathBuf, Vec<Root>) {
        let base = std::env::temp_dir().join(format!("peek-files-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let shared = base.join("shared");
        fs::create_dir_all(shared.join("Docs")).unwrap();
        fs::create_dir_all(base.join("outside")).unwrap();
        fs::write(shared.join("Docs").join("a.txt"), "a").unwrap();
        fs::write(shared.join(".hidden"), "h").unwrap();
        fs::write(base.join("outside").join("secret.txt"), "s").unwrap();
        let real = real(&shared).unwrap();
        (base, vec![Root { id: "0".into(), label: "shared".into(), real }])
    }

    #[test]
    fn only_paths_inside_a_share() {
        let (base, roots) = tree();
        assert!(resolve(&roots, "0", "Docs/a.txt").is_ok());
        for bad in ["..", "Docs/../../outside/secret.txt", "..\\outside\\secret.txt", "C:/Windows/win.ini", ".hidden", "Docs/./a.txt"] {
            assert!(resolve(&roots, "0", bad).is_err(), "{bad}");
        }
        assert!(resolve(&roots, "7", "").is_err());
        let names: Vec<String> = list(&roots, "0", "").unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["Docs"]);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn inside_is_by_whole_names() {
        let root = if cfg!(windows) { Path::new(r"D:\Shared") } else { Path::new("/srv/shared") };
        let (same, under, beside) = if cfg!(windows) {
            (r"d:\shared", r"D:\Shared\a", r"D:\SharedX\a")
        } else {
            ("/srv/shared", "/srv/shared/a", "/srv/sharedx/a")
        };
        assert!(inside(Path::new(same), root));
        assert!(inside(Path::new(under), root));
        assert!(!inside(Path::new(beside), root));
    }
}
