//! The mixer, and the sound device it plays through.
//!
//! Every file is decoded once into memory at the device's own rate, so
//! playing is only adding numbers: for each track, the clips under the
//! current stretch of time, times the clip's gain, times the track's gain
//! and pan, into the output. Nothing is resampled, decoded or allocated
//! while playing.
//!
//! The interface never shares the project with the device's callback.
//! Whenever the project changes it builds a [`Mix`], a frozen copy in
//! frames with everything precomputed, and hands it over through a slot the
//! callback only ever `try_lock`s, so the callback never waits. Old mixes
//! are freed by the interface, never in the callback. Where playback is
//! (`pos`), and how loud each track was (`meters`), come back as atomics.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::project::{Project, gain};

/// Meters for this many tracks; tracks past it play, without a meter.
pub const METERS: usize = 256;

/// At a clip's cut edges the sound fades over this many frames, so a cut
/// in the middle of a wave doesn't click.
const DECLICK: i64 = 96;

/// A clip, in frames, with its sound.
pub struct Piece {
    pub at: i64,
    pub len: i64,
    /// The first frame of the file it plays.
    pub src: i64,
    pub gain: f32,
    /// Whether it starts or ends inside the file, and so fades there.
    fade_in: bool,
    fade_out: bool,
    pub samples: Arc<[f32]>,
}

/// A track, ready to mix.
pub struct Lane {
    /// Gain into the left and right outputs (the track's gain, pan, mute
    /// and solo, together).
    pub left: f32,
    pub right: f32,
    pub pieces: Vec<Piece>,
    /// The furthest any piece so far reaches: sorted, so the pieces under
    /// any time are found by binary search.
    reach: Vec<i64>,
}

impl Lane {
    pub fn new(left: f32, right: f32, mut pieces: Vec<Piece>) -> Self {
        pieces.sort_by_key(|p| p.at);
        let mut far = i64::MIN;
        let reach = pieces
            .iter()
            .map(|p| {
                far = far.max(p.at + p.len);
                far
            })
            .collect();
        Self { left, right, pieces, reach }
    }

    /// Adds this lane's frames `pos..pos + out.len() / 2` into `out`;
    /// returns the loudest sample it added.
    fn mix_into(&self, pos: i64, out: &mut [f32]) -> f32 {
        if self.left == 0.0 && self.right == 0.0 {
            return 0.0;
        }
        let n = (out.len() / 2) as i64;
        let end = pos + n;
        let mut peak = 0.0f32;
        let first = self.reach.partition_point(|&r| r <= pos);
        for p in self.pieces[first..].iter().take_while(|p| p.at < end) {
            let (a, b) = (p.at.max(pos), (p.at + p.len).min(end));
            let frames = p.samples.len() as i64 / 2;
            for t in a..b {
                let s = p.src + (t - p.at);
                if s < 0 || s >= frames {
                    continue;
                }
                let mut g = p.gain;
                if p.fade_in && t - p.at < DECLICK {
                    g *= (t - p.at) as f32 / DECLICK as f32;
                }
                if p.fade_out && p.at + p.len - t <= DECLICK {
                    g *= (p.at + p.len - t - 1).max(0) as f32 / DECLICK as f32;
                }
                let (l, r) = (p.samples[s as usize * 2] * g * self.left, p.samples[s as usize * 2 + 1] * g * self.right);
                let o = (t - pos) as usize * 2;
                out[o] += l;
                out[o + 1] += r;
                peak = peak.max(l.abs()).max(r.abs());
            }
        }
        peak
    }
}

/// The whole project, frozen for playing or exporting.
pub struct Mix {
    pub lanes: Vec<Lane>,
    pub master: f32,
    /// Frames until the last clip ends.
    pub end: i64,
}

