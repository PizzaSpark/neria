//! Cutting a section out of a local file with ffmpeg.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Result, bail};

use crate::picture::Picture;
use crate::sound::{self, AudioEdit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipMode {
    /// Stream copy: instant and lossless, but cuts snap to keyframes.
    Fast,
    /// Re-encode: frame-exact cuts, takes longer.
    Exact,
}

#[derive(Clone, Debug)]
pub struct ClipRequest {
    pub input: PathBuf,
    pub start: f64,
    pub end: f64,
    pub mode: ClipMode,
    pub has_video: bool,
    /// What happens to the sound (Clip page → Sound).
    pub audio: AudioEdit,
    /// Save only the sound, in this format (`mp3`, `m4a`, `opus`, `flac`, `wav`).
    pub audio_only: Option<String>,
    /// Turned, mirrored, cropped (Clip page → Picture). Re-encodes the video.
    pub picture: Picture,
    /// A name from the clip list: the file becomes `video - Name.mp4`.
    pub name: Option<String>,
    /// An instant replay's own sound tracks (Clip → Sound → Tracks), mixed again as set into
    /// the first track. Empty: the first track as it is.
    pub tracks: Vec<TrackMix>,
    /// …and kept as tracks of their own after it too.
    pub tracks_apart: bool,
}

/// One of a file's sound tracks in the mix.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackMix {
    /// Its place among the file's sound tracks (`0:a:N`).
    pub index: u32,
    pub name: String,
    pub on: bool,
    /// 1.0 = as it is.
    pub volume: f32,
}

/// The mix of `tracks` as filter graph lines, ending in `[aout]` (and `[k<N>]` for each
/// kept apart). `pre`: the stream label prefix (`0:a:` for ffmpeg, `aid` for mpv, 1-based).
pub fn tracks_graph(tracks: &[TrackMix], apart: bool, chain: Option<&str>, mpv: bool) -> Option<String> {
    let on: Vec<&TrackMix> = tracks.iter().filter(|t| t.on).collect();
    if on.is_empty() {
        return None;
    }
    let mut g = Vec::new();
    for t in &on {
        let input = if mpv { format!("[aid{}]", t.index + 1) } else { format!("[0:a:{}]", t.index) };
        let vol = format!("volume={:.2}", t.volume.max(0.0));
        if apart {
            g.push(format!("{input}{vol},asplit=2[m{i}][k{i}]", i = t.index));
        } else {
            g.push(format!("{input}{vol}[m{}]", t.index));
        }
    }
    let ins: String = on.iter().map(|t| format!("[m{}]", t.index)).collect();
    let tail = chain.map(|c| format!(",{c}")).unwrap_or_default();
    let out = if mpv { "[ao]" } else { "[aout]" };
    g.push(format!("{ins}amix=inputs={}:normalize=0,alimiter=limit=0.97{tail}{out}", on.len()));
    Some(g.join(";"))
}

impl ClipRequest {
    pub fn new(input: impl Into<PathBuf>, start: f64, end: f64, mode: ClipMode, has_video: bool) -> Self {
        Self {
            input: input.into(),
            start,
            end,
            mode,
            has_video,
            audio: AudioEdit::default(),
            audio_only: None,
            picture: Picture::default(),
            name: None,
            tracks: Vec::new(),
            tracks_apart: false,
        }
    }

    /// The replay's tracks are mixed again (one muted or turned up or down, or kept apart).
    pub fn mixes_tracks(&self) -> bool {
        !self.audio.mutes()
            && !self.tracks.is_empty()
            && (self.tracks_apart || self.tracks.iter().any(|t| !t.on || (t.volume - 1.0).abs() > 0.01))
    }

    /// The sound is re-encoded (changed, or its tracks mixed again).
    pub fn sound_reencodes(&self) -> bool {
        self.audio.reencodes() || self.mixes_tracks()
    }

    /// How it's really cut: a changed picture can't be copied, so it's always re-encoded.
    pub fn effective_mode(&self) -> ClipMode {
        if self.has_video && self.audio_only.is_none() && !self.picture.is_none() { ClipMode::Exact } else { self.mode }
    }

    /// A lossless cut that copies the video but re-encodes the sound: both have to start
    /// on the same keyframe, or the sound would run ahead of the picture.
    fn copies_video_with_new_sound(&self) -> bool {
        self.effective_mode() == ClipMode::Fast && self.has_video && self.audio_only.is_none() && self.sound_reencodes()
    }
}

/// File-name-safe timestamp: `1h02m03s` / `2m03s`.
pub fn stamp(t: f64) -> String {
    let s = t.max(0.0).floor() as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}h{m:02}m{s:02}s") } else { format!("{m}m{s:02}s") }
}

