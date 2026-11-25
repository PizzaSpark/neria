//! GIFs: real .gif files from the silent MP4s that X (Twitter) serves for "GIFs", and the
//! Clip page's "Make a GIF" (GIF, animated WebP or a GIF-like MP4 from a selection).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{clip, paths, proc, sound};

/// X serves every "GIF" from here, as a short silent MP4; real videos come from
/// `ext_tw_video/` or `amplify_video/`.
pub fn is_x_gif_url(url: &str) -> bool {
    url.contains("video.twimg.com/tweet_video/")
}

/// The ffmpeg filter: cap the width at 720 px (GIFs get huge), then a palette made for this
/// clip so colours stay clean, re-used only where frames change.
const FILTER: &str = "scale='min(720,iw)':-2:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle";

/// Turn `video` into a looping .gif next to it. Returns the new file; `video` is left as is.
pub fn from_video(video: &Path) -> Result<PathBuf> {
    let dir = video.parent().unwrap_or(Path::new("."));
    let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "gif".into());
    let out = clip::unique_path(dir, &stem, "gif");
    let result = proc::command(paths::tool("ffmpeg"))
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video)
        .args(["-an", "-filter_complex", FILTER, "-loop", "0"])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("failed to start ffmpeg")?;
    if !result.status.success() || !out.is_file() {
        let _ = std::fs::remove_file(&out);
        let err = String::from_utf8_lossy(&result.stderr);
        bail!("{}", err.lines().last().unwrap_or("ffmpeg failed").trim());
    }
    Ok(out)
}

/// What "Make a GIF" saves: a real GIF, an animated WebP (much smaller, plays in browsers
/// and Discord), or a short silent MP4 (smallest; what sites call a GIF anyway).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Gif,
    Webp,
    Mp4,
}

