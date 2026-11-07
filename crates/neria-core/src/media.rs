//! Local media inspection: ffprobe info, filmstrip thumbnails, waveform image.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{paths, proc};

#[derive(Clone, Debug, Default)]
pub struct FileInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    /// A real video stream (cover art doesn't count).
    pub has_video: bool,
    pub has_audio: bool,
}

pub fn file_info(path: &Path) -> Result<FileInfo> {
    let out = proc::command(paths::tool("ffprobe"))
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to start ffprobe")?;
    if !out.status.success() {
        bail!("Can't read this file: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let v: Value = serde_json::from_slice(&out.stdout)?;
    let mut info = FileInfo {
        duration: v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0),
        ..Default::default()
    };
    for s in v["streams"].as_array().into_iter().flatten() {
        match s["codec_type"].as_str() {
            Some("video") if s["disposition"]["attached_pic"].as_i64() != Some(1) => {
                if !info.has_video {
                    info.width = s["width"].as_u64().unwrap_or(0) as u32;
                    info.height = s["height"].as_u64().unwrap_or(0) as u32;
                }
                info.has_video = true;
            }
            Some("audio") => info.has_audio = true,
            _ => {}
        }
    }
    Ok(info)
}

/// Timestamps (seconds) of the video keyframes, sorted.
///
/// Reads packet headers only (no decoding), so it's fast even for long files.
/// A lossless cut can only start on one of these.
pub fn keyframes(path: &Path) -> Result<Vec<f64>> {
    let out = proc::command(paths::tool("ffprobe"))
        .args(["-v", "error", "-select_streams", "V:0", "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("failed to start ffprobe")?;
    Ok(parse_keyframes(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_keyframes(csv: &str) -> Vec<f64> {
    let mut times: Vec<f64> = csv
        .lines()
        .filter_map(|l| {
            let (pts, flags) = l.trim().split_once(',')?;
            flags.contains('K').then(|| pts.parse::<f64>().ok()).flatten()
        })
        .filter(|t| t.is_finite() && *t >= 0.0)
        .collect();
    times.sort_by(f64::total_cmp);
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
    times
}

/// Latest keyframe at or before `t` (where a stream-copy cut really starts).
pub fn keyframe_before(keyframes: &[f64], t: f64) -> f64 {
    let i = keyframes.partition_point(|k| *k <= t + 1e-3);
    if i == 0 { keyframes.first().map_or(t, |k| k.min(t)) } else { keyframes[i - 1] }
}

/// Thin keyframes out so at most ~`max` ticks are drawn.
pub fn decimate(keyframes: &[f64], duration: f64, max: usize) -> Vec<f64> {
    let gap = duration / max.max(1) as f64;
    let mut out: Vec<f64> = Vec::new();
    for &k in keyframes {
        if out.last().is_none_or(|last| k - last >= gap) {
            out.push(k);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframe_parsing_and_lookup() {
        let kf = parse_keyframes("0.000000,K__\n0.033000,___\n4.004000,K__\nN/A,K__\n8.008000,K_\n");
        assert_eq!(kf, vec![0.0, 4.004, 8.008]);
        assert_eq!(keyframe_before(&kf, 3.5), 0.0);
        assert_eq!(keyframe_before(&kf, 4.004), 4.004);
        assert_eq!(keyframe_before(&kf, 9.0), 8.008);
        assert_eq!(keyframe_before(&[], 2.0), 2.0);
        assert_eq!(decimate(&kf, 8.0, 2), vec![0.0, 4.004, 8.008]);
        assert_eq!(decimate(&kf, 8.0, 1), vec![0.0, 8.008]);
    }
}

/// Per-file cache folder, keyed by path + size + mtime.
fn cache_for(path: &Path) -> PathBuf {
    let meta = std::fs::metadata(path).ok();
    let key = format!(
        "{}|{}|{:?}",
        path.display(),
        meta.as_ref().map_or(0, |m| m.len()),
        meta.and_then(|m| m.modified().ok())
    );
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let dir = paths::cache_dir().join(format!("clip-{hash:016x}"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Evenly spaced frames for the timeline filmstrip. Fast: each is a keyframe seek.
pub fn thumbnails(path: &Path, duration: f64, count: usize, height: u32) -> Vec<PathBuf> {
    thumbnails_into(path.as_os_str().to_owned(), &cache_for(path), duration, count, height)
}

/// Filmstrip for a remote stream URL; `key` (e.g. the page URL) names the cache,
/// since stream URLs expire.
pub fn thumbnails_remote(stream_url: &str, key: &str, duration: f64, count: usize, height: u32) -> Vec<PathBuf> {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let dir = paths::cache_dir().join(format!("link-{hash:016x}"));
    let _ = std::fs::create_dir_all(&dir);
    thumbnails_into(stream_url.into(), &dir, duration, count, height)
}

fn thumbnails_into(input: std::ffi::OsString, dir: &Path, duration: f64, count: usize, height: u32) -> Vec<PathBuf> {
    let jobs: Vec<_> = (0..count)
        .map(|i| {
            let t = duration * (i as f64 + 0.5) / count as f64;
            let out = dir.join(format!("thumb-{height}-{i:02}.jpg"));
            let path = input.clone();
            thread::spawn(move || {
                if !out.is_file() {
                    let _ = proc::command(paths::tool("ffmpeg"))
                        .args(["-v", "error", "-y", "-ss", &format!("{t:.3}"), "-i"])
                        .arg(&path)
                        .args(["-frames:v", "1", "-vf", &format!("scale=-2:{height}"), "-q:v", "5"])
                        .arg(&out)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
                out.is_file().then_some(out)
            })
        })
        .collect();
    jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect()
}

/// Render the audio track as a PNG waveform.
pub fn waveform(path: &Path, width: u32, height: u32) -> Result<PathBuf> {
    let out = cache_for(path).join(format!("wave-{width}x{height}.png"));
    if out.is_file() {
        return Ok(out);
    }
    let filter = format!(
        "[0:a:0]aformat=channel_layouts=mono,showwavespic=s={width}x{height}:colors=0x9a82ff:scale=sqrt"
    );
    let status = proc::command(paths::tool("ffmpeg"))
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-filter_complex", &filter, "-frames:v", "1"])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() || !out.is_file() {
        bail!("waveform failed");
    }
    Ok(out)
}