/// `video.mp4` -> `video (clip 0.12-0.45).mp4`, never overwriting an existing file.
pub fn output_path(req: &ClipRequest) -> PathBuf {
    let dir = req.input.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = req.input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "clip".into());
    let src_ext = req.input.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
    let ext = match (&req.audio_only, req.effective_mode(), req.has_video) {
        (Some(fmt), _, _) => fmt.clone(),
        (None, ClipMode::Exact, true) => "mp4".to_string(),
        _ => src_ext,
    };
    let base = match req.name.as_deref().map(crate::music::sanitize).filter(|n| !n.trim().is_empty()) {
        Some(name) => format!("{stem} - {}", name.trim()),
        None => format!("{stem} (clip {}-{})", stamp(req.start), stamp(req.end)),
    };
    unique_path(&dir, &base, &ext)
}

/// `dir/base.ext`, or `dir/base (2).ext`, … if that already exists.
pub fn unique_path(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let mut out = dir.join(format!("{base}.{ext}"));
    let mut n = 2;
    while out.exists() {
        out = dir.join(format!("{base} ({n}).{ext}"));
        n += 1;
    }
    out
}

/// The ffmpeg arguments. `start` is where the cut really begins (the keyframe before In
/// when a lossless cut gets new sound); `measured` is the loudness pass when evening out.
pub fn build_args(req: &ClipRequest, start: f64, measured: Option<&sound::Loudness>, out: &Path) -> Vec<String> {
    let dur = (req.end - start).max(0.01);
    let mut a: Vec<String> =
        ["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y"].map(String::from).to_vec();
    a.extend(["-ss".into(), format!("{start:.3}"), "-i".into(), req.input.to_string_lossy().into_owned()]);
    a.extend(["-t".into(), format!("{dur:.3}")]);
    let out_ext = out.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    // An instant replay's tracks mixed again: the sound's own changes go after the mix.
    let chain = req.audio.chain(dur, measured);
    let mix = if req.mixes_tracks() {
        tracks_graph(&req.tracks, req.tracks_apart && req.audio_only.is_none(), chain.as_deref(), false)
    } else {
        None
    };
    if let Some(g) = &mix {
        a.extend(["-filter_complex".into(), g.clone()]);
    }
    let first_sound: &str = if mix.is_some() { "[aout]" } else { "0:a:0" };

    if req.audio_only.is_some() {
        // Just the sound, always encoded into the picked format.
        a.extend(["-map", first_sound, "-vn", "-sn", "-dn"].map(String::from));
        a.extend(sound::encoder_for(&out_ext));
    } else {
        // `V` = real video only (skips embedded cover art).
        a.extend(["-map".into(), "0:V:0?".into()]);
        if !req.audio.mutes() {
            a.extend(["-map".into(), if mix.is_some() { "[aout]".into() } else { "0:a:0?".into() }]);
        }
        if mix.is_some() {
            a.extend([
                "-metadata:s:a:0".into(),
                "title=Everything".into(),
                "-metadata:s:a:0".into(),
                "handler_name=Everything".into(),
            ]);
            if req.tracks_apart {
                for (n, t) in req.tracks.iter().filter(|t| t.on).enumerate() {
                    a.extend(["-map".into(), format!("[k{}]", t.index)]);
                    for key in ["title", "handler_name"] {
                        a.extend([format!("-metadata:s:a:{}", n + 1), format!("{key}={}", t.name)]);
                    }
                }
            }
        }
        match (req.effective_mode(), req.has_video) {
            (ClipMode::Fast, _) if !req.sound_reencodes() => {
                a.extend(["-c", "copy", "-avoid_negative_ts", "make_zero"].map(String::from))
            }
            // Lossless video, new sound.
            (ClipMode::Fast, true) => {
                a.extend(["-c:v", "copy", "-avoid_negative_ts", "make_zero"].map(String::from));
                a.extend(sound::encoder_for(&out_ext));
            }
            (ClipMode::Exact, true) => {
                if let Some(vf) = req.picture.filter() {
                    a.extend(["-vf".into(), vf]);
                }
                // The processor or the graphics card, as Settings → General → Video work on says.
                a.extend(crate::gpu::h264_args(18));
                a.extend(["-c:a", "aac", "-b:a", "192k"].map(String::from))
            }
            // Audio-only files: the natural encoder for the container.
            (_, false) => a.extend(sound::encoder_for(&out_ext)),
        }
        if req.audio.mutes() {
            a.push("-an".into());
        }
    }
    if let (Some(chain), None) = (chain, &mix) {
        a.extend(["-af".into(), chain]);
    }
    if measured.is_some() {
        // loudnorm works at 192 kHz inside; save at a normal rate.
        a.extend(["-ar".into(), "48000".into()]);
    }
    if out_ext.eq_ignore_ascii_case("mp4") || out_ext.eq_ignore_ascii_case("m4a") {
        a.extend(["-movflags".into(), "+faststart".into()]);
    }
    a.push(out.to_string_lossy().into_owned());
    a
}

