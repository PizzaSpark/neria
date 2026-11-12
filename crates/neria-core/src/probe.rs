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
    /// An X (Twitter) "GIF" (a short silent MP4).
    pub gif: bool,
    /// Everything in a post or playlist, for the picker. Empty for a single video.
    pub items: Vec<crate::post::Item>,
    /// Something the user should know about the items (e.g. a video Neria can't reach).
    pub note: Option<String>,
    /// `Extractor:id` of a single video, for the duplicate check.
    pub key: Option<String>,
    /// A single video's captions, audio tracks (when there's a choice) and chapters.
    pub captions: Vec<crate::tracks::Caption>,
    pub audio_tracks: Vec<crate::tracks::AudioTrack>,
    pub chapters: Vec<crate::tracks::Chapter>,
    /// A channel or playlist (something new can show up in): "Follow" is offered.
    pub followable: bool,
}

impl MediaInfo {
    /// One-line summary such as "Channel · 12:34 · up to 1080p".
    pub fn meta_line(&self) -> String {
        let mut parts = Vec::new();
        if self.gif {
            parts.push("GIF".to_string());
        }
        if !self.items.is_empty() {
            parts.push(crate::fmt::item_counts(&self.items));
        } else if let Some(n) = self.playlist_count {
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

/// Every link in pasted text (one per line, or separated by spaces or commas), in order,
/// each once. (The one-line link box turns pasted line breaks into spaces.) A link inside
/// another one (`web.archive.org/web/…/https://…`, `?q=https://…`) stays whole.
/// Spotify URIs count; `https://` is added to links typed without it.
pub fn split_links(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || c == ',') {
        let word = word.trim_matches(|c: char| matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | ';'));
        if word.is_empty() {
            continue;
        }
        let link = if word.starts_with("spotify:") { Some(word.to_string()) } else { normalize_url(word) };
        if let Some(link) = link
            && !out.contains(&link)
        {
            out.push(link);
        }
    }
    out
}

/// A YouTube channel's own link (`/@name`, `/channel/…`, `/c/…`, `/user/…`, or its Home
/// tab) lists the channel's tabs (Videos, Shorts, Live), not videos: its Videos tab
/// instead. `None` for anything else.
pub fn channel_videos_url(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    if !matches!(host.as_str(), "youtube.com" | "www.youtube.com" | "m.youtube.com") {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let base = match segs.as_slice() {
        [h, ..] if h.starts_with('@') => 1,
        ["channel" | "c" | "user", _, ..] => 2,
        _ => return None,
    };
    match segs.get(base) {
        None | Some(&"featured") => Some(format!("https://www.youtube.com/{}/videos", segs[..base].join("/"))),
        _ => None,
    }
}

pub fn probe(url: &str, settings: &Settings) -> Result<MediaInfo> {
    let videos = channel_videos_url(url);
    let url = videos.as_deref().unwrap_or(url);
    // Photos in X/Facebook posts: ask gallery-dl at the same time.
    let gallery = crate::post::wants_gallery(url).then(|| {
        let (url, settings) = (url.to_string(), settings.clone());
        std::thread::spawn(move || crate::post::gallery(&url, &settings))
    });
    let ytdlp = ytdlp_json(url, settings, &[]);
    let mut gallery = gallery.and_then(|h| h.join().ok()).unwrap_or_default();
    // gallery-dl doesn't follow Facebook `share/` links; yt-dlp knows where they lead.
    if gallery.items.is_empty()
        && url.contains("/share/")
        && let Some(real) = ytdlp.as_ref().ok().and_then(|v| v.get("webpage_url")?.as_str())
    {
        let real = real.split('?').next().unwrap_or(real);
        if real != url && crate::post::wants_gallery(real) {
            gallery = crate::post::gallery(real, settings);
        }
    }

    let v = match ytdlp {
        Ok(v) => v,
        // yt-dlp found no video, but there are photos.
        Err(_) if !gallery.items.is_empty() => {
            let title = gallery.title.clone().unwrap_or_else(|| "Post".into());
            return Ok(post_info(title, None, gallery.items, None));
        }
        Err(e) => return Err(e),
    };
    let is_playlist = v.get("_type").and_then(Value::as_str) == Some("playlist");

    let title = v.get("title").and_then(Value::as_str).unwrap_or("Untitled").to_string();
    let uploader = ["uploader", "channel", "playlist_uploader"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(str::to_string);
    let formats = v.get("formats").and_then(Value::as_array);
    let max_height = formats.and_then(|fs| fs.iter().filter_map(|f| f.get("height")?.as_u64()).max()).map(|h| h as u32);

    // A single "video" without any formats: yt-dlp looked at a post and found no video.
    let no_video = !is_playlist && formats.is_none_or(|f| f.is_empty()) && v.get("url").is_none();
    if no_video {
        if gallery.items.is_empty() {
            return Err(no_video_problem().into());
        }
        let title = gallery.title.clone().unwrap_or(title);
        let note = "Neria couldn't get a video from this post, only its photos. If it has a video, open the video itself and copy its link.";
        return Ok(post_info(title, uploader, gallery.items, Some(note.into())));
    }

    let items =
        crate::post::from_ytdlp(&v, url).map(|y| crate::post::merge(y, gallery.items.clone())).unwrap_or_default();
    // One video plus photos only gallery-dl saw (X post with a photo and a video).
    let items = if items.is_empty() && gallery.items.iter().any(|i| i.kind == crate::post::ItemKind::Photo) {
        let gif = v.get("url").and_then(Value::as_str).is_some_and(crate::gif::is_x_gif_url);
        let single = crate::post::Item {
            kind: if gif { crate::post::ItemKind::Gif } else { crate::post::ItemKind::Video },
            title: String::new(),
            duration: v.get("duration").and_then(Value::as_f64),
            thumbnail: pick_thumbnail(&v),
            url: url.to_string(),
            playlist_index: None,
            direct_url: None,
            ext: String::new(),
        };
        crate::post::merge(vec![single], gallery.items)
    } else {
        items
    };

    Ok(MediaInfo {
        title,
        uploader,
        duration: v.get("duration").and_then(Value::as_f64),
        thumbnail_url: pick_thumbnail(&v).or_else(|| items.iter().find_map(|i| i.thumbnail.clone())),
        max_height,
        playlist_count: is_playlist.then(|| v.get("entries").and_then(Value::as_array).map_or(0, |e| e.len())),
        gif: v.get("url").and_then(Value::as_str).is_some_and(crate::gif::is_x_gif_url),
        key: (!is_playlist)
            .then(|| {
                let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("");
                crate::download::media_key(s("extractor_key"), s("id"))
            })
            .flatten(),
        captions: if is_playlist { Vec::new() } else { crate::tracks::captions(&v) },
        audio_tracks: if is_playlist { Vec::new() } else { crate::tracks::audio_tracks(&v) },
        chapters: if is_playlist { Vec::new() } else { crate::tracks::chapters(&v) },
        // Channels and playlists (their items have titles); not a post's photos.
        followable: is_playlist && crate::post::is_playlist(&items),
        items,
        note: None,
    })
}

/// What a followed channel or playlist holds now, for its check: the newest `newest`
/// entries (channels list newest first), or all of them. Upcoming premieres and streams
/// that haven't started are left out (they'd only fail; they're picked up once they're on).
pub fn list_entries(url: &str, settings: &Settings, newest: Option<usize>) -> Result<Vec<crate::post::Item>> {
    let end = newest.map(|n| n.to_string());
    let mut extra = vec!["--yes-playlist"];
    if let Some(end) = &end {
        extra.extend(["--playlist-end", end.as_str()]);
    }
    let mut v = ytdlp_json(url, settings, &extra)?;
    if let Some(entries) = v.get_mut("entries").and_then(Value::as_array_mut) {
        entries.retain(|e| !matches!(e.get("live_status").and_then(Value::as_str), Some("is_upcoming" | "is_live")));
    }
    Ok(crate::post::from_ytdlp(&v, url).unwrap_or_default())
}

/// A video link that's also in a playlist (`watch?v=…&list=…`): the playlist's own link,
/// so the preview can offer "Get the whole playlist". `None` for anything else.
pub fn playlist_link(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let youtube = ["youtube.com/", "youtu.be/", "music.youtube.com/"].iter().any(|h| lower.contains(h));
    if !youtube || lower.contains("/playlist?") {
        return None;
    }
    let query = url.split_once('?')?.1.split('#').next()?;
    let list = query.split('&').find_map(|p| p.strip_prefix("list="))?;
    // Mixes (RD…) are endless radio lists made up per person, not playlists.
    if list.is_empty() || list.starts_with("RD") {
        return None;
    }
    let host = if lower.contains("music.youtube.com") { "music.youtube.com" } else { "www.youtube.com" };
    Some(format!("https://{host}/playlist?list={list}"))
}

fn post_info(
    title: String,
    uploader: Option<String>,
    items: Vec<crate::post::Item>,
    note: Option<String>,
) -> MediaInfo {
    MediaInfo {
        title,
        uploader,
        thumbnail_url: items.iter().find_map(|i| i.thumbnail.clone()),
        playlist_count: Some(items.len()),
        items,
        note,
        ..MediaInfo::default()
    }
}

fn no_video_problem() -> LinkProblem {
    LinkProblem::new(
        "Neria couldn't find a video in this post",
        "If it has one, open the video itself and copy its link. Posts you only see when signed in need you to sign in.",
        false,
    )
    .with_sign_in()
}

/// yt-dlp's `-J` for the link. Photos (Instagram) and posts without a video come back as
/// items without formats instead of failing the whole lookup.
fn ytdlp_json(url: &str, settings: &Settings, extra: &[&str]) -> Result<Value> {
    let mut cmd = proc::command(paths::tool("yt-dlp"));
    cmd.args(["-J", "--encoding", "utf-8", "--no-warnings", "--flat-playlist", "--ignore-no-formats-error"]);
    cmd.arg(if settings.allow_playlists { "--yes-playlist" } else { "--no-playlist" });
    cmd.args(extra);
    cmd.args(crate::cookies::args_for(url, settings));
    cmd.arg("--").arg(url);

    let out = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output().context("failed to start yt-dlp")?;

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
    serde_json::from_slice(&out.stdout).context("unexpected yt-dlp output")
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
    cmd.args(crate::cookies::args_for(url, settings));
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
pub(crate) fn pick_thumbnail(v: &Value) -> Option<String> {
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
    // YouTube playlist thumbnails (`hqdefault.jpg?sqp=…`) come back as WebP despite the
    // name; without the query it's a real JPEG.
    let url = if url.contains("i.ytimg.com/") { clean } else { url };
    let ext = match clean.rsplit('.').next() {
        Some(e) if ["jpg", "jpeg", "png", "webp"].contains(&e) => e,
        _ => "jpg",
    };
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let path = paths::cache_dir().join(format!("thumb-{hash:016x}.{ext}"));
    // Saved before, maybe under the type it really turned out to be.
    if let Some(p) = [path.clone(), path.with_extension("webp")].into_iter().find(|p| p.is_file()) {
        return Ok(p);
    }
    let bytes = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?
        .get(url)
        .send()?
        .error_for_status()?
        .bytes()?;
    // Name it by what it is, not what the address says (image loaders go by the name).
    let path = if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        path.with_extension("webp")
    } else {
        path
    };
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
    /// Signing in to the site may help (private, age-restricted, members-only, login wall):
    /// the preview offers "Sign in to …".
    pub sign_in: bool,
}

impl LinkProblem {
    pub fn new(title: impl Into<String>, hint: impl Into<String>, permanent: bool) -> Self {
        Self { title: title.into(), hint: hint.into(), permanent, sign_in: false }
    }

    fn with_sign_in(mut self) -> Self {
        self.sign_in = true;
        self
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
        Some(rest) => rest.split_once("] ").map(|(_, r)| r.split_once(": ").map_or(r, |(_, m)| m)).unwrap_or(raw),
        None => raw,
    };
    let low = raw.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| low.contains(n));
    let gone = |t: &str| LinkProblem::new(t, "There's nothing left to download at this link.", true);
    let cookies = "Sign in to the site and Neria can get it. Your password goes to the site, never to Neria.";

    if has(&["not enough space"]) {
        return LinkProblem::new(msg, "", false);
    }
    if has(&["no space left on device", "errno 28", "disk full", "not enough space on the disk"]) {
        return LinkProblem::new(
            "Your drive ran out of space",
            "Free some up, or pick another folder in Settings → Download.",
            false,
        );
    }
    // Reading the browser's cookies failed (before the site was even asked).
    if has(&[
        "cookie database",
        "cookies database",
        "failed to decrypt with dpapi",
        "app-bound encryption",
        "app bound encryption",
    ]) {
        return LinkProblem::new(
            "Neria couldn't read your browser's cookies",
            "Chrome and Edge encrypt them, and Brave, Opera and Vivaldi lock them while open. Easier: sign in to the site in Neria instead.",
            false,
        )
        .with_sign_in();
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
        return LinkProblem::new(
            "That doesn't look like a complete link",
            "Copy the whole address from your browser and paste it again.",
            true,
        );
    }
    if has(&["private video", "video is private"]) {
        return LinkProblem::new(
            "This video is private",
            "Only the uploader and people they shared it with can watch it. Shared with you? Sign in.",
            true,
        )
        .with_sign_in();
    }
    if has(&["account associated with this video has been terminated", "account has been terminated"]) {
        return gone("This video's channel was closed");
    }
    if has(&[
        "removed by the uploader",
        "has been removed",
        "no longer available",
        "video unavailable",
        "video is unavailable",
        "does not exist",
        "http error 404",
        "http error 410",
    ]) {
        return gone("This video was removed or doesn't exist");
    }
    if has(&[
        "not available in your country",
        "not made this video available in your country",
        "geo restrict",
        "geo-restrict",
        "not available in your region",
    ]) {
        return LinkProblem::new(
            "This video isn't available in your country",
            "The uploader limited where it can be watched.",
            true,
        );
    }
    if has(&["confirm your age", "age-restricted", "age restricted", "inappropriate for some users"]) {
        return LinkProblem::new("This video is age-restricted", cookies, false).with_sign_in();
    }
    if has(&["members-only", "members only", "join this channel"]) {
        return LinkProblem::new("This video is for channel members only", cookies, false).with_sign_in();
    }
    // Instagram after a few lookups without an account: it sends you to the login page.
    if has(&["exceeded the rate-limit", "rate-limit for accessing"]) {
        return LinkProblem::new(
            "The site is limiting how much you can see without signing in",
            "Wait an hour or so, or sign in.",
            false,
        )
        .with_sign_in();
    }
    if has(&["sign in to confirm", "login required", "log in", "sign in", "use --cookies", "cookies"]) {
        return LinkProblem::new("The site wants you to sign in", cookies, false).with_sign_in();
    }
    if has(&["live event will begin", "premieres in", "this live event", "will begin in"]) {
        return LinkProblem::new("This stream hasn't started yet", "Try again once it's live.", false);
    }
    if has(&["http error 429", "too many requests", "rate-limit", "rate limit"]) {
        return LinkProblem::new(
            "The site is getting too many requests from you",
            "Wait a few minutes, then try again.",
            false,
        );
    }
    if has(&[
        "getaddrinfo",
        "failed to resolve",
        "unable to download webpage",
        "timed out",
        "connection",
        "network is unreachable",
        "errno 11001",
        "ssl",
    ]) {
        return LinkProblem::new("Couldn't reach the site", "Check your internet connection, then try again.", false);
    }
    if is_no_video(&low) {
        return no_video_problem();
    }
    if has(&["signature extraction failed", "nsig extraction failed", "please report this issue", "unable to extract"])
    {
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
    // A post without a video says "please report this issue" too; a new yt-dlp won't help.
    if is_no_video(&low) {
        return false;
    }
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

/// yt-dlp looked at a post and found no video in it (photos only, or a video it can't reach).
fn is_no_video(low: &str) -> bool {
    ["no video formats found", "no video could be found", "no media found"].iter().any(|n| low.contains(n))
}

/// One-line version of [`explain`] for queue rows.
pub fn clean_error(msg: &str) -> String {
    explain(msg).title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_pasted_links() {
        let text = "https://youtu.be/a\n  https://vimeo.com/1, youtube.com/watch?v=b\r\nhello\nhttps://youtu.be/a";
        assert_eq!(split_links(text), ["https://youtu.be/a", "https://vimeo.com/1", "https://youtube.com/watch?v=b"]);
        // A link inside a link is one link.
        let archived = "https://web.archive.org/web/2020/https://www.youtube.com/watch?v=a";
        assert_eq!(split_links(archived), [archived]);
        let redirect = "https://www.google.com/url?q=https://youtu.be/a&sa=D";
        assert_eq!(split_links(redirect), [redirect]);
        assert_eq!(split_links("<https://youtu.be/a> spotify:track:1"), ["https://youtu.be/a", "spotify:track:1"]);
        assert_eq!(split_links("https://youtu.be/a"), ["https://youtu.be/a"]);
        assert!(split_links("just some words").is_empty());
    }

    #[test]
    fn playlist_of_a_video_link() {
        assert_eq!(
            playlist_link("https://www.youtube.com/watch?v=6JeAnrziLLo&list=PLu2Oeu&index=2").as_deref(),
            Some("https://www.youtube.com/playlist?list=PLu2Oeu")
        );
        assert_eq!(playlist_link("https://www.youtube.com/watch?v=6JeAnrziLLo"), None);
        assert_eq!(playlist_link("https://www.youtube.com/playlist?list=PLu2Oeu"), None);
        // Mixes aren't real playlists.
        assert_eq!(playlist_link("https://www.youtube.com/watch?v=a&list=RDa"), None);
        assert_eq!(playlist_link("https://vimeo.com/1?list=x"), None);
    }

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
        assert!(
            explain("[DRM] The requested site is known to use DRM protection. It will NOT be supported.").permanent
        );
        assert!(explain("[youtube] x: Private video. Sign in if you've been granted access to this video").permanent);
        let net = explain(
            "[generic] video: Unable to download webpage: HTTPSConnection(host='a.invalid', port=443): Failed to resolve 'a.invalid' ([Errno 11001] getaddrinfo failed)",
        );
        assert!(!net.permanent);
        assert_eq!(net.title, "Couldn't reach the site");
        assert!(!explain("[youtube] x: Sign in to confirm you’re not a bot. Use --cookies-from-browser").permanent);
        assert_eq!(
            explain("[youtube] x: Sign in to confirm your age. This video may be inappropriate for some users.").title,
            "This video is age-restricted"
        );
        assert!(!explain("[youtube] x: HTTP Error 429: Too Many Requests").permanent);
        assert_eq!(explain("ERROR: [Errno 28] No space left on device").title, "Your drive ran out of space");
        assert_eq!(
            explain("ERROR: Could not copy Chrome cookie database. See https://github.com/yt-dlp/yt-dlp/issues/7271")
                .title,
            "Neria couldn't read your browser's cookies"
        );
        assert_eq!(
            explain("ERROR: Failed to decrypt with DPAPI. See https://github.com/yt-dlp/yt-dlp/issues/10927").title,
            "Neria couldn't read your browser's cookies"
        );
        assert!(needs_newer_ytdlp("[youtube] x: nsig extraction failed: You may experience throttling"));
        assert!(needs_newer_ytdlp(
            "ERROR: [youtube] x: Unable to extract uploader id; please report this issue on https://github.com/yt-dlp/yt-dlp/issues?q= , filling out the appropriate issue template. Confirm you are on the latest version using yt-dlp -U"
        ));
        assert!(!needs_newer_ytdlp("[youtube] x: Private video. Sign in if you've been granted access to this video"));
        assert!(!needs_newer_ytdlp("ERROR: [youtube] x: Video unavailable"));
        assert!(needs_newer_ytdlp("yt-dlp.exe: error: no such option: --js-runtimes"));
        assert!(explain("[youtube] x: Sign in to confirm your age.").sign_in);
        assert!(!explain("[youtube] x: Video unavailable").sign_in);
        let other = explain("[foo] 1: something odd");
        assert_eq!(other.title, "something odd");
        assert!(!other.permanent);
    }
}
