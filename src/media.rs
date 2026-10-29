//! Sound files and the ffmpeg programs that read and write them.
//!
//! wave9000 links no codecs: it runs the system's `ffmpeg` and `ffprobe` as
//! separate programs, the way splice9000 does. A broken file or a crashing
//! decoder can't take the workstation down with it. Each file is decoded
//! once, at the sound device's own rate, as stereo 32-bit float, so the
//! mixer never resamples or converts anything while playing.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A sound file used by the project. Video files work too: their sound is
/// used.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub path: PathBuf,
    pub duration: f64,
    /// As stored in the file.
    #[serde(default)]
    pub rate: u32,
    #[serde(default)]
    pub channels: u32,
}

impl Media {
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }
}

/// A file's sound, decoded: stereo frames, left and right interleaved, at
/// the engine's rate, and its overview for drawing.
pub struct Sound {
    pub samples: Arc<[f32]>,
    pub peaks: Peaks,
}

/// The loudest moment in each block of frames, at a few block sizes, so a
/// waveform of any zoom is drawn from at most a few values a pixel.
pub struct Peaks {
    /// `levels[k]` holds one value per `BLOCK << (2 * k)` frames, 0 to 255.
    pub levels: Vec<Vec<u8>>,
}

pub const BLOCK: usize = 64;

impl Peaks {
    pub fn of(samples: &[f32]) -> Self {
        let first = samples.chunks(BLOCK * 2).map(|b| level(b.iter().fold(0.0f32, |m, s| m.max(s.abs())))).collect();
        let mut levels: Vec<Vec<u8>> = vec![first];
        // Each level is a quarter of the one before.
        while levels.last().is_some_and(|l| l.len() > 64) {
            let next = levels.last().expect("checked").chunks(4).map(|c| c.iter().copied().max().unwrap_or(0)).collect();
            levels.push(next);
        }
        Self { levels }
    }

    /// The loudest value in frames `a..b`, 0 to 1: read from the coarsest
    /// level that still has a block or more per span.
    pub fn max(&self, a: usize, b: usize) -> f32 {
        if b <= a || self.levels.is_empty() {
            return 0.0;
        }
        let span = b - a;
        let mut k = 0;
        while k + 1 < self.levels.len() && BLOCK << (2 * (k + 1)) <= span {
            k += 1;
        }
        let size = BLOCK << (2 * k);
        let l = &self.levels[k];
        let (i, j) = (a / size, b.div_ceil(size).min(l.len()));
        let m = if i < j { l[i..j].iter().copied().max().unwrap_or(0) } else { 0 };
        f32::from(m) / 255.0
    }
}

/// 0 to 1 as 0 to 255, on a square-root curve so quiet passages still show.
pub fn level(m: f32) -> u8 {
    (m.min(1.0).sqrt() * 255.0).round() as u8
}

#[derive(Clone, Debug)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Tools {
    /// Finds ffmpeg and ffprobe: `configured` (from settings.toml) if set,
    /// otherwise on the PATH or where package managers put them. Apps
    /// started from Finder don't get the shell's PATH, hence the list.
    pub fn find(configured: &str) -> Option<Self> {
        let ffmpeg = if configured.trim().is_empty() {
            search(exe("ffmpeg"))?
        } else {
            let p = PathBuf::from(configured.trim());
            if !p.is_file() {
                return None;
            }
            p
        };
        // ffprobe sits beside ffmpeg in every package of it.
        let ffprobe = ffmpeg.with_file_name(exe("ffprobe"));
        let ffprobe = if ffprobe.is_file() { ffprobe } else { search(exe("ffprobe"))? };
        Some(Self { ffmpeg, ffprobe })
    }

    /// The encoders this ffmpeg was built with, such as "libmp3lame".
    pub fn encoders(&self) -> Vec<String> {
        let out = quiet(Command::new(&self.ffmpeg)).args(["-hide_banner", "-encoders"]).output();
        out.map(|o| parse_encoders(&String::from_utf8_lossy(&o.stdout))).unwrap_or_default()
    }

    pub fn probe(&self, path: &Path) -> Result<Media, String> {
        let out = quiet(Command::new(&self.ffprobe))
            .args(["-v", "error", "-of", "json", "-show_entries"])
            .arg("format=duration:stream=codec_type,sample_rate,channels,duration")
            .arg(path)
            .output()
            .map_err(|e| format!("couldn't run ffprobe: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(err.lines().last().unwrap_or("ffprobe couldn't read it").to_owned());
        }
        parse_probe(path, &out.stdout)
    }

    /// The whole file's sound at `rate`, stereo (mono goes to both sides,
    /// more channels are mixed down by ffmpeg).
    pub fn decode(&self, path: &Path, rate: u32) -> Result<Sound, String> {
        let mut child = quiet(Command::new(&self.ffmpeg))
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-vn", "-sn", "-dn", "-ac", "2", "-ar", &rate.to_string(), "-f", "f32le", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
        let mut bytes = Vec::new();
        child.stdout.take().expect("piped").read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let mut err = String::new();
        let _ = child.stderr.take().expect("piped").read_to_string(&mut err);
        let status = child.wait().map_err(|e| e.to_string())?;
        if !status.success() && bytes.is_empty() {
            return Err(err.lines().last().unwrap_or("ffmpeg couldn't read it").to_owned());
        }
        let samples: Arc<[f32]> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        let peaks = Peaks::of(&samples);
        Ok(Sound { samples, peaks })
    }
}

