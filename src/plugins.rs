//! Plugins: every provider beside the screen, which of them run, and the
//! ones installed here (a spike: see docs/frames.md).
//!
//! Three kinds, all providers to the server:
//!
//! - peek's own (`vitals`, `power`);
//! - commands added with `peek provider add` (`providers.json`);
//! - plugins: folders in `plugins/`, each with a `peek-plugin.json` that
//!   says what it is, the command that runs it, what it needs on the
//!   machine, and its settings.
//!
//! Which are turned off, each plugin's settings, and whether a browser may
//! install, remove or change plugins are kept in `plugins.json`. A plugin is
//! an ordinary program running as peek's user: peek cannot hold it to what
//! it says it does, and says so where it is installed.
//!
//! The plugins page (`/p/plugins/`, in peek itself) speaks to `answer`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::provider::{Link, Program, Start};

pub const PROVIDERS_FILE: &str = "providers.json";
const STATE_FILE: &str = "plugins.json";
const PLUGINS_DIR: &str = "plugins";
const DATA_DIR: &str = "plugin-data";
const STAGING_DIR: &str = ".staging";
const MANIFEST: &str = "peek-plugin.json";
/// Names that are peek's own, which nothing added may take.
pub const OWN: [&str; 6] = ["screen", "vitals", "power", "plugins", "files", "processes"];
/// How long a plugin fetched to be looked at waits to be installed.
const STAGED_FOR: Duration = Duration::from_secs(10 * 60);

/// A name that can stand in an address and a message: short, lowercase
/// letters, digits and dashes, and not one of peek's own.
pub fn name_ok(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !OWN.contains(&name)
}

