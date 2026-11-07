//! Cutting a section out of a local file with ffmpeg.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};

use crate::{paths, proc};

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
    let ext = match (req.mode, req.has_video) {
        (ClipMode::Exact, true) => "mp4".to_string(),
        _ => src_ext,
    };
    let base = format!("{stem} (clip {}-{})", stamp(req.start), stamp(req.end));
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

pub fn build_args(req: &ClipRequest, out: &Path) -> Vec<String> {
    let dur = (req.end - req.start).max(0.01);
    let mut a: Vec<String> = ["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y"]
        .map(String::from)
        .to_vec();
    a.extend(["-ss".into(), format!("{:.3}", req.start), "-i".into(), req.input.to_string_lossy().into_owned()]);
    a.extend(["-t".into(), format!("{dur:.3}")]);
    // `V` = real video only (skips embedded cover art).
    a.extend(["-map", "0:V:0?", "-map", "0:a:0?"].map(String::from));
    match (req.mode, req.has_video) {
        (ClipMode::Fast, _) => a.extend(["-c", "copy", "-avoid_negative_ts", "make_zero"].map(String::from)),
        (ClipMode::Exact, true) => a.extend(
            [
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a",
                "192k",
            ]
            .map(String::from),
        ),
        // Audio-only: let ffmpeg pick the natural encoder for the container.
        (ClipMode::Exact, false) => {}
    }
    if out.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp4") || e.eq_ignore_ascii_case("m4a")) {
        a.extend(["-movflags".into(), "+faststart".into()]);
    }
    a.push(out.to_string_lossy().into_owned());
    a
}

/// Export the clip. `on_progress` receives 0..1. Returns the output path.
pub fn export(req: &ClipRequest, cancel: &AtomicBool, mut on_progress: impl FnMut(f32)) -> Result<PathBuf> {
    if req.end <= req.start {
        bail!("The clip is empty: set the out point after the in point.");
    }
    let out = output_path(req);
    let mut child = proc::command(paths::tool("ffmpeg"))
        .args(build_args(req, &out))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg")?;

    let mut stderr = child.stderr.take().expect("piped");
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let total = (req.end - req.start).max(0.01);
    let stdout = child.stdout.take().expect("piped");
    for line in BufReader::new(stdout).lines() {
        if cancel.load(Ordering::SeqCst) {
            proc::kill_tree(&mut child);
            break;
        }
        let Ok(line) = line else { break };
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
            on_progress(((us / 1_000_000.0) / total).clamp(0.0, 1.0) as f32);
        }
    }

    let status = child.wait()?;
    let errors = err_reader.join().unwrap_or_default();
    if cancel.load(Ordering::SeqCst) {
        let _ = std::fs::remove_file(&out);
        bail!("Cancelled");
    }
    if !status.success() {
        let _ = std::fs::remove_file(&out);
        let msg = errors.lines().last().unwrap_or("ffmpeg failed").to_string();
        bail!("Export failed: {msg}");
    }
    on_progress(1.0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_video_goes_to_mp4() {
        let req = ClipRequest {
            input: PathBuf::from("Z:/nope/My video.webm"),
            start: 12.0,
            end: 45.5,
            mode: ClipMode::Exact,
            has_video: true,
        };
        let out = output_path(&req);
        assert_eq!(out.file_name().unwrap().to_string_lossy(), "My video (clip 0m12s-0m45s).mp4");
        let args = build_args(&req, &out);
        assert!(args.contains(&"libx264".to_string()));
        assert!(args.windows(2).any(|w| w[0] == "-t" && w[1] == "33.500"));
    }

    #[test]
    fn fast_keeps_container() {
        let req = ClipRequest {
            input: PathBuf::from("Z:/nope/a.mkv"),
            start: 0.0,
            end: 5.0,
            mode: ClipMode::Fast,
            has_video: true,
        };
        assert!(output_path(&req).to_string_lossy().ends_with(".mkv"));
    }
}
