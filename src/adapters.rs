//! wave9000's adapters: the 9000 series' shared host (core9000), with what
//! wave9000 adds to `adapter.toml` (export formats) and the permissions of
//! its own requests. The protocol is in docs/adapters.md.

use serde::{Deserialize, Serialize};

pub use core9000::adapters::{Env, Message, StartError, builtin_dir, command_name, describe, is_package, parse_command, remember_approval, summary};

/// The version of wave9000's own requests (`edit.*`, `playback.set`), sent
/// in `initialize` as `protocol`. New versions only add things.
pub const PROTOCOL: u32 = 1;

pub const EDIT_READ: &str = "edit.read";
pub const EDIT_CHANGE: &str = "edit.change";

/// Who wave9000 is, for core9000.
pub static APP: core9000::App = core9000::App {
    name: "wave9000",
    version: env!("CARGO_PKG_VERSION"),
    env_prefix: "WAVE9000",
    legacy_env_prefix: "",
    protocol: PROTOCOL,
    permissions: &[(EDIT_READ, "see your project: the tracks, clips, sound files and playhead"), (EDIT_CHANGE, "change your project, add sound files and move the playhead")],
    events_permission: EDIT_READ,
};

/// What wave9000 reads from `adapter.toml` beyond what every sister does.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Extra {
    #[serde(default)]
    pub exports: Vec<ExportSpec>,
}

impl core9000::adapters::Extra for Extra {
    fn adds(&self) -> Vec<String> {
        let n = self.exports.len();
        if n == 0 { Vec::new() } else { vec![format!("{n} export format{}", if n == 1 { "" } else { "s" })] }
    }

    fn without_program(&self) -> bool {
        !self.exports.is_empty()
    }

    const ALONE: &'static str = "`[[exports]]`";
}

pub type Manifest = core9000::adapters::Manifest<Extra>;
pub type Host = core9000::adapters::Host<Extra>;

pub fn package_manifest(path: &std::path::Path) -> Result<Manifest, String> {
    core9000::adapters::package_manifest(path)
}

pub fn install(source: &std::path::Path, config: &std::path::Path) -> Result<Manifest, String> {
    core9000::adapters::install(source, config)
}

/// wave9000's sections, across every adapter.
pub trait Sections {
    /// Every export format, with the adapter it comes from.
    fn exports(&self) -> impl Iterator<Item = (&str, &ExportSpec)>;
    fn export(&self, adapter: &str, id: &str) -> Option<&ExportSpec>;
}

impl Sections for Host {
    fn exports(&self) -> impl Iterator<Item = (&str, &ExportSpec)> {
        self.all.iter().flat_map(|a| a.manifest.extra.exports.iter().map(move |e| (a.manifest.name.as_str(), e)))
    }

    fn export(&self, adapter: &str, id: &str) -> Option<&ExportSpec> {
        self.exports().find(|(a, e)| *a == adapter && e.id == id).map(|(_, e)| e)
    }
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
