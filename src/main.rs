// No console window behind the app on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

//! wave9000: a bare, modular audio workstation.
//!
//! One owner of state: everything lives in `App`, on the interface thread.
//! Reading files, recording and exporting run on their own threads, and
//! the sound device's callback plays a frozen copy of the project (see
//! `engine`), so nothing the interface does can make the sound stutter.

mod adapters;
use adapters::Sections;
// The 9000 series' shared core: the window, the adapter host, the sisters.
#[cfg(target_os = "macos")]
use core9000::incoming;
use core9000::{log, shell, watch};
mod engine;
mod export;
mod hosting;
mod keys;
mod layout;
mod media;
mod menu;
mod panels;
mod project;
mod record;
mod settings;
mod theme;
mod wav;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{ViewportBuilder, ViewportCommand};

use crate::engine::{Engine, METERS, Mix};
use crate::export::Job;
use crate::keys::Keymap;
use crate::layout::{LayoutUi, Node, Side};
use crate::media::{Media, Sound, Tools};
use crate::project::{Clip, Project, SpliceProject, Track};
use crate::record::Recording;
use crate::settings::Settings;

/// 256 px is all the dock, the window and About ever show.
const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

/// The strip across the top of the window: on macOS under the transparent
/// titlebar (the menus are the system's), elsewhere the menus themselves.
const TOP: f32 = 32.0;

/// The project file format this version writes.
const FORMAT: u32 = 1;

/// Where a file being read goes once it's read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    /// Only into Media.
    Media,
    /// On a new track of its own, at this time.
    NewTrack(f64),
    /// On this track at this time.
    On(usize, f64),
}

/// A file that's been read, or why it couldn't be, and where it goes.
/// A file read on another thread: its path, what was read and at which
/// rate, and where it goes.
type Loaded = (PathBuf, Result<(Media, Sound), String>, u32, Place);

/// Track view state.
pub struct View {
    /// Pixels per second.
    pub pps: f32,
    /// The time at the left edge.
    pub scroll: f64,
    /// How far the tracks are scrolled down, in points.
    pub down: f32,
    /// The lanes' width when last drawn.
    pub width: f32,
    /// Zoom to fit once the lanes' width is known.
    pub fit: bool,
    pub drag: panels::Drag,
}

pub struct App {
    dir: PathBuf,
    palette: theme::Palette,
    menu: menu::MenuBar,
    settings: Settings,
    keymap: Keymap,
    layout: Node,
    layout_ui: LayoutUi,
    config_changed: Arc<AtomicBool>,
    tools: Option<Tools>,

    pub project: Project,
    /// Decoded sound, by file.
    sounds: HashMap<PathBuf, Arc<Sound>>,
    /// Files being read, so they aren't read twice.
    reading: Vec<PathBuf>,
    loaded_tx: Sender<Loaded>,
    loaded_rx: Receiver<Loaded>,
    undo: Vec<Vec<Track>>,
    redo: Vec<Vec<Track>>,
    selected_media: Option<usize>,
    selected_track: Option<usize>,
    /// (track, clip)
    selected_clip: Option<(usize, usize)>,
    engine: Engine,
    /// The project changed since the mixer last got a copy.
    mix_dirty: bool,
    recording: Option<Recording>,
    /// Said once per take that the input gives only silence.
    silence_warned: bool,
    /// What the meters show: each track's level, falling back slowly, and
    /// the master's last.
    levels: Vec<f32>,
    view: View,
    file: Option<PathBuf>,
    /// Unsaved changes to `file`.
    dirty: bool,
    /// The session needs writing.
    session_dirty: bool,
    /// The system theme last asked for (the traffic lights follow it).
    system_dark: Option<bool>,
    export: Option<Job>,
    last_export: Option<Result<String, String>>,
    /// Commands from buttons, run at the start of the next frame.
    queue: Vec<String>,
    adapters: adapters::Host,
    adapter_rx: Receiver<adapters::Message>,
    /// What adapters put in their panels, by panel kind.
    adapter_panels: HashMap<String, Vec<serde_json::Value>>,
    /// The question on screen, and the ones waiting behind it.
    prompt: Option<hosting::Prompt>,
    prompts: Vec<hosting::Prompt>,
    /// The export format picked in the Export panel (adapter, id); None:
    /// settings.toml's.
    export_choice: Option<(String, String)>,
    /// A short message at the bottom, until the time given.
    note: Option<(String, f64)>,
    /// The project changed since adapters were last told.
    emit_changed: bool,
    /// The media list changed since adapters were last told.
    emit_media: bool,
    /// The encoders this ffmpeg has, read the first time they matter.
    encoders: Option<Vec<String>>,
    title: String,
    ctx: egui::Context,
}

fn main() {
    core9000::init(&adapters::APP);
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("wave9000 {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // Files macOS opens with the app, from Finder or another app.
    incoming::listen_early();
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    crash_report(&dir);
    let mut builder = ViewportBuilder::default()
        .with_title("wave9000")
        .with_app_id("wave9000")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([480.0, 320.0]);
    if let Some((rgba, width, height)) = icon_rgba() {
        builder = builder.with_icon(Arc::new(egui::IconData { rgba, width, height }));
    }
    if cfg!(target_os = "macos") {
        // Drawn under a transparent titlebar, so the top strip is ours.
        builder = builder.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false);
    }
    if let Err(e) = shell::run(builder, move |ctx| App::new(ctx, dir)) {
        eprintln!("wave9000: {e}");
        std::process::exit(1);
    }
}

/// The app icon as RGBA pixels.
fn icon_rgba() -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(ICON_PNG));
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    (info.color_type == png::ColorType::Rgba).then(|| {
        buf.truncate(info.buffer_size());
        (buf, info.width, info.height)
    })
}

