//! Adapters: optional add-ons, each a folder with an `adapter.toml` and,
//! usually, a program in any language that talks to wave9000 with JSON
//! messages, one per line, over stdin and stdout. The protocol is in
//! docs/adapters.md.
//!
//! An adapter can add commands (to any menu, and to keys.toml), panels for
//! the layout, and export formats. Export formats need no program at all: an
//! adapter can be just an `adapter.toml` of presets.
//!
//! Adapters come from two places. Built-in ones ship inside the app and can
//! be removed (and added back); installed ones are dropped on the window as
//! a zip or folder and live in the config folder's `adapters`. Every
//! program-backed adapter the user installed is approved before it first
//! runs, and any of them can crash without taking wave9000 down.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The protocol version sent in `initialize`. New versions only add things.
pub const PROTOCOL: u32 = 1;

pub const EDIT_READ: &str = "edit.read";
pub const EDIT_CHANGE: &str = "edit.change";
pub const NETWORK: &str = "network";
const KNOWN_PERMISSIONS: [&str; 3] = [EDIT_READ, EDIT_CHANGE, NETWORK];

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    /// The oldest wave9000 it works with, such as "0.0.1".
    #[serde(default)]
    pub requires: String,
    /// The program and its arguments, run from the adapter's folder. Empty
    /// for an adapter that only adds export formats.
    #[serde(default)]
    pub run: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub start: Start,
    /// Events to receive while running: "changed", "exported", "saved".
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub panels: Vec<PanelSpec>,
    #[serde(default)]
    pub exports: Vec<ExportSpec>,
}

#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    /// When one of its commands is first chosen, or its panel is shown.
    #[default]
    Command,
    /// When wave9000 opens.
    Launch,
}

#[derive(Deserialize, Clone, Debug)]
pub struct CommandSpec {
    pub id: String,
    pub label: String,
    /// A default shortcut, such as "cmd+alt+k". keys.toml can override it.
    #[serde(default)]
    pub keys: Option<String>,
    /// "file", "edit", "track", "view" or "playback" to show it there
    /// instead of the Adapters menu.
    #[serde(default)]
    pub menu: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct PanelSpec {
    pub id: String,
    pub title: String,
}

/// An export format: how the mix is written. Anything left out comes from
/// settings.toml.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq)]
pub struct ExportSpec {
    pub id: String,
    pub label: String,
    /// The file extension, such as "wav" or "mp3".
    pub extension: String,
    /// ffmpeg's encoder arguments, in place of the extension's own, such as
    /// ["-c:a", "libopus", "-b:a", "128k"].
    #[serde(default)]
    pub audio: Option<Vec<String>>,
    /// An ffmpeg filter run on the finished mix, such as `loudnorm`.
    #[serde(default)]
    pub audio_filter: String,
}

/// What the reader threads tell the interface.
pub enum Message {
    /// A notification or request from an adapter. Requests have an `id` and
    /// must be answered with [`Host::reply`].
    Call { adapter: String, id: Option<Value>, method: String, params: Value },
    Stopped { adapter: String, generation: u64 },
}

/// A running adapter's pipe.
pub struct Link {
    name: String,
    stdin: Mutex<ChildStdin>,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    alive: AtomicBool,
}

impl Link {
    fn write(&self, message: Value) {
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        // An adapter that has quit just misses the message.
        let _ = writeln!(stdin, "{message}").and_then(|()| stdin.flush());
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.write(json!({ "method": method, "params": params }));
    }

    /// Sends a request and waits for the answer.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(format!("{} has stopped", self.name));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        self.write(json!({ "id": id, "method": method, "params": params }));
        rx.recv_timeout(timeout).unwrap_or_else(|_| {
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            Err(format!("{} didn't answer {method} in time", self.name))
        })
    }

    fn answered(&self, id: u64, result: Result<Value, String>) {
        if let Some(tx) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
            let _ = tx.send(result);
        }
    }

    fn stopped(&self) {
        self.alive.store(false, Ordering::SeqCst);
        for (_, tx) in self.pending.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            let _ = tx.send(Err(format!("{} stopped", self.name)));
        }
    }
}

struct Running {
    child: Child,
    link: Arc<Link>,
    generation: u64,
}

pub struct Adapter {
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// Ships inside the app.
    pub builtin: bool,
    running: Option<Running>,
}

impl Adapter {
    pub fn is_running(&self) -> bool {
        self.running.as_ref().is_some_and(|r| r.link.alive.load(Ordering::SeqCst))
    }

    pub fn has(&self, permission: &str) -> bool {
        self.manifest.permissions.iter().any(|p| p == permission)
    }
}

