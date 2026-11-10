//! Real .gif files from the silent MP4s that X (Twitter) serves for "GIFs".

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use crate::{clip, paths, proc};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_x_gifs() {
        assert!(is_x_gif_url("https://video.twimg.com/tweet_video/HWyQRlkXMAAQ7Pg.mp4"));
        assert!(!is_x_gif_url("https://video.twimg.com/amplify_video/2120202/vid/avc1/1920x1080/a.mp4"));
        assert!(!is_x_gif_url("https://video.twimg.com/ext_tw_video/1/pu/vid/a.mp4"));
    }
}
