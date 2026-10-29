//! settings.toml. Every field has a default, so a missing or half-written
//! file still works, and unknown keys are ignored so old files keep working.

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Path to ffmpeg; empty: look for it.
    pub ffmpeg: String,
    pub sound: Sound,
    pub record: Record,
    pub export: Export,
    pub edit: Edit,
    pub look: Look,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Sound {
    /// The output and input by name; empty: the system's own.
    pub output: String,
    pub input: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Record {
    /// Where takes are kept when the project isn't saved yet; empty: the
    /// `takes` folder in the settings folder.
    pub folder: String,
    /// Moves new takes earlier by this many milliseconds, for the time the
    /// sound spends in the input and output.
    pub offset_ms: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Export {
    /// The format File → Export suggests: wav, flac, mp3, m4a, ogg, opus.
    pub format: String,
    /// WAV and FLAC: 16, 24 or 32 (float).
    pub bits: u32,
    pub mp3_bitrate: String,
    pub aac_bitrate: String,
    /// Passed to ffmpeg just before the output file.
    pub extra_args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Edit {
    /// How far ← and → move the playhead, in seconds.
    pub nudge: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Look {
    pub dark: bool,
    pub accent: String,
    pub font_size: f32,
    /// Each panel has a slim header to drag it by.
    pub panel_headers: bool,
    /// Each track's height on the Tracks panel, in points.
    pub track_height: f32,
}

impl Default for Export {
    fn default() -> Self {
        Self { format: "wav".into(), bits: 24, mp3_bitrate: "320k".into(), aac_bitrate: "256k".into(), extra_args: Vec::new() }
    }
}

impl Default for Edit {
    fn default() -> Self {
        Self { nudge: 0.1 }
    }
}

impl Default for Look {
    fn default() -> Self {
        Self { dark: true, accent: "#3fb8ff".into(), font_size: 13.0, panel_headers: true, track_height: 72.0 }
    }
}

impl Look {
    pub fn accent(&self) -> egui::Color32 {
        let hex = self.accent.trim_start_matches('#');
        match (hex.len(), u32::from_str_radix(hex, 16)) {
            (6, Ok(v)) => egui::Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8),
            _ => crate::theme::WAVE_BLUE,
        }
    }
}

pub const EXAMPLE: &str = r##"# wave9000 settings. Saved changes apply at once; delete a line to get
# its default back. Everything here is optional.

# Where ffmpeg is, if wave9000 can't find it on its own. It reads every
# file you add; WAV exports don't need it.
# ffmpeg = "/opt/homebrew/bin/ffmpeg"

[sound]
# The output to play through and the input to record from, by name (the
# Log lists them when wave9000 starts). Empty: the system's own.
output = ""
input = ""

[record]
# Where takes go while the project isn't saved. Once it is, they go in a
# "<project> takes" folder beside it. Empty: "takes" in this folder.
folder = ""
# Moves new takes earlier by this many milliseconds, to make up for the
# time sound spends in your interface. Record a click played out loud to
# measure it.
offset_ms = 0

[export]
# What Export suggests: wav, flac, mp3, m4a, ogg or opus. WAV is written by
# wave9000 itself; the others by ffmpeg.
format = "wav"
# WAV and FLAC: 16, 24, or 32 (float, never clips).
bits = 24
mp3_bitrate = "320k"
aac_bitrate = "256k"
# Anything else for ffmpeg, put just before the output file.
# extra_args = ["-af", "loudnorm=I=-14"]

[edit]
# How far ← and → move the playhead, in seconds.
nudge = 0.1

[look]
dark = true
# The one signal color: playhead, selection, play button.
accent = "#3fb8ff"
font_size = 13
# A slim header on each panel, to drag it somewhere else. Off: use
# Edit Layout (cmd+L) to move panels.
panel_headers = true
track_height = 72
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses_and_half_files_work() {
        let s: Settings = toml::from_str(EXAMPLE).unwrap();
        assert_eq!(s.export.bits, 24);
        let s: Settings = toml::from_str("[export]\nbits = 16\nsomething_new = 1").unwrap();
        assert_eq!((s.export.bits, s.export.format.as_str(), s.edit.nudge), (16, "wav", 0.1));
        assert_eq!(Look { accent: "#ff0000".into(), ..Look::default() }.accent(), egui::Color32::RED);
    }
}
