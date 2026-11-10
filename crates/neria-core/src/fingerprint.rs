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
use serde_json::Value;

use crate::spotify::Track;
use crate::ytmusic::{self, Candidate};
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
    Ok(out.stdout.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
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

/// Thresholds measured on real tracks (reference preview vs file):
/// same master 0.98, same recording edited (radio edit) 0.95, same recording
/// other master/length 0.76–0.88, re-recording 0.58–0.64, different song 0.57.
pub fn verdict(similarity: f64) -> Verdict {
    if similarity >= SAME {
        Verdict::Same
    } else if similarity >= UNSURE {
        Verdict::Unsure
    } else {
        Verdict::Different
    }
}

pub const SAME: f64 = 0.72;
pub const UNSURE: f64 = 0.66;

/// Download (and cache) a preview clip.
pub fn fetch_preview(url: &str) -> Result<PathBuf> {
    // Key on the URL path: Deezer adds expiring tokens in the query string.
    let key = url.split('?').next().unwrap_or(url);
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let path = paths::cache_dir().join(format!("preview-{hash:016x}.audio"));
    if !path.is_file() {
        let bytes = web::client()?.get(url).send()?.error_for_status()?.bytes()?;
        std::fs::write(&path, &bytes)?;
    }
    Ok(path)
}

/// The 30 s clip we trust as "the right recording", and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    pub url: String,
    /// "Spotify", "Deezer" or "Apple Music".
    pub source: &'static str,
}

/// Find a reference preview for a Spotify track: Spotify's own preview first,
/// then Deezer (exact ISRC, else a strict title/artist/length match), then the
/// iTunes Search API. All three are public preview clips meant for this.
/// Fallback clips must match title, artist and length (±3 s) so a re-recording
/// can't become the reference.
pub fn reference_for(track: &Track) -> Option<Reference> {
    let spotify = track.preview_url.clone().or_else(|| {
        (!track.id.is_empty())
            .then(|| crate::spotify::track_details(&track.id).ok())
            .flatten()
            .and_then(|t| t.preview_url)
    });
    if let Some(url) = spotify {
        return Some(Reference { url, source: "Spotify" });
    }
    deezer_reference(track).or_else(|| itunes_reference(track))
}

/// A fallback result is only trusted if it's clearly the same release.
fn trusted(track: &Track, title: &str, artist: &str, secs: f64) -> bool {
    let c = Candidate {
        video_id: String::new(),
        title: title.into(),
        artists: artist.into(),
        album: None,
        duration: Some(secs),
        score: 0.0,
    };
    (secs - track.duration_secs()).abs() <= 3.0 && ytmusic::score(track, &c).is_some_and(|s| s >= 0.85)
}

fn deezer_reference(track: &Track) -> Option<Reference> {
    let get = |url: &str| web::get_text(url).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    if let Some(isrc) = &track.isrc
        && let Some(t) = get(&format!("https://api.deezer.com/track/isrc:{}", web::encode(isrc)))
        && let Some(url) = t["preview"].as_str().filter(|u| !u.is_empty())
    {
        return Some(Reference { url: url.to_string(), source: "Deezer" });
    }
    let q = web::encode(&format!("{} {}", track.main_artist(), ytmusic::normalize_title(&track.title)));
    let results = get(&format!("https://api.deezer.com/search/track?q={q}&limit=15"))?;
    results["data"].as_array()?.iter().find_map(|t| {
        let url = t["preview"].as_str().filter(|u| !u.is_empty())?;
        let ok = trusted(track, t["title"].as_str()?, t["artist"]["name"].as_str()?, t["duration"].as_f64()?);
        ok.then(|| Reference { url: url.to_string(), source: "Deezer" })
    })
}

fn itunes_reference(track: &Track) -> Option<Reference> {
    let term = web::encode(&format!("{} {}", track.main_artist(), ytmusic::normalize_title(&track.title)));
    let text = web::get_text(&format!("https://itunes.apple.com/search?term={term}&entity=song&limit=15")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v["results"].as_array()?.iter().find_map(|t| {
        let url = t["previewUrl"].as_str()?;
        let secs = t["trackTimeMillis"].as_f64()? / 1000.0;
        let ok = trusted(track, t["trackName"].as_str()?, t["artistName"].as_str()?, secs);
        ok.then(|| Reference { url: url.to_string(), source: "Apple Music" })
    })
}

/// Fingerprint of a reference clip (cached on disk).
pub fn reference_fingerprint(r: &Reference) -> Result<Vec<u32>> {
    let clip = fetch_preview(&r.url)?;
    fingerprint(clip.as_os_str())
}

/// Compare a downloaded file with a reference clip.
pub fn compare_file(reference: &[u32], file: &Path) -> Result<Alignment> {
    let song = fingerprint(file.as_os_str())?;
    align(reference, &song).context("audio too short to compare")
}

/// Compare a YouTube Music result *before* the user picks it. YouTube refuses
/// plain ffmpeg reads of its streams, so yt-dlp fetches the audio (a few MB,
/// no conversion) into the cache; it's deleted after fingerprinting.
pub fn compare_candidate(reference: &[u32], watch_url: &str) -> Result<Alignment> {
    let dir = paths::cache_dir().join("check");
    std::fs::create_dir_all(&dir)?;
    let id = watch_url.rsplit("v=").next().unwrap_or("x").split('&').next().unwrap_or("x");
    let stem = format!("cand-{}", crate::music::sanitize(id));
    let status = proc::command(paths::tool("yt-dlp"))
        .args([
            "--no-warnings",
            "--quiet",
            "--no-playlist",
            "--no-part",
            "--force-overwrites",
            "-f",
            "ba[ext=m4a]/ba/b",
        ])
        .arg("-P")
        .arg(&dir)
        .args(["-o", &format!("{stem}.%(ext)s"), "--", watch_url])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to start yt-dlp")?;
    let file = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy() == stem));
    let Some(file) = file.filter(|_| status.success()) else { bail!("couldn't get that audio") };
    let result = compare_file(reference, &file);
    let _ = std::fs::remove_file(&file);
    result
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
