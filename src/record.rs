//! Recording from a sound input into a WAV file.
//!
//! The input's callback only copies samples into a lock-free ring; a
//! writer thread empties it into the file, and keeps the waveform and level
//! the interface draws while recording. The take is recorded at the input's
//! own rate and channel count (the first two channels at most), then read
//! like any other file, so wave9000 never resamples it by itself.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

use crate::engine::Trouble;
use crate::media::{BLOCK, level};
use crate::wav::{Depth, Writer};

/// One producer, one consumer, no locks: the device callback must never wait.
pub struct Ring {
    buf: Box<[AtomicU32]>,
    /// Samples ever written and read; the difference is what's waiting.
    written: AtomicUsize,
    read: AtomicUsize,
}

impl Ring {
    pub fn new(len: usize) -> Self {
        Self { buf: (0..len).map(|_| AtomicU32::new(0)).collect(), written: AtomicUsize::new(0), read: AtomicUsize::new(0) }
    }

    /// Writes as much of `samples` as fits; returns how much did.
    pub fn push(&self, samples: impl ExactSizeIterator<Item = f32>) -> usize {
        let w = self.written.load(Ordering::Relaxed);
        let free = self.buf.len() - (w - self.read.load(Ordering::Acquire));
        let n = samples.len().min(free);
        for (i, s) in samples.take(n).enumerate() {
            self.buf[(w + i) % self.buf.len()].store(s.to_bits(), Ordering::Relaxed);
        }
        self.written.store(w + n, Ordering::Release);
        n
    }

    /// Fills `out` from the ring; returns how many samples were there.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let r = self.read.load(Ordering::Relaxed);
        let n = out.len().min(self.written.load(Ordering::Acquire) - r);
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = f32::from_bits(self.buf[(r + i) % self.buf.len()].load(Ordering::Relaxed));
        }
        self.read.store(r + n, Ordering::Release);
        n
    }
}

/// What the writer shares with the interface while recording.
pub struct Live {
    /// Frames written so far.
    pub frames: AtomicU64,
    /// The waveform so far: one value per `BLOCK` frames, as `Peaks` keeps.
    pub peaks: Mutex<Vec<u8>>,
    /// The loudest sample since the interface last looked.
    pub level: AtomicU32,
    /// Samples the ring had no room for (the disk fell behind).
    pub dropped: AtomicU64,
    /// Whether any sample so far wasn't exactly zero. A real input always
    /// hisses a little; macOS sends pure silence when the microphone isn't
    /// allowed.
    pub heard: AtomicBool,
    /// What the input reported (see `engine::Trouble`).
    pub trouble: Trouble,
}

pub struct Recording {
    _stream: cpal::Stream,
    stop: Arc<AtomicBool>,
    writer: JoinHandle<Result<(), String>>,
    pub live: Arc<Live>,
    pub rate: u32,
    pub path: PathBuf,
    /// Where on the project it began, and on which track.
    pub at: f64,
    pub track: usize,
    pub started: std::time::Instant,
}

