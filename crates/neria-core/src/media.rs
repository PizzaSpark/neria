//! Local media inspection: ffprobe info, filmstrip thumbnails, sound levels for the waveform.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::timeline::{PEAKS_RATE, Peaks};
use crate::{paths, proc};

#[derive(Clone, Debug, Default)]
pub struct FileInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    /// Frames per second of the video stream (0 if unknown).
    pub fps: f64,
    pub video_codec: String,
    pub size_bytes: u64,
    /// A real video stream (cover art doesn't count).
    pub has_video: bool,
    pub has_audio: bool,
    /// How many sound tracks (films often carry several languages).
    pub audio_tracks: u32,
    /// Each sound track's name ("Everything", "VALORANT", "Mic" in an instant replay), "" if none.
    pub audio_titles: Vec<String>,
    /// The first sound track's codec ("aac", "opus") and channels (2 = stereo; 0 unknown).
    pub audio_codec: String,
    pub audio_channels: u32,
    /// Stored as interlaced fields (old camcorders, TV recordings): needs deinterlacing.
    pub interlaced: bool,
    /// One entry per caption track: true for text (goes into an .mp4), false for
    /// picture captions (Blu-ray/DVD), which an .mp4 can't hold.
    pub subtitles: Vec<bool>,
    /// Chapter marks stored in the file (yt-dlp embeds YouTube's).
    pub chapters: Vec<crate::tracks::Chapter>,
    /// The video is shown turned by this much (phone footage), in degrees, clockwise.
    /// `width` × `height` are as stored; ffmpeg and mpv show it turned.
    pub rotation: i32,
}

fn parse_rate(s: &str) -> f64 {
    match s.split_once('/') {
        Some((n, d)) => {
            let (n, d) = (n.parse::<f64>().unwrap_or(0.0), d.parse::<f64>().unwrap_or(0.0));
            if d > 0.0 { n / d } else { 0.0 }
        }
        None => s.parse().unwrap_or(0.0),
    }
}

pub fn file_info(path: &Path) -> Result<FileInfo> {
    let out = proc::command(paths::tool("ffprobe"))
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", "-show_chapters"])
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
        size_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        ..Default::default()
    };
    for s in v["streams"].as_array().into_iter().flatten() {
        match s["codec_type"].as_str() {
            Some("video") if s["disposition"]["attached_pic"].as_i64() != Some(1) => {
                if !info.has_video {
                    info.width = s["width"].as_u64().unwrap_or(0) as u32;
                    info.height = s["height"].as_u64().unwrap_or(0) as u32;
                    info.video_codec = s["codec_name"].as_str().unwrap_or("").to_string();
                    info.interlaced = matches!(s["field_order"].as_str(), Some("tt" | "bb" | "tb" | "bt"));
                    let avg = parse_rate(s["avg_frame_rate"].as_str().unwrap_or(""));
                    info.fps = if avg > 0.0 { avg } else { parse_rate(s["r_frame_rate"].as_str().unwrap_or("")) };
                    // The display matrix says counter-clockwise; 90 here = shown turned left.
                    let r = s["side_data_list"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find_map(|d| d["rotation"].as_f64())
                        .unwrap_or(0.0) as i32;
                    info.rotation = (-r).rem_euclid(360);
                }
                info.has_video = true;
            }
            Some("audio") => {
                if !info.has_audio {
                    info.audio_codec = s["codec_name"].as_str().unwrap_or("").to_string();
                    info.audio_channels = s["channels"].as_u64().unwrap_or(0) as u32;
                }
                info.has_audio = true;
                info.audio_tracks += 1;
                let tags = &s["tags"];
                let title = tags["title"].as_str().or_else(|| tags["handler_name"].as_str()).unwrap_or("");
                // ffmpeg's own placeholder handler name isn't a name.
                let title = if title == "SoundHandler" { "" } else { title };
                info.audio_titles.push(title.to_string());
            }
            Some("subtitle") => {
                let codec = s["codec_name"].as_str().unwrap_or("");
                // Only the text kinds an .mp4 (mov_text) can be made from; teletext,
                // closed captions and picture captions count as not text.
                info.subtitles.push(matches!(codec, "subrip" | "srt" | "ass" | "ssa" | "webvtt" | "mov_text" | "text"));
            }
            _ => {}
        }
    }
    let secs = |c: &Value, k: &str| c[k].as_str().and_then(|t| t.parse::<f64>().ok());
    info.chapters = v["chapters"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, c)| {
            let (start, end) = (secs(c, "start_time")?, secs(c, "end_time")?);
            let title = c["tags"]["title"].as_str().filter(|t| !t.trim().is_empty());
            let title = title.map_or_else(|| format!("Chapter {}", i + 1), |t| t.trim().to_string());
            (end > start).then_some(crate::tracks::Chapter { start, end, title })
        })
        .collect();
    if info.chapters.len() < 2 {
        info.chapters.clear();
    }
    Ok(info)
}

