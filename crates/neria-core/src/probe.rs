//! Fast metadata lookup for a URL (title, thumbnail, duration…).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::settings::Settings;
use crate::{fmt, paths, proc};

#[derive(Clone, Debug, Default)]
pub struct MediaInfo {
    pub title: String,
    pub uploader: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail_url: Option<String>,
    pub max_height: Option<u32>,
    pub playlist_count: Option<usize>,
}

impl MediaInfo {
    /// One-line summary such as "Channel · 12:34 · up to 1080p".
    pub fn meta_line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(n) = self.playlist_count {
            parts.push(format!("Playlist · {n} items"));
        }
        if let Some(u) = &self.uploader {
            parts.push(u.clone());
        }
        if let Some(d) = self.duration {
            parts.push(fmt::duration(d));
        }
        if let Some(h) = self.max_height {
            parts.push(format!("up to {h}p"));
        }
        parts.join("  ·  ")
    }
}

pub fn looks_like_url(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("http://") || s.starts_with("https://")) && s.len() > 10 && !s.contains(char::is_whitespace)
}

pub fn probe(url: &str, settings: &Settings) -> Result<MediaInfo> {
    let mut cmd = proc::command(paths::tool("yt-dlp"));
    cmd.args(["-J", "--encoding", "utf-8", "--no-warnings", "--flat-playlist"]);
    cmd.arg(if settings.allow_playlists { "--yes-playlist" } else { "--no-playlist" });
    if !settings.cookies_browser.is_empty() {
        cmd.args(["--cookies-from-browser", &settings.cookies_browser]);
    }
    cmd.arg("--").arg(url);

    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to start yt-dlp")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = stderr
            .lines()
            .rev()
            .find_map(|l| l.strip_prefix("ERROR: "))
            .unwrap_or("Couldn't read this link")
            .to_string();
        return Err(anyhow!(clean_error(&msg)));
    }

    let v: Value = serde_json::from_slice(&out.stdout).context("unexpected yt-dlp output")?;
    let is_playlist = v.get("_type").and_then(Value::as_str) == Some("playlist");

    let title = v.get("title").and_then(Value::as_str).unwrap_or("Untitled").to_string();
    let uploader = ["uploader", "channel", "playlist_uploader"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(str::to_string);
    let max_height = v
        .get("formats")
        .and_then(Value::as_array)
        .and_then(|fs| fs.iter().filter_map(|f| f.get("height")?.as_u64()).max())
        .map(|h| h as u32);

    Ok(MediaInfo {
        title,
        uploader,
        duration: v.get("duration").and_then(Value::as_f64),
        thumbnail_url: pick_thumbnail(&v),
        max_height,
        playlist_count: is_playlist.then(|| {
            v.get("entries").and_then(Value::as_array).map_or(0, |e| e.len())
        }),
    })
}

pub struct PreviewStream {
    pub duration: f64,
    pub title: String,
    /// Direct URL of a small, seekable video stream (expires after a while).
    pub stream_url: String,
}

/// Title, duration and a low-res stream URL, used to build the clipper's
/// filmstrip for a link without downloading it.
pub fn preview_stream(url: &str, settings: &Settings) -> Result<PreviewStream> {
    let mut cmd = proc::command(paths::tool("yt-dlp"));
    cmd.args(["--no-warnings", "--encoding", "utf-8", "--no-playlist"]);
    cmd.args(["-f", "bv*[height<=240][protocol^=http]/wv*[protocol^=http]/b[protocol^=http]/w"]);
    // URLs never contain '|', titles can: keep the title last.
    cmd.args(["--print", "%(duration)s|%(url)s|%(title)s"]);
    if !settings.cookies_browser.is_empty() {
        cmd.args(["--cookies-from-browser", &settings.cookies_browser]);
    }
    cmd.arg("--").arg(url);
    let out = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains('|')).ok_or_else(|| anyhow!("no preview stream"))?;
    let mut parts = line.splitn(3, '|');
    let duration: f64 = parts.next().unwrap_or("").trim().parse().map_err(|_| anyhow!("unknown duration"))?;
    let stream_url = parts.next().unwrap_or("").trim().to_string();
    let title = parts.next().unwrap_or("").trim().to_string();
    Ok(PreviewStream { duration, title, stream_url })
}

/// Prefer a JPEG around 320–640px wide: small, fast, and decodable everywhere.
fn pick_thumbnail(v: &Value) -> Option<String> {
    let list = v.get("thumbnails").and_then(Value::as_array);
    if let Some(list) = list {
        let mut jpgs: Vec<(u64, &str)> = list
            .iter()
            .filter_map(|t| {
                let url = t.get("url")?.as_str()?;
                let path = url.split('?').next().unwrap_or(url);
                (path.ends_with(".jpg") || path.ends_with(".jpeg"))
                    .then(|| (t.get("width").and_then(Value::as_u64).unwrap_or(0), url))
            })
            .collect();
        jpgs.sort_by_key(|(w, _)| *w);
        if let Some((_, url)) = jpgs.iter().find(|(w, _)| *w >= 320).or(jpgs.last()) {
            return Some(url.to_string());
        }
    }
    v.get("thumbnail").and_then(Value::as_str).map(str::to_string).or_else(|| {
        // Playlists: use the first entry's thumbnail.
        v.get("entries")?.as_array()?.first().and_then(pick_thumbnail)
    })
}

/// Download a thumbnail into the cache directory and return its path.
pub fn fetch_thumbnail(url: &str) -> Result<PathBuf> {
    let clean = url.split('?').next().unwrap_or(url);
    let ext = match clean.rsplit('.').next() {
        Some(e) if ["jpg", "jpeg", "png", "webp"].contains(&e) => e,
        _ => "jpg",
    };
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let path = paths::cache_dir().join(format!("thumb-{hash:016x}.{ext}"));
    if path.is_file() {
        return Ok(path);
    }
    let bytes = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?
        .get(url)
        .send()?
        .error_for_status()?
        .bytes()?;
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

/// Turn yt-dlp's error text into something a person can act on.
pub fn clean_error(msg: &str) -> String {
    let msg = msg.trim();
    // Drop the "[extractor] id: " prefix.
    let msg = match msg.strip_prefix('[') {
        Some(rest) => rest
            .split_once("] ")
            .map(|(_, r)| r.split_once(": ").map_or(r, |(_, m)| m))
            .unwrap_or(msg),
        None => msg,
    };
    if msg.contains("Sign in to confirm") {
        return "YouTube wants a sign-in. Pick a browser under Advanced → Cookies.".into();
    }
    if msg.contains("Unsupported URL") {
        return "This site isn't supported.".into();
    }
    if msg.contains("Private video") {
        return "This video is private.".into();
    }
    msg.to_string()
}