impl Mix {
    /// The project in frames at `rate`; `sound(media)` is a file's decoded
    /// sound, None while it's still being read.
    pub fn of(p: &Project, rate: u32, sound: impl Fn(usize) -> Option<Arc<[f32]>>) -> Self {
        let r = f64::from(rate);
        let frames = |t: f64| (t * r).round() as i64;
        let soloing = p.tracks.iter().any(|t| t.solo);
        let mut end = 0;
        let lanes = p
            .tracks
            .iter()
            .map(|t| {
                let on = !t.mute && (!soloing || t.solo);
                let g = if on { gain(t.gain_db) } else { 0.0 };
                // Constant-power pan, scaled so the middle is unity (exactly,
                // so an untouched track passes through bit for bit) and
                // hard left or right is +3 dB on that side.
                let (left, right) = if t.pan == 0.0 {
                    (g, g)
                } else {
                    let angle = (t.pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
                    (g * angle.cos() * std::f32::consts::SQRT_2, g * angle.sin() * std::f32::consts::SQRT_2)
                };
                let pieces = t
                    .clips
                    .iter()
                    .filter_map(|c| {
                        let samples = sound(c.media)?;
                        let (at, src, len) = (frames(c.at), frames(c.src_in), frames(c.len()));
                        end = end.max(at + len);
                        let fade_in = src > 0;
                        let fade_out = src + len < samples.len() as i64 / 2;
                        Some(Piece { at, len, src, gain: gain(c.gain_db), fade_in, fade_out, samples })
                    })
                    .collect();
                Lane::new(left, right, pieces)
            })
            .collect();
        Self { lanes, master: gain(p.master_db), end }
    }

    pub fn empty() -> Self {
        Self { lanes: Vec::new(), master: 1.0, end: 0 }
    }

    /// Fills `out` (stereo, interleaved) with frames from `pos` on.
    /// `meters` gets each track's loudest sample, and the master's last.
    pub fn render(&self, pos: i64, out: &mut [f32], meters: Option<&Meters>) {
        out.fill(0.0);
        for (i, lane) in self.lanes.iter().enumerate() {
            let peak = lane.mix_into(pos, out);
            if let Some(m) = meters {
                m.hit(i, peak);
            }
        }
        let mut peak = 0.0f32;
        for s in out.iter_mut() {
            *s *= self.master;
            peak = peak.max(s.abs());
        }
        if let Some(m) = meters {
            m.hit(METERS, peak);
        }
    }
}

/// The loudest sample since the interface last looked, per track and for
/// the master.
pub struct Meters {
    peaks: Box<[AtomicU32]>,
}

impl Meters {
    fn new() -> Self {
        Self { peaks: (0..=METERS).map(|_| AtomicU32::new(0)).collect() }
    }

    fn hit(&self, i: usize, peak: f32) {
        if let Some(m) = self.peaks.get(i) {
            // Positive floats sort like their bits.
            m.fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
    }

    /// Track `i`'s loudest sample since the last call (`METERS` is the
    /// master), and starts over.
    pub fn take(&self, i: usize) -> f32 {
        self.peaks.get(i).map_or(0.0, |m| f32::from_bits(m.swap(0, Ordering::Relaxed)))
    }
}

/// What the interface and the device's callback share.
pub struct Shared {
    /// The next frame to play.
    pub pos: AtomicI64,
    pub playing: AtomicBool,
    pending: Mutex<Option<Arc<Mix>>>,
    pub meters: Meters,
    /// Frames the device asked for while playing, for measuring.
    pub played: AtomicU64,
}

pub struct Engine {
    pub rate: u32,
    channels: u16,
    device: Option<cpal::Device>,
    config: Option<cpal::StreamConfig>,
    stream: Option<cpal::Stream>,
    pub shared: Arc<Shared>,
    /// Mixes handed over, kept until the callback lets go of them, so it's
    /// never the one to free them.
    sent: Vec<Arc<Mix>>,
    pub error: Option<String>,
}

impl Engine {
    /// Finds the output (`name`, or the system's default) and its rate.
    /// Nothing is opened until the first play, so an idle workstation
    /// costs nothing.
    pub fn new(name: &str) -> Self {
        let shared = Arc::new(Shared {
            pos: AtomicI64::new(0),
            playing: AtomicBool::new(false),
            pending: Mutex::new(None),
            meters: Meters::new(),
            played: AtomicU64::new(0),
        });
        let mut e = Self { rate: 48_000, channels: 2, device: None, config: None, stream: None, shared, sent: Vec::new(), error: None };
        let host = cpal::default_host();
        let device = if name.trim().is_empty() {
            host.default_output_device()
        } else {
            host.output_devices().ok().and_then(|mut d| d.find(|d| d.description().is_ok_and(|n| n.name() == name.trim()))).or_else(|| {
                crate::log::error(format!("No sound output called \"{}\"; using the default.", name.trim()));
                host.default_output_device()
            })
        };
        match device.ok_or_else(|| "no sound output".to_owned()).and_then(|d| d.default_output_config().map(|c| (d, c)).map_err(|e| e.to_string())) {
            Ok((d, c)) => {
                let config = c.config();
                e.rate = config.sample_rate;
                e.channels = config.channels;
                e.device = Some(d);
                e.config = Some(config);
            }
            Err(err) => e.error = Some(err),
        }
        e
    }