/// Reads a file for the project: a plain WAV at the device's rate by
/// wave9000 itself (so recording and WAV work without ffmpeg), anything
/// else through ffmpeg.
pub fn load(tools: Option<&Tools>, path: &Path, rate: u32) -> Result<(Media, Sound), String> {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav"))
        && let Some((r, samples)) = crate::wav::read(path)
        && r == rate
    {
        let media = Media { path: path.to_path_buf(), duration: samples.len() as f64 / 2.0 / f64::from(r), rate: r, channels: 0 };
        let peaks = Peaks::of(&samples);
        return Ok((media, Sound { samples: samples.into(), peaks }));
    }
    let tools = tools.ok_or("ffmpeg wasn't found; without it only WAV files at the sound device's rate can be read")?;
    let media = tools.probe(path)?;
    let sound = tools.decode(path, rate)?;
    Ok((media, sound))
}

fn parse_probe(path: &Path, json: &[u8]) -> Result<Media, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let audio = streams.iter().find(|s| s["codec_type"] == "audio").ok_or("no sound in it")?;
    let number = |x: &serde_json::Value| x.as_str().and_then(|s| s.parse::<f64>().ok()).filter(|d| *d > 0.0);
    let duration = number(&v["format"]["duration"]).or_else(|| number(&audio["duration"])).ok_or("no length")?;
    Ok(Media {
        path: path.to_path_buf(),
        duration,
        rate: number(&audio["sample_rate"]).unwrap_or(0.0) as u32,
        channels: audio["channels"].as_u64().unwrap_or(0) as u32,
    })
}

/// The names in `ffmpeg -encoders`: the second word of each line after
/// the "------" that ends its legend.
fn parse_encoders(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| !l.trim_start().starts_with("---"))
        .skip(1)
        .filter_map(|l| l.split_whitespace().nth(1))
        .map(String::from)
        .collect()
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() }
}

fn search(name: String) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin", "/snap/bin"].map(PathBuf::from));
    dirs.into_iter().map(|d| d.join(&name)).find(|p| p.is_file())
}

/// A command whose window (on Windows) never shows.
pub fn quiet(mut cmd: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.stdin(std::process::Stdio::null());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ffprobe_output() {
        let json = br#"{"streams":[{"codec_type":"video"},{"codec_type":"audio","sample_rate":"44100","channels":1}],"format":{"duration":"12.5"}}"#;
        let m = parse_probe(Path::new("/a/b.wav"), json).unwrap();
        assert_eq!((m.rate, m.channels, m.duration), (44100, 1, 12.5));
        assert_eq!(m.name(), "b.wav");
        assert!(parse_probe(Path::new("x"), br#"{"streams":[{"codec_type":"video"}],"format":{"duration":"3"}}"#).is_err());
    }

    #[test]
    fn reads_the_encoder_list() {
        let text = "Encoders:\n A..... = Audio\n ------\n A....D flac                 FLAC\n A....D libmp3lame           MP3\n";
        assert_eq!(parse_encoders(text), ["flac", "libmp3lame"]);
    }

    #[test]
    fn peaks_at_any_span() {
        // 10 000 stereo frames of silence with one loud sample in frame 7000.
        let mut s = vec![0.0f32; 20_000];
        s[14_000] = -0.25;
        let p = Peaks::of(&s);
        assert!(p.levels.len() >= 2);
        assert_eq!(p.max(0, 6000), 0.0);
        assert!((p.max(6990, 7010) - 0.5).abs() < 0.01, "{}", p.max(6990, 7010));
        assert!((p.max(0, 10_000) - 0.5).abs() < 0.01);
        assert_eq!(p.max(5, 5), 0.0);
        assert_eq!(p.max(50_000, 60_000), 0.0);
    }
}