/// Timestamps (seconds) of the video keyframes, sorted.
///
/// Reads packet headers only (no decoding), so it's fast even for long files.
/// A lossless cut can only start on one of these.
/// How long a file plays, from its container (no full read).
pub fn duration(path: &Path) -> Option<f64> {
    start_and_end(path).map(|(s, e)| e - s)
}

/// Where a file's timestamps start and end, in seconds (a recording's pieces keep the
/// stream's own clock, so they don't start at 0).
pub fn start_and_end(path: &Path) -> Option<(f64, f64)> {
    let out = proc::command(paths::tool("ffprobe"))
        .args(["-v", "error", "-show_entries", "format=start_time,duration", "-of", "csv=p=0"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut f = text.trim().split(',').map(|v| v.trim().parse::<f64>().ok().filter(|x| x.is_finite()));
    let start = f.next().flatten().unwrap_or(0.0);
    let len = f.next().flatten()?;
    (len > 0.0).then_some((start, start + len))
}

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

/// Longest link that gets a waveform: it means fetching all of its sound (the smallest
/// format, ~30 MB an hour), so very long ones keep the filmstrip only.
pub const LINK_WAVEFORM_MAX_SECS: f64 = 3.0 * 3600.0;

/// The sound levels of a link ([`Peaks`]): its smallest sound-only format is fetched into
/// the cache (with the site's sign-in, if any), measured, and the sound is thrown away again.
/// Cached per link. Stops (and cleans up) once `cancel` is set. Not for live streams.
pub fn peaks_link(
    url: &str,
    settings: &crate::settings::Settings,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Peaks> {
    let dir = link_dir(url);
    let cached = dir.join(PEAKS_FILE);
    if let Some(p) = Peaks::load(&cached) {
        return Ok(p);
    }
    std::fs::create_dir_all(&dir)?;
    let mut cmd = proc::command(paths::tool("yt-dlp"));
    // A site with no sound-only format falls back to a whole small video: capped.
    cmd.args(["--no-warnings", "--no-playlist", "--no-part", "--quiet", "-f", "wa/ba/w", "--max-filesize", "300M"]);
    cmd.args(crate::cookies::args_for(url, settings));
    cmd.args(["--ffmpeg-location", &paths::bin_dir().to_string_lossy()]);
    cmd.arg("-o").arg(dir.join("sound.%(ext)s"));
    let mut child = cmd.arg("--").arg(url).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let sound = |dir: &Path| {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.file_stem().is_some_and(|s| s == "sound"))
    };
    let cleanup = |dir: &Path| {
        if let Some(p) = sound(dir) {
            let _ = std::fs::remove_file(p);
        }
    };
    loop {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            proc::kill_tree(&mut child);
            std::thread::sleep(std::time::Duration::from_millis(200));
            cleanup(&dir);
            bail!("Cancelled");
        }
        match child.try_wait()? {
            Some(status) if status.success() => break,
            Some(_) => {
                cleanup(&dir);
                bail!("couldn't fetch the sound");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(150)),
        }
    }
    let audio = sound(&dir).context("couldn't fetch the sound")?;
    let measured = measure_peaks(&audio);
    let _ = std::fs::remove_file(&audio);
    let peaks = measured?;
    let _ = peaks.save(&cached);
    Ok(peaks)
}

/// Cache folder for a link (page URL), shared by its filmstrip, storyboard and sound levels.
fn link_dir(url: &str) -> PathBuf {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    paths::cache_dir().join(format!("link-{hash:016x}"))
}

const PEAKS_FILE: &str = "peaks-200.bin";

/// The sound levels of a file ([`Peaks`]), cached next to its filmstrip.
pub fn peaks(path: &Path) -> Result<Peaks> {
    let cached = cache_for(path).join(PEAKS_FILE);
    if let Some(p) = Peaks::load(&cached) {
        return Ok(p);
    }
    let peaks = measure_peaks(path)?;
    let _ = peaks.save(&cached);
    Ok(peaks)
}

/// Decodes the first sound track as mono 16-bit at a low rate and measures it.
fn measure_peaks(path: &Path) -> Result<Peaks> {
    use std::io::Read;
    let mut child = proc::command(paths::tool("ffmpeg"))
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-vn", "-ac", "1", "-ar", &PEAKS_RATE.to_string(), "-f", "s16le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    // Read as it decodes (an hour is ~58 MB of samples; never all held at once).
    let out = std::io::BufReader::with_capacity(1 << 16, child.stdout.take().context("no output")?);
    let mut bytes = out.bytes().map_while(|b| b.ok());
    let samples = std::iter::from_fn(|| Some(i16::from_le_bytes([bytes.next()?, bytes.next()?])));
    let peaks = Peaks::from_samples(samples);
    let status = child.wait()?;
    if !status.success() || peaks.data.is_empty() {
        bail!("couldn't read the sound");
    }
    Ok(peaks)
}

/// Evenly spaced frames for the timeline filmstrip. Fast: each is a keyframe seek.
pub fn thumbnails(path: &Path, duration: f64, count: usize, height: u32) -> Vec<PathBuf> {
    thumbnails_into(path.as_os_str().to_owned(), &cache_for(path), duration, count, height)
}

/// Filmstrip for a link from the site's storyboards (YouTube's hover-preview
/// sprite sheets): small image files, so no stream access is needed. YouTube
/// answers direct ffmpeg reads of its video streams with 403, so this is the
/// reliable way. Returns (title, duration, thumbnails).
pub fn storyboard_strip(
    url: &str,
    settings: &crate::settings::Settings,
    count: usize,
    height: u32,
) -> Result<(String, f64, Vec<PathBuf>)> {
    let mut cmd = proc::command(paths::tool("yt-dlp"));
    cmd.args(["-J", "--no-warnings", "--no-playlist", "--encoding", "utf-8"]);
    cmd.args(crate::cookies::args_for(url, settings));
    let out = cmd.arg("--").arg(url).stdout(Stdio::piped()).stderr(Stdio::null()).output()?;
    let v: Value = serde_json::from_slice(&out.stdout).context("couldn't read the link")?;
    let title = v["title"].as_str().unwrap_or("").to_string();
    let duration = v["duration"].as_f64().filter(|d| *d > 0.0).context("unknown duration")?;
    let board = pick_storyboard(&v).context("no storyboard for this link")?;
    let dir = link_dir(url);
    std::fs::create_dir_all(&dir)?;
    // Kept for the close-up filmstrip when zoomed (`storyboard_tiles`).
    let boards: Vec<&Value> = v["formats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["format_id"].as_str().is_some_and(|id| id.starts_with("sb")))
        .collect();
    let _ = std::fs::write(dir.join(BOARD_FILE), serde_json::json!({ "formats": boards }).to_string());

    // Which sprite and which cell of it each thumbnail comes from.
    let cells: Vec<(usize, u32)> =
        (0..count).map(|i| board.cell_at(duration * (i as f64 + 0.5) / count as f64)).collect();

    let thumbs: Vec<PathBuf> = board
        .crop_cells(&dir, &cells, height, |i, _, _| format!("sbthumb-{height}-{i:02}.jpg"))
        .into_iter()
        .flatten()
        .collect();
    if thumbs.is_empty() {
        bail!("couldn't build the filmstrip");
    }
    Ok((title, duration, thumbs))
}

struct Storyboard {
    width: u32,
    height: u32,
    rows: u32,
    cols: u32,
    fps: f64,
    /// (sprite url, seconds covered)
    fragments: Vec<(String, f64)>,
}

impl Storyboard {
    /// Which sprite, and which cell of it, shows time `t`.
    fn cell_at(&self, t: f64) -> (usize, u32) {
        let per_sheet = (self.rows * self.cols).max(1);
        let mut start = 0.0;
        for (k, (_, d)) in self.fragments.iter().enumerate() {
            if t < start + d || k + 1 == self.fragments.len() {
                let idx = (((t - start) * self.fps).floor().max(0.0) as u32).min(per_sheet - 1);
                return (k, idx);
            }
            start += d;
        }
        (0, 0)
    }

    /// Fetches the sprites these cells need (in parallel, cached), then crops each cell out
    /// at `height`; `name(i, sprite, cell)` names a cell's file. `None` where it failed.
    fn crop_cells(
        &self,
        dir: &Path,
        cells: &[(usize, u32)],
        height: u32,
        name: impl Fn(usize, usize, u32) -> String,
    ) -> Vec<Option<PathBuf>> {
        let mut needed: Vec<usize> = cells.iter().map(|(k, _)| *k).collect();
        needed.sort_unstable();
        needed.dedup();
        let fetches: Vec<_> = needed
            .into_iter()
            .map(|k| {
                let (url, path) = (self.fragments[k].0.clone(), dir.join(format!("sb-{k}.jpg")));
                thread::spawn(move || {
                    if !path.is_file() {
                        let bytes =
                            crate::web::client().ok()?.get(&url).send().ok()?.error_for_status().ok()?.bytes().ok()?;
                        std::fs::write(&path, &bytes).ok()?;
                    }
                    Some(())
                })
            })
            .collect();
        for f in fetches {
            let _ = f.join();
        }
        let jobs: Vec<_> = cells
            .iter()
            .enumerate()
            .map(|(i, &(k, idx))| {
                let sprite = dir.join(format!("sb-{k}.jpg"));
                let out = dir.join(name(i, k, idx));
                let (w, h) = (self.width, self.height);
                let (x, y) = ((idx % self.cols) * w, (idx / self.cols) * h);
                thread::spawn(move || {
                    if !out.is_file() && sprite.is_file() {
                        let _ = proc::command(paths::tool("ffmpeg"))
                            .args(["-v", "error", "-y", "-i"])
                            .arg(&sprite)
                            .args(["-vf", &format!("crop={w}:{h}:{x}:{y},scale=-2:{height}"), "-q:v", "4"])
                            .arg(&out)
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    }
                    out.is_file().then_some(out)
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().ok().flatten()).collect()
    }
}

const BOARD_FILE: &str = "storyboard.json";

/// Close-up filmstrip frames for a link at these times (zoomed in), cropped from the
/// storyboard `storyboard_strip` saved. A storyboard has a frame every few seconds, so
/// neighbouring times can share one. Same order as `times`; `None` where none could be made.
pub fn storyboard_tiles(url: &str, times: &[f64], height: u32) -> Vec<Option<PathBuf>> {
    let dir = link_dir(url);
    let board = std::fs::read(dir.join(BOARD_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| pick_storyboard(&v));
    let Some(board) = board else { return vec![None; times.len()] };
    let cells: Vec<(usize, u32)> = times.iter().map(|t| board.cell_at(*t)).collect();
    board.crop_cells(&dir, &cells, height, |_, k, idx| format!("sbcell-{height}-{k}-{idx}.jpg"))
}

/// Close-up filmstrip frames for a file at these times; `key` names each in the cache (the
/// tile grid's step and index, so scrolling back finds them). A few ffmpegs at a time.
pub fn tiles(path: &Path, frames: &[(String, f64)], height: u32) -> Vec<Option<PathBuf>> {
    let dir = cache_for(path);
    let mut out = Vec::with_capacity(frames.len());
    for chunk in frames.chunks(6) {
        let jobs: Vec<_> = chunk
            .iter()
            .map(|(key, t)| {
                let file = dir.join(format!("tile-{height}-{key}.jpg"));
                let (path, t) = (path.to_path_buf(), *t);
                thread::spawn(move || {
                    if !file.is_file() {
                        let _ = proc::command(paths::tool("ffmpeg"))
                            .args(["-v", "error", "-y", "-ss", &format!("{t:.3}"), "-i"])
                            .arg(&path)
                            .args(["-frames:v", "1", "-vf", &format!("scale=-2:{height}"), "-q:v", "5"])
                            .arg(&file)
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    }
                    file.is_file().then_some(file)
                })
            })
            .collect();
        out.extend(jobs.into_iter().map(|j| j.join().ok().flatten()));
    }
    out
}

/// Prefer the smallest storyboard that is at least 90 px tall (few sprites, sharp enough).
fn pick_storyboard(v: &Value) -> Option<Storyboard> {
    let mut boards: Vec<Storyboard> = v["formats"]
        .as_array()?
        .iter()
        .filter(|f| f["format_id"].as_str().is_some_and(|id| id.starts_with("sb")))
        .filter_map(|f| {
            let fragments: Vec<(String, f64)> = f["fragments"]
                .as_array()?
                .iter()
                .filter_map(|fr| Some((fr["url"].as_str()?.to_string(), fr["duration"].as_f64()?)))
                .collect();
            (!fragments.is_empty()).then(|| Storyboard {
                width: f["width"].as_u64().unwrap_or(0) as u32,
                height: f["height"].as_u64().unwrap_or(0) as u32,
                rows: f["rows"].as_u64().unwrap_or(1) as u32,
                cols: f["columns"].as_u64().unwrap_or(1) as u32,
                fps: f["fps"].as_f64().unwrap_or(0.0),
                fragments,
            })
        })
        .filter(|b| b.width > 0 && b.height > 0 && b.fps > 0.0)
        .collect();
    boards.sort_by_key(|b| b.height);
    let idx = boards.iter().position(|b| b.height >= 90).unwrap_or(boards.len().checked_sub(1)?);
    Some(boards.swap_remove(idx))
}

/// Filmstrip for a remote stream URL; `key` (e.g. the page URL) names the cache,
/// since stream URLs expire.
pub fn thumbnails_remote(stream_url: &str, key: &str, duration: f64, count: usize, height: u32) -> Vec<PathBuf> {
    let dir = link_dir(key);
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