/// Why an adapter didn't start.
#[derive(Debug, PartialEq)]
pub enum StartError {
    /// The user hasn't allowed it yet (or it asks for more than before).
    NeedsApproval,
    Failed(String),
}

type Deliver = Arc<dyn Fn(Message) + Send + Sync>;

/// What adapters see of wave9000's surroundings.
#[derive(Clone, Default)]
pub struct Env {
    pub config: PathBuf,
    /// Where the built-in adapters are, inside the app.
    pub builtin: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
}

/// All adapters, owned by the interface thread.
pub struct Host {
    pub all: Vec<Adapter>,
    pub env: Env,
    send: Deliver,
    generation: u64,
}

impl Host {
    pub fn new(env: Env, send: impl Fn(Message) + Send + Sync + 'static) -> Self {
        let all = discover(&env);
        Host { all, env, send: Arc::new(send), generation: 0 }
    }

    /// Looks for adapters again, stopping the ones that were running.
    pub fn reload(&mut self) {
        self.stop_all();
        self.all = discover(&self.env);
    }

    /// Starts the adapters that ask to start with wave9000 and may.
    pub fn start_launch_adapters(&mut self) {
        for i in 0..self.all.len() {
            if self.all[i].manifest.start == Start::Launch
                && let Err(StartError::Failed(e)) = self.start(i)
            {
                crate::log::error(e);
            }
        }
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.all.iter().position(|a| a.manifest.name == name)
    }

    pub fn approved(&self, i: usize) -> bool {
        let a = &self.all[i];
        a.builtin || is_approved(&self.env.config, &a.manifest)
    }