/// Export the clip. `on_progress` receives 0..1. Returns the output path.
pub fn export(req: &ClipRequest, cancel: &AtomicBool, on_progress: impl FnMut(f32)) -> Result<PathBuf> {
    export_to(req, None, cancel, on_progress)
}

/// Several clips of one video (the clip list), all with the same settings: one file each,
/// or joined into one. Joining cuts frame-exact (a lossless cut would carry extra footage
/// at every seam) and changes the sound once, over the whole joined file (fades at its
/// ends, not at every seam). Returns the files made.
pub fn export_many(
    reqs: &[ClipRequest],
    join: bool,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f32),
) -> Result<Vec<PathBuf>> {
    let total: f64 = reqs.iter().map(|r| (r.end - r.start).max(0.01)).sum();
    let mut done = 0.0;
    let mut made: Vec<PathBuf> = Vec::new();
    // Joining: the pieces take 90%, joining and the sound the rest.
    let share = if join { 0.9 } else { 1.0 };
    let dir = reqs.first().and_then(|r| r.input.parent()).map(Path::to_path_buf).unwrap_or_default();
    let run = std::process::id() as u64 * 1000 + PIECE_RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    for (i, req) in reqs.iter().enumerate() {
        let len = (req.end - req.start).max(0.01);
        let mut piece = req.clone();
        let out = if join {
            piece.mode = ClipMode::Exact;
            piece.audio = AudioEdit::default();
            piece.name = None;
            let ext = output_path(&piece).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
            Some(dir.join(format!(".neria-join-{run}-{i:03}.{ext}")))
        } else {
            None
        };
        let base = done;
        match export_to(&piece, out, cancel, |p| on_progress(((base + p as f64 * len) / total * share) as f32)) {
            Ok(path) => made.push(path),
            Err(e) => {
                if join || e.to_string() == "Cancelled" {
                    for f in &made {
                        let _ = std::fs::remove_file(f);
                    }
                }
                if e.to_string() == "Cancelled" {
                    bail!("Cancelled");
                }
                let which = req.name.clone().unwrap_or_else(|| format!("clip {}", i + 1));
                bail!("{which}: {e}");
            }
        }
        done += len;
    }
    if !join {
        on_progress(1.0);
        return Ok(made);
    }

    // Join the pieces as they are (same settings, so the same streams), then the sound.
    let first = &reqs[0];
    let ext = made[0].extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
    let stem = first.input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "clip".into());
    let out = unique_path(&dir, &format!("{stem} ({} clips)", reqs.len()), &ext);
    let list = crate::paths::cache_dir().join(format!("join-{run}.txt"));
    let result = (|| -> Result<()> {
        std::fs::create_dir_all(crate::paths::cache_dir())?;
        let lines: Vec<String> =
            made.iter().map(|p| format!("file '{}'", p.to_string_lossy().replace('\'', "'\\''"))).collect();
        std::fs::write(&list, lines.join("\n"))?;
        let mut a: Vec<String> = [
            "-hide_banner",
            "-v",
            "error",
            "-nostats",
            "-progress",
            "pipe:1",
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
        ]
        .map(String::from)
        .to_vec();
        a.push(list.to_string_lossy().into_owned());
        a.extend(["-map", "0", "-c", "copy"].map(String::from));
        if ext.eq_ignore_ascii_case("mp4") || ext.eq_ignore_ascii_case("m4a") {
            a.extend(["-movflags".into(), "+faststart".into()]);
        }
        a.push(out.to_string_lossy().into_owned());
        sound::run_ffmpeg(&a, total, cancel, |p| on_progress((0.9 + p as f64 * 0.05) as f32))?;
        if !first.audio.is_none() {
            sound::apply_in_place(&out, &first.audio, cancel, |p| on_progress((0.95 + p as f64 * 0.05) as f32))?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&list);
    for f in &made {
        let _ = std::fs::remove_file(f);
    }
    if let Err(e) = result {
        let _ = std::fs::remove_file(&out);
        if e.to_string() == "Cancelled" {
            bail!("Cancelled");
        }
        bail!("Couldn't join the clips: {e}");
    }
    on_progress(1.0);
    Ok(vec![out])
}

/// Keeps the hidden pieces of two joins apart.
static PIECE_RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`export`] to `out` (default: next to the video, named after the clip).
fn export_to(
    req: &ClipRequest,
    out: Option<PathBuf>,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f32),
) -> Result<PathBuf> {
    if req.end <= req.start {
        bail!("The clip is empty: set the out point after the in point.");
    }
    if req.audio_only.is_some() && req.audio.mutes() {
        bail!("Sound only and muted would leave nothing to save.");
    }
    // No sound in the file: nothing to change, and nothing to save on its own.
    let mut req = req.clone();
    if (req.audio_only.is_some() || !req.audio.is_none())
        && crate::media::file_info(&req.input).is_ok_and(|i| !i.has_audio)
    {
        if req.audio_only.is_some() {
            bail!("This video has no sound to save.");
        }
        req.audio = AudioEdit::default();
    }
    let start = if req.copies_video_with_new_sound() {
        let kf = crate::media::keyframes(&req.input).unwrap_or_default();
        let k = crate::media::keyframe_before(&kf, req.start);
        // Just past it, so the seek lands on this keyframe and not the one before.
        if k < req.start { k + 0.001 } else { req.start }
    } else {
        req.start
    };
    let dur = (req.end - start).max(0.01);

    // Evening out takes two passes: measure, then apply.
    let (measured, base) = match req.audio.normalize_target() {
        Some(t) => {
            let share = if req.effective_mode() == ClipMode::Exact && req.has_video && req.audio_only.is_none() {
                0.2
            } else {
                0.4
            };
            let m = sound::measure(&req.input, start, dur, t, cancel, |p| on_progress(p * share))?;
            if m.is_none() {
                // Silence: nothing to even out (fades still apply).
                req.audio.level = sound::Level::AsIs;
            }
            (m, share)
        }
        None => (None, 0.0),
    };

    let out = out.unwrap_or_else(|| output_path(&req));
    if let Err(e) = crate::gpu::or_on_cpu(|| {
        sound::run_ffmpeg(&build_args(&req, start, measured.as_ref(), &out), dur, cancel, |p| {
            on_progress(base + p * (1.0 - base))
        })
    }) {
        let _ = std::fs::remove_file(&out);
        if e.to_string() == "Cancelled" {
            bail!("Cancelled");
        }
        bail!("Export failed: {e}");
    }
    on_progress(1.0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sound::Level;

    #[test]
    fn exact_video_goes_to_mp4() {
        crate::gpu::set(crate::gpu::Processing::Cpu);
        let req = ClipRequest::new("Z:/nope/My video.webm", 12.0, 45.5, ClipMode::Exact, true);
        let out = output_path(&req);
        assert_eq!(out.file_name().unwrap().to_string_lossy(), "My video (clip 0m12s-0m45s).mp4");
        let args = build_args(&req, req.start, None, &out);
        assert!(args.contains(&"libx264".to_string()));
        assert!(args.windows(2).any(|w| w[0] == "-t" && w[1] == "33.500"));
    }

    #[test]
    fn fast_keeps_container() {
        let req = ClipRequest::new("Z:/nope/a.mkv", 0.0, 5.0, ClipMode::Fast, true);
        let out = output_path(&req);
        assert!(out.to_string_lossy().ends_with(".mkv"));
        let args = build_args(&req, 0.0, None, &out);
        assert!(args.windows(2).any(|w| w[0] == "-c" && w[1] == "copy"));
    }

    #[test]
    fn fast_with_new_sound_copies_only_the_video() {
        let mut req = ClipRequest::new("Z:/nope/a.webm", 10.0, 20.0, ClipMode::Fast, true);
        req.audio.level = Level::Gain(3.0);
        req.audio.fade_out = 1.0;
        assert!(req.copies_video_with_new_sound());
        // Started on the keyframe at 8 s: the fade out still ends at Out.
        let args = build_args(&req, 8.0, None, Path::new("Z:/nope/o.webm"));
        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "copy"));
        assert!(args.windows(2).any(|w| w[0] == "-c:a" && w[1] == "libopus"));
        let af = &args[args.iter().position(|a| a == "-af").unwrap() + 1];
        assert!(af.contains("afade=t=out:st=11.000:d=1.000"), "{af}");
    }

    #[test]
    fn mute_stays_lossless() {
        let mut req = ClipRequest::new("Z:/nope/a.mp4", 0.0, 5.0, ClipMode::Fast, true);
        req.audio.level = Level::Mute;
        let args = build_args(&req, 0.0, None, Path::new("Z:/nope/o.mp4"));
        assert!(args.windows(2).any(|w| w[0] == "-c" && w[1] == "copy"));
        assert!(args.contains(&"-an".to_string()));
        assert!(!args.contains(&"0:a:0?".to_string()));
    }

    #[test]
    fn sound_only_takes_its_format() {
        let mut req = ClipRequest::new("Z:/nope/talk.mp4", 60.0, 90.0, ClipMode::Fast, true);
        req.audio_only = Some("mp3".into());
        let out = output_path(&req);
        assert!(out.to_string_lossy().ends_with("talk (clip 1m00s-1m30s).mp3"));
        let args = build_args(&req, 60.0, None, &out);
        assert!(args.contains(&"-vn".to_string()) && args.contains(&"libmp3lame".to_string()));
    }
}