    /// Hands the callback a new mix.
    pub fn set_mix(&mut self, mix: Mix) {
        let mix = Arc::new(mix);
        *self.shared.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(mix.clone());
        self.sent.push(mix);
        // Ones only we hold are done with; the newest is kept for the next
        // play, which may be before the callback first runs.
        let last = self.sent.len() - 1;
        let mut i = 0;
        self.sent.retain(|m| {
            i += 1;
            i - 1 == last || Arc::strong_count(m) > 1
        });
    }

    pub fn pos(&self) -> i64 {
        self.shared.pos.load(Ordering::Acquire)
    }

    pub fn seek(&self, frame: i64) {
        self.shared.pos.store(frame.max(0), Ordering::Release);
    }

    pub fn playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    pub fn play(&mut self) -> Result<(), String> {
        if self.stream.is_none() {
            self.stream = Some(self.open()?);
        }
        self.shared.playing.store(true, Ordering::Release);
        self.stream.as_ref().expect("opened").play().map_err(|e| e.to_string())
    }

    pub fn stop(&mut self) {
        self.shared.playing.store(false, Ordering::Release);
        if let Some(s) = &self.stream {
            let _ = s.pause();
        }
    }

    fn open(&self) -> Result<cpal::Stream, String> {
        let (Some(device), Some(config)) = (&self.device, &self.config) else {
            return Err(self.error.clone().unwrap_or_else(|| "no sound output".into()));
        };
        let s = self.shared.clone();
        let channels = usize::from(self.channels.max(1));
        let mut mix = Arc::new(Mix::empty());
        // Room for the largest buffer devices ask for, so it never grows
        // while playing; a bigger ask is played in pieces.
        let mut stereo = vec![0.0f32; 8192 * 2];
        device
            .build_output_stream::<f32, _, _>(
                *config,
                move |out: &mut [f32], _| {
                    if let Ok(mut slot) = s.pending.try_lock()
                        && let Some(m) = slot.take()
                    {
                        mix = m;
                    }
                    if !s.playing.load(Ordering::Acquire) {
                        out.fill(0.0);
                        return;
                    }
                    let start = s.pos.load(Ordering::Acquire);
                    let mut pos = start;
                    for chunk in out.chunks_mut(channels * 8192) {
                        let frames = chunk.len() / channels;
                        let buf = &mut stereo[..frames * 2];
                        mix.render(pos, buf, Some(&s.meters));
                        for (f, o) in chunk.chunks_mut(channels).enumerate() {
                            let (l, r) = (buf[f * 2].clamp(-1.0, 1.0), buf[f * 2 + 1].clamp(-1.0, 1.0));
                            match o.len() {
                                1 => o[0] = (l + r) * 0.5,
                                _ => {
                                    o[0] = l;
                                    o[1] = r;
                                    o[2..].fill(0.0);
                                }
                            }
                        }
                        pos += frames as i64;
                    }
                    s.played.fetch_add((pos - start) as u64, Ordering::Relaxed);
                    // A seek from the interface meanwhile wins.
                    let _ = s.pos.compare_exchange(start, pos, Ordering::AcqRel, Ordering::Acquire);
                },
                |e| crate::log::error(format!("Sound: {e}")),
                None,
            )
            .map_err(|e| e.to_string())
    }
}

/// The sound outputs and inputs there are, by name.
pub fn devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let name = |d: cpal::Device| d.description().ok().map(|n| n.name().to_owned());
    let outs = host.output_devices().map(|d| d.filter_map(name).collect()).unwrap_or_default();
    let ins = host.input_devices().map(|d| d.filter_map(name).collect()).unwrap_or_default();
    (outs, ins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Clip, Track};

    fn tone(frames: usize, v: f32) -> Arc<[f32]> {
        vec![v; frames * 2].into()
    }