    /// Starts adapter `i` if it isn't running. An adapter the user installed
    /// must have been allowed first ([`Host::allow`]).
    pub fn start(&mut self, i: usize) -> Result<Arc<Link>, StartError> {
        if let Some(r) = &self.all[i].running
            && r.link.alive.load(Ordering::SeqCst)
        {
            return Ok(r.link.clone());
        }
        let manifest = self.all[i].manifest.clone();
        if manifest.run.is_empty() {
            return Err(StartError::Failed(format!("{} has no program to run", manifest.name)));
        }
        if !self.approved(i) {
            return Err(StartError::NeedsApproval);
        }
        let mut child = spawn(&self.all[i].dir, &manifest.run, &self.env)
            .map_err(|e| StartError::Failed(format!("Couldn't start {}: {e}", manifest.name)))?;
        self.generation += 1;
        let generation = self.generation;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stdin = child.stdin.take().expect("stdin is piped");
        let link = Arc::new(Link {
            name: manifest.name.clone(),
            stdin: Mutex::new(stdin),
            pending: Mutex::default(),
            next_id: AtomicU64::new(0),
            alive: AtomicBool::new(true),
        });
        if let Some(stderr) = child.stderr.take() {
            let name = manifest.name.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    crate::log::info(format!("{name}: {line}"));
                }
            });
        }
        let (reader, send, name) = (link.clone(), self.send.clone(), manifest.name.clone());
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let line = match read_message(&mut stdout) {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        crate::log::error(format!("{name}: {e}"));
                        continue;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    crate::log::error(format!("{name}: not a JSON message: {}", line.trim()));
                    continue;
                };
                match message.get("method").and_then(Value::as_str) {
                    Some(method) => send(Message::Call {
                        adapter: name.clone(),
                        id: message.get("id").cloned(),
                        method: method.to_owned(),
                        params: message.get("params").cloned().unwrap_or(Value::Null),
                    }),
                    None => {
                        let Some(id) = message.get("id").and_then(Value::as_u64) else { continue };
                        let result = match message.get("error") {
                            Some(e) => Err(e.as_str().map(String::from).unwrap_or_else(|| e.to_string())),
                            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        reader.answered(id, result);
                    }
                }
            }
            reader.stopped();
            send(Message::Stopped { adapter: name, generation });
        });

        let paths = |p: &Option<PathBuf>| p.as_ref().map(|p| p.to_string_lossy().into_owned());
        link.notify(
            "initialize",
            json!({
                "wave9000": env!("CARGO_PKG_VERSION"),
                "protocol": PROTOCOL,
                "permissions": manifest.permissions,
                "ffmpeg": paths(&self.env.ffmpeg),
                "ffprobe": paths(&self.env.ffprobe),
            }),
        );
        crate::log::info(format!("Started {} {}", manifest.name, manifest.version));
        self.all[i].running = Some(Running { child, link: link.clone(), generation });
        Ok(link)
    }

    /// Runs an adapter's command, starting the adapter if needed.
    pub fn run_command(&mut self, name: &str, id: &str) -> Result<(), StartError> {
        let i = self.index(name).ok_or_else(|| StartError::Failed(format!("No adapter called \"{name}\"")))?;
        if !self.all[i].manifest.commands.iter().any(|c| c.id == id) {
            return Err(StartError::Failed(format!("{name} has no command \"{id}\"")));
        }
        self.start(i)?.notify("command", json!({ "command": id }));
        Ok(())
    }

    /// Sends an event to the running adapters that asked for it and may read
    /// the edit.
    pub fn emit(&self, kind: &str, mut params: Value) {
        params["kind"] = json!(kind);
        for a in &self.all {
            if let Some(r) = &a.running
                && a.manifest.events.iter().any(|e| e == kind)
                && a.has(EDIT_READ)
            {
                r.link.notify("event", params.clone());
            }
        }
    }

    pub fn reply(&self, adapter: &str, id: Value, result: Result<Value, String>) {
        let Some(link) = self.index(adapter).and_then(|i| self.all[i].running.as_ref()).map(|r| &r.link) else { return };
        link.write(match result {
            Ok(value) => json!({ "id": id, "result": value }),
            Err(e) => json!({ "id": id, "error": e }),
        });
    }

    /// An adapter's program ended. Ignored (and false) if it has been
    /// stopped on purpose or restarted since.
    pub fn stopped(&mut self, adapter: &str, generation: u64) -> bool {
        let Some(i) = self.index(adapter) else { return false };
        if !self.all[i].running.as_ref().is_some_and(|r| r.generation == generation) {
            return false;
        }
        let mut r = self.all[i].running.take().expect("checked");
        let status = r.child.wait().map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
        crate::log::info(format!("{adapter} stopped ({status})"));
        true
    }

    /// Asks every running adapter to shut down, then stops the ones that don't.
    pub fn stop_all(&mut self) {
        let mut children = Vec::new();
        for a in &mut self.all {
            if let Some(r) = a.running.take() {
                r.link.notify("shutdown", Value::Null);
                children.push(r.child);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        for mut child in children {
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// The user allowed adapter `i` with its current permissions.
    pub fn allow(&self, i: usize) {
        remember_approval(&self.env.config, &self.all[i].manifest);
    }

    /// Takes an adapter out. A built-in one is only switched off (it's part
    /// of the app) and can be added back; an installed one goes to the trash.
    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        let i = self.index(name).ok_or_else(|| format!("No adapter called \"{name}\""))?;
        self.stop_all();
        if self.all[i].builtin {
            let mut list = Removed::load(&self.env.config);
            if !list.removed.iter().any(|n| n == name) {
                list.removed.push(name.to_owned());
            }
            list.save(&self.env.config)?;
        } else {
            to_trash(&self.all[i].dir)?;
            forget_approval(&self.env.config, name);
        }
        self.all = discover(&self.env);
        Ok(())
    }

    /// Built-in adapters that were removed, to offer back.
    pub fn removed_builtins(&self) -> Vec<Manifest> {
        let removed = Removed::load(&self.env.config).removed;
        let Some(dir) = &self.env.builtin else { return Vec::new() };
        discover_in(dir).into_iter().map(|a| a.manifest).filter(|m| removed.contains(&m.name)).collect()
    }

    pub fn add_back(&mut self, name: &str) -> Result<(), String> {
        let mut list = Removed::load(&self.env.config);
        list.removed.retain(|n| n != name);
        list.save(&self.env.config)?;
        self.reload();
        Ok(())
    }

    /// Every export format, with the adapter it comes from.
    pub fn exports(&self) -> impl Iterator<Item = (&str, &ExportSpec)> {
        self.all.iter().flat_map(|a| a.manifest.exports.iter().map(move |e| (a.manifest.name.as_str(), e)))
    }

    pub fn export(&self, adapter: &str, id: &str) -> Option<&ExportSpec> {
        self.exports().find(|(a, e)| *a == adapter && e.id == id).map(|(_, e)| e)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// Built-in adapters the user took out, in the config folder's adapters.toml.
#[derive(Serialize, Deserialize, Default)]
struct Removed {
    #[serde(default)]
    removed: Vec<String>,
}

impl Removed {
    fn load(config: &Path) -> Self {
        std::fs::read_to_string(config.join("adapters.toml")).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, config: &Path) -> Result<(), String> {
        let text = format!(
            "# Built-in adapters you've removed. Take a name out (or use Add Back in\n\
             # the Adapters panel) to have it again.\n\n{}",
            toml::to_string(self).map_err(|e| e.to_string())?
        );
        std::fs::write(config.join("adapters.toml"), text).map_err(|e| e.to_string())
    }
}

/// The longest message wave9000 reads from an adapter.
const MAX_MESSAGE: u64 = 32 << 20;

/// Reads one line, refusing (and skipping) one longer than MAX_MESSAGE.
/// None at the end of the output.
fn read_message(r: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(MAX_MESSAGE).read_until(b'\n', &mut buf).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') && n as u64 == MAX_MESSAGE {
        loop {
            buf.clear();
            let n = r.by_ref().take(1 << 20).read_until(b'\n', &mut buf).map_err(|e| e.to_string())?;
            if n == 0 || buf.last() == Some(&b'\n') {
                break;
            }
        }
        return Err(format!("sent a message over {} MB; it was ignored", MAX_MESSAGE >> 20));
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Refuses an adapter whose program was built for another kind of computer,
/// with a message saying which to get, instead of failing to start later.
pub fn check_platform(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let Some(program) = manifest.run.first().filter(|p| p.contains(['/', '\\'])) else {
        return Ok(()); // no program, a script run by an interpreter, or one on the PATH
    };
    let path = program_path(dir, program);
    let mut magic = [0u8; 20];
    let n = std::fs::File::open(&path)
        .and_then(|mut f| f.read(&mut magic))
        .map_err(|_| format!("{} is missing its program ({program})", manifest.name))?;
    let magic = &magic[..n];
    let built_for = if magic.starts_with(b"MZ") {
        "Windows"
    } else if magic.starts_with(b"\x7fELF") {
        "Linux"
    } else if magic.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]) || magic.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) {
        // A thin Mach-O names its processor; Apple Silicon runs Intel ones too.
        let arm_only = magic.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]) && magic.get(4..8) == Some(&[0x0c, 0x00, 0x00, 0x01]);
        if arm_only && cfg!(target_arch = "x86_64") { "Apple Silicon Macs" } else { "macOS" }
    } else {
        return Ok(()); // a script; let it try
    };
    let ours = if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Linux"
    };
    if built_for == ours {
        Ok(())
    } else {
        let get = if built_for == "Apple Silicon Macs" { "Intel Mac (macos-x64)" } else { ours };
        Err(format!("{} is built for {built_for}, not this computer. Download its {get} version.", manifest.name))
    }
}

