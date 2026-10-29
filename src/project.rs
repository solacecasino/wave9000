//! The project: media files, and tracks of clips placed on them.
//!
//! Times are seconds (`f64`). Each track keeps its clips sorted by where
//! they start, so finding what's under a time or in view is a binary search
//! (see `Track::first_reaching`), and the mixer does the same in frames.

use serde::{Deserialize, Serialize};

use crate::media::Media;

/// A piece of a media file placed on a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub media: usize,
    /// Where it starts on the track.
    pub at: f64,
    /// The part of the file it plays.
    pub src_in: f64,
    pub src_out: f64,
    /// Its own volume, in dB.
    #[serde(default)]
    pub gain_db: f32,
}

impl Clip {
    pub fn len(&self) -> f64 {
        self.src_out - self.src_in
    }

    pub fn end(&self) -> f64 {
        self.at + self.len()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub gain_db: f32,
    /// -1 is left, 1 is right.
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    /// Recording goes here.
    #[serde(default)]
    pub armed: bool,
    /// Sorted by `at`.
    #[serde(default)]
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), gain_db: 0.0, pan: 0.0, mute: false, solo: false, armed: false, clips: Vec::new() }
    }

    /// Puts the clips back in order after one moved; returns where clip
    /// `i` went.
    pub fn sort(&mut self, i: usize) -> usize {
        let moved = self.clips.get(i).cloned();
        self.clips.sort_by(|a, b| a.at.total_cmp(&b.at));
        moved.and_then(|m| self.clips.iter().position(|c| *c == m)).unwrap_or(0)
    }

    /// Adds a clip in order; returns its index.
    pub fn insert(&mut self, clip: Clip) -> usize {
        let i = self.clips.partition_point(|c| c.at <= clip.at);
        self.clips.insert(i, clip);
        i
    }

    /// The first clip whose furthest-so-far end is after `t` (clips can
    /// overlap, so ends alone aren't sorted, but the running furthest end
    /// is). Clips from here on that start before a time are the ones under
    /// it. The interface asks this a few times a frame of a track's few
    /// clips, so it walks them; the mixer, which asks it constantly, keeps
    /// the running ends and binary-searches them (`engine::Lane`).
    pub fn first_reaching(&self, t: f64) -> usize {
        let mut reach = f64::NEG_INFINITY;
        self.clips
            .iter()
            .position(|c| {
                reach = reach.max(c.end());
                reach > t
            })
            .unwrap_or(self.clips.len())
    }

    /// The clip under time `t`: the last one to start there, which is the
    /// one drawn on top.
    pub fn clip_at(&self, t: f64) -> Option<usize> {
        let from = self.first_reaching(t);
        (from..self.clips.len()).take_while(|&i| self.clips[i].at <= t).filter(|&i| self.clips[i].end() > t).last()
    }

    /// Cuts clip `i` in two at `t`. False if `t` isn't inside it by more
    /// than `min` either side.
    pub fn split(&mut self, i: usize, t: f64, min: f64) -> bool {
        let Some(c) = self.clips.get(i).cloned() else { return false };
        if t - c.at <= min || c.end() - t <= min {
            return false;
        }
        let cut = c.src_in + (t - c.at);
        self.clips[i].src_out = cut;
        self.clips.insert(i + 1, Clip { at: t, src_in: cut, ..c });
        true
    }

    pub fn end(&self) -> f64 {
        self.clips.iter().map(Clip::end).fold(0.0, f64::max)
    }
}

/// Every track's clip edges, sorted: where ↑ and ↓ go.
pub fn cuts(tracks: &[Track]) -> Vec<f64> {
    let mut v: Vec<f64> = tracks.iter().flat_map(|t| t.clips.iter().flat_map(|c| [c.at, c.end()])).collect();
    v.push(0.0);
    v.sort_by(f64::total_cmp);
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    v
}

pub fn duration(tracks: &[Track]) -> f64 {
    tracks.iter().map(Track::end).fold(0.0, f64::max)
}

/// dB as a gain: 0 dB is 1, and anything at or below -90 dB is silence.
pub fn gain(db: f32) -> f32 {
    if db <= -90.0 { 0.0 } else { 10f32.powf(db / 20.0) }
}

/// A gain as dB, for meters: silence is -inf.
pub fn db(gain: f32) -> f32 {
    20.0 * gain.max(1e-9).log10()
}