    #[test]
    fn mixes_tracks_with_gain_pan_mute_and_solo() {
        let mut p = Project::default();
        let mut a = Track::new("a");
        a.insert(Clip { media: 0, at: 0.0, src_in: 0.0, src_out: 1.0, gain_db: 0.0 });
        let mut b = Track::new("b");
        b.pan = 1.0;
        b.insert(Clip { media: 1, at: 0.5, src_in: 0.0, src_out: 1.0, gain_db: -6.0 });
        p.tracks = vec![a, b];
        let sounds = [tone(100, 0.5), tone(100, 0.4)];
        let mix = Mix::of(&p, 100, |m| Some(sounds[m].clone()));
        assert_eq!(mix.end, 150);
        let mut out = vec![0.0; 200];
        mix.render(0, &mut out, None);
        // Track a alone at the start, centered (constant power: 1 each side).
        assert!((out[0] - 0.5).abs() < 1e-4 && (out[1] - 0.5).abs() < 1e-4, "{:?}", &out[..2]);
        // From 0.5 s track b joins, panned hard right, 6 dB down, √2 louder
        // on the side it's panned to.
        let f = 60 * 2;
        assert!((out[f] - 0.5).abs() < 1e-4);
        let b_right = 0.4 * gain(-6.0) * std::f32::consts::SQRT_2;
        assert!((out[f + 1] - (0.5 + b_right)).abs() < 1e-3, "{}", out[f + 1]);

        p.tracks[1].solo = true;
        let mix = Mix::of(&p, 100, |m| Some(sounds[m].clone()));
        mix.render(0, &mut out, None);
        assert_eq!(out[0], 0.0);
        p.tracks[1].mute = true;
        let mix = Mix::of(&p, 100, |m| Some(sounds[m].clone()));
        mix.render(0, &mut out, None);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn cuts_fade_and_files_still_reading_are_skipped() {
        let mut p = Project::default();
        let mut a = Track::new("a");
        // From the middle of a 1000-frame file: fades in at its start.
        a.insert(Clip { media: 0, at: 1.0, src_in: 2.0, src_out: 5.0, gain_db: 0.0 });
        a.insert(Clip { media: 1, at: 0.0, src_in: 0.0, src_out: 1.0, gain_db: 0.0 });
        p.tracks = vec![a];
        let s = tone(1000, 1.0);
        let mix = Mix::of(&p, 100, |m| (m == 0).then(|| s.clone()));
        assert_eq!(mix.lanes[0].pieces.len(), 1);
        let mut out = vec![0.0; 2 * 400];
        mix.render(0, &mut out, None);
        assert_eq!(out[2 * 99], 0.0);
        assert_eq!(out[2 * 100], 0.0, "starts from silence");
        assert!(out[2 * 150] > 0.0 && out[2 * 150] < 1.0, "fading in");
        assert!((out[2 * 299] - 1.0).abs() < 1e-4, "{}", out[2 * 299]);
    }

    #[test]
    fn meters_keep_the_loudest() {
        let m = Meters::new();
        m.hit(3, 0.25);
        m.hit(3, 0.75);
        m.hit(3, 0.5);
        assert_eq!(m.take(3), 0.75);
        assert_eq!(m.take(3), 0.0);
        m.hit(METERS + 5, 1.0);
    }
}

/// How fast the mixer runs: 16 tracks of 3-minute clips cut every 5 s,
/// panned and with gain, mixed into stereo at 48 kHz.
/// `cargo test --release mix_speed -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn mix_speed() {
    use crate::project::{Clip, Track};
    let rate = 48_000;
    let secs = 180.0;
    let s: Arc<[f32]> = (0..(secs as usize * rate * 2)).map(|i| ((i / 2) as f32 * 0.01).sin() * 0.1).collect::<Vec<_>>().into();
    let mut p = Project::default();
    for k in 0..16 {
        let mut t = Track::new(format!("{k}"));
        t.pan = (k as f32 / 8.0) - 1.0;
        t.gain_db = -6.0;
        let mut at = 0.0;
        while at < secs {
            t.insert(Clip { media: 0, at, src_in: at, src_out: (at + 5.0).min(secs), gain_db: -1.0 });
            at += 5.0;
        }
        p.tracks.push(t);
    }
    let mix = Mix::of(&p, rate as u32, |_| Some(s.clone()));
    let mut buf = vec![0.0f32; 512 * 2];
    let started = std::time::Instant::now();
    let mut pos = 0;
    while pos < mix.end {
        mix.render(pos, &mut buf, None);
        pos += 512;
    }
    let took = started.elapsed().as_secs_f64();
    println!("16 tracks x {secs} s in {took:.3} s: {:.0}x real time, {:.1}% of one core while playing", secs / took, took / secs * 100.0);
}
