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
        return Err(explain(&msg).into());
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

/// Sites people ask about most, for the "can't download from this site" message.
pub const WORKS_WITH: &str =
    "Works with YouTube, Spotify, TikTok, Instagram, X, Twitch, SoundCloud and 1,800+ other sites.";

/// Why a link can't be read, in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkProblem {
    /// What's wrong ("This video was removed or doesn't exist.").
    pub title: String,
    /// What to do about it, or what does work.
    pub hint: String,
    /// Trying again won't help (deleted, private, unsupported site…), so the
    /// Download button should be greyed out.
    pub permanent: bool,
}

impl LinkProblem {
    pub fn new(title: impl Into<String>, hint: impl Into<String>, permanent: bool) -> Self {
        Self { title: title.into(), hint: hint.into(), permanent }
    }
}

impl std::fmt::Display for LinkProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.title)
    }
}

impl std::error::Error for LinkProblem {}

/// Accept links pasted without `https://` (e.g. `youtube.com/watch?v=…`).
/// Returns `None` for text that isn't a link.
pub fn normalize_url(s: &str) -> Option<String> {
    let s = s.trim();
    if looks_like_url(s) {
        return Some(s.to_string());
    }
    if s.contains(char::is_whitespace) || s.contains("://") {
        return None;
    }
    let host = s.split(['/', '?', '#']).next().unwrap_or("");
    let tld = host.rsplit('.').next().unwrap_or("");
    let host_ok = host.contains('.')
        && !host.starts_with('.')
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && tld.len() >= 2
        && tld.bytes().all(|b| b.is_ascii_alphabetic());
    host_ok.then(|| format!("https://{s}"))
}

/// Turn yt-dlp's error text into something a person can act on.
pub fn explain(msg: &str) -> LinkProblem {
    let raw = msg.trim();
    // Drop the "[extractor] id: " prefix.
    let msg = match raw.strip_prefix('[') {
        Some(rest) => rest
            .split_once("] ")
            .map(|(_, r)| r.split_once(": ").map_or(r, |(_, m)| m))
            .unwrap_or(raw),
        None => raw,
    };
    let low = raw.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| low.contains(n));
    let gone = |t: &str| LinkProblem::new(t, "There's nothing left to download at this link.", true);
    let cookies = "Pick the browser you're signed in with under Settings → Network → Cookies. Firefox works best; Chrome and Edge usually don't.";

    if has(&["not enough space"]) {
        return LinkProblem::new(msg, "", false);
    }
    if has(&["no space left on device", "errno 28", "disk full", "not enough space on the disk"]) {
        return LinkProblem::new("Your drive ran out of space", "Free some up, or pick another folder in Settings.", false);
    }
    // Reading the browser's cookies failed (before the site was even asked).
    if has(&["cookie database", "cookies database", "failed to decrypt with dpapi", "app-bound encryption", "app bound encryption"]) {
        return LinkProblem::new(
            "Neria couldn't read your browser's cookies",
            "Chrome and Edge lock their cookies while open and encrypt them. Sign in to the site in Firefox and pick Firefox under Settings → Network → Cookies.",
            false,
        );
    }

    if has(&["unsupported url"]) {
        return LinkProblem::new("Neria can't download from this site", WORKS_WITH, true);
    }
    if has(&["[drm]", "drm protect"]) {
        return LinkProblem::new(
            "This site locks its videos (DRM), so Neria can't download them",
            "Streaming services like Netflix, Disney+ and Spotify's own audio are protected.",
            true,
        );
    }
    if has(&["is not a valid url"]) {
        return LinkProblem::new("That doesn't look like a complete link", "Copy the whole address from your browser and paste it again.", true);
    }
    if has(&["private video", "video is private"]) {
        return LinkProblem::new(
            "This video is private",
            "Only the uploader and people they shared it with can watch it.",
            true,
        );
    }
    if has(&["account associated with this video has been terminated", "account has been terminated"]) {
        return gone("This video's channel was closed");
    }
    if has(&["removed by the uploader", "has been removed", "no longer available", "video unavailable", "video is unavailable", "does not exist", "http error 404", "http error 410"]) {
        return gone("This video was removed or doesn't exist");
    }
    if has(&["not available in your country", "not made this video available in your country", "geo restrict", "geo-restrict", "not available in your region"]) {
        return LinkProblem::new("This video isn't available in your country", "The uploader limited where it can be watched.", true);
    }
    if has(&["confirm your age", "age-restricted", "age restricted", "inappropriate for some users"]) {
        return LinkProblem::new("This video is age-restricted", cookies, false);
    }
    if has(&["members-only", "members only", "join this channel"]) {
        return LinkProblem::new("This video is for channel members only", cookies, false);
    }
    if has(&["sign in to confirm", "login required", "log in", "sign in", "use --cookies", "cookies"]) {
        return LinkProblem::new("The site wants you to sign in", cookies, false);
    }
    if has(&["live event will begin", "premieres in", "this live event", "will begin in"]) {
        return LinkProblem::new("This stream hasn't started yet", "Try again once it's live.", false);
    }
    if has(&["http error 429", "too many requests", "rate-limit", "rate limit"]) {
        return LinkProblem::new("The site is getting too many requests from you", "Wait a few minutes, then try again.", false);
    }
    if has(&["getaddrinfo", "failed to resolve", "unable to download webpage", "timed out", "connection", "network is unreachable", "errno 11001", "ssl"]) {
        return LinkProblem::new("Couldn't reach the site", "Check your internet connection, then try again.", false);
    }
    if has(&["signature extraction failed", "nsig extraction failed", "please report this issue", "unable to extract"]) {
        return LinkProblem::new(
            "The site changed and the downloader needs an update",
            "Neria updates it when you download. Fixes usually land within a day or two.",
            false,
        );
    }
    let title = if msg.is_empty() { "Couldn't read this link" } else { msg };
    LinkProblem::new(title, "You can still try downloading it.", false)
}

