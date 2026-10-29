//! Exporting the mix: the same mixer that plays it, run as fast as it
//! goes, into a WAV file written by wave9000 itself, or into ffmpeg's input
//! for any other format.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::adapters::ExportSpec;
use crate::engine::Mix;
use crate::media::{Tools, quiet};
use crate::settings::Export;
use crate::wav::{Depth, Writer};

/// How the mix becomes a file.
#[derive(Clone, Debug, PartialEq)]
pub enum How {
    Wav(Depth),
    /// ffmpeg reads the mix from its input; these go between that and the
    /// output file.
    Ffmpeg(Vec<String>),
}

/// The formats wave9000 knows by their extension.
pub const EXTENSIONS: &[(&str, &str)] = &[("wav", "WAV"), ("flac", "FLAC"), ("mp3", "MP3"), ("m4a", "AAC"), ("ogg", "Ogg Vorbis"), ("opus", "Opus")];

/// How to write a file ending in `ext`, as settings.toml says, or as an
/// adapter's format says. None: wave9000 doesn't know the extension.
pub fn how(ext: &str, s: &Export, spec: Option<&ExportSpec>) -> Option<How> {
    if let Some(spec) = spec {
        let mut args = Vec::new();
        if !spec.audio_filter.is_empty() {
            args.extend(["-af".into(), spec.audio_filter.clone()]);
        }
        match &spec.audio {
            Some(a) => args.extend(a.iter().cloned()),
            None if spec.extension == "wav" && spec.audio_filter.is_empty() => return Some(How::Wav(Depth::from_bits(s.bits))),
            None => args.extend(how(&spec.extension, s, None).map(|h| match h {
                How::Ffmpeg(a) => a,
                How::Wav(d) => vec!["-c:a".into(), pcm(d).into()],
            })?),
        }
        return Some(How::Ffmpeg(args));
    }
    let codec = |list: &[&str]| -> Vec<String> { list.iter().map(|s| (*s).to_owned()).collect() };
    let mut args = match ext.to_lowercase().as_str() {
        "wav" if s.extra_args.is_empty() => return Some(How::Wav(Depth::from_bits(s.bits))),
        "wav" => codec(&["-c:a", pcm(Depth::from_bits(s.bits))]),
        "flac" => codec(&["-c:a", "flac", "-sample_fmt", if s.bits == 16 { "s16" } else { "s32" }]),
        "mp3" => codec(&["-c:a", "libmp3lame", "-b:a", &s.mp3_bitrate]),
        "m4a" | "aac" => codec(&["-c:a", "aac", "-b:a", &s.aac_bitrate]),
        "ogg" => codec(&["-c:a", "libvorbis", "-q:a", "6"]),
        "opus" => codec(&["-c:a", "libopus", "-b:a", "192k"]),
        _ => return None,
    };
    args.extend(s.extra_args.iter().cloned());
    Some(How::Ffmpeg(args))
}

fn pcm(d: Depth) -> &'static str {
    match d {
        Depth::I16 => "pcm_s16le",
        Depth::I24 => "pcm_s24le",
        Depth::F32 => "pcm_f32le",
    }
}

/// The encoder `args` name with -c:a.
pub fn encoder(how: &How) -> Option<&str> {
    let How::Ffmpeg(args) = how else { return None };
    args.iter().position(|a| a == "-c:a").and_then(|i| args.get(i + 1)).map(String::as_str)
}

#[derive(Default)]
pub struct Progress {
    /// Seconds of the mix written.
    pub done: f64,
    pub finished: Option<Result<String, String>>,
}

pub struct Job {
    pub progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    pub out: PathBuf,
    /// Seconds to write.
    pub total: f64,
    pub started: Instant,
}