/// macOS shows a bare program's icon as a blank one in the dock; give it ours.
#[cfg(all(target_os = "macos", not(test)))]
fn set_dock_icon() {
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{ClassType, msg_send};
    let data = objc2_foundation::NSData::with_bytes(ICON_PNG);
    // SAFETY: plain AppKit calls on the main thread with live objects.
    unsafe {
        let (Some(image), Some(app)) = (AnyClass::get(c"NSImage"), AnyClass::get(c"NSApplication")) else { return };
        let image: *mut AnyObject = msg_send![image, alloc];
        let image: *mut AnyObject = msg_send![image, initWithData: &*data];
        let app: *mut AnyObject = msg_send![app, sharedApplication];
        if !image.is_null() && !app.is_null() {
            let _: () = msg_send![app, setApplicationIconImage: image];
        }
    }
    let _ = objc2_foundation::NSData::class();
}

fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("WAVE9000_CONFIG_DIR") {
        return d.into();
    }
    let home = || PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/wave9000")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("wave9000")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home().join(".config"), PathBuf::from).join("wave9000")
    }
}

/// A crash leaves crash.txt; the next start reports it in the Log.
fn crash_report(dir: &Path) {
    let file = dir.join("crash.txt");
    if let Ok(text) = std::fs::read_to_string(&file) {
        log::error(format!("wave9000 stopped unexpectedly last time. Your project was kept. {}", text.trim()));
        let _ = std::fs::rename(&file, dir.join("crash-last.txt"));
    }
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::fs::write(&file, format!("{info}"));
        default(info);
    }));
}

/// Writes `text` to `path` through a temporary file, so a crash midway
/// can't leave half a file.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