/// yt-dlp output that usually means the site changed and a newer yt-dlp fixes it.
pub fn needs_newer_ytdlp(text: &str) -> bool {
    let low = text.to_lowercase();
    [
        "signature extraction failed",
        "nsig extraction failed",
        "n challenge solving failed",
        "unable to extract",
        "please report this issue",
        "confirm you are on the latest version",
        "update to the latest version",
        "yt-dlp is out of date",
        "your yt-dlp version",
        // An old yt-dlp next to a newer Neria that passes options it doesn't know yet.
        "error: no such option",
    ]
    .iter()
    .any(|n| low.contains(n))
}

/// One-line version of [`explain`] for queue rows.
pub fn clean_error(msg: &str) -> String {
    explain(msg).title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_links() {
        assert_eq!(normalize_url("youtube.com/watch?v=abc").as_deref(), Some("https://youtube.com/watch?v=abc"));
        assert_eq!(normalize_url(" https://x.com/a/status/1 ").as_deref(), Some("https://x.com/a/status/1"));
        assert_eq!(normalize_url("www.tiktok.com/@a/video/1").as_deref(), Some("https://www.tiktok.com/@a/video/1"));
        assert_eq!(normalize_url("hello world"), None);
        assert_eq!(normalize_url("never gonna give you up"), None);
        assert_eq!(normalize_url("file.mp4"), None);
        assert_eq!(normalize_url("v1.2"), None);
        assert_eq!(normalize_url("ftp://a.b/c"), None);
    }

    #[test]
    fn sorts_errors() {
        let p = explain("Unsupported URL: https://example.com/");
        assert!(p.permanent);
        assert_eq!(p.hint, WORKS_WITH);
        assert!(explain("[youtube] aaaaaaaaaaa: This video is unavailable").permanent);
        assert!(explain("[vimeo] 1: Unable to download webpage: HTTP Error 404: Not Found (caused by <HTTPError 404: Not Found>)").permanent);
        assert!(explain("[DRM] The requested site is known to use DRM protection. It will NOT be supported.").permanent);
        assert!(explain("[youtube] x: Private video. Sign in if you've been granted access to this video").permanent);
        let net = explain("[generic] video: Unable to download webpage: HTTPSConnection(host='a.invalid', port=443): Failed to resolve 'a.invalid' ([Errno 11001] getaddrinfo failed)");
        assert!(!net.permanent);
        assert_eq!(net.title, "Couldn't reach the site");
        assert!(!explain("[youtube] x: Sign in to confirm you’re not a bot. Use --cookies-from-browser").permanent);
        assert_eq!(explain("[youtube] x: Sign in to confirm your age. This video may be inappropriate for some users.").title, "This video is age-restricted");
        assert!(!explain("[youtube] x: HTTP Error 429: Too Many Requests").permanent);
        assert_eq!(explain("ERROR: [Errno 28] No space left on device").title, "Your drive ran out of space");
        assert_eq!(
            explain("ERROR: Could not copy Chrome cookie database. See https://github.com/yt-dlp/yt-dlp/issues/7271").title,
            "Neria couldn't read your browser's cookies"
        );
        assert_eq!(explain("ERROR: Failed to decrypt with DPAPI. See https://github.com/yt-dlp/yt-dlp/issues/10927").title, "Neria couldn't read your browser's cookies");
        assert!(needs_newer_ytdlp("[youtube] x: nsig extraction failed: You may experience throttling"));
        assert!(needs_newer_ytdlp("ERROR: [youtube] x: Unable to extract uploader id; please report this issue on https://github.com/yt-dlp/yt-dlp/issues?q= , filling out the appropriate issue template. Confirm you are on the latest version using yt-dlp -U"));
        assert!(!needs_newer_ytdlp("[youtube] x: Private video. Sign in if you've been granted access to this video"));
        assert!(!needs_newer_ytdlp("ERROR: [youtube] x: Video unavailable"));
        assert!(needs_newer_ytdlp("yt-dlp.exe: error: no such option: --js-runtimes"));
        let other = explain("[foo] 1: something odd");
        assert_eq!(other.title, "something odd");
        assert!(!other.permanent);
    }
}