/// A program given as a path (`./frame-grab`) is found in the adapter's
/// folder; on Windows `.exe` is added when that's what's there, so one
/// adapter.toml works everywhere.
fn program_path(dir: &Path, program: &str) -> PathBuf {
    let path = dir.join(program);
    if cfg!(windows) && !path.exists() {
        let exe = path.with_extension("exe");
        if exe.exists() {
            return exe;
        }
    }
    path
}

fn spawn(dir: &Path, run: &[String], env: &Env) -> Result<Child, String> {
    let (program, args) = run.split_first().ok_or("adapter.toml has an empty `run`")?;
    let program = if program.contains(['/', '\\']) { program_path(dir, program).into_os_string() } else { program.into() };
    let mut cmd = Command::new(&program);
    cmd.args(args)
        .current_dir(dir)
        // So adapters can keep their own files, and use the same ffmpeg.
        .env("WAVE9000_CONFIG_DIR", &env.config)
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, path) in [("WAVE9000_FFMPEG", &env.ffmpeg), ("WAVE9000_FFPROBE", &env.ffprobe)] {
        if let Some(p) = path {
            cmd.env(key, p);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{} not found", program.to_string_lossy()),
        _ => e.to_string(),
    })
}

// ---- Finding and installing ----------------------------------------------

pub fn adapters_dir(config: &Path) -> PathBuf {
    config.join("adapters")
}

/// Where the built-in adapters are: inside the macOS app, or beside the
/// program elsewhere. WAVE9000_BUILTIN_ADAPTERS points somewhere else
/// (for development).
pub fn builtin_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("WAVE9000_BUILTIN_ADAPTERS") {
        return Some(dir.into());
    }
    let exe = std::env::current_exe().ok()?;
    let here = exe.parent()?;
    [here.join("../Resources/adapters"), here.join("adapters")].into_iter().find(|d| d.is_dir())
}

/// Built-in adapters that weren't removed, then installed ones; an installed
/// adapter replaces a built-in one of the same name (a newer version).
fn discover(env: &Env) -> Vec<Adapter> {
    let removed = Removed::load(&env.config).removed;
    let mut all: Vec<Adapter> = env
        .builtin
        .as_deref()
        .map(discover_in)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !removed.contains(&a.manifest.name))
        .map(|a| Adapter { builtin: true, ..a })
        .collect();
    for a in discover_in(&adapters_dir(&env.config)) {
        all.retain(|b| b.manifest.name != a.manifest.name);
        all.push(a);
    }
    all
}