/// A project file (.wave), and the session wave9000 reopens.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Project {
    /// Which format this is; files written by newer versions still open.
    #[serde(default)]
    pub wave9000: u32,
    #[serde(default)]
    pub media: Vec<Media>,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub master_db: f32,
    #[serde(default)]
    pub playhead: f64,
    /// Session only: the project file it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<std::path::PathBuf>,
}

/// A splice9000 project's clips, as wave9000 reads them: played one after
/// another, each from `src_in` to `src_out` of its media.
#[derive(Deserialize)]
pub struct SpliceProject {
    #[serde(default)]
    pub media: Vec<SpliceMedia>,
    #[serde(default)]
    pub clips: Vec<SpliceClip>,
}

#[derive(Deserialize)]
pub struct SpliceMedia {
    pub path: std::path::PathBuf,
    #[serde(default)]
    pub audio: bool,
}

#[derive(Deserialize)]
pub struct SpliceClip {
    pub media: usize,
    pub src_in: f64,
    pub src_out: f64,
}

impl SpliceProject {
    /// Where each clip with sound plays in the edit: (media path, at,
    /// src_in, src_out).
    pub fn placed(&self) -> Vec<(std::path::PathBuf, f64, f64, f64)> {
        let mut at = 0.0;
        let mut out = Vec::new();
        for c in &self.clips {
            let len = (c.src_out - c.src_in).max(0.0);
            if let Some(m) = self.media.get(c.media).filter(|m| m.audio && len > 0.0) {
                out.push((m.path.clone(), at, c.src_in, c.src_out));
            }
            at += len;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(at: f64, len: f64) -> Clip {
        Clip { media: 0, at, src_in: 1.0, src_out: 1.0 + len, gain_db: 0.0 }
    }

    #[test]
    fn finds_clips_even_when_they_overlap() {
        let mut t = Track::new("a");
        for c in [clip(0.0, 10.0), clip(2.0, 1.0), clip(4.0, 1.0), clip(20.0, 5.0)] {
            t.insert(c);
        }
        // The long first clip reaches past the short ones.
        assert_eq!(t.first_reaching(6.0), 0);
        assert_eq!(t.first_reaching(12.0), 3);
        assert_eq!(t.clip_at(2.5), Some(1));
        assert_eq!(t.clip_at(3.5), Some(0));
        assert_eq!(t.clip_at(15.0), None);
        assert_eq!(t.clip_at(24.9), Some(3));
        assert_eq!(t.end(), 25.0);
    }

    #[test]
    fn splits_and_keeps_order() {
        let mut t = Track::new("a");
        t.insert(clip(5.0, 4.0));
        assert!(!t.split(0, 5.0, 0.01));
        assert!(t.split(0, 6.5, 0.01));
        assert_eq!(t.clips[0].src_out, 2.5);
        assert_eq!((t.clips[1].at, t.clips[1].src_in, t.clips[1].src_out), (6.5, 2.5, 5.0));
        t.clips[1].at = 1.0;
        assert_eq!(t.sort(1), 0);
        assert_eq!(cuts(&[t]), vec![0.0, 1.0, 3.5, 5.0, 6.5]);
    }

    #[test]
    fn gains() {
        assert_eq!(gain(0.0), 1.0);
        assert!((gain(-6.0) - 0.501).abs() < 0.001);
        assert_eq!(gain(-120.0), 0.0);
        assert!((db(0.5) + 6.02).abs() < 0.01);
    }

    #[test]
    fn reads_splice9000_projects() {
        let text = r#"{"media":[{"path":"/a.mp4","duration":10,"width":1,"height":1,"fps":30,"audio":true},
            {"path":"/b.mp4","duration":5,"width":1,"height":1,"fps":30,"audio":false}],
            "clips":[{"media":0,"src_in":1,"src_out":3,"fade":0},{"media":1,"src_in":0,"src_out":2},{"media":0,"src_in":5,"src_out":6}],
            "playhead":0}"#;
        let p: SpliceProject = serde_json::from_str(text).unwrap();
        let placed = p.placed();
        assert_eq!(placed.len(), 2);
        assert_eq!(placed[1], ("/a.mp4".into(), 4.0, 5.0, 6.0));
    }

    #[test]
    fn old_and_new_files_open() {
        let p: Project = serde_json::from_str(r#"{"tracks":[{"name":"x","clips":[{"media":0,"at":1,"src_in":0,"src_out":2}]}],"from_the_future":1}"#).unwrap();
        assert_eq!(p.tracks[0].clips[0].gain_db, 0.0);
        assert_eq!(p.tracks[0].pan, 0.0);
    }
}