impl Recording {
    /// Starts recording from input `name` (or the system's default) into
    /// `path`.
    pub fn start(name: &str, path: PathBuf, at: f64, track: usize, repaint: impl Fn() + Send + 'static) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = if name.trim().is_empty() {
            host.default_input_device()
        } else {
            host.input_devices().ok().and_then(|mut d| d.find(|d| d.description().is_ok_and(|n| n.name() == name.trim())))
        }
        .ok_or("no sound input (is a microphone connected, and is wave9000 allowed to use it?)")?;
        let supported = device.default_input_config().map_err(|e| e.to_string())?;
        let config = supported.config();
        let (rate, channels) = (config.sample_rate, usize::from(config.channels.max(1)));
        let keep = channels.min(2);
        let ring = Arc::new(Ring::new(rate as usize * keep * 2));
        let live = Arc::new(Live {
            frames: AtomicU64::new(0),
            peaks: Mutex::new(Vec::new()),
            level: AtomicU32::new(0),
            dropped: AtomicU64::new(0),
            heard: AtomicBool::new(false),
            trouble: Trouble::default(),
        });
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => input::<f32>(&device, &config, channels, keep, ring.clone(), live.clone()),
            cpal::SampleFormat::I16 => input::<i16>(&device, &config, channels, keep, ring.clone(), live.clone()),
            cpal::SampleFormat::I32 => input::<i32>(&device, &config, channels, keep, ring.clone(), live.clone()),
            cpal::SampleFormat::U16 => input::<u16>(&device, &config, channels, keep, ring.clone(), live.clone()),
            other => Err(format!("the input gives {other} samples, which wave9000 doesn't read yet")),
        }?;
        let mut file = Writer::create(&path, rate, keep as u16, Depth::F32).map_err(|e| format!("{}: {e}", path.display()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (stop, live) = (stop.clone(), live.clone());
            std::thread::spawn(move || {
                let mut buf = vec![0.0f32; 8192 * keep];
                // The block being measured: its loudest sample, and frames.
                let (mut peak, mut in_block) = (0.0f32, 0);
                loop {
                    let n = ring.pop(&mut buf);
                    if n == 0 {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    file.write(&buf[..n]).map_err(|e| e.to_string())?;
                    if buf[..n].iter().any(|&s| s != 0.0) {
                        live.heard.store(true, Ordering::Relaxed);
                    }
                    let mut fresh = Vec::new();
                    for frame in buf[..n].chunks(keep) {
                        peak = frame.iter().fold(peak, |m, s| m.max(s.abs()));
                        in_block += 1;
                        if in_block == BLOCK {
                            fresh.push(level(peak));
                            live.level.fetch_max(peak.to_bits(), Ordering::Relaxed);
                            (peak, in_block) = (0.0, 0);
                        }
                    }
                    live.peaks.lock().unwrap_or_else(|e| e.into_inner()).extend(fresh);
                    live.frames.store(file.frames(), Ordering::Release);
                    repaint();
                }
                file.finish().map_err(|e| e.to_string())
            })
        };
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self { _stream: stream, stop, writer, live, rate, path, at, track, started: std::time::Instant::now() })
    }

    pub fn seconds(&self) -> f64 {
        self.live.frames.load(Ordering::Acquire) as f64 / f64::from(self.rate)
    }

    /// Stops, and finishes the file.
    pub fn finish(self) -> Result<PathBuf, String> {
        drop(self._stream);
        self.stop.store(true, Ordering::Release);
        self.writer.join().map_err(|_| "the recording stopped unexpectedly".to_owned())??;
        let dropped = self.live.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            crate::log::error(format!("The disk fell behind while recording: {dropped} samples were lost."));
        }
        let (_, _, dropouts) = self.live.trouble.take();
        if dropouts > 0 {
            crate::log::error(format!("The input reported {} while recording.", crate::dropouts_text(dropouts)));
        }
        Ok(self.path)
    }
}

fn input<T: SizedSample + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    keep: usize,
    ring: Arc<Ring>,
    live: Arc<Live>,
) -> Result<cpal::Stream, String>
where
    f32: FromSample<T>,
{
    let errors = live.clone();
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], _| {
                let frames = data.len() / channels;
                let samples = (0..frames * keep).map(|i| data[i / keep * channels + i % keep].to_sample::<f32>());
                let n = ring.push(samples);
                if n < frames * keep {
                    live.dropped.fetch_add((frames * keep - n) as u64, Ordering::Relaxed);
                }
            },
            // Can run on the sound thread: only counted, never logged here.
            move |e| errors.trouble.report(e.kind()),
            None,
        )
        .map_err(|e| e.to_string())
}

/// A free name for the next take in `dir`: "Take 1.wav", "Take 2.wav"...
pub fn next_take(dir: &Path) -> PathBuf {
    (1..).map(|n| dir.join(format!("Take {n}.wav"))).find(|p| !p.exists()).expect("some number is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_wraps_and_fills_to_capacity() {
        let r = Ring::new(4);
        assert_eq!(r.push([1.0, 2.0, 3.0].into_iter()), 3);
        let mut out = [0.0; 2];
        assert_eq!(r.pop(&mut out), 2);
        assert_eq!(out, [1.0, 2.0]);
        assert_eq!(r.push([4.0, 5.0, 6.0, 7.0].into_iter()), 3);
        let mut out = [9.0; 5];
        assert_eq!(r.pop(&mut out), 4);
        assert_eq!(&out[..4], &[3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn takes_get_free_names() {
        let dir = std::env::temp_dir().join(format!("wave9000-takes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Take 1.wav"), b"").unwrap();
        assert_eq!(next_take(&dir), dir.join("Take 2.wav"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