impl Kind {
    pub fn ext(self) -> &'static str {
        match self {
            Kind::Gif => "gif",
            Kind::Webp => "webp",
            Kind::Mp4 => "mp4",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GifSpec {
    pub kind: Kind,
    /// Widest it gets, in pixels (0 = as it is; never made bigger).
    pub width: u32,
    /// Frames per second (0 = as it is).
    pub fps: f32,
    /// Plays over and over (else once). MP4s loop as the player decides.
    pub loop_forever: bool,
    /// Turned, mirrored, cropped first (Clip page → Picture).
    pub picture: crate::picture::Picture,
}

impl Default for GifSpec {
    fn default() -> Self {
        Self { kind: Kind::Gif, width: 480, fps: 15.0, loop_forever: true, picture: Default::default() }
    }
}

/// The picture changes, frame rate and size, shared by every kind.
fn base_filter(spec: &GifSpec) -> Vec<String> {
    let mut f: Vec<String> = spec.picture.filter().into_iter().collect();
    if spec.fps > 0.0 {
        f.push(format!("fps={}", spec.fps));
    }
    if spec.width > 0 {
        f.push(format!("scale='min({},iw)':-2:flags=lanczos", spec.width));
    } else if spec.kind == Kind::Mp4 {
        // H.264 needs even sides.
        f.push("scale=trunc(iw/2)*2:trunc(ih/2)*2".into());
    }
    f
}

/// The ffmpeg arguments for `dur` seconds of `input` from `start`.
pub fn build_args(input: &Path, start: f64, dur: f64, spec: &GifSpec, out: &Path) -> Vec<String> {
    let mut a: Vec<String> =
        ["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y"].map(String::from).to_vec();
    if start > 0.0 {
        a.extend(["-ss".into(), format!("{start:.3}")]);
    }
    a.extend(["-i".into(), input.to_string_lossy().into_owned()]);
    if dur > 0.0 {
        a.extend(["-t".into(), format!("{dur:.3}")]);
    }
    a.extend(["-an", "-sn", "-dn"].map(String::from));
    let base = base_filter(spec);
    if spec.kind != Kind::Gif {
        // `V` = real video only (skips embedded cover art).
        a.extend(["-map".into(), "0:V:0".into()]);
    }
    match spec.kind {
        Kind::Gif => {
            // A palette made for this clip, re-used only where frames change (clean colours).
            let mut chain = String::from("[0:V:0]");
            chain.push_str(&base.join(","));
            if !base.is_empty() {
                chain.push(',');
            }
            chain.push_str(
                "split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle[g]",
            );
            a.extend(["-filter_complex".into(), chain, "-map".into(), "[g]".into()]);
            a.extend(["-loop".into(), if spec.loop_forever { "0" } else { "-1" }.into()]);
        }
        Kind::Webp => {
            if !base.is_empty() {
                a.extend(["-vf".into(), base.join(",")]);
            }
            a.extend(
                ["-c:v", "libwebp_anim", "-lossless", "0", "-q:v", "70", "-compression_level", "4"].map(String::from),
            );
            a.extend(["-loop".into(), if spec.loop_forever { "0" } else { "1" }.into()]);
        }
        Kind::Mp4 => {
            let mut vf = base;
            vf.push("format=yuv420p".into());
            a.extend(["-vf".into(), vf.join(",")]);
            a.extend(crate::gpu::h264_args(23));
            a.extend(["-movflags", "+faststart"].map(String::from));
        }
    }
    a.push(out.to_string_lossy().into_owned());
    a
}

/// `video.mp4` -> `video (clip 0m12s-0m15s).gif`, never overwriting.
pub fn output_path(input: &Path, start: f64, end: f64, kind: Kind) -> PathBuf {
    let dir = input.parent().unwrap_or(Path::new("."));
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "gif".into());
    let base = format!("{stem} (clip {}-{})", clip::stamp(start), clip::stamp(end));
    clip::unique_path(dir, &base, kind.ext())
}

/// Make it from the selection. `on_progress` gets 0..1. Returns the new file.
pub fn make(
    input: &Path,
    start: f64,
    end: f64,
    spec: &GifSpec,
    cancel: &AtomicBool,
    on_progress: impl FnMut(f32),
) -> Result<PathBuf> {
    if end <= start {
        bail!("The clip is empty: set the out point after the in point.");
    }
    let out = output_path(input, start, end, spec.kind);
    let dur = end - start;
    let mut on_progress = on_progress;
    if let Err(e) = crate::gpu::or_on_cpu(|| {
        sound::run_ffmpeg(&build_args(input, start, dur, spec, &out), dur, cancel, &mut on_progress)
    }) {
        let _ = std::fs::remove_file(&out);
        if e.to_string() == "Cancelled" {
            bail!("Cancelled");
        }
        bail!("Couldn't make the {}: {e}", what(spec.kind));
    }
    Ok(out)
}

/// "GIF" / "WebP" / "MP4", for messages.
pub fn what(kind: Kind) -> &'static str {
    match kind {
        Kind::Gif => "GIF",
        Kind::Webp => "WebP",
        Kind::Mp4 => "MP4",
    }
}

/// About how big it'll be, in bytes: a few seconds from the middle of the selection are
/// made for real and scaled up (a GIF's size depends on what moves, so a formula would lie).
pub fn estimate(input: &Path, start: f64, end: f64, spec: &GifSpec, cancel: &AtomicBool) -> Result<u64> {
    let dur = end - start;
    if dur <= 0.0 {
        bail!("empty selection");
    }
    let sample = dur.min(3.0);
    let from = start + (dur - sample) / 2.0;
    let dir = paths::cache_dir();
    std::fs::create_dir_all(&dir)?;
    // Its own name: a stopped estimate may still be cleaning up when the next one starts.
    static RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let run = RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let tmp = dir.join(format!("estimate-{}-{run}.{}", std::process::id(), spec.kind.ext()));
    let result = crate::gpu::or_on_cpu(|| {
        sound::run_ffmpeg(&build_args(input, from, sample, spec, &tmp), sample, cancel, |_| {})
    });
    let size = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_file(&tmp);
    result?;
    if size == 0 {
        bail!("nothing came out");
    }
    Ok((size as f64 * dur / sample) as u64)
}