impl Job {
    pub fn start(mix: Arc<Mix>, rate: u32, tools: Option<Tools>, how: How, out: PathBuf, repaint: impl Fn() + Send + 'static) -> Self {
        let progress = Arc::new(Mutex::new(Progress::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let total = mix.end as f64 / f64::from(rate);
        {
            let (progress, cancel, out) = (progress.clone(), cancel.clone(), out.clone());
            std::thread::spawn(move || {
                let started = Instant::now();
                let report = |done: f64| {
                    progress.lock().unwrap_or_else(|e| e.into_inner()).done = done;
                    repaint();
                };
                let r = write(&mix, rate, tools.as_ref(), &how, &out, &cancel, &report).map(|()| {
                    let secs = started.elapsed().as_secs_f64();
                    format!("{:.1} s in {:.1} s, {:.0}× real time", total, secs, total / secs.max(1e-3))
                });
                if r.is_err() {
                    let _ = std::fs::remove_file(&out);
                }
                progress.lock().unwrap_or_else(|e| e.into_inner()).finished = Some(r);
                repaint();
            });
        }
        Self { progress, cancel, out, total, started: Instant::now() }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The mix in pieces, as fast as it renders.
fn write(mix: &Mix, rate: u32, tools: Option<&Tools>, how: &How, out: &Path, cancel: &AtomicBool, report: &dyn Fn(f64)) -> Result<(), String> {
    const CHUNK: i64 = 16_384;
    let mut buf = vec![0.0f32; CHUNK as usize * 2];
    let mut each = |sink: &mut dyn FnMut(&[f32]) -> Result<(), String>| -> Result<(), String> {
        let mut pos = 0;
        while pos < mix.end {
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled".into());
            }
            let n = CHUNK.min(mix.end - pos);
            let b = &mut buf[..n as usize * 2];
            mix.render(pos, b, None);
            sink(b)?;
            pos += n;
            report(pos as f64 / f64::from(rate));
        }
        Ok(())
    };
    match how {
        How::Wav(depth) => {
            let mut w = Writer::create(out, rate, 2, *depth).map_err(|e| e.to_string())?;
            each(&mut |b| w.write(b).map_err(|e| e.to_string()))?;
            w.finish().map_err(|e| e.to_string())
        }
        How::Ffmpeg(args) => {
            let tools = tools.ok_or("this format needs ffmpeg, which wasn't found")?;
            let mut child = quiet(Command::new(&tools.ffmpeg))
                .args(["-y", "-v", "error", "-f", "f32le", "-ar", &rate.to_string(), "-ac", "2", "-i", "-"])
                .args(args)
                .arg(out)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
            let mut stdin = child.stdin.take().expect("piped");
            let mut bytes = Vec::with_capacity(CHUNK as usize * 8);
            let r = each(&mut |b| {
                bytes.clear();
                bytes.extend(b.iter().flat_map(|s| s.to_le_bytes()));
                stdin.write_all(&bytes).map_err(|e| e.to_string())
            });
            drop(stdin);
            let mut err = String::new();
            let _ = child.stderr.take().expect("piped").read_to_string(&mut err);
            let status = child.wait().map_err(|e| e.to_string())?;
            if r.as_ref().is_err_and(|e| e == "Cancelled") {
                return r;
            }
            if !status.success() {
                return Err(err.lines().last().unwrap_or("ffmpeg failed").to_owned());
            }
            r
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_by_extension() {
        let s = Export::default();
        assert_eq!(how("WAV", &s, None), Some(How::Wav(Depth::I24)));
        assert_eq!(encoder(&how("mp3", &s, None).unwrap()), Some("libmp3lame"));
        assert_eq!(encoder(&how("flac", &s, None).unwrap()), Some("flac"));
        assert_eq!(how("xyz", &s, None), None);
        let spec = ExportSpec { id: "l".into(), label: "L".into(), extension: "wav".into(), audio_filter: "loudnorm".into(), ..Default::default() };
        let How::Ffmpeg(a) = how("wav", &s, Some(&spec)).unwrap() else { panic!() };
        assert_eq!(a, ["-af", "loudnorm", "-c:a", "pcm_s24le"]);
        let spec = ExportSpec { id: "w".into(), label: "W".into(), extension: "wav".into(), ..Default::default() };
        assert_eq!(how("wav", &s, Some(&spec)), Some(How::Wav(Depth::I24)));
    }

    /// Through the real ffmpeg, when there is one (WAVE9000_REQUIRE_FFMPEG=1
    /// makes its absence a failure, for CI), and read back by ffprobe.
    #[test]
    fn exports_through_ffmpeg() {
        use crate::project::{Clip, Project, Track};
        let Some(tools) = Tools::find("") else {
            assert!(std::env::var_os("WAVE9000_REQUIRE_FFMPEG").is_none(), "ffmpeg wasn't found");
            return;
        };
        let mut p = Project::default();
        let mut t = Track::new("a");
        t.insert(Clip { media: 0, at: 0.25, src_in: 0.0, src_out: 1.0, gain_db: -6.0 });
        p.tracks.push(t);
        let s: Arc<[f32]> = (0..96_000).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.5).collect::<Vec<_>>().into();
        let mix = Arc::new(Mix::of(&p, 48_000, |_| Some(s.clone())));
        for ext in ["flac", "mp3", "m4a"] {
            let how = how(ext, &Export::default(), None).unwrap();
            let out = std::env::temp_dir().join(format!("wave9000-export-{}.{ext}", std::process::id()));
            let job = Job::start(mix.clone(), 48_000, Some(tools.clone()), how, out.clone(), || {});
            let r = loop {
                if let Some(r) = job.progress.lock().unwrap().finished.take() {
                    break r;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            assert!(r.is_ok(), "{ext}: {r:?}");
            let m = tools.probe(&out).unwrap();
            let _ = std::fs::remove_file(&out);
            assert!((m.duration - 1.25).abs() < 0.06, "{ext}: {}", m.duration);
            assert_eq!(m.channels, 2, "{ext}");
        }
    }

    #[test]
    fn exports_a_wav_without_ffmpeg() {
        use crate::project::{Clip, Project, Track};
        let mut p = Project::default();
        let mut t = Track::new("a");
        t.insert(Clip { media: 0, at: 0.5, src_in: 0.0, src_out: 1.0, gain_db: 0.0 });
        p.tracks.push(t);
        let s: Arc<[f32]> = vec![0.25f32; 2000].into();
        let mix = Arc::new(Mix::of(&p, 1000, |_| Some(s.clone())));
        let out = std::env::temp_dir().join(format!("wave9000-export-{}.wav", std::process::id()));
        let job = Job::start(mix, 1000, None, How::Wav(Depth::F32), out.clone(), || {});
        let r = loop {
            if let Some(r) = job.progress.lock().unwrap().finished.take() {
                break r;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(r.is_ok(), "{r:?}");
        let b = std::fs::read(&out).unwrap();
        let _ = std::fs::remove_file(&out);
        // Half a second of silence, then the clip: 1500 frames of 2 floats.
        assert_eq!(b.len(), 44 + 1500 * 8);
        assert_eq!(f32::from_le_bytes(b[44..48].try_into().unwrap()), 0.0);
        assert_eq!(f32::from_le_bytes(b[44 + 1000 * 8..44 + 1000 * 8 + 4].try_into().unwrap()), 0.25);
    }
}