fn discover_in(root: &Path) -> Vec<Adapter> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join("adapter.toml").is_file()).collect();
    dirs.sort();
    let mut found: Vec<Adapter> = Vec::new();
    for dir in dirs {
        match load_manifest(&dir).and_then(|m| check_platform(&dir, &m).map(|()| m)) {
            Ok(manifest) => {
                found.retain(|a| a.manifest.name != manifest.name);
                found.push(Adapter { dir, manifest, builtin: false, running: None });
            }
            Err(e) => crate::log::error(e),
        }
    }
    found
}

/// Whether this wave9000 is at least `version` ("0.0.6").
fn new_enough(version: &str) -> bool {
    let parts = |v: &str| v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(env!("CARGO_PKG_VERSION")) >= parts(version)
}

fn parse_manifest(text: &str, at: &str) -> Result<Manifest, String> {
    let manifest: Manifest = toml::from_str(text).map_err(|e| format!("{at}: {}", e.message()))?;
    if manifest.name.trim().is_empty() {
        return Err(format!("{at}: `name` is empty"));
    }
    if !manifest.requires.is_empty() && !new_enough(&manifest.requires) {
        return Err(format!(
            "{} needs wave9000 {} or newer (this is {}). Get the latest from github.com/solacecasino/wave9000/releases.",
            manifest.name,
            manifest.requires,
            env!("CARGO_PKG_VERSION")
        ));
    }
    if manifest.run.is_empty() && manifest.exports.is_empty() {
        return Err(format!("{at}: needs a program to `run`, or `[[exports]]`"));
    }
    if manifest.run.is_empty() && !(manifest.commands.is_empty() && manifest.panels.is_empty()) {
        return Err(format!("{at}: commands and panels need a program to `run`"));
    }
    for p in &manifest.permissions {
        if !KNOWN_PERMISSIONS.contains(&p.as_str()) {
            crate::log::error(format!("{}: unknown permission \"{p}\"", manifest.name));
        }
    }
    Ok(manifest)
}

fn load_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join("adapter.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_manifest(&text, &path.display().to_string())
}

/// Whether `path` is an adapter to install rather than media: a folder with
/// `adapter.toml`, or a zip with one at its top or one folder down.
pub fn is_package(path: &Path) -> bool {
    if path.is_dir() {
        return path.join("adapter.toml").is_file();
    }
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) && zip_manifest(path).is_some()
}

fn zip_manifest(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let archive = zip::ZipArchive::new(std::io::BufReader::new(file)).ok()?;
    archive
        .file_names()
        .filter_map(Result::ok)
        .find(|n| n.rsplit('/').next() == Some("adapter.toml") && n.matches('/').count() <= 1)
        .map(|n| n.into_owned())
}

/// Reads the manifest of a package without installing it.
pub fn package_manifest(path: &Path) -> Result<Manifest, String> {
    if path.is_dir() {
        return load_manifest(path);
    }
    let name = zip_manifest(path).ok_or("not an adapter package")?;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    let mut text = String::new();
    archive.by_name(&name).map_err(|e| e.to_string())?.read_to_string(&mut text).map_err(|e| e.to_string())?;
    parse_manifest(&text, "adapter.toml")
}

/// Copies a package into the adapters folder, replacing an older copy of
/// the same adapter. The new copy is put together beside it first, so a
/// failure leaves what was there.
pub fn install(source: &Path, config: &Path) -> Result<Manifest, String> {
    install_into(source, &adapters_dir(config))
}

fn install_into(source: &Path, root: &Path) -> Result<Manifest, String> {
    let manifest = package_manifest(source)?;
    let folder: String = manifest
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let incoming = root.join(format!(".incoming-{folder}"));
    let _ = std::fs::remove_dir_all(&incoming);
    let result = if source.is_dir() { copy_dir(source, &incoming).map_err(|e| e.to_string()) } else { unzip(source, &incoming) };
    if let Err(e) = result.and_then(|()| check_platform(&incoming, &manifest)) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(format!("Couldn't install {}: {e}", manifest.name));
    }
    let target = root.join(&folder);
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&incoming, &target).map_err(|e| e.to_string())?;
    Ok(manifest)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if [".git", "target", "dist"].iter().any(|skip| name == *skip) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            copy_dir(&path, &to.join(&name))?;
        } else {
            std::fs::copy(&path, to.join(&name))?;
        }
    }
    Ok(())
}

