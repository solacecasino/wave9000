//! WAV files, written by wave9000 itself: recordings, and exports that need
//! no encoder. The sizes in the header are filled in when the file is
//! finished, so a file is written straight through without knowing its
//! length first.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    I16,
    I24,
    F32,
}

impl Depth {
    /// settings.toml's `bits`: 16, 24 or 32 (float).
    pub fn from_bits(bits: u32) -> Self {
        match bits {
            16 => Self::I16,
            32 => Self::F32,
            _ => Self::I24,
        }
    }

    fn bytes(self) -> u16 {
        match self {
            Self::I16 => 2,
            Self::I24 => 3,
            Self::F32 => 4,
        }
    }
}

pub struct Writer {
    file: BufWriter<File>,
    channels: u16,
    depth: Depth,
    /// Bytes of sound written.
    data: u64,
}

impl Writer {
    pub fn create(path: &Path, rate: u32, channels: u16, depth: Depth) -> std::io::Result<Self> {
        let mut w = Self { file: BufWriter::new(File::create(path)?), channels, depth, data: 0 };
        let block = channels * depth.bytes();
        let format: u16 = if depth == Depth::F32 { 3 } else { 1 };
        let f = &mut w.file;
        f.write_all(b"RIFF\0\0\0\0WAVEfmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&format.to_le_bytes())?;
        f.write_all(&channels.to_le_bytes())?;
        f.write_all(&rate.to_le_bytes())?;
        f.write_all(&(rate * u32::from(block)).to_le_bytes())?;
        f.write_all(&block.to_le_bytes())?;
        f.write_all(&(depth.bytes() * 8).to_le_bytes())?;
        f.write_all(b"data\0\0\0\0")?;
        Ok(w)
    }

    /// Writes interleaved samples, `channels` to a frame. Whole-number
    /// formats are clipped at full scale.
    pub fn write(&mut self, samples: &[f32]) -> std::io::Result<()> {
        for &s in samples {
            match self.depth {
                Depth::F32 => self.file.write_all(&s.to_le_bytes())?,
                Depth::I16 => self.file.write_all(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())?,
                Depth::I24 => {
                    let v = (s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                    self.file.write_all(&v.to_le_bytes()[..3])?;
                }
            }
        }
        self.data += samples.len() as u64 * u64::from(self.depth.bytes());
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.data / u64::from(self.channels * self.depth.bytes())
    }

    /// Fills in the sizes. WAV can't hold more than 4 GB.
    pub fn finish(mut self) -> std::io::Result<()> {
        let data = u32::try_from(self.data).map_err(|_| std::io::Error::other("longer than a WAV file can hold (4 GB)"))?;
        let f = &mut self.file;
        f.seek(SeekFrom::Start(4))?;
        f.write_all(&(36 + data).to_le_bytes())?;
        f.seek(SeekFrom::Start(40))?;
        f.write_all(&data.to_le_bytes())?;
        f.flush()
    }
}

/// A plain WAV file's sound as stereo frames: 16, 24 or 32-bit whole
/// numbers, or 32-bit float, one or two channels (mono goes to both
/// sides). Returns the rate too. None for anything else, which ffmpeg
/// reads instead. This is what lets takes come back without ffmpeg.
pub fn read(path: &Path) -> Option<(u32, Vec<f32>)> {
    let b = std::fs::read(path).ok()?;
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let (mut fmt, mut data) = (None, None);
    let mut at = 12;
    while at + 8 <= b.len() {
        let size = u32::from_le_bytes(b[at + 4..at + 8].try_into().ok()?) as usize;
        let body = at + 8..(at + 8 + size).min(b.len());
        match &b[at..at + 4] {
            b"fmt " => fmt = Some(body),
            b"data" => data = Some(body),
            _ => {}
        }
        at += 8 + size + size % 2;
    }
    let f = &b[fmt?];
    if f.len() < 16 {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([f[i], f[i + 1]]);
    let (format, channels, bits) = (u16_at(0), usize::from(u16_at(2)), u16_at(14));
    let rate = u32::from_le_bytes(f[4..8].try_into().ok()?);
    // WAVE_FORMAT_EXTENSIBLE keeps the real format in its sub-format.
    let format = if format == 0xfffe && f.len() >= 26 { u16_at(24) } else { format };
    let width = usize::from(bits / 8);
    if !(1..=2).contains(&channels) || rate == 0 {
        return None;
    }
    let sample: fn(&[u8]) -> f32 = match (format, bits) {
        (1, 16) => |s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
        (1, 24) => |s| (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
        (1, 32) => |s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
        (3, 32) => |s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        _ => return None,
    };
    let d = &b[data?];
    let frames = d.len() / (width * channels);
    let mut out = Vec::with_capacity(frames * 2);
    for frame in d.chunks_exact(width * channels).take(frames) {
        let l = sample(&frame[..width]);
        let r = if channels == 2 { sample(&frame[width..]) } else { l };
        out.extend([l, r]);
    }
    Some((rate, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_what_it_writes() {
        for (depth, channels) in [(Depth::I16, 1), (Depth::I24, 2), (Depth::F32, 1)] {
            let path = std::env::temp_dir().join(format!("wave9000-read-{}-{depth:?}.wav", std::process::id()));
            let mut w = Writer::create(&path, 44_100, channels, depth).unwrap();
            let samples: Vec<f32> = (0..channels as usize * 4).map(|i| i as f32 * 0.1 - 0.3).collect();
            w.write(&samples).unwrap();
            w.finish().unwrap();
            let (rate, got) = read(&path).unwrap();
            let _ = std::fs::remove_file(&path);
            assert_eq!((rate, got.len()), (44_100, 8));
            let want = if channels == 1 { samples.iter().flat_map(|&s| [s, s]).collect() } else { samples.clone() };
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 1e-3, "{depth:?}: {g} vs {w}");
            }
        }
    }

    #[test]
    fn writes_a_readable_header() {
        let path = std::env::temp_dir().join(format!("wave9000-wav-{}.wav", std::process::id()));
        let mut w = Writer::create(&path, 48_000, 2, Depth::I24).unwrap();
        w.write(&[0.0, 1.0, -1.0, 2.0]).unwrap();
        assert_eq!(w.frames(), 2);
        w.finish().unwrap();
        let b = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(b.len(), 44 + 12);
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 36 + 12);
        assert_eq!(u16::from_le_bytes(b[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u16::from_le_bytes(b[34..36].try_into().unwrap()), 24);
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 12);
        // Full scale, and clipped past it.
        assert_eq!(&b[47..50], &[0xff, 0xff, 0x7f]);
        assert_eq!(&b[53..56], &[0xff, 0xff, 0x7f]);
        assert_eq!(&b[50..53], &[0x01, 0x00, 0x80]);
    }
}
