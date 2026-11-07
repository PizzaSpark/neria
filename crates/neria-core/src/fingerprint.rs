//! "Is this the same recording?" using Chromaprint (the AcoustID algorithm),
//! via ffmpeg's `chromaprint` muxer.
//!
//! Spotify's 30 s preview is compared against the downloaded file: we slide the
//! preview's fingerprint along the song's and keep the best alignment. The same
//! master lines up almost bit-for-bit; re-recordings, live takes and covers don't.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use crate::{paths, proc, web};

/// Seconds of audio per fingerprint item (Chromaprint default: 4096-sample
/// frames with 2/3 overlap at 11025 Hz ≈ 0.1238 s).
pub const ITEM_SECS: f64 = 0.1238;

/// Raw Chromaprint fingerprint (one u32 per ~0.124 s).
pub fn fingerprint(input: &OsStr) -> Result<Vec<u32>> {
    let out = proc::command(paths::tool("ffmpeg"))
        .args(["-hide_banner", "-v", "error", "-i"])
        .arg(input)
        .args(["-vn", "-f", "chromaprint", "-fp_format", "raw", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to start ffmpeg")?;
    if !out.status.success() {
        bail!("fingerprint failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""));
    }
    Ok(out.stdout.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Alignment {
    /// 0..1, 1 = identical bits at the best offset. Unrelated audio sits near 0.5.
    pub similarity: f64,
    /// Where the clip starts inside the song, in seconds.
    pub offset_secs: f64,
}

/// Slide `clip` along `song`, return the best bit-agreement.
/// Allows the clip to hang off either end by up to a third (edits/fades).
pub fn align(clip: &[u32], song: &[u32]) -> Option<Alignment> {
    if clip.len() < 40 || song.len() < 40 {
        return None;
    }
    let min_overlap = (clip.len() * 2 / 3).max(40);
    let start = -(clip.len() as isize - min_overlap as isize);
    let end = song.len() as isize - min_overlap as isize;
    let mut best: Option<(f64, isize)> = None;
    for off in start..=end {
        let (mut diff, mut n) = (0u64, 0u64);
        for (i, a) in clip.iter().enumerate() {
            let j = off + i as isize;
            if j < 0 || j as usize >= song.len() {
                continue;
            }
            diff += (a ^ song[j as usize]).count_ones() as u64;
            n += 32;
        }
        if n == 0 {
            continue;
        }
        let sim = 1.0 - diff as f64 / n as f64;
        if best.is_none_or(|(b, _)| sim > b) {
            best = Some((sim, off));
        }
    }
    best.map(|(similarity, off)| Alignment { similarity, offset_secs: off as f64 * ITEM_SECS })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The download is the recording Spotify plays.
    Same,
    /// Similar but not the same master (could be a remaster or a re-recording).
    Unsure,
    /// Different recording (re-recorded, live, cover…) or a different song.
    Different,
}

/// Thresholds measured on real tracks (preview vs file):
/// same master 0.98, same recording edited (radio edit) 0.95, same recording
/// other master/length 0.76, re-recording 0.58, different song 0.57.
pub fn verdict(similarity: f64) -> Verdict {
    if similarity >= SAME {
        Verdict::Same
    } else if similarity >= UNSURE {
        Verdict::Unsure
    } else {
        Verdict::Different
    }
}

pub const SAME: f64 = 0.70;
pub const UNSURE: f64 = 0.62;

/// Download (and cache) Spotify's preview MP3.
pub fn fetch_preview(url: &str) -> Result<PathBuf> {
    let id = url.rsplit('/').next().unwrap_or("preview").split('?').next().unwrap_or("preview");
    let path = paths::cache_dir().join(format!("preview-{}.mp3", crate::music::sanitize(id)));
    if !path.is_file() {
        let bytes = web::client()?.get(url).send()?.error_for_status()?.bytes()?;
        std::fs::write(&path, &bytes)?;
    }
    Ok(path)
}

/// Compare a downloaded file with Spotify's preview.
pub fn compare_with_preview(preview_url: &str, file: &Path) -> Result<Alignment> {
    let preview = fetch_preview(preview_url)?;
    let clip = fingerprint(preview.as_os_str())?;
    let song = fingerprint(file.as_os_str())?;
    align(&clip, &song).context("audio too short to compare")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<u32> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x
            })
            .collect()
    }

    #[test]
    fn finds_clip_inside_song() {
        let song = noise(2000, 7);
        let clip: Vec<u32> = song[700..940].to_vec();
        let a = align(&clip, &song).unwrap();
        assert!(a.similarity > 0.999);
        assert!((a.offset_secs - 700.0 * ITEM_SECS).abs() < 1e-6);
        assert_eq!(verdict(a.similarity), Verdict::Same);
    }

    #[test]
    fn unrelated_audio_is_different() {
        let a = align(&noise(240, 1), &noise(2000, 2)).unwrap();
        assert!(a.similarity < 0.6, "{}", a.similarity);
        assert_eq!(verdict(a.similarity), Verdict::Different);
    }
}