/// Unpacks a zip into `to`. A zip holding one folder is unpacked from inside
/// that folder. Paths that would escape `to` are refused.
fn unzip(zip_path: &Path, to: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    let manifest = zip_manifest(zip_path).ok_or("not an adapter package")?;
    let prefix = manifest.trim_end_matches("adapter.toml").to_owned();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(inside) = entry.enclosed_name() else { return Err(format!("unsafe path in zip: entry {i}")) };
        let Ok(rel) = inside.strip_prefix(&prefix) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = to.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        std::fs::write(&out, bytes).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

/// Moves a folder to the system's trash, so a removed adapter can still be
/// fished out.
fn to_trash(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject, Bool};
        use objc2_foundation::NSString;
        let path = NSString::from_str(&path.to_string_lossy());
        // SAFETY: plain Foundation calls with live objects.
        let done = unsafe {
            let (Some(manager), Some(url)) = (AnyClass::get(c"NSFileManager"), AnyClass::get(c"NSURL")) else {
                return Err("no file manager".into());
            };
            let manager: *mut AnyObject = msg_send![manager, defaultManager];
            let url: *mut AnyObject = msg_send![url, fileURLWithPath: &*path];
            let ok: Bool = msg_send![manager, trashItemAtURL: url, resultingItemURL: std::ptr::null_mut::<*mut AnyObject>(), error: std::ptr::null_mut::<*mut AnyObject>()];
            ok.as_bool()
        };
        if done { Ok(()) } else { Err(format!("couldn't move {} to the Trash", path)) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        trash::delete(path).map_err(|e| e.to_string())
    }
}

// ---- Approval --------------------------------------------------------------

pub fn describe(permission: &str) -> String {
    match permission {
        EDIT_READ => "see your project: the tracks, clips, sound files and playhead".into(),
        EDIT_CHANGE => "change your project, add sound files and move the playhead".into(),
        NETWORK => "connect to the internet or other computers".into(),
        other => other.to_owned(),
    }
}

/// What an adapter is and may do, in words, for the install and approval
/// prompts.
pub fn summary(m: &Manifest) -> Vec<String> {
    let mut lines = Vec::new();
    let by = if m.author.is_empty() { String::new() } else { format!(" by {}", m.author) };
    let license = if m.license.is_empty() { String::new() } else { format!(" · {}", m.license) };
    lines.push(format!("{} {}{by}{license}", m.name, m.version));
    if !m.description.is_empty() {
        lines.push(m.description.clone());
    }
    let mut adds = Vec::new();
    if !m.commands.is_empty() {
        adds.push(format!("{} command{}", m.commands.len(), if m.commands.len() == 1 { "" } else { "s" }));
    }
    if !m.panels.is_empty() {
        adds.push(format!("{} panel{}", m.panels.len(), if m.panels.len() == 1 { "" } else { "s" }));
    }
    if !m.exports.is_empty() {
        adds.push(format!("{} export format{}", m.exports.len(), if m.exports.len() == 1 { "" } else { "s" }));
    }
    if !adds.is_empty() {
        lines.push(format!("Adds {}.", adds.join(", ")));
    }
    if m.run.is_empty() {
        lines.push("It has no program: nothing of it runs.".into());
    } else if m.permissions.is_empty() {
        lines.push("It doesn't ask for any permissions.".into());
    } else {
        lines.push("It asks to:".into());
        lines.extend(m.permissions.iter().map(|p| format!("•  {}", describe(p))));
    }
    lines
}

fn approvals_path(config: &Path) -> PathBuf {
    config.join("approvals.json")
}