impl App {
    fn new(ctx: &egui::Context, dir: PathBuf) -> Self {
        core9000::init(&adapters::APP);
        incoming::started(ctx);
        for (name, text) in [("settings.toml", settings::EXAMPLE), ("keys.toml", keys::EXAMPLE)] {
            let path = dir.join(name);
            if !path.exists() {
                let _ = std::fs::write(path, text);
            }
        }
        let layout_path = dir.join("layout.toml");
        if !layout_path.exists() {
            let _ = std::fs::write(&layout_path, layout::to_toml(&layout::default_layout()));
        }
        let config_changed = Arc::new(AtomicBool::new(false));
        {
            let (flag, ctx) = (config_changed.clone(), ctx.clone());
            watch::config_folder(&dir, move || {
                flag.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            });
        }
        #[cfg(all(target_os = "macos", not(test)))]
        set_dock_icon();
        let (tx, adapter_rx) = channel();
        let adapters = {
            let (tx, ctx) = (std::sync::Mutex::new(tx), ctx.clone());
            let env = adapters::Env { config: dir.clone(), builtin: adapters::builtin_dir(), ..adapters::Env::default() };
            adapters::Host::new(env, move |m| {
                let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(m);
                ctx.request_repaint();
            })
        };
        let (loaded_tx, loaded_rx) = channel();
        let mut app = Self {
            menu: menu::MenuBar::install(&Keymap::load("", &[]).0, ctx, None, &menu::Extras::default()),
            palette: theme::Palette::new(&settings::Look::default()),
            dir,
            settings: Settings::default(),
            keymap: Keymap::load("", &[]).0,
            layout: layout::default_layout(),
            layout_ui: LayoutUi::default(),
            config_changed,
            tools: None,
            project: Project::default(),
            sounds: HashMap::new(),
            reading: Vec::new(),
            loaded_tx,
            loaded_rx,
            undo: Vec::new(),
            redo: Vec::new(),
            selected_media: None,
            selected_track: None,
            selected_clip: None,
            engine: Engine::new(""),
            mix_dirty: true,
            recording: None,
            silence_warned: false,
            levels: vec![0.0; METERS + 1],
            view: View { pps: 40.0, scroll: 0.0, down: 0.0, width: 800.0, fit: false, drag: panels::Drag::None },
            file: None,
            dirty: false,
            session_dirty: false,
            system_dark: None,
            export: None,
            last_export: None,
            queue: Vec::new(),
            adapters,
            adapter_rx,
            adapter_panels: HashMap::new(),
            prompt: None,
            prompts: Vec::new(),
            export_choice: None,
            note: None,
            emit_changed: false,
            emit_media: false,
            encoders: None,
            title: String::new(),
            ctx: ctx.clone(),
        };
        app.load_config(true);
        app.adapters.start_launch_adapters();
        let session = std::fs::read_to_string(app.dir.join("session.json")).ok().and_then(|t| serde_json::from_str::<Project>(&t).ok());
        if let Some(p) = session {
            app.file = p.file.clone();
            app.set_project(p);
        }
        // Files named on the command line (Open With on Windows and Linux).
        let named: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).filter(|p| p.exists()).collect();
        app.open_paths(named);
        app
    }

    /// (Re)reads settings.toml, keys.toml and layout.toml. A file that
    /// can't be read leaves the rest working and says why in the Log.
    fn load_config(&mut self, first: bool) {
        let read = |name: &str| std::fs::read_to_string(self.dir.join(name)).unwrap_or_default();
        let settings = match toml::from_str::<Settings>(&read("settings.toml")) {
            Ok(s) => s,
            Err(e) => {
                log::error(format!("settings.toml: {} (using the defaults until it's fixed)", e.message()));
                Settings::default()
            }
        };
        match layout::from_toml(&read("layout.toml")) {
            Ok(root) => self.layout = root,
            Err(e) => log::error(format!("layout.toml: {e} (keeping the current layout)")),
        }
        let new_tools = first || settings.ffmpeg != self.settings.ffmpeg;
        let new_output = first || settings.sound.output != self.settings.sound.output;
        self.settings = settings;
        self.apply_look();
        if new_tools {
            self.tools = Tools::find(&self.settings.ffmpeg);
            match &self.tools {
                Some(t) => log::info(format!("Using {}", t.ffmpeg.display())),
                None => log::error("ffmpeg wasn't found, so only WAV files at the device's rate can be added. Install it (ffmpeg.org, or `brew install ffmpeg`) or set `ffmpeg = \"...\"` in settings.toml."),
            }
            self.encoders = None;
            // Adapters get the same ffmpeg.
            // Adapters use the same ffmpeg, so they need none of their own.
            let (mut init, mut vars) = (serde_json::Map::new(), Vec::new());
            for (key, path) in [("ffmpeg", self.tools.as_ref().map(|t| &t.ffmpeg)), ("ffprobe", self.tools.as_ref().map(|t| &t.ffprobe))] {
                init.insert(key.into(), path.map_or(serde_json::Value::Null, |p| p.to_string_lossy().into()));
                if let Some(p) = path {
                    vars.push((format!("WAVE9000_{}", key.to_uppercase()), p.clone().into_os_string()));
                }
            }
            self.adapters.env.init = init;
            self.adapters.env.vars = vars;
        }
        if new_output && self.recording.is_none() {
            self.open_output();
        }
        // Keys (the user's and adapters') and the menus that show them.
        self.refresh_commands();
        self.ctx.request_repaint();
    }

    /// (Re)opens the sound output. Files are decoded at its rate, so a new
    /// rate means reading them again.
    fn open_output(&mut self) {
        let at = self.playhead();
        self.engine.stop();
        let old = self.engine.rate;
        self.engine = Engine::new(&self.settings.sound.output);
        match &self.engine.error {
            Some(e) => log::error(format!("No sound output: {e}")),
            None => {
                let (outs, ins) = engine::devices();
                log::info(format!("Sound: {} Hz. Outputs: {}. Inputs: {}.", self.engine.rate, outs.join(", "), if ins.is_empty() { "none".into() } else { ins.join(", ") }));
            }
        }
        self.seek(at);
        if self.engine.rate != old {
            self.sounds.clear();
            for m in self.project.media.clone() {
                self.read(m.path, Place::Media);
            }
        }
        self.mix_dirty = true;
    }

    fn apply_look(&mut self) {
        let look = &self.settings.look;
        self.palette = theme::Palette::new(look);
        let theme = if look.dark { egui::Theme::Dark } else { egui::Theme::Light };
        self.ctx.set_theme(theme);
        self.ctx.set_visuals_of(theme, self.palette.visuals());
        let size = look.font_size.clamp(8.0, 32.0);
        self.ctx.global_style_mut(|s| {
            use egui::TextStyle::*;
            for (style, scale) in [(Body, 1.0), (Button, 1.0), (Monospace, 1.0), (Small, 0.8), (Heading, 1.4)] {
                if let Some(f) = s.text_styles.get_mut(&style) {
                    f.size = size * scale;
                }
            }
        });
    }

    fn set_project(&mut self, mut p: Project) {
        self.stop();
        let n = p.media.len();
        for t in &mut p.tracks {
            t.clips.retain(|c| c.media < n && c.len() > 0.0);
            t.clips.sort_by(|a, b| a.at.total_cmp(&b.at));
        }
        let at = p.playhead.max(0.0);
        self.project = p;
        self.project.file = None;
        self.selected_clip = None;
        self.selected_track = None;
        self.selected_media = None;
        self.undo.clear();
        self.redo.clear();
        self.dirty = false;
        self.emit_media = true;
        for m in self.project.media.clone() {
            if !self.sounds.contains_key(&m.path) {
                self.read(m.path, Place::Media);
            }
        }
        self.mix_dirty = true;
        self.seek(at);
        self.view.fit = true;
    }

    pub fn duration(&self) -> f64 {
        project::duration(&self.project.tracks)
    }

    pub fn playhead(&self) -> f64 {
        self.engine.pos() as f64 / f64::from(self.engine.rate)
    }

    pub fn seek(&mut self, t: f64) {
        if self.recording.is_some() {
            return;
        }
        self.engine.seek((t.max(0.0) * f64::from(self.engine.rate)).round() as i64);
        self.session_dirty = true;
        self.ctx.request_repaint();
    }

    /// Call before changing the tracks, so it can be undone.
    pub fn snapshot(&mut self) {
        self.undo.push(self.project.tracks.clone());
        if self.undo.len() > 500 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.edited();
    }

    pub fn edited(&mut self) {
        self.dirty = true;
        self.session_dirty = true;
        self.emit_changed = true;
        self.mix_dirty = true;
    }

    /// The mixer needs a new copy of the project (gain, pan, mute...);
    /// these changes aren't undo steps but are saved.
    pub fn mix_changed(&mut self) {
        self.mix_dirty = true;
        self.dirty = true;
        self.session_dirty = true;
    }

    fn mix(&self) -> Mix {
        let sounds = &self.sounds;
        let media = &self.project.media;
        Mix::of(&self.project, self.engine.rate, |m| media.get(m).and_then(|m| sounds.get(&m.path)).map(|s| s.samples.clone()))
    }

    pub fn sound(&self, media: usize) -> Option<&Sound> {
        self.project.media.get(media).and_then(|m| self.sounds.get(&m.path)).map(|s| &**s)
    }

    pub fn run(&mut self, command: &str) {
        if self.run_adapter_command(command) {
            return;
        }
        let nudge = self.settings.edit.nudge.max(0.001);
        match command {
            "play" => {
                if self.recording.is_some() {
                    self.stop_recording();
                } else if self.engine.playing() {
                    self.stop();
                } else {
                    self.play();
                }
            }
            "record" => {
                if self.recording.is_some() {
                    self.stop_recording();
                } else {
                    self.start_recording();
                }
            }
            "split" => self.split(),
            "delete" => {
                if let Some((t, c)) = self.selected_clip.filter(|&(t, c)| self.project.tracks.get(t).is_some_and(|tr| c < tr.clips.len())) {
                    self.snapshot();
                    self.project.tracks[t].clips.remove(c);
                    self.selected_clip = None;
                }
            }
            "undo" | "redo" => {
                let (from, to) = if command == "undo" { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
                if let Some(t) = from.pop() {
                    to.push(std::mem::replace(&mut self.project.tracks, t));
                    self.selected_clip = None;
                    self.selected_track = self.selected_track.filter(|&t| t < self.project.tracks.len());
                    self.edited();
                }
            }
            "nudge next" => self.seek(self.playhead() + nudge),
            "nudge previous" => self.seek(self.playhead() - nudge),
            "cut next" => {
                let t = self.playhead();
                if let Some(&c) = project::cuts(&self.project.tracks).iter().find(|&&c| c > t + 1e-4) {
                    self.seek(c);
                }
            }
            "cut previous" => {
                let t = self.playhead();
                let to = project::cuts(&self.project.tracks).into_iter().rev().find(|&c| c < t - 1e-4).unwrap_or(0.0);
                self.seek(to);
            }
            "start" => self.seek(0.0),
            "end" => self.seek(self.duration()),
            "zoom in" => self.view.pps = (self.view.pps * 1.5).min(4000.0),
            "zoom out" => self.view.pps = (self.view.pps / 1.5).max(0.5),
            "zoom fit" => self.zoom_fit(),
            "track new" => {
                self.snapshot();
                let n = self.project.tracks.len() + 1;
                self.project.tracks.push(Track::new(format!("Track {n}")));
                self.selected_track = Some(n - 1);
            }
            "track delete" => {
                if let Some(t) = self.selected_track.filter(|&t| t < self.project.tracks.len()) {
                    self.snapshot();
                    self.project.tracks.remove(t);
                    self.selected_track = None;
                    self.selected_clip = None;
                }
            }
            "track mute" | "track solo" | "track arm" => {
                if let Some(t) = self.selected_track.and_then(|t| self.project.tracks.get_mut(t)) {
                    match command {
                        "track mute" => t.mute = !t.mute,
                        "track solo" => t.solo = !t.solo,
                        _ => {
                            let on = !t.armed;
                            let i = self.selected_track.unwrap_or(0);
                            self.arm(i, on);
                        }
                    }
                    self.mix_changed();
                }
            }
            "import" => {
                let picked = rfd::FileDialog::new()
                    .add_filter("Sound", &["wav", "aif", "aiff", "flac", "mp3", "m4a", "aac", "ogg", "opus", "wma", "caf", "mp4", "mov", "mkv", "webm"])
                    .add_filter("All files", &["*"])
                    .pick_files();
                if let Some(paths) = picked {
                    self.import(paths, true);
                }
            }
            "import splice" => {
                if let Some(path) = rfd::FileDialog::new().add_filter("splice9000 project", &["splice"]).pick_file() {
                    self.import_splice(&path);
                }
            }
            "export" => self.start_export(self.export_choice.clone()),
            "new" => {
                self.stop();
                self.set_project(Project::default());
                self.file = None;
                self.session_dirty = true;
            }
            "open" => {
                if let Some(path) = rfd::FileDialog::new().add_filter("wave9000 project", &["wave"]).pick_file() {
                    self.open_project(&path);
                }
            }
            "save" | "save as" => {
                let path = match (&self.file, command) {
                    (Some(f), "save") => Some(f.clone()),
                    _ => rfd::FileDialog::new().add_filter("wave9000 project", &["wave"]).set_file_name("song.wave").save_file(),
                };
                if let Some(path) = path {
                    let mut p = self.project.clone();
                    p.wave9000 = FORMAT;
                    p.playhead = self.playhead();
                    let text = serde_json::to_string_pretty(&p).unwrap_or_default();
                    match write_atomic(&path, &text) {
                        Ok(()) => {
                            self.adapters.emit("saved", serde_json::json!({ "path": path }));
                            self.file = Some(path);
                            self.dirty = false;
                            self.session_dirty = true;
                        }
                        Err(e) => log::error(format!("Couldn't save {}: {e}", path.display())),
                    }
                }
            }
            "layout edit" => self.layout_ui.editing = !self.layout_ui.editing,
            c if c.starts_with("panel ") => self.toggle_panel(&c["panel ".len()..]),
            "layout reset" => {
                self.layout = layout::default_layout();
                self.save_layout();
            }
            "config" => open_folder(&self.dir),
            "sister splice9000" => self.video_to_splice9000(),
            "quit" => self.ctx.send_viewport_cmd(ViewportCommand::Close),
            other => log::error(format!("No command called \"{other}\"")),
        }
    }

    /// Cuts the selected clip at the playhead, or, with none selected
    /// there, every clip under it.
    fn split(&mut self) {
        let t = self.playhead();
        let min = 1.0 / f64::from(self.engine.rate) * 64.0;
        let mut tracks = self.project.tracks.clone();
        let mut any = false;
        let selected = self.selected_clip.filter(|&(tr, c)| tracks.get(tr).and_then(|x| x.clips.get(c)).is_some_and(|c| c.at < t && c.end() > t));
        match selected {
            Some((tr, c)) => any = tracks[tr].split(c, t, min),
            None => {
                for track in &mut tracks {
                    // From the last, so earlier indexes stay put.
                    let from = track.first_reaching(t);
                    let under: Vec<usize> = (from..track.clips.len()).take_while(|&i| track.clips[i].at < t).collect();
                    for &i in under.iter().rev() {
                        any |= track.split(i, t, min);
                    }
                }
            }
        }
        if any {
            self.snapshot();
            self.project.tracks = tracks;
            self.selected_clip = None;
        }
    }

    /// Arms track `i` for recording (one at a time: there's one input).
    fn arm(&mut self, i: usize, on: bool) {
        for (k, t) in self.project.tracks.iter_mut().enumerate() {
            t.armed = on && k == i;
        }
        self.session_dirty = true;
    }

    fn play(&mut self) {
        if self.mix_dirty {
            self.push_mix();
        }
        let end = self.duration();
        if end > 0.0 && self.playhead() >= end - 1e-3 {
            self.seek(0.0);
        }
        if let Err(e) = self.engine.play() {
            log::error(format!("Can't play: {e}"));
        }
        self.ctx.request_repaint();
    }

    pub fn stop(&mut self) {
        if self.recording.is_some() {
            self.stop_recording();
        }
        self.engine.stop();
        self.session_dirty = true;
    }

    /// Records onto the armed track, or the selected one, or a new one, from
    /// the playhead, playing the rest meanwhile.
    fn start_recording(&mut self) {
        self.engine.stop();
        let track = match self.project.tracks.iter().position(|t| t.armed).or(self.selected_track.filter(|&t| t < self.project.tracks.len())) {
            Some(t) => t,
            None => {
                self.snapshot();
                let n = self.project.tracks.len() + 1;
                self.project.tracks.push(Track::new(format!("Track {n}")));
                n - 1
            }
        };
        self.arm(track, true);
        self.selected_track = Some(track);
        let dir = self.takes_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.say(format!("Can't record into {}: {e}", dir.display()));
            return;
        }
        let path = record::next_take(&dir);
        let (ctx, frame) = (self.ctx.clone(), self.playback_frame());
        let at = self.playhead();
        match Recording::start(&self.settings.sound.input, path, at, track, move || ctx.request_repaint_after(frame)) {
            Ok(r) => {
                log::info(format!("Recording into {} at {} Hz", r.path.display(), r.rate));
                self.silence_warned = false;
                self.recording = Some(r);
                if self.mix_dirty {
                    self.push_mix();
                }
                if let Err(e) = self.engine.play() {
                    log::error(format!("Recording without playback: {e}"));
                }
            }
            Err(e) => self.say(format!("Can't record: {e}")),
        }
    }

    fn stop_recording(&mut self) {
        let Some(r) = self.recording.take() else { return };
        self.engine.stop();
        let (at, track) = (r.at, r.track);
        let offset = self.settings.record.offset_ms / 1000.0;
        match r.finish() {
            Ok(path) => self.read(path, Place::On(track, (at - offset).max(0.0))),
            Err(e) => self.say(format!("The recording failed: {e}")),
        }
    }

    /// Takes go beside the saved project, or to settings.toml's folder.
    fn takes_dir(&self) -> PathBuf {
        if let Some(f) = &self.file {
            let stem = f.file_stem().map_or("project".into(), |s| s.to_string_lossy().into_owned());
            return f.with_file_name(format!("{stem} takes"));
        }
        let configured = self.settings.record.folder.trim();
        if configured.is_empty() { self.dir.join("takes") } else { PathBuf::from(configured) }
    }

    /// Hides a panel that's showing, or shows one that isn't (beside the
    /// tracks). The last panel stays.
    fn toggle_panel(&mut self, kind: &str) {
        if !layout::PANELS.iter().any(|p| p.0 == kind) {
            return;
        }
        let mut at = Vec::new();
        if self.layout.find(kind, &mut at) {
            if at.is_empty() {
                return;
            }
            self.layout.remove(&at);
        } else {
            let mut beside = Vec::new();
            if !self.layout.find("tracks", &mut beside) {
                beside.clear();
            }
            self.layout.insert(&beside, kind, Side::Right);
        }
        self.save_layout();
    }

    /// The strip across the top: the mark and name, the project, and the
    /// tools used most. Dragging it moves the window.
    fn top_strip(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        use egui::{Align2, FontId, RichText, Sense};
        let pal = self.palette;
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, pal.header);
        painter.rect_filled(egui::Rect::from_x_y_ranges(rect.x_range(), rect.bottom() - 1.0..=rect.bottom()), 0.0, pal.line);
        let drag = ui.interact(rect, egui::Id::unique("top strip"), Sense::click_and_drag());
        if drag.double_clicked() {
            let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        } else if drag.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }

        // Left: the mark and the name (after the traffic lights on macOS),
        // or the menus elsewhere.
        let mut x = rect.left() + if cfg!(target_os = "macos") { 84.0 } else { 8.0 };
        if !cfg!(target_os = "macos") {
            let menus = egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(rect.left() + 400.0, rect.bottom()));
            let mut mui = ui.new_child(egui::UiBuilder::new().max_rect(menus));
            let layout = &self.layout;
            let extras = self.menu_extras();
            menu::draw(&mut mui, &self.keymap, &extras, &|k| layout.contains(k), &mut self.queue);
            x = menus.right() + 8.0;
        }
        let cy = rect.center().y;
        panels::mark(&painter, egui::pos2(x, cy), 22.0, &pal);
        let name = painter.text(egui::pos2(x + 30.0, cy), Align2::LEFT_CENTER, "wave", FontId::proportional(14.0), pal.text);
        painter.text(egui::pos2(name.right() + 1.0, cy + 0.5), Align2::LEFT_CENTER, "9000", FontId::monospace(12.5), pal.accent);

        // Middle: the project.
        let project = self.file.as_ref().and_then(|f| f.file_stem()).map_or("Untitled".to_owned(), |s| s.to_string_lossy().into_owned());
        let project = if self.dirty { format!("{project}  •") } else { project };
        painter.text(rect.center(), Align2::CENTER_CENTER, project, FontId::proportional(12.0), pal.weak);

        // Right: tools.
        let tools = egui::Rect::from_min_max(egui::pos2(rect.center().x + 120.0, rect.top() + 4.0), egui::pos2(rect.right() - 10.0, rect.bottom() - 4.0));
        let mut tui = ui.new_child(egui::UiBuilder::new().max_rect(tools).layout(egui::Layout::right_to_left(egui::Align::Center)));
        tui.spacing_mut().item_spacing.x = 6.0;
        let can_export = self.duration() > 0.0 && self.export.is_none();
        let export = egui::Button::new(RichText::new("Export").size(12.0).color(pal.on_accent)).fill(pal.accent);
        if tui.add_enabled(can_export, export).on_hover_text("Export the mix").clicked() {
            self.queue.push("export".into());
        }
        if tui.add(egui::Button::new(RichText::new("Import").size(12.0))).on_hover_text("Add sound files").clicked() {
            self.queue.push("import".into());
        }
        let shown = self.layout.contains("adapters");
        if tui.add(egui::Button::selectable(shown, RichText::new("Adapters").size(12.0))).on_hover_text("Your adapters").clicked() {
            self.queue.push("panel adapters".into());
        }
        let layout = egui::Button::selectable(self.layout_ui.editing, RichText::new("Layout").size(12.0));
        if tui.add(layout).on_hover_text("Move, add and remove panels (or drag any panel's header)").clicked() {
            self.queue.push("layout edit".into());
        }
    }

    /// Puts media `m` on track `track` (a new one if None) at `at`.
    pub fn place(&mut self, m: usize, track: Option<usize>, at: f64) {
        let Some(media) = self.project.media.get(m).cloned() else { return };
        let clip = Clip { media: m, at: at.max(0.0), src_in: 0.0, src_out: media.duration, gain_db: 0.0 };
        let was_empty = self.duration() == 0.0;
        self.snapshot();
        let t = match track.filter(|&t| t < self.project.tracks.len()) {
            Some(t) => t,
            None => {
                let name = media.path.file_stem().map_or("Track".into(), |s| s.to_string_lossy().into_owned());
                self.project.tracks.push(Track::new(name));
                self.project.tracks.len() - 1
            }
        };
        let c = self.project.tracks[t].insert(clip);
        self.selected_track = Some(t);
        self.selected_clip = Some((t, c));
        if was_empty {
            self.view.fit = true;
        }
    }

    pub fn zoom_fit(&mut self) {
        self.view.fit = false;
        let width = self.view.width.max(200.0);
        let d = self.duration();
        if d > 0.0 {
            self.view.pps = ((width - 40.0) / d as f32).clamp(0.5, 4000.0);
        }
        self.view.scroll = 0.0;
    }

    /// Adds files, each on a new track at the playhead (`place`), or only
    /// to Media.
    fn import(&mut self, paths: Vec<PathBuf>, place: bool) {
        let at = self.playhead();
        for p in paths {
            self.read(p, if place { Place::NewTrack(at) } else { Place::Media });
        }
    }

    /// Reads a file on another thread; `take_loaded` puts it where it goes.
    fn read(&mut self, path: PathBuf, place: Place) {
        if place == Place::Media && self.reading.contains(&path) {
            return;
        }
        self.reading.push(path.clone());
        let (tx, tools, rate, ctx) = (self.loaded_tx.clone(), self.tools.clone(), self.engine.rate, self.ctx.clone());
        std::thread::spawn(move || {
            let r = media::load(tools.as_ref(), &path, rate).map_err(|e| format!("{}: {e}", path.display()));
            let _ = tx.send((path, r, rate, place));
            ctx.request_repaint();
        });
    }

    fn take_loaded(&mut self) {
        while let Ok((path, r, rate, place)) = self.loaded_rx.try_recv() {
            if let Some(i) = self.reading.iter().position(|p| *p == path) {
                self.reading.remove(i);
            }
            // The output changed rate while it was being read: it would
            // play at the wrong pitch, so it's read again.
            if rate != self.engine.rate {
                self.read(path, place);
                continue;
            }
            match r {
                Ok((m, sound)) => {
                    let i = match self.project.media.iter().position(|x| x.path == m.path) {
                        Some(i) => {
                            // Keep what the file said when it was added; a
                            // recording's length is only known now.
                            if self.project.media[i].duration <= 0.0 {
                                self.project.media[i] = m;
                            }
                            i
                        }
                        None => {
                            self.project.media.push(m);
                            self.emit_media = true;
                            self.project.media.len() - 1
                        }
                    };
                    self.sounds.insert(path, Arc::new(sound));
                    self.mix_dirty = true;
                    match place {
                        Place::Media => self.selected_media = self.selected_media.or(Some(i)),
                        Place::NewTrack(at) => self.place(i, None, at),
                        Place::On(t, at) => self.place(i, Some(t), at),
                    }
                }
                Err(e) => log::error(e),
            }
        }
    }

    /// A splice9000 project's sound, on a track of its own: each clip where
    /// it plays in the edit.
    fn import_splice(&mut self, path: &Path) {
        let p = match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<SpliceProject>(&t).map_err(|e| e.to_string())) {
            Ok(p) => p,
            Err(e) => {
                self.say(format!("Couldn't read {}: {e}", path.display()));
                return;
            }
        };
        let placed = p.placed();
        if placed.is_empty() {
            self.say("That project has no clips with sound.");
            return;
        }
        self.snapshot();
        let name = path.file_stem().map_or("splice9000".into(), |s| s.to_string_lossy().into_owned());
        let mut track = Track::new(name);
        for (file, at, src_in, src_out) in placed {
            let m = match self.project.media.iter().position(|m| m.path == file) {
                Some(m) => m,
                None => {
                    // Its length comes when it's read; the clip says how
                    // much of it is used.
                    self.project.media.push(Media { path: file.clone(), duration: 0.0, rate: 0, channels: 0 });
                    self.read(file, Place::Media);
                    self.project.media.len() - 1
                }
            };
            track.insert(Clip { media: m, at, src_in, src_out, gain_db: 0.0 });
        }
        self.project.tracks.push(track);
        self.emit_media = true;
        self.selected_track = Some(self.project.tracks.len() - 1);
        self.say(format!("Added the sound of {}", path.display()));
        self.view.fit = true;
    }

    fn open_project(&mut self, path: &Path) {
        match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Project>(&t).map_err(|e| e.to_string())) {
            Ok(p) => {
                self.set_project(p);
                self.file = Some(path.to_path_buf());
                self.session_dirty = true;
            }
            Err(e) => log::error(format!("Couldn't open {}: {e}", path.display())),
        }
    }

    /// Exports the mix, in settings.toml's format or an adapter's `preset`.
    fn start_export(&mut self, preset: Option<(String, String)>) {
        if self.export.is_some() {
            return;
        }
        if self.duration() <= 0.0 {
            log::error("Nothing to export yet: add some sound to a track first.");
            return;
        }
        let spec = match &preset {
            Some((a, id)) => match self.adapters.export(a, id) {
                Some(s) => Some(s.clone()),
                None => {
                    log::error(format!("No export format \"{id}\" in {a}"));
                    return;
                }
            },
            None => None,
        };
        let name = self.file.as_ref().and_then(|f| f.file_stem()).map_or("mix".into(), |s| s.to_string_lossy().into_owned());
        let ext = spec.as_ref().map_or(self.settings.export.format.to_lowercase(), |s| s.extension.clone());
        let label = export::EXTENSIONS.iter().find(|e| e.0 == ext).map_or(ext.to_uppercase(), |e| e.1.to_owned());
        let Some(out) = rfd::FileDialog::new().add_filter(spec.as_ref().map_or(label.as_str(), |s| s.label.as_str()), &[ext.as_str()]).set_file_name(format!("{name}.{ext}")).save_file() else {
            return;
        };
        let ext = out.extension().map_or(ext, |e| e.to_string_lossy().to_lowercase());
        let Some(how) = export::how(&ext, &self.settings.export, spec.as_ref()) else {
            self.say(format!("wave9000 doesn't know how to write .{ext} files: try wav, flac, mp3, m4a, ogg or opus."));
            return;
        };
        if let Some(missing) = self.missing_encoder(&how) {
            self.say(format!("Your ffmpeg has no {missing} encoder, so it can't write that format."));
            return;
        }
        self.stop();
        let ctx = self.ctx.clone();
        self.last_export = None;
        self.export = Some(Job::start(Arc::new(self.mix()), self.engine.rate, self.tools.clone(), how, out, move || ctx.request_repaint()));
    }

    /// The encoder a format needs that this ffmpeg doesn't have.
    pub fn missing_encoder(&mut self, how: &export::How) -> Option<String> {
        let enc = export::encoder(how)?;
        let Some(tools) = &self.tools else { return Some(enc.to_owned()) };
        let have = self.encoders.get_or_insert_with(|| tools.encoders());
        // An empty list: couldn't tell; let ffmpeg say.
        (!have.is_empty() && !have.iter().any(|e| e == enc)).then(|| enc.to_owned())
    }

    pub fn cancel_export(&self) {
        if let Some(job) = &self.export {
            job.cancel();
        }
    }

    fn check_export(&mut self) {
        let Some(job) = &self.export else { return };
        let finished = job.progress.lock().unwrap_or_else(|e| e.into_inner()).finished.take();
        if let Some(r) = finished {
            match &r {
                Ok(s) => {
                    log::info(format!("Exported {}: {s}", job.out.display()));
                    self.adapters.emit("exported", serde_json::json!({ "path": job.out }));
                }
                Err(e) => log::error(format!("Export failed: {e}")),
            }
            self.last_export = Some(r);
            self.export = None;
        }
    }

    fn push_mix(&mut self) {
        let mix = self.mix();
        self.engine.set_mix(mix);
        self.mix_dirty = false;
    }

    /// What the sound output and input reported: a device that went away
    /// stops playing (and recording) instead of leaving a frozen playhead.
    fn watch_sound(&mut self) {
        let (lost, rerouted, dropouts) = self.engine.shared.trouble.take();
        if rerouted {
            log::info("Sound: following the system's new output.");
        }
        if dropouts > 0 {
            log::info(format!("Sound: {} (the output wasn't given sound in time).", dropouts_text(dropouts)));
        }
        if lost {
            self.stop();
            self.open_output();
            self.say("The sound output went away, so playing stopped. Press play to go on with the output there is now.");
        }
        let Some(r) = &self.recording else { return };
        let (input_lost, _, _) = r.live.trouble.take();
        // Pure silence, or nothing at all, for a second and a half.
        let silent = r.started.elapsed().as_secs_f64() > 1.5 && !r.live.heard.load(Ordering::Relaxed);
        if input_lost {
            self.stop();
            self.say("The sound input went away, so recording stopped. What was recorded is kept.");
        } else if silent && !self.silence_warned {
            self.silence_warned = true;
            self.say(SILENT_INPUT);
        }
    }

    /// While playing: meters, the playhead's view, and stopping at the end.
    fn follow_playback(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        // Meters fall 24 dB a second.
        let fall = 10f32.powf(-24.0 * dt / 20.0);
        let mut moving = false;
        for (i, l) in self.levels.iter_mut().enumerate() {
            let hit = self.engine.shared.meters.take(i);
            *l = hit.max(*l * fall);
            if *l < 1e-4 {
                *l = 0.0;
            }
            moving |= *l > 0.0;
        }
        let playing = self.engine.playing();
        if playing && self.recording.is_none() && self.playhead() > self.duration() + 0.25 {
            self.stop();
            self.seek(self.duration());
        }
        // Paced, not every frame the screen can show: each frame redraws
        // the playhead's column and the meters, and that's what playing
        // costs.
        if playing || moving || self.recording.is_some() {
            ctx.request_repaint_after(self.playback_frame());
        }
    }

    /// How long to wait between frames while playing or recording.
    fn playback_frame(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f32(1.0 / self.settings.look.playback_fps.clamp(1.0, 240.0))
    }

    fn save_layout(&self) {
        if let Err(e) = write_atomic(&self.dir.join("layout.toml"), &layout::to_toml(&self.layout)) {
            log::error(format!("Couldn't save layout.toml: {e}"));
        }
    }

    fn save_session(&mut self) {
        let mut p = self.project.clone();
        p.wave9000 = FORMAT;
        p.playhead = self.playhead();
        p.file = self.file.clone();
        if let Ok(text) = serde_json::to_string(&p) {
            let _ = write_atomic(&self.dir.join("session.json"), &text);
        }
        self.session_dirty = false;
    }

    fn dropped_files(&mut self, ctx: &egui::Context) {
        let paths: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        self.open_paths(paths);
    }

    /// Opens dropped files, or files named when wave9000 was started:
    /// adapters are offered for install, a project opens, splice9000
    /// projects bring their sound, and sound files go on new tracks.
    /// Opens the selected media in splice9000 when it's a video: the
    /// picture's sister.
    fn video_to_splice9000(&mut self) {
        let m = self.selected_media.or_else(|| self.selected_clip.and_then(|(t, c)| self.project.tracks.get(t)?.clips.get(c).map(|c| c.media)));
        let Some(path) = m.and_then(|m| self.project.media.get(m)).map(|m| m.path.clone()) else {
            return self.say("Select a video's sound to open the video in splice9000.");
        };
        if !core9000::sisters::is(&path, core9000::sisters::VIDEO) {
            return self.say(format!("{} is sound only; splice9000 opens videos.", path.display()));
        }
        if let Err(e) = core9000::sisters::get("splice9000").expect("a sister").open(&[&path], None) {
            self.say(e);
        }
    }

    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        // wave9000 takes sound, its projects, and splice9000's projects and
        // videos (for their sound); photos, documents and comics go to the
        // sisters that open them.
        use core9000::sisters::{SOUND, VIDEO, is};
        let (paths, problems) = core9000::sisters::hand_off(paths, |p| is(p, SOUND) || is(p, VIDEO) || is(p, &["wave", "splice"]) || adapters::is_package(p));
        for p in problems {
            self.say(p);
        }
        let (packages, paths): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| adapters::is_package(p));
        for p in packages {
            self.offer_install(p);
        }
        let ext = |p: &PathBuf, e: &str| p.extension().is_some_and(|x| x.eq_ignore_ascii_case(e));
        let mut sound = Vec::new();
        for p in paths {
            if ext(&p, "wave") {
                self.open_project(&p);
            } else if ext(&p, "splice") {
                self.import_splice(&p);
            } else {
                sound.push(p);
            }
        }
        if !sound.is_empty() {
            self.import(sound, true);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = self.file.as_ref().and_then(|f| f.file_stem()).map(|s| s.to_string_lossy().into_owned());
        let title = match name {
            Some(n) => format!("{n}{} — wave9000", if self.dirty { " •" } else { "" }),
            None => "wave9000".to_owned(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl shell::App for App {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.config_changed.swap(false, Ordering::Relaxed) {
            self.load_config(false);
        }
        self.dropped_files(&ctx);
        self.open_paths(incoming::take());
        self.serve_adapters();
        let mut commands = std::mem::take(&mut self.queue);
        commands.extend(self.menu.take());
        // While a question is on screen, keys go to it, not to the project.
        if self.prompt.is_none() {
            commands.extend(self.keymap.take(&ctx));
        }
        if self.layout_ui.editing && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.layout_ui.editing = false;
        }
        for c in commands {
            self.run(&c);
        }
        self.take_loaded();
        self.check_export();

        // The traffic lights follow the system theme: match it to ours.
        if self.system_dark != Some(self.palette.dark) {
            self.system_dark = Some(self.palette.dark);
            let theme = if self.palette.dark { egui::SystemTheme::Dark } else { egui::SystemTheme::Light };
            ctx.send_viewport_cmd(ViewportCommand::SetTheme(theme));
        }
        let full = ui.max_rect();
        let strip = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), TOP));
        self.top_strip(ui, strip);
        let rect = egui::Rect::from_min_max(egui::pos2(full.left(), strip.bottom()), full.max);
        let mut root = std::mem::replace(&mut self.layout, Node::panel("tracks"));
        let mut lui = std::mem::take(&mut self.layout_ui);
        let palette = self.palette;
        let headers = self.settings.look.panel_headers;
        let titles: HashMap<String, String> = self
            .adapters
            .all
            .iter()
            .flat_map(|a| a.manifest.panels.iter().map(move |p| (format!("adapter:{}/{}", a.manifest.name, p.id), p.title.clone())))
            .collect();
        let title = |k: &str| titles.get(k).cloned().unwrap_or_else(|| layout::title(k).to_owned());
        let out = lui.show(
            ui,
            &mut root,
            rect,
            &mut layout::Frame { palette: &palette, headers, panel: &mut |kind, part, ui| panels::draw(self, kind, part, ui), title: &title },
        );
        self.layout = root;
        self.layout_ui = lui;
        if out.changed {
            self.save_layout();
        }
        if self.layout_ui.editing {
            panels::edit_banner(ui, rect, &palette);
        }
        let layout = &self.layout;
        self.menu.sync(&|k| layout.contains(k));
        self.show_prompt(&ctx);
        panels::note(ui, rect, self);
        if std::mem::take(&mut self.emit_media) {
            let paths: Vec<&PathBuf> = self.project.media.iter().map(|m| &m.path).collect();
            self.adapters.emit("media", serde_json::json!({ "paths": paths }));
        }
        if std::mem::take(&mut self.emit_changed) {
            let summary = serde_json::json!({ "duration": self.duration(), "tracks": self.project.tracks.len() });
            self.adapters.emit("changed", summary);
        }
        // The mixer gets a fresh copy once a frame at most.
        if self.mix_dirty {
            self.push_mix();
        }
        self.watch_sound();
        self.follow_playback(&ctx);

        if self.session_dirty && !ctx.input(|i| i.pointer.any_down()) && !self.engine.playing() {
            self.save_session();
        }
        self.update_title(&ctx);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.stop();
            self.save_session();
            self.adapters.stop_all();
        }
    }
}

/// Said when an input gives nothing but exact zeros: how each system
/// answers when the microphone isn't allowed, or is muted.
const SILENT_INPUT: &str = if cfg!(target_os = "macos") {
    "Nothing at all is coming from the input. If macOS asked about the microphone and it wasn't allowed, allow wave9000 in System Settings → Privacy & Security → Microphone, then record again."
} else if cfg!(windows) {
    "Nothing at all is coming from the input. Check that Windows lets apps use the microphone (Settings → Privacy & security → Microphone) and that the input isn't muted, then record again."
} else {
    "Nothing at all is coming from the input. Check that it isn't muted (in your sound settings or alsamixer) and that it's the input you meant (`input` in settings.toml), then record again."
};

/// "a dropout", "3 dropouts".
pub fn dropouts_text(n: u64) -> String {
    if n == 1 { "a dropout".into() } else { format!("{n} dropouts") }
}

fn open_folder(dir: &Path) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(program).arg(dir).spawn() {
        log::error(format!("Couldn't open {}: {e}", dir.display()));
    }
}