/// A link clip downloaded as video, turned into `spec` next to it. The video is removed;
/// returns the new file.
pub fn convert_in_place(
    video: &Path,
    spec: &GifSpec,
    cancel: &AtomicBool,
    on_progress: impl FnMut(f32),
) -> Result<PathBuf> {
    let dir = video.parent().unwrap_or(Path::new("."));
    let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "gif".into());
    let same_ext = video.extension().is_some_and(|e| e.eq_ignore_ascii_case(spec.kind.ext()));
    // An MP4 made from an .mp4 takes its place under the same name.
    let out = if same_ext {
        dir.join(format!("{stem}.neria-gif.{}", spec.kind.ext()))
    } else {
        clip::unique_path(dir, &stem, spec.kind.ext())
    };
    let dur = crate::media::file_info(video).map(|i| i.duration).unwrap_or(0.0);
    if let Err(e) = sound::run_ffmpeg(&build_args(video, 0.0, 0.0, spec, &out), dur, cancel, on_progress) {
        let _ = std::fs::remove_file(&out);
        if e.to_string() == "Cancelled" {
            bail!("Cancelled");
        }
        bail!("Couldn't make the {}: {e}", what(spec.kind));
    }
    std::fs::remove_file(video)?;
    if same_ext {
        std::fs::rename(&out, video)?;
        return Ok(video.to_path_buf());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_gets_its_own_palette_and_loop() {
        let spec = GifSpec::default();
        let a = build_args(Path::new("Z:/a.mp4"), 2.0, 3.0, &spec, Path::new("Z:/a.gif"));
        let fc = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(fc.starts_with("[0:V:0]fps=15,scale='min(480,iw)'"), "{fc}");
        assert!(fc.contains("palettegen") && fc.contains("paletteuse"));
        assert!(a.windows(2).any(|w| w[0] == "-loop" && w[1] == "0"));
        let once = GifSpec { loop_forever: false, width: 0, fps: 0.0, ..spec };
        let a = build_args(Path::new("Z:/a.gif"), 0.0, 3.0, &once, Path::new("Z:/b.gif"));
        let fc = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(fc.starts_with("[0:V:0]split"), "{fc}");
        assert!(a.windows(2).any(|w| w[0] == "-loop" && w[1] == "-1"));
    }

    #[test]
    fn webp_and_mp4() {
        let w = GifSpec { kind: Kind::Webp, ..Default::default() };
        let a = build_args(Path::new("Z:/a.mp4"), 0.0, 3.0, &w, Path::new("Z:/a.webp"));
        assert!(a.contains(&"libwebp_anim".to_string()));
        let m = GifSpec { kind: Kind::Mp4, width: 0, ..Default::default() };
        let a = build_args(Path::new("Z:/a.mp4"), 0.0, 3.0, &m, Path::new("Z:/a2.mp4"));
        let vf = &a[a.iter().position(|x| x == "-vf").unwrap() + 1];
        assert_eq!(vf, "fps=15,scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p");
        assert!(a.contains(&"-an".to_string()));
        assert_eq!(
            output_path(Path::new("Z:/nope/cat.mp4"), 1.0, 4.0, Kind::Webp).file_name().unwrap().to_string_lossy(),
            "cat (clip 0m01s-0m04s).webp"
        );
    }

    #[test]
    fn spots_x_gifs() {
        assert!(is_x_gif_url("https://video.twimg.com/tweet_video/HWyQRlkXMAAQ7Pg.mp4"));
        assert!(!is_x_gif_url("https://video.twimg.com/amplify_video/2120202/vid/avc1/1920x1080/a.mp4"));
        assert!(!is_x_gif_url("https://video.twimg.com/ext_tw_video/1/pu/vid/a.mp4"));
    }
}