fn load_approvals(config: &Path) -> BTreeMap<String, Vec<String>> {
    std::fs::read_to_string(approvals_path(config)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn is_approved(config: &Path, m: &Manifest) -> bool {
    load_approvals(config).get(&m.name).is_some_and(|granted| m.permissions.iter().all(|p| granted.contains(p)))
}

/// Records that the user allowed `m` with its current permissions.
/// Installing through wave9000's prompt counts, since it showed the same.
pub fn remember_approval(config: &Path, m: &Manifest) {
    let mut approvals = load_approvals(config);
    approvals.insert(m.name.clone(), m.permissions.clone());
    let _ = std::fs::write(approvals_path(config), serde_json::to_string_pretty(&approvals).expect("serializes"));
}

/// Forgets an approval, for a removed adapter: if it comes back, it's asked
/// about again.
fn forget_approval(config: &Path, name: &str) {
    let mut approvals = load_approvals(config);
    if approvals.remove(name).is_some() {
        let _ = std::fs::write(approvals_path(config), serde_json::to_string_pretty(&approvals).expect("serializes"));
    }
}

/// Reads `adapter "Name" id` (or `export "Name" id`) from keys.toml or a menu.
pub fn parse_command<'a>(text: &'a str, kind: &str) -> Option<(&'a str, &'a str)> {
    let rest = text.strip_prefix(kind)?.strip_prefix(' ')?.trim_start().strip_prefix('"')?;
    let (name, id) = rest.split_once('"')?;
    let id = id.trim();
    (!name.is_empty() && !id.is_empty()).then_some((name, id))
}

pub fn command_name(kind: &str, adapter: &str, id: &str) -> String {
    format!("{kind} \"{adapter}\" {id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_formats_need_no_program() {
        let m = parse_manifest("name = \"Loud\"\nrequires = \"0.0.1\"\n[[exports]]\nid = \"l\"\nlabel = \"L\"\nextension = \"wav\"\naudio_filter = \"loudnorm\"\n", "x").unwrap();
        assert_eq!(m.exports[0].audio_filter, "loudnorm");
        assert!(summary(&m).iter().any(|l| l.contains("1 export format")));
        assert!(parse_manifest("name = \"Empty\"\n", "x").is_err());
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wave9000-adapters-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A shell-script adapter: on its "hello" command it asks for the edit,
    /// and it answers a "ping" request.
    fn write_adapter(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("adapter.toml"),
            format!(
                r#"name = "{name}"
version = "0.1.0"
run = ["sh", "adapter.sh"]
permissions = ["edit.read"]
[[commands]]
id = "hello"
label = "Say Hello"
keys = "cmd+alt+h"
[[panels]]
id = "info"
title = "Info"
"#
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("adapter.sh"),
            r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"ping"'*) printf '{"id":%s,"result":"pong"}\n' "$id" ;;
    *'"command"'*) printf '{"id":7,"method":"edit.get","params":{}}\n' ;;
    *'"shutdown"'*) exit 0 ;;
  esac
done
"#,
        )
        .unwrap();
    }

    fn env(root: &Path) -> Env {
        Env { config: root.join("config"), builtin: Some(root.join("builtin")), ffmpeg: None, ffprobe: None }
    }

    #[cfg(unix)]
    #[test]
    fn talks_both_ways_after_approval() {
        let root = temp("talk");
        let env = env(&root);
        write_adapter(&adapters_dir(&env.config).join("test"), "Test");
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let mut host = Host::new(env, move |m| drop(tx.lock().unwrap().send(m)));
        assert_eq!(host.all.len(), 1);
        assert_eq!(host.all[0].manifest.panels[0].title, "Info");
        // Installed by hand: asked about first.
        assert_eq!(host.run_command("Test", "hello").unwrap_err(), StartError::NeedsApproval);
        host.allow(0);
        host.run_command("Test", "hello").unwrap();
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            Message::Call { method, id, .. } => assert_eq!((method.as_str(), id), ("edit.get", Some(json!(7)))),
            Message::Stopped { .. } => panic!("stopped early"),
        }
        let link = host.start(0).unwrap();
        assert_eq!(link.request("ping", Value::Null, Duration::from_secs(10)).unwrap(), "pong");
        assert!(matches!(host.run_command("Test", "nope"), Err(StartError::Failed(_))));
        host.stop_all();
        assert!(link.request("ping", Value::Null, Duration::from_secs(1)).is_err());
    }

    #[test]
    fn builtins_can_be_removed_and_added_back() {
        let root = temp("builtin");
        let env = env(&root);
        std::fs::create_dir_all(&env.config).unwrap();
        let presets = env.builtin.as_ref().unwrap().join("presets");
        std::fs::create_dir_all(&presets).unwrap();
        std::fs::write(
            presets.join("adapter.toml"),
            "name = \"Presets\"\n[[exports]]\nid = \"mp3\"\nlabel = \"MP3\"\nextension = \"mp3\"\n",
        )
        .unwrap();
        let mut host = Host::new(env.clone(), |_| {});
        assert!(host.all[0].builtin && host.approved(0));
        assert_eq!(host.export("Presets", "mp3").map(|e| e.extension.as_str()), Some("mp3"));
        host.remove("Presets").unwrap();
        assert!(host.all.is_empty());
        assert_eq!(host.removed_builtins()[0].name, "Presets");
        // Still removed after a restart, and the files inside the app are untouched.
        assert!(Host::new(env.clone(), |_| {}).all.is_empty());
        assert!(presets.join("adapter.toml").is_file());
        host.add_back("Presets").unwrap();
        assert_eq!(host.all.len(), 1);
        assert!(host.removed_builtins().is_empty());
    }

    #[test]
    fn installed_replaces_builtin_of_the_same_name() {
        let root = temp("replace");
        let env = env(&root);
        write_adapter(&env.builtin.as_ref().unwrap().join("a"), "Same");
        write_adapter(&adapters_dir(&env.config).join("a"), "Same");
        let host = Host::new(env, |_| {});
        assert_eq!(host.all.len(), 1);
        assert!(!host.all[0].builtin);
    }

    #[test]
    fn installs_folders_and_zips() {
        let root = temp("install");
        let src = root.join("src");
        write_adapter(&src, "Test Pages");
        let dest = root.join("adapters");
        let m = install_into(&src, &dest).unwrap();
        assert_eq!(m.name, "Test Pages");
        assert!(dest.join("test-pages/adapter.sh").is_file());

        // The same adapter zipped inside a folder, as release zips are.
        let zip_path = root.join("test-pages-0.1.0.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
        for name in ["adapter.toml", "adapter.sh"] {
            zip.start_file(format!("test-pages/{name}"), opts).unwrap();
            zip.write_all(&std::fs::read(src.join(name)).unwrap()).unwrap();
        }
        zip.finish().unwrap();
        assert!(is_package(&zip_path));
        install_into(&zip_path, &dest).unwrap();
        assert_eq!(discover_in(&dest).len(), 1);
        assert!(!is_package(&root));
        assert!(!is_package(Path::new("/nowhere/take.wav")));
    }

    #[test]
    fn refuses_zips_that_escape() {
        let root = temp("escape");
        let zip_path = root.join("evil.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("adapter.toml", opts).unwrap();
        zip.write_all(b"name = \"Evil\"\nrun = [\"sh\", \"x.sh\"]\n").unwrap();
        zip.start_file("../../outside.txt", opts).unwrap();
        zip.write_all(b"no").unwrap();
        zip.finish().unwrap();
        assert!(install_into(&zip_path, &root.join("adapters")).is_err());
        assert!(!root.join("outside.txt").exists() && !root.parent().unwrap().join("outside.txt").exists());
    }

    #[test]
    fn adapters_can_ask_for_a_newer_wave9000() {
        assert!(new_enough("0.0.1") && new_enough(env!("CARGO_PKG_VERSION")));
        assert!(!new_enough("99.0.0") && !new_enough("0.0.999"));
        let e = parse_manifest("name = \"X\"\nrequires = \"99.0\"\n[[exports]]\nid = \"e\"\nlabel = \"E\"\nextension = \"mp4\"", "a").unwrap_err();
        assert!(e.contains("needs wave9000 99.0 or newer"), "{e}");
    }

    #[test]
    fn manifests_are_checked() {
        assert!(parse_manifest("name = \"X\"", "a").is_err(), "nothing to do");
        assert!(parse_manifest("name = \"X\"\n[[commands]]\nid = \"a\"\nlabel = \"A\"\n[[exports]]\nid = \"e\"\nlabel = \"E\"\nextension = \"mp4\"", "a").is_err());
        assert!(parse_manifest("name = \"X\"\n[[exports]]\nid = \"e\"\nlabel = \"E\"\nextension = \"mp4\"", "a").is_ok());
        assert!(parse_manifest("name = \"X\"\nrun = [\"./x\"]\nsomething_new = 1", "a").is_ok(), "unknown keys are fine");
    }

    #[test]
    fn refuses_programs_for_other_computers() {
        let dir = temp("platform");
        let m = |run: &str| Manifest { name: "X".into(), run: vec![run.into()], ..Manifest::default() };
        std::fs::write(dir.join("win.exe"), b"MZ\x90\x00").unwrap();
        std::fs::write(dir.join("linux"), b"\x7fELF\x02\x01").unwrap();
        std::fs::write(dir.join("mac"), [0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2]).unwrap();
        std::fs::write(dir.join("script.sh"), b"#!/bin/sh").unwrap();
        for (file, system, ours) in [
            ("./win.exe", "Windows", cfg!(windows)),
            ("./linux", "Linux", cfg!(target_os = "linux")),
            ("./mac", "macOS", cfg!(target_os = "macos")),
        ] {
            match check_platform(&dir, &m(file)) {
                Ok(()) => assert!(ours, "{file} was let through"),
                Err(e) => assert!(!ours && e.contains(system), "{file}: {e}"),
            }
        }
        assert!(check_platform(&dir, &m("./script.sh")).is_ok());
        assert!(check_platform(&dir, &m("python3")).is_ok());
        assert!(check_platform(&dir, &m("./missing")).is_err());
    }

    #[test]
    fn long_messages_are_refused_not_kept() {
        let mut big = vec![b'x'; (MAX_MESSAGE + 10) as usize];
        big.extend_from_slice(b"\n{\"method\":\"log\"}\n");
        let mut r = std::io::Cursor::new(big);
        assert!(read_message(&mut r).is_err());
        assert_eq!(read_message(&mut r).unwrap().unwrap().trim(), "{\"method\":\"log\"}");
        assert_eq!(read_message(&mut r).unwrap(), None);
    }

    #[test]
    fn reads_adapter_commands() {
        assert_eq!(parse_command(r#"adapter "Frame Grab" grab"#, "adapter"), Some(("Frame Grab", "grab")));
        assert_eq!(parse_command(&command_name("export", "A", "b"), "export"), Some(("A", "b")));
        assert_eq!(parse_command("play", "adapter"), None);
        assert_eq!(parse_command(r#"adapterx "A" b"#, "adapter"), None);
    }
}