pub fn read_commands(dir: &Path) -> BTreeMap<String, Vec<String>> {
    fs::read_to_string(dir.join(PROVIDERS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// What a plugin says of itself, in `peek-plugin.json`.
#[derive(Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Version of the manifest and pipe contract. Omitted means version 1.
    #[serde(default = "contract_version")]
    pub manifest_version: u32,
    #[serde(default = "contract_version")]
    pub provider_protocol: u32,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// The command that runs it, started in its folder.
    pub run: Vec<String>,
    /// Programs it needs on the machine, like `node`.
    #[serde(default)]
    pub needs: Vec<String>,
    /// Its settings, by key: `{"label": …, "kind": "folders" | "text" | "switch", "default": …}`.
    /// They reach it as JSON in `PEEK_SETTINGS`.
    #[serde(default)]
    pub settings: Map<String, Value>,
    #[serde(default = "yes")]
    pub on_by_default: bool,
}

#[derive(Serialize, Deserialize)]
struct State {
    /// A browser may install, remove and change plugins (`peek plugin remote-install`).
    #[serde(default = "yes")]
    remote_install: bool,
    /// Turned off: not started.
    #[serde(default)]
    off: BTreeSet<String>,
    #[serde(default)]
    on: BTreeSet<String>,
    /// Each plugin's settings, as last saved.
    #[serde(default)]
    settings: BTreeMap<String, Map<String, Value>>,
    #[serde(default)]
    sources: BTreeMap<String, String>,
}

fn contract_version() -> u32 { 1 }

fn yes() -> bool {
    true
}

impl Default for State {
    fn default() -> Self {
        State { remote_install: true, off: BTreeSet::new(), on: BTreeSet::new(), settings: BTreeMap::new(), sources: BTreeMap::new() }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Own,
    Command,
    Plugin,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Own => "peek's own",
            Kind::Command => "a command",
            Kind::Plugin => "a plugin",
        }
    }
}

/// Everything there is to start.
struct Spec {
    name: String,
    kind: Kind,
    start: Start,
    description: String,
    /// What it runs, as shown before installing and on the page.
    runs: String,
    manifest: Option<Manifest>,
    dir: Option<PathBuf>,
}

/// A plugin fetched to be looked at, before it is installed.
struct Staged {
    dir: PathBuf,
    /// The folder inside `dir` holding the plugin (npm puts it in `package/`).
    root: PathBuf,
    manifest: Manifest,
    from: String,
    until: Instant,
}

/// The files: what is installed and how it is set. Used by `peek plugin`
/// commands as well as by a running peek.
pub struct Catalog {
    dir: PathBuf,
    own: Vec<(&'static str, Start)>,
}

impl Catalog {
    /// `dir`: peek's config folder. `own`: peek's own providers, with what
    /// each is and how it starts.
    pub fn new(dir: PathBuf, own: Vec<(&'static str, Start)>) -> Catalog {
        Catalog { dir, own }
    }

    fn plugins_dir(&self) -> PathBuf {
        self.dir.join(PLUGINS_DIR)
    }

    pub fn data_dir(&self, name: &str) -> PathBuf {
        let dir = self.dir.join(DATA_DIR).join(name);
        if dir.is_absolute() { dir } else { std::env::current_dir().unwrap_or_default().join(dir) }
    }

    pub fn has_data(&self, name: &str) -> bool {
        fs::read_dir(self.data_dir(name)).ok().is_some_and(|mut entries| entries.next().is_some())
    }

    pub fn kept_data(&self) -> Vec<String> {
        let specs = self.specs();
        let mut names: Vec<String> = fs::read_dir(self.dir.join(DATA_DIR)).into_iter().flatten().flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| name_ok(n) && self.has_data(n) && !specs.iter().any(|s| s.name == *n && s.kind == Kind::Plugin)).collect();
        names.sort();
        names
    }

    fn state(&self) -> State {
        fs::read_to_string(self.dir.join(STATE_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, state: &State) -> Result<(), String> {
        let path = self.dir.join(STATE_FILE);
        fs::create_dir_all(&self.dir)
            .and_then(|()| fs::write(&path, serde_json::to_string_pretty(state).unwrap_or_default()))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    pub fn remote_install(&self) -> bool {
        self.state().remote_install
    }

    pub fn set_remote_install(&self, on: bool) -> Result<(), String> {
        let mut state = self.state();
        state.remote_install = on;
        self.save(&state)
    }

    /// Everything there is, on or off.
    fn specs(&self) -> Vec<Spec> {
        let state = self.state();
        let mut all: Vec<Spec> = self.own.iter().filter_map(|(text, start)| {
            let mut manifest = match parse_manifest(text, true) {
                Ok(m) => m,
                Err(e) => { crate::log!("plugins: built-in manifest: {e}"); return None; }
            };
            manifest.version = env!("CARGO_PKG_VERSION").into();
            let name = manifest.name.clone();
            let env = vec![("PEEK_SETTINGS".into(), Value::Object(settings_of(&manifest, state.settings.get(&name))).to_string()),
                ("PEEK_DATA_DIR".into(), self.data_dir(&name).to_string_lossy().into_owned())];
            let start = match start { Start::Own { flags, .. } => Start::Own { flags: flags.clone(), env }, _ => start.clone() };
            Some(Spec { name, kind: Kind::Own, start, description: manifest.description.clone(),
                runs: format!("peek --provider {}", manifest.name), manifest: Some(manifest), dir: None })
        }).collect();
        for (name, words) in read_commands(&self.dir) {
            if !name_ok(&name) || words.is_empty() || all.iter().any(|s| s.name == name) {
                crate::log!("{PROVIDERS_FILE}: skipped {name:?}, which is no name for a provider, is taken, or has no command");
                continue;
            }
            all.push(Spec {
                name,
                kind: Kind::Command,
                runs: words.join(" "),
                start: Start::Program(Program { words, ..Default::default() }),
                description: String::new(),
                manifest: None,
                dir: None,
            });
        }
        let Ok(entries) = fs::read_dir(self.plugins_dir()) else { return all };
        for entry in entries.flatten() {
            let dir = entry.path();
            let folder = entry.file_name().to_string_lossy().into_owned();
            if folder.starts_with('.') || !dir.is_dir() {
                continue;
            }
            let manifest = match read_manifest(&dir) {
                Ok(m) if m.name == folder => m,
                Ok(m) => {
                    crate::log!("plugins: {folder} says it is {:?}; skipped", m.name);
                    continue;
                }
                Err(e) => {
                    crate::log!("plugins: {folder}: {e}");
                    continue;
                }
            };
            if all.iter().any(|s| s.name == manifest.name) {
                crate::log!("plugins: {folder} has the name of another provider; skipped");
                continue;
            }
            let settings = settings_of(&manifest, state.settings.get(&manifest.name));
            let mut words = manifest.run.clone();
            // The command itself, found where the machine keeps programs.
            if let Some(found) = words.first().and_then(|w| find_program(w, Some(&dir))) {
                words[0] = found.to_string_lossy().into_owned();
            }
            all.push(Spec {
                name: manifest.name.clone(),
                kind: Kind::Plugin,
                runs: manifest.run.join(" "),
                start: Start::Program(Program {
                    words,
                    dir: Some(dir.clone()),
                    env: vec![("PEEK_SETTINGS".into(), Value::Object(settings).to_string()),
                        ("PEEK_DATA_DIR".into(), self.data_dir(&manifest.name).to_string_lossy().into_owned())],
                }),
                description: manifest.description.clone(),
                manifest: Some(manifest),
                dir: Some(dir),
            });
        }
        all
    }

    /// Remove a plugin or an added command; peek's own cannot be.
    pub fn remove(&self, name: &str, purge: bool) -> Result<(), String> {
        let spec = self.specs().into_iter().find(|s| s.name == name);
        let Some(spec) = spec else {
            if purge && name_ok(name) && self.has_data(name) {
                return fs::remove_dir_all(self.data_dir(name)).map_err(|e| format!("cannot remove data for {name}: {e}"));
            }
            return Err(format!("nothing here is called {name}"));
        };
        match spec.kind {
            Kind::Own => return Err(format!("{name} is part of peek: it can be turned off, and it updates with peek")),
            Kind::Command => {
                let mut all = read_commands(&self.dir);
                all.remove(name);
                let path = self.dir.join(PROVIDERS_FILE);
                fs::write(&path, serde_json::to_string_pretty(&all).unwrap_or_default())
                    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            }
            Kind::Plugin => {
                if let Some(dir) = spec.dir {
                    fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
                }
                if purge && self.data_dir(name).exists() {
                    fs::remove_dir_all(self.data_dir(name)).map_err(|e| format!("cannot remove data for {name}: {e}"))?;
                }
            }
        }
        let mut state = self.state();
        state.off.remove(name);
        state.settings.remove(name);
        state.sources.remove(name);
        self.save(&state)
    }

    pub fn set_on(&self, name: &str, on: bool) -> Result<(), String> {
        if name == "screen" || !self.specs().iter().any(|s| s.name == name) {
            return Err(format!("{name} cannot be turned on or off here"));
        }
        let mut state = self.state();
        if on { state.off.remove(name); state.on.insert(name.into()); } else { state.on.remove(name); state.off.insert(name.into()); }
        self.save(&state)
    }

    fn set_settings(&self, name: &str, values: &Map<String, Value>) -> Result<(), String> {
        let spec = self.specs().into_iter().find(|s| s.name == name).ok_or_else(|| format!("nothing here is called {name}"))?;
        let manifest = spec.manifest.ok_or_else(|| format!("{name} has no settings"))?;
        let mut clean = Map::new();
        for (key, about) in &manifest.settings {
            let Some(value) = values.get(key) else { continue };
            clean.insert(key.clone(), check_setting(key, about, value)?);
        }
        let mut state = self.state();
        state.settings.insert(name.into(), clean);
        self.save(&state)
    }

    // ---- installing ----

    /// Fetch a plugin into a folder of its own to be looked at: from a
    /// folder on this machine, a git repository (https), or npm.
    fn fetch(&self, source: &str) -> Result<Staged, String> {
        let source = source.trim();
        let local = Path::new(source);
        let absolute = if local.is_dir() || (local.is_file() && source.ends_with(".tgz")) {
            Some(std::path::absolute(local).map_err(|e| format!("cannot find {source}: {e}"))?.to_string_lossy().into_owned())
        } else { None };
        let source = absolute.as_deref().unwrap_or(source);
        let staging = self.plugins_dir().join(STAGING_DIR).join(crate::pairing::random_hex(8).map_err(|e| e.to_string())?);
        fs::create_dir_all(&staging).map_err(|e| format!("cannot make {}: {e}", staging.display()))?;
        let fetched = (|| {
            let root = if Path::new(source).is_dir() {
                let root = staging.join("plugin");
                copy_dir(Path::new(source), &root).map_err(|e| format!("cannot copy {source}: {e}"))?;
                root
            } else if source.starts_with("https://") {
                if !source.bytes().all(|b| b.is_ascii_alphanumeric() || b"._~:/@%+-".contains(&b)) {
                    return Err("that is no address of a repository".into());
                }
                let git = find_program("git", None).ok_or("git is not installed here")?;
                let root = staging.join("plugin");
                run(std::process::Command::new(git).args(["clone", "--depth", "1", "--", source]).arg(&root), "git")?;
                let _ = fs::remove_dir_all(root.join(".git"));
                root
            } else if npm_spec_ok(source) || (Path::new(source).is_file() && source.ends_with(".tgz")) {
                let npm = find_program("npm", None).ok_or("npm is not installed here")?;
                let tar = find_program("tar", None).ok_or("tar is not installed here")?;
                run(std::process::Command::new(npm).args(["pack", "--ignore-scripts", source, "--pack-destination"]).arg(&staging), "npm")?;
                let tgz = fs::read_dir(&staging)
                    .ok()
                    .and_then(|d| d.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "tgz")))
                    .ok_or("npm did not fetch it")?;
                run(std::process::Command::new(tar).arg("-xzf").arg(&tgz).arg("-C").arg(&staging), "tar")?;
                staging.join("package")
            } else {
                return Err("give a folder on this machine, an https address of a git repository, or an npm package".into());
            };
            let manifest = read_manifest(&root)?;
            if !name_ok(&manifest.name) {
                return Err(format!("{:?} cannot name a plugin: up to 32 lowercase letters, digits and dashes", manifest.name));
            }
            if manifest.run.is_empty() {
                return Err(format!("{MANIFEST} says nothing to run"));
            }
            Ok(Staged { dir: staging.clone(), root, manifest, from: source.into(), until: Instant::now() + STAGED_FOR })
        })();
        if fetched.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        fetched
    }

    /// What installing it would mean, said before it is installed.
    fn describe(&self, staged: &Staged) -> Map<String, Value> {
        let m = &staged.manifest;
        let missing: Vec<&String> = m.needs.iter().filter(|n| find_program(n, None).is_none()).collect();
        let specs = self.specs();
        let taken = specs.iter().any(|s| s.name == m.name);
        let installed = specs.iter().find(|s| s.name == m.name && s.kind == Kind::Plugin).and_then(|s| s.manifest.as_ref());
        let deps = has_dependencies(&staged.root);
        let mut out = Map::new();
        out.insert("name".into(), m.name.clone().into());
        out.insert("version".into(), m.version.clone().into());
        out.insert("installed".into(), installed.map_or(Value::Null, |m| json!({"version": m.version})));
        out.insert("data".into(), self.has_data(&m.name).into());
        out.insert("description".into(), m.description.clone().into());
        out.insert("runs".into(), m.run.join(" ").into());
        out.insert("needs".into(), json!(m.needs));
        out.insert("missing".into(), json!(missing));
        out.insert("from".into(), staged.from.clone().into());
        out.insert("taken".into(), taken.into());
        out.insert("dependencies".into(), deps.into());
        out
    }

    /// Install what was fetched: into `plugins/<name>`, with its npm
    /// dependencies if it has some (installed without running their scripts).
    fn install(&self, staged: Staged) -> Result<String, String> {
        let name = staged.manifest.name.clone();
        let result = (|| {
            if staged.until <= Instant::now() {
                return Err("look at it again first: the staged plugin expired".into());
            }
            if self.specs().iter().any(|s| s.name == name) {
                return Err(format!("{name} is installed already: remove it first"));
            }
            self.prepare(&staged)?;
            let dest = self.plugins_dir().join(&name);
            fs::rename(&staged.root, &dest).map_err(|e| format!("cannot move it into {}: {e}", dest.display()))?;
            let mut state = self.state();
            state.off.remove(&name);
            state.on.remove(&name);
            state.sources.insert(name.clone(), staged.from.clone());
            self.save(&state)
        })();
        let _ = fs::remove_dir_all(&staged.dir);
        result.map(|()| name)
    }

    fn prepare(&self, staged: &Staged) -> Result<(), String> {
            let missing: Vec<_> = staged.manifest.needs.iter().filter(|n| find_program(n, None).is_none()).collect();
            if !missing.is_empty() {
                return Err(format!("missing required programs: {}", missing.into_iter().cloned().collect::<Vec<_>>().join(", ")));
            }
            if has_dependencies(&staged.root) && !staged.root.join("node_modules").is_dir() {
                let npm = find_program("npm", None).ok_or("it has npm dependencies, and npm is not installed here")?;
                run(
                    std::process::Command::new(npm).args(["install", "--omit=dev", "--ignore-scripts"]).current_dir(&staged.root),
                    "npm install",
                )?;
            }
            if find_program(&staged.manifest.run[0], Some(&staged.root)).is_none() {
                return Err(format!("plugin program not found: {}", staged.manifest.run[0]));
            }
        Ok(())
    }

    fn update(&self, staged: Staged) -> Result<String, String> {
        let name = staged.manifest.name.clone();
        let result = (|| {
            if staged.until <= Instant::now() { return Err("look at it again first: the staged plugin expired".into()); }
            if !self.specs().iter().any(|s| s.name == name && s.kind == Kind::Plugin) {
                return Err(format!("{name} is not installed: install it instead"));
            }
            self.prepare(&staged)?;
            let dest = self.plugins_dir().join(&name);
            let old = staged.dir.join("old");
            let until = Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match fs::rename(&dest, &old) {
                    Ok(()) => break,
                    Err(e) if Instant::now() >= until => return Err(format!("cannot move {}: {e}", dest.display())),
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
            if let Err(e) = fs::rename(&staged.root, &dest) {
                fs::rename(&old, &dest).map_err(|restore| format!("cannot move it into {}: {e}; cannot restore old plugin: {restore}", dest.display()))?;
                return Err(format!("cannot move it into {}: {e}", dest.display()));
            }
            let mut state = self.state();
            if let Some(settings) = state.settings.get_mut(&name) {
                settings.retain(|key, value| staged.manifest.settings.get(key).is_some_and(|def| check_setting(key, def, value).is_ok()));
            }
            state.sources.insert(name.clone(), staged.from.clone());
            self.save(&state)
        })();
        // Keep the backup if restoring it failed.
        if !staged.dir.join("old").exists() || self.plugins_dir().join(&name).exists() {
            let _ = fs::remove_dir_all(&staged.dir);
        }
        result.map(|()| name)
    }

    pub fn update_from(&self, name: &str, source: Option<&str>, say: impl Fn(&str)) -> Result<String, String> {
        if self.specs().iter().any(|s| s.name == name && s.kind == Kind::Own) {
            return Err(format!("{name} is part of peek: it can be turned off, and it updates with peek"));
        }
        let source = source.map(str::to_owned).or_else(|| self.state().sources.get(name).cloned())
            .ok_or_else(|| format!("give a source for {name}"))?;
        let staged = self.fetch(&source)?;
        if staged.manifest.name != name {
            let other = staged.manifest.name.clone();
            let _ = fs::remove_dir_all(&staged.dir);
            return Err(format!("that is {other}, not {name}"));
        }
        let old = self.specs().into_iter().find(|s| s.name == name).and_then(|s| s.manifest).map(|m| m.version).unwrap_or_else(|| "unknown".into());
        let new = if staged.manifest.version.is_empty() { "unknown" } else { &staged.manifest.version };
        let d = self.describe(&staged);
        say(&format!("{name}: {} ({old} to {new})", staged.manifest.description));
        say(&format!("  runs:  {} (as this machine's peek user, with its full rights)", d["runs"].as_str().unwrap_or("")));
        if let Some(missing) = d["missing"].as_array().filter(|m| !m.is_empty()) {
            say(&format!("  needs: {} (not found here)", missing.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")));
        }
        self.update(staged)
    }

    /// Fetch and install at once, saying first what it is: for `peek plugin add`.
    pub fn add(&self, source: &str, say: impl Fn(&str)) -> Result<String, String> {
        let staged = self.fetch(source)?;
        let d = self.describe(&staged);
        say(&format!("{}: {}", staged.manifest.name, staged.manifest.description));
        say(&format!("  runs:  {} (as this machine's peek user, with its full rights)", d["runs"].as_str().unwrap_or("")));
        if let Some(missing) = d["missing"].as_array().filter(|m| !m.is_empty()) {
            say(&format!("  needs: {} (not found here)", missing.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")));
        }
        self.install(staged)
    }

    pub fn list(&self) -> Vec<(String, &'static str, String, bool)> {
        let state = self.state();
        self.specs().into_iter().map(|s| { let on = is_on(&state, &s); (s.name, s.kind.word(), s.runs, on) }).collect()
    }

    pub fn clear_staging(&self) {
        let _ = fs::remove_dir_all(self.plugins_dir().join(STAGING_DIR));
    }
}

/// The running providers, kept as the catalogue says: started, stopped and
/// started again as plugins are turned on and off, installed and removed.
pub struct Plugins {
    pub catalog: Catalog,
    view_only: bool,
    running: RwLock<BTreeMap<String, Arc<Link>>>,
    staged: Mutex<HashMap<String, Staged>>,
    /// One change at a time.
    changing: Mutex<()>,
}

impl Plugins {
    pub fn start(catalog: Catalog, view_only: bool) -> Arc<Plugins> {
        catalog.clear_staging();
        let plugins = Arc::new(Plugins {
            catalog,
            view_only,
            running: RwLock::default(),
            staged: Mutex::default(),
            changing: Mutex::default(),
        });
        plugins.reconcile();
        plugins
    }

    pub fn get(&self, name: &str) -> Option<Arc<Link>> {
        self.running.read().unwrap().get(name).cloned()
    }

    pub fn running(&self) -> Vec<(String, Arc<Link>)> {
        self.running.read().unwrap().iter().map(|(n, l)| (n.clone(), l.clone())).collect()
    }

    /// Start what should run and is not; stop what runs and should not.
    /// `again`: stop and start this one whatever it is doing.
    fn apply(&self, again: Option<&str>) {
        let _one = self.changing.lock().unwrap();
        let state = self.catalog.state();
        let wanted: Vec<Spec> = self.catalog.specs().into_iter().filter(|s| is_on(&state, s)).collect();
        let mut running = self.running.write().unwrap();
        let gone: Vec<String> = running
            .keys()
            .filter(|n| !wanted.iter().any(|s| &s.name == *n) || again == Some(n.as_str()))
            .cloned()
            .collect();
        for name in gone {
            if let Some(link) = running.remove(&name) {
                crate::log!("plugins: stopping {name}");
                link.stop();
            }
        }
        for spec in wanted {
            if !running.contains_key(&spec.name) {
                if spec.manifest.is_some() {
                    let dir = self.catalog.data_dir(&spec.name);
                    if let Err(e) = fs::create_dir_all(&dir) {
                        crate::log!("plugins: cannot make {}: {e}", dir.display());
                    }
                }
                crate::log!("plugins: starting {} ({})", spec.name, spec.runs);
                running.insert(spec.name.clone(), Link::spawn(&spec.name, spec.start, self.view_only));
            }
        }
    }

    pub fn reconcile(&self) {
        self.apply(None);
    }

    /// The plugins page's requests, which come from a browser: it may only
    /// look on a view-only machine, and may only turn things on and off and
    /// restart them where installing from a browser is turned off.
    pub fn answer(&self, ask: &Value) -> Value {
        let at = ask["ref"].clone();
        let name = ask["name"].as_str().unwrap_or("");
        let done = |r: Result<(), String>| match r {
            Ok(()) => json!({ "t": "ok", "ref": at }),
            Err(why) => json!({ "t": "error", "ref": at, "why": why }),
        };
        let what = ask["t"].as_str().unwrap_or("");
        if what == "list" {
            return self.list();
        }
        if self.view_only {
            return done(Err("this machine is view-only".into()));
        }
        let changes = matches!(what, "settings" | "remove" | "inspect" | "install" | "update");
        if changes && !self.catalog.remote_install() {
            return done(Err("Installing and changing plugins from a browser is turned off on this machine. \
                             On it, `peek plugin remote-install on` turns it on."
                .into()));
        }
        match what {
            "on" => done(self.catalog.set_on(name, ask["on"].as_bool().unwrap_or(true)).map(|()| self.reconcile())),
            "restart" => done(if self.get(name).is_some() { self.apply(Some(name)); Ok(()) } else { Err(format!("{name} is not running")) }),
            "settings" => done(
                self.catalog
                    .set_settings(name, ask["values"].as_object().unwrap_or(&Map::new()))
                    .map(|()| self.apply(Some(name))),
            ),
            "remove" => done(self.stop_then(name, || self.catalog.remove(name, ask["purge"].as_bool().unwrap_or(false)))),
            "inspect" => match self.catalog.fetch(ask["source"].as_str().unwrap_or("")) {
                Ok(staged) => {
                    let mut out = self.catalog.describe(&staged);
                    let id = crate::pairing::random_hex(12).unwrap_or_default();
                    let now = Instant::now();
                    let mut all = self.staged.lock().unwrap();
                    all.retain(|_, s| {
                        let keep = s.until > now;
                        if !keep {
                            let _ = fs::remove_dir_all(&s.dir);
                        }
                        keep
                    });
                    all.insert(id.clone(), staged);
                    out.insert("t".into(), "inspected".into());
                    out.insert("ref".into(), at.clone());
                    out.insert("id".into(), id.into());
                    Value::Object(out)
                }
                Err(why) => done(Err(why)),
            },
            "update" => {
                let staged = self.staged.lock().unwrap().remove(ask["id"].as_str().unwrap_or(""));
                let Some(staged) = staged else { return done(Err("look at it again first: that was a while ago".into())) };
                let name = staged.manifest.name.clone();
                match self.stop_then(&name, || self.catalog.update(staged).map(|_| ())) {
                    Ok(()) => {
                        crate::log!("plugins: updated {name} from a browser");
                        json!({ "t": "ok", "ref": at, "name": name })
                    }
                    Err(why) => done(Err(why)),
                }
            }
            "install" => {
                let staged = self.staged.lock().unwrap().remove(ask["id"].as_str().unwrap_or(""));
                let Some(staged) = staged else { return done(Err("look at it again first: that was a while ago".into())) };
                match self.catalog.install(staged) {
                    Ok(name) => {
                        crate::log!("plugins: installed {name} from a browser");
                        self.reconcile();
                        json!({ "t": "ok", "ref": at, "name": name })
                    }
                    Err(why) => done(Err(why)),
                }
            }
            _ => done(Err("the plugins page does not know that".into())),
        }
    }

    /// Stop a provider before its files go.
    fn stop_then(&self, name: &str, change: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        if let Some(link) = self.running.write().unwrap().remove(name) {
            link.stop();
        }
        let result = change();
        self.reconcile();
        result
    }

    fn list(&self) -> Value {
        let state = self.catalog.state();
        let items: Vec<Value> = std::iter::once(json!({
            "name": "screen", "kind": Kind::Own.word(), "on": true, "fixed": true,
            "description": "The screen, keyboard and mouse.", "runs": "peek",
        }))
        .chain(self.catalog.specs().into_iter().map(|s| {
            let link = self.get(&s.name);
            let mut item = json!({
                "name": s.name,
                "kind": s.kind.word(),
                "description": s.description,
                "runs": s.runs,
                "on": is_on(&state, &s),
                "removable": s.kind != Kind::Own,
                "status": link.as_ref().map(|l| l.status()),
                "page": link.as_ref().is_some_and(|l| l.about().contains_key("page")),
            });
            if let Some(m) = &s.manifest {
                item["version"] = json!(m.version);
                item["from"] = json!(state.sources.get(&s.name));
                item["data"] = json!(self.catalog.has_data(&s.name));
                item["settings"] = Value::Object(m.settings.clone());
                item["values"] = Value::Object(settings_of(m, state.settings.get(&s.name)));
                item["needs"] = json!(m.needs);
                item["missing"] = json!(m.needs.iter().filter(|n| find_program(n, None).is_none()).collect::<Vec<_>>());
            }
            item
        }))
        .collect();
        json!({
            "t": "list",
            "machine": crate::pairing::machine_name(),
            "items": items,
            "kept_data": self.catalog.kept_data(),
            "remote_install": state.remote_install,
            "view_only": self.view_only,
        })
    }
}

// ---- helpers ----

fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let text = fs::read_to_string(dir.join(MANIFEST)).map_err(|_| format!("there is no {MANIFEST} in it"))?;
    parse_manifest(&text, false)
}

fn parse_manifest(text: &str, own: bool) -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_str(text).map_err(|e| format!("{MANIFEST} cannot be read: {e}"))?;
    if manifest.version.chars().count() > 64 {
        return Err(format!("{MANIFEST} version is longer than 64 characters"));
    }
    if manifest.manifest_version != contract_version() || manifest.provider_protocol != contract_version() {
        return Err(format!("unsupported plugin contract: manifest {}, provider {}; this host supports version 1", manifest.manifest_version, manifest.provider_protocol));
    }
    if !(name_ok(&manifest.name) || own && OWN.contains(&manifest.name.as_str())) || manifest.run.first().is_none_or(|p| p.trim().is_empty()) {
        return Err(format!("{MANIFEST} needs a valid name and a nonempty run command"));
    }
    for (key, about) in &manifest.settings {
        check_setting(key, about, &about["default"])?;
    }
    Ok(manifest)
}

fn is_on(state: &State, spec: &Spec) -> bool {
    if state.off.contains(&spec.name) { false }
    else if state.on.contains(&spec.name) { true }
    else { spec.manifest.as_ref().is_none_or(|m| m.on_by_default) }
}

/// A plugin's settings as saved, or as its defaults.
fn settings_of(m: &Manifest, saved: Option<&Map<String, Value>>) -> Map<String, Value> {
    m.settings
        .iter()
        .map(|(key, about)| {
            let value = saved.and_then(|s| s.get(key)).cloned().unwrap_or_else(|| about["default"].clone());
            (key.clone(), value)
        })
        .collect()
}

/// A setting's value, checked against what kind it is.
fn check_setting(key: &str, about: &Value, value: &Value) -> Result<Value, String> {
    let label = about["label"].as_str().unwrap_or(key);
    match about["kind"].as_str().unwrap_or("text") {
        "switch" => value.as_bool().map(Value::from).ok_or_else(|| format!("{label}: on or off")),
        "text" => value.as_str().map(|s| Value::from(s.chars().take(1000).collect::<String>())).ok_or_else(|| format!("{label}: text")),
        "folders" => {
            let list = value.as_array().ok_or_else(|| format!("{label}: a list of folders"))?;
            let mut folders = Vec::new();
            for f in list {
                let f = f.as_str().map(str::trim).filter(|f| !f.is_empty()).ok_or_else(|| format!("{label}: a list of folders"))?;
                let path = Path::new(f);
                if !path.is_absolute() || !path.is_dir() {
                    return Err(format!("{label}: {f} is no folder on this machine"));
                }
                folders.push(Value::from(f));
            }
            Ok(Value::Array(folders))
        }
        other => Err(format!("{label}: peek does not know settings of the kind {other:?}")),
    }
}

/// `name`, `@scope/name`, either with `@version`: nothing a shell would read.
fn npm_spec_ok(spec: &str) -> bool {
    let (scope, rest) = match spec.strip_prefix('@') {
        Some(rest) => match rest.split_once('/') {
            Some((scope, rest)) => (Some(scope), rest),
            None => return false,
        },
        None => (None, spec),
    };
    let (name, version) = rest.split_once('@').map_or((rest, None), |(n, v)| (n, Some(v)));
    let plain = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-._".contains(&b));
    scope.is_none_or(plain)
        && plain(name)
        && !name.starts_with(['.', '-'])
        && version.is_none_or(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.+".contains(&b)))
}

fn has_dependencies(dir: &Path) -> bool {
    fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|p| p["dependencies"].as_object().is_some_and(|d| !d.is_empty()))
}

/// A program by name, as a shell would find it; or a path, in `dir` if relative.
pub fn find_program(name: &str, dir: Option<&Path>) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into()).split(';').map(str::to_lowercase).collect()
    } else {
        vec![String::new()]
    };
    let has_ext = cfg!(windows) && exts.iter().any(|e| !e.is_empty() && name.to_lowercase().ends_with(e.as_str()));
    if name.contains(['/', '\\']) {
        // A program the plugin brings: `bin/peek-files` is `bin\peek-files.exe` on Windows.
        let path = dir.map_or_else(|| PathBuf::from(name), |d| d.join(name));
        if path.is_file() {
            return Some(path);
        }
        if has_ext || !cfg!(windows) {
            return None;
        }
        return exts.iter().map(|e| PathBuf::from(format!("{}{e}", path.display()))).find(|c| c.is_file());
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|p| {
        if has_ext || !cfg!(windows) {
            let candidate = p.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        exts.iter().map(|e| p.join(format!("{name}{e}"))).find(|c| c.is_file())
    })
}

fn run(command: &mut std::process::Command, what: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = command.stdin(std::process::Stdio::null()).output().map_err(|e| format!("cannot run {what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    // The line that says what went wrong: npm ends with where its log is,
    // and puts "404 Not Found" further up.
    let err = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = err.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains("complete log of this run")).collect();
    let why = lines
        .iter()
        .find(|l| l.contains("Not Found") || l.contains("fatal:") || l.contains("not found"))
        .or(lines.last())
        .copied()
        .unwrap_or("");
    Err(format!("{what} failed: {why}"))
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        // Not part of a plugin: its history, and what building it left behind
        // (a Rust plugin brings its program in `bin/`).
        if name == ".git" || name == "target" {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&entry.path(), &to.join(&name))?;
        } else if kind.is_file() {
            fs::copy(entry.path(), to.join(&name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_manifests_and_state() {
        for text in [include_str!("apps/vitals.json"), include_str!("apps/power.json")] {
            assert!(parse_manifest(text, true).unwrap().on_by_default);
        }
        const APP: &str = r#"{"name":"sample","run":["peek"],"on_by_default":false,"settings":{"share":{"kind":"text","default":"hello","label":"Share"}}}"#;
        let root = std::env::temp_dir().join(format!("peek-own-{}", crate::pairing::random_hex(6).unwrap()));
        let own = || vec![(APP, Start::Own { flags: vec![], env: vec![] })];
        let catalog = Catalog::new(root.clone(), own());
        let spec = catalog.specs().pop().unwrap();
        if let Start::Own { env, .. } = &spec.start {
            assert!(env.iter().any(|(k,v)| k == "PEEK_SETTINGS" && v.contains("hello")));
            assert!(env.iter().any(|(k,v)| k == "PEEK_DATA_DIR" && v.contains("sample")));
        } else { panic!("wrong start"); }
        assert!(!catalog.list()[0].3);
        assert!(catalog.remove("sample", false).unwrap_err().contains("part of peek"));
        assert!(catalog.update_from("sample", None, |_| {}).unwrap_err().contains("part of peek"));
        let plugins = Plugins::start(catalog, false);
        assert!(plugins.get("sample").is_none());
        plugins.catalog.set_on("sample", true).unwrap();
        assert!(Catalog::new(root.clone(), own()).list()[0].3);
        plugins.catalog.set_on("sample", false).unwrap();
        assert!(!Catalog::new(root.clone(), own()).list()[0].3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_contract_is_checked_before_installation() {
        let root = std::env::temp_dir().join(format!("peek-contract-{}", crate::pairing::random_hex(6).unwrap()));
        fs::create_dir_all(&root).unwrap();
        for manifest in [
            json!({"name":"sample","run":["node"],"manifest_version":2}),
            json!({"name":"sample","run":["node"],"provider_protocol":2}),
            json!({"name":"sample","run":[""]}),
            json!({"name":"screen","run":["node"]}),
            json!({"name":"files","run":["node"]}),
            json!({"name":"sample","run":["node"],"settings":{"bad":{"kind":"switch","default":"yes"}}}),
        ] {
            fs::write(root.join(MANIFEST), manifest.to_string()).unwrap();
            assert!(read_manifest(&root).is_err(), "{manifest}");
        }
        fs::write(root.join(MANIFEST), r#"{"name":"sample","run":["node"]}"#).unwrap();
        assert_eq!(read_manifest(&root).unwrap().provider_protocol, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_dependencies_leave_no_installed_plugin() {
        let root = std::env::temp_dir().join(format!("peek-needs-{}", crate::pairing::random_hex(6).unwrap()));
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(MANIFEST), r#"{"name":"sample","run":["absent"],"needs":["peek-definitely-absent-runtime"]}"#).unwrap();
        let catalog = Catalog::new(root.join("config"), Vec::new());
        let staged = catalog.fetch(source.to_str().unwrap()).unwrap();
        assert!(catalog.install(staged).unwrap_err().contains("missing required programs"));
        assert!(catalog.list().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn specs_expose_data_dir_without_creating_it() {
        let root = std::env::temp_dir().join(format!("peek-data-{}", crate::pairing::random_hex(6).unwrap()));
        let plugin = root.join("plugins").join("sample");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join(MANIFEST), r#"{"name":"sample","run":["node"]}"#).unwrap();
        let catalog = Catalog::new(root.clone(), Vec::new());
        let spec = catalog.specs().pop().unwrap();
        let Start::Program(program) = spec.start else { panic!("not a program") };
        assert!(program.env.iter().any(|(key, value)| key == "PEEK_DATA_DIR" && Path::new(value) == catalog.data_dir("sample")));
        assert!(!catalog.data_dir("sample").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_and_data_are_described() {
        let root = std::env::temp_dir().join(format!("peek-source-{}", crate::pairing::random_hex(6).unwrap()));
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(MANIFEST), r#"{"name":"sample","version":"1","run":["bin/app"]}"#).unwrap();
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(source.join("bin").join(if cfg!(windows) { "app.exe" } else { "app" }), "").unwrap();
        let catalog = Catalog::new(root.join("config"), Vec::new());
        let staged = catalog.fetch(source.to_str().unwrap()).unwrap();
        assert_eq!(catalog.describe(&staged)["installed"], Value::Null);
        assert_eq!(catalog.describe(&staged)["data"], false);
        catalog.install(staged).unwrap();
        assert_eq!(catalog.state().sources["sample"], std::path::absolute(&source).unwrap().to_string_lossy());
        assert!(!catalog.state().sources["sample"].starts_with(r"\\?\"));
        fs::create_dir_all(catalog.data_dir("sample")).unwrap();
        fs::write(catalog.data_dir("sample").join("file"), "data").unwrap();
        let staged = catalog.fetch(source.to_str().unwrap()).unwrap();
        assert_eq!(catalog.describe(&staged)["installed"]["version"], "1");
        assert_eq!(catalog.describe(&staged)["data"], true);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removal_keeps_or_purges_data() {
        let root = std::env::temp_dir().join(format!("peek-remove-{}", crate::pairing::random_hex(6).unwrap()));
        let source = root.join("source");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(source.join(MANIFEST), r#"{"name":"sample","run":["bin/app"],"settings":{"flag":{"kind":"switch","default":false}}}"#).unwrap();
        fs::write(source.join("bin").join(if cfg!(windows) { "app.exe" } else { "app" }), "").unwrap();
        let catalog = Catalog::new(root.join("config"), Vec::new());
        catalog.install(catalog.fetch(source.to_str().unwrap()).unwrap()).unwrap();
        catalog.set_settings("sample", &json!({"flag":true}).as_object().unwrap().clone()).unwrap();
        fs::create_dir_all(catalog.data_dir("sample")).unwrap();
        fs::write(catalog.data_dir("sample").join("file"), "data").unwrap();
        catalog.remove("sample", false).unwrap();
        assert!(catalog.data_dir("sample").join("file").exists());
        assert!(!catalog.state().sources.contains_key("sample"));
        assert!(!catalog.state().settings.contains_key("sample"));
        assert!(catalog.remove("sample", false).is_err());
        assert_eq!(catalog.kept_data(), vec!["sample"]);
        catalog.remove("sample", true).unwrap();
        assert!(catalog.kept_data().is_empty());
        catalog.install(catalog.fetch(source.to_str().unwrap()).unwrap()).unwrap();
        fs::create_dir_all(catalog.data_dir("sample")).unwrap();
        fs::write(catalog.data_dir("sample").join("file"), "data").unwrap();
        catalog.remove("sample", true).unwrap();
        assert!(!catalog.data_dir("sample").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_preserves_state_and_rejects_invalid_replacements() {
        let root = std::env::temp_dir().join(format!("peek-update-{}", crate::pairing::random_hex(6).unwrap()));
        let source = root.join("source");
        fs::create_dir_all(source.join("bin")).unwrap();
        let manifest = |version: &str, run: &str, settings: Value| json!({"name":"sample","version":version,"run":[run],"settings":settings}).to_string();
        let settings = json!({"keep":{"kind":"switch","default":false},"drop":{"kind":"switch","default":false},"gone":{"kind":"switch","default":false}});
        fs::write(source.join(MANIFEST), manifest("1", "bin/app", settings)).unwrap();
        fs::write(source.join("bin").join(if cfg!(windows) { "app.exe" } else { "app" }), "one").unwrap();
        let catalog = Catalog::new(root.join("config"), Vec::new());
        assert!(catalog.update(catalog.fetch(source.to_str().unwrap()).unwrap()).is_err());
        assert!(catalog.update_from("sample", None, |_| {}).is_err());
        catalog.install(catalog.fetch(source.to_str().unwrap()).unwrap()).unwrap();
        catalog.set_on("sample", false).unwrap();
        catalog.set_settings("sample", json!({"keep":true,"drop":true,"gone":true}).as_object().unwrap()).unwrap();
        fs::create_dir_all(catalog.data_dir("sample")).unwrap();
        fs::write(catalog.data_dir("sample").join("file"), "data").unwrap();
        fs::write(source.join(MANIFEST), manifest("2", "bin/missing", json!({}))).unwrap();
        assert!(catalog.update(catalog.fetch(source.to_str().unwrap()).unwrap()).is_err());
        assert_eq!(read_manifest(&catalog.plugins_dir().join("sample")).unwrap().version, "1");
        fs::write(source.join(MANIFEST), manifest("2", "bin/app", json!({"keep":{"kind":"switch","default":false},"drop":{"kind":"text","default":"new"}}))).unwrap();
        fs::write(source.join("bin").join(if cfg!(windows) { "app.exe" } else { "app" }), "two").unwrap();
        let replacement = root.join("replacement");
        copy_dir(&source, &replacement).unwrap();
        catalog.update_from("sample", Some(replacement.to_str().unwrap()), |_| {}).unwrap();
        assert_eq!(read_manifest(&catalog.plugins_dir().join("sample")).unwrap().version, "2");
        assert_eq!(catalog.state().settings["sample"].get("keep"), Some(&json!(true)));
        assert!(!catalog.state().settings["sample"].contains_key("drop"));
        assert!(!catalog.state().settings["sample"].contains_key("gone"));
        assert_eq!(catalog.state().sources["sample"], std::path::absolute(&replacement).unwrap().to_string_lossy());
        assert_eq!(fs::read(catalog.plugins_dir().join("sample").join("bin").join(if cfg!(windows) { "app.exe" } else { "app" })).unwrap(), b"two");
        assert!(catalog.state().off.contains("sample"));
        assert!(catalog.data_dir("sample").join("file").exists());
        assert!(catalog.state().sources.contains_key("sample"));
        assert!(catalog.update_from("other", Some(source.to_str().unwrap()), |_| {}).unwrap_err().contains("that is sample, not other"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn npm_names_that_reach_no_shell() {
        for ok in ["peek-files", "@peek/files", "files@1.2.3", "@a/b@0.1.0-beta.1"] {
            assert!(npm_spec_ok(ok), "{ok}");
        }
        for bad in ["files & calc", "a|b", "@scope", "../x", "-x", "files@>1", "Files", "", "a@", "x\"y"] {
            assert!(!npm_spec_ok(bad), "{bad}");
        }
    }

    #[test]
    fn settings_are_checked_by_kind() {
        let folders = json!({ "label": "Shared folders", "kind": "folders" });
        let here = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        assert_eq!(check_setting("f", &folders, &json!([here])).unwrap(), json!([here]));
        assert!(check_setting("f", &folders, &json!(["relative"])).is_err());
        assert!(check_setting("f", &folders, &json!(["Z:\\not\\there"])).is_err());
        assert!(check_setting("f", &folders, &json!("not a list")).is_err());
        assert_eq!(check_setting("s", &json!({ "kind": "switch" }), &json!(true)).unwrap(), json!(true));
        assert!(check_setting("x", &json!({ "kind": "script" }), &json!("rm")).is_err());
    }

    #[test]
    fn a_plugin_installs_from_a_folder_and_is_removed() {
        let root = std::env::temp_dir().join(format!("peek-plugins-{}", std::process::id()));
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join(MANIFEST), r#"{"name":"hello","description":"Says hello","run":["node","hello.mjs"],"needs":["node"]}"#).unwrap();
        fs::write(source.join("hello.mjs"), "").unwrap();
        let catalog = Catalog::new(root.join("config"), Vec::new());
        let staged = catalog.fetch(source.to_str().unwrap()).unwrap();
        assert_eq!(catalog.describe(&staged)["runs"], "node hello.mjs");
        assert_eq!(catalog.install(staged).unwrap(), "hello");
        assert!(catalog.list().iter().any(|(n, k, _, on)| n == "hello" && *k == "a plugin" && *on));
        // Installed already.
        assert!(catalog.install(catalog.fetch(source.to_str().unwrap()).unwrap()).is_err());
        catalog.set_on("hello", false).unwrap();
        assert!(catalog.list().iter().any(|(n, _, _, on)| n == "hello" && !*on));
        catalog.remove("hello", false).unwrap();
        assert!(!catalog.list().iter().any(|(n, ..)| n == "hello"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_plugin_brings_its_own_program() {
        let root = std::env::temp_dir().join(format!("peek-plugins-native-{}", std::process::id()));
        let source = root.join("source");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::create_dir_all(source.join("target").join("release")).unwrap();
        fs::write(source.join(MANIFEST), r#"{"name":"native","run":["bin/native"]}"#).unwrap();
        let program = if cfg!(windows) { "native.exe" } else { "native" };
        fs::write(source.join("bin").join(program), "").unwrap();
        fs::write(source.join("target").join("release").join(program), "").unwrap();
        let catalog = Catalog::new(root.join("config"), Vec::new());
        catalog.install(catalog.fetch(source.to_str().unwrap()).unwrap()).unwrap();
        let dir = root.join("config").join("plugins").join("native");
        // What building it left behind is not copied.
        assert!(dir.join("bin").join(program).is_file());
        assert!(!dir.join("target").exists());
        // `bin/native` is found as the program in its folder, with its extension on Windows.
        let spec = catalog.specs().into_iter().find(|s| s.name == "native").unwrap();
        let Start::Program(p) = spec.start else { panic!("not a program") };
        assert_eq!(PathBuf::from(&p.words[0]), dir.join("bin").join(program));
        let _ = fs::remove_dir_all(&root);
    }
}
