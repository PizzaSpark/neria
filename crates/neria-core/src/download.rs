//! Running a single yt-dlp download and turning its output into events.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::music::MusicJob;
use crate::settings::{Preset, Quality, Settings};
use crate::{paths, proc};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadRequest {
    pub url: String,
    pub quality: Quality,
    /// Shown until yt-dlp reports the real title.
    pub title_hint: String,
    /// Only download this time range (seconds).
    pub section: Option<(f64, f64)>,
    /// Re-encode around the cuts so the section is frame-exact
    /// (otherwise it starts on the keyframe before `section.0`).
    pub exact_cuts: bool,
    /// Spotify track: matched on YouTube Music first (`url` is filled in then),
    /// saved under a fixed name, and tagged afterwards.
    pub music: Option<Box<MusicJob>>,
    /// One item of a post/playlist: its 1-based position (`--playlist-items`).
    pub playlist_index: Option<usize>,
    /// Save here instead of the Audio/Video folder (items picked together share a folder).
    pub dir: Option<PathBuf>,
    /// Fixed file name without extension (`02 Video`), instead of the filename template.
    pub stem: Option<String>,
    /// A photo (or a video yt-dlp can't see): this file address is saved as is, with `ext`.
    pub direct: Option<(String, String)>,
    /// Known up front to be an X GIF (from the item picker).
    pub gif: bool,
    /// Picture for the queue row until yt-dlp reports one (photos never do).
    pub thumbnail: Option<String>,
    /// Length in seconds, if known up front (for the playlist file).
    pub duration: Option<f64>,
    /// `Extractor:id` (e.g. `Youtube:dQw4w9WgXcQ`), to spot the same video again.
    pub key: Option<String>,
    /// "Download again": overwrite a file of the same name instead of keeping it.
    pub force: bool,
    /// Captions to save with it (or instead of it).
    pub captions: Option<crate::tracks::CaptionChoice>,
    /// Audio tracks by language (empty = the default one). Video: several become several
    /// tracks in one file. Audio only: the first one.
    pub audio_langs: Vec<String>,
    /// Added to the file name, e.g. " (Spanish)" for one language of several.
    pub name_tag: Option<String>,
    /// Chapters as one file: these ranges are downloaded and joined (neighbours already
    /// merged). Needs two or more; one range goes in `section`.
    pub sections: Vec<(f64, f64)>,
    /// Tags for a file made from one chapter: its title and number.
    pub meta_title: Option<String>,
    pub track: Option<usize>,
    /// "Download later": waits until this time (Unix seconds), shown as `start_label`.
    pub start_at: Option<u64>,
    pub start_label: String,
    /// This item's own container (video) or audio format, instead of Settings' (picker).
    pub format: Option<String>,
    /// Best quality / Compatible / Smallest file (Audio only is `quality`).
    pub preset: Preset,
}

impl Default for DownloadRequest {
    fn default() -> Self {
        Self::new("", Quality::Best, "")
    }
}

impl DownloadRequest {
    pub fn new(url: impl Into<String>, quality: Quality, title_hint: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            quality,
            title_hint: title_hint.into(),
            section: None,
            exact_cuts: false,
            music: None,
            playlist_index: None,
            dir: None,
            stem: None,
            direct: None,
            gif: false,
            thumbnail: None,
            duration: None,
            key: None,
            force: false,
            captions: None,
            audio_langs: Vec::new(),
            name_tag: None,
            sections: Vec::new(),
            meta_title: None,
            track: None,
            start_at: None,
            start_label: String::new(),
            format: None,
            preset: Preset::Best,
        }
    }

    /// Only the captions are saved (no video file).
    pub fn captions_only(&self) -> bool {
        self.captions.as_ref().is_some_and(|c| c.only && !c.is_empty())
    }

    pub fn music(job: MusicJob) -> Self {
        let title = format!("{} - {}", job.track.artist_line(), job.track.title);
        Self { music: Some(Box::new(job)), ..Self::new("", Quality::Audio, title) }
    }
}

#[derive(Clone, Debug)]
pub enum DlEvent {
    /// Video title, how many streams will be downloaded (video+audio = 2)
    /// and the thumbnail URL, if the site has one.
    Info {
        title: String,
        streams: u32,
        thumbnail: Option<String>,
        /// An X (Twitter) "GIF": really a short silent MP4.
        gif: bool,
        /// `Extractor:id`, when the site gave both.
        key: Option<String>,
    },
    Progress {
        finished: bool,
        downloaded: Option<f64>,
        total: Option<f64>,
        speed: Option<f64>,
        eta: Option<f64>,
        fragment: Option<(f64, f64)>,
    },
    Postprocess(String),
    File(PathBuf),
    Error(String),
    /// A raw output line (everything but progress), kept for "Report a problem".
    Log(String),
}

const DL_TEMPLATE: &str = "download:NERIA_DL %(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s|%(progress.fragment_index)s|%(progress.fragment_count)s";
const PP_TEMPLATE: &str = "postprocess:NERIA_PP %(progress.postprocessor)s|%(progress.status)s";

/// Where this download is saved.
pub fn out_dir(req: &DownloadRequest, s: &Settings) -> PathBuf {
    if let Some(m) = &req.music {
        return m.dir.clone();
    }
    if let Some(d) = &req.dir {
        return d.clone();
    }
    let kind = if req.direct.is_some() && !req.gif {
        crate::folders::Kind::Photo
    } else if req.quality == Quality::Audio {
        crate::folders::Kind::Audio
    } else {
        crate::folders::Kind::Video
    };
    crate::folders::for_kind(s, kind)
}

pub fn build_args(req: &DownloadRequest, s: &Settings) -> Vec<String> {
    // A preset, or an item picked with its own format: as if Settings said so, for this one.
    let own;
    let video = req.quality != Quality::Audio;
    let s = if req.format.as_deref().is_some_and(|f| !f.is_empty()) || (video && req.preset != Preset::Best) {
        let mut c = s.clone();
        if video && matches!(req.preset, Preset::Compatible | Preset::Smallest) {
            // Plays everywhere: MP4, and H.264/AAC for Compatible.
            c.container = "mp4".into();
            c.prefer_h264 |= req.preset == Preset::Compatible;
        }
        if let Some(f) = req.format.as_deref().filter(|f| !f.is_empty()) {
            if video {
                c.container = f.to_string();
            } else {
                c.audio_format = f.to_string();
            }
        }
        own = c;
        &own
    } else {
        s
    };
    let mut a: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(|x| x.to_string()));

    push(&["--encoding", "utf-8", "--newline", "--progress", "--color", "never", "--no-mtime", "--no-simulate"]);
    push(&["--progress-template", DL_TEMPLATE, "--progress-template", PP_TEMPLATE]);
    // URLs never contain '|' in practice, titles can: keep the title last.
    push(&["--print", "video:NERIA_INFO %(format_id)s|%(url)s|%(thumbnail)s|%(extractor_key)s:%(id)s|%(title)s"]);
    push(&["--print", "after_move:NERIA_FILE %(filepath)s"]);
    // Where it will land, so leftovers can be removed if the drive fills up.
    push(&["--print", "video:NERIA_DEST %(filename)s"]);
    push(&["--ffmpeg-location", &paths::bin_dir().to_string_lossy()]);
    push(&["-P", &out_dir(req, s).to_string_lossy()]);
    let stem = req.music.as_ref().map(|m| &m.stem).or(req.stem.as_ref());
    let template = if let Some(stem) = stem {
        // Fixed, predictable name so resume/M3U/sync can find the file. `%` must be escaped.
        format!("{}.%(ext)s", stem.replace('%', "%%"))
    } else if s.filename_template.trim().is_empty() {
        "%(title)s [%(id)s].%(ext)s".to_string()
    } else {
        s.filename_template.trim().to_string()
    };
    // A clip of a link says which part it is; a chapter's fixed name already does.
    let mut tags = String::new();
    if let (Some((a, b)), None) = (req.section, stem) {
        tags.push_str(&format!(" (clip {}-{})", crate::clip::stamp(a), crate::clip::stamp(b)));
    }
    if let Some(t) = &req.name_tag {
        tags.push_str(&t.replace('%', "%%"));
    }
    // Chapters as one file: each range is its own piece first, joined afterwards. Named by
    // where it starts (time ranges have no `section_number`: it came out "NA" for all of
    // them, so each piece overwrote the last), padded so the names sort in time order.
    if req.sections.len() > 1 {
        tags.push_str(".neria-part%(section_start)07d");
    }
    let template = match template.strip_suffix(".%(ext)s") {
        Some(base) => format!("{base}{tags}.%(ext)s"),
        None => format!("{template}{tags}"),
    };
    push(&["-o", &template]);
    if req.force || req.music.as_ref().is_some_and(|m| m.forced.is_some()) {
        // Replacing a wrong match: same file name, so overwrite it.
        push(&["--force-overwrites"]);
    }
    if let Some((a, b)) = req.section {
        // Same range again (e.g. switching Fast -> Exact) must redo it, not
        // report the earlier file as "already downloaded".
        push(&["--download-sections", &format!("*{a:.3}-{b:.3}"), "--force-overwrites"]);
        if req.exact_cuts {
            push(&["--force-keyframes-at-cuts"]);
        }
    }
    if req.sections.len() > 1 {
        for (a, b) in &req.sections {
            push(&["--download-sections", &format!("*{a:.3}-{b:.3}")]);
        }
        // Clean cuts, so the pieces join without a glitch.
        push(&["--force-keyframes-at-cuts", "--force-overwrites"]);
    }
    match req.playlist_index {
        Some(i) => push(&["--yes-playlist", "--playlist-items", &i.to_string()]),
        None => push(&[if s.allow_playlists { "--yes-playlist" } else { "--no-playlist" }]),
    }

    match req.quality {
        Quality::Audio => {
            let f = if req.audio_langs.is_empty() {
                "ba/b".to_string()
            } else {
                crate::tracks::audio_format(&req.audio_langs, false, None)
            };
            push(&["-f", &f, "-x", "--audio-format", &s.audio_format, "--audio-quality", "0"]);
        }
        q => {
            if req.audio_langs.is_empty() {
                push(&["-f", "bv*+ba/b"]);
            } else {
                push(&["-f", &crate::tracks::audio_format(&req.audio_langs, true, None)]);
                if req.audio_langs.len() > 1 {
                    // One file, one audio track per language (players show a track menu).
                    push(&["--audio-multistreams"]);
                }
            }
            let mut sort = Vec::new();
            if s.prefer_h264 {
                sort.push("vcodec:h264".to_string());
                sort.push("acodec:aac".to_string());
            }
            let cap = match req.preset {
                Preset::Smallest => Some(q.max_height().unwrap_or(480).min(480)),
                _ => q.max_height(),
            };
            if let Some(h) = cap {
                sort.insert(0, format!("res:{h}"));
            }
            if req.preset == Preset::Smallest {
                // Within that, the smallest streams.
                sort.push("+size".into());
                sort.push("+br".into());
            }
            if s.container == "webm" {
                // webm only holds VP9/AV1 + Opus: pick webm streams where the site has them.
                sort.push("ext:webm:webm".to_string());
            }
            if !sort.is_empty() {
                push(&["-S", &sort.join(",")]);
            }
            push(&["--merge-output-format", &s.container]);
            // A single file that already has audio (e.g. YouTube format 18) isn't merged, so
            // the merge format never applies: remux it. mp4 and mkv take any stream we get;
            // webm can't hold H.264/AAC, so there it stays as it came.
            if s.container != "webm" {
                push(&["--remux-video", &s.container]);
            }
        }
    }

    // webm can't hold cover art; yt-dlp would just warn, so skip it.
    // Captions only: no video to put a picture in, and none to save next to.
    let thumb_ok = (req.quality == Quality::Audio || s.container != "webm") && !req.captions_only();
    if s.embed_thumbnail && thumb_ok {
        push(&["--embed-thumbnail"]);
    }
    if s.save_thumbnail && req.music.is_none() && !req.captions_only() {
        push(&["--write-thumbnail", "--convert-thumbnails", "jpg"]);
    }
    if s.embed_metadata || req.meta_title.is_some() {
        push(&["--embed-metadata"]);
    }
    // A chapter saved as its own file: its own title and number in the tags.
    if let Some(title) = &req.meta_title {
        let q = |v: &str| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""));
        let mut ppa = format!("Metadata+ffmpeg_o:-metadata title={}", q(title));
        if let Some(n) = req.track {
            ppa.push_str(&format!(" -metadata track={n}"));
        }
        push(&["--postprocessor-args", &ppa]);
    }
    match &req.captions {
        Some(c) if !c.is_empty() => a.extend(c.args(req.quality == Quality::Audio, &s.container)),
        Some(_) => {}
        None if s.embed_subs && req.quality != Quality::Audio => {
            a.extend(["--embed-subs", "--sub-langs", "all,-live_chat"].map(String::from));
        }
        None => {}
    }
    let mut push = |items: &[&str]| a.extend(items.iter().map(|x| x.to_string()));
    // Not for Spotify tracks: YouTube Music audio has no sponsor segments, and
    // cutting "music_offtopic" would shorten songs.
    if s.sponsorblock && req.music.is_none() {
        push(&["--sponsorblock-remove", "sponsor,selfpromo,interaction"]);
    }
    a.extend(crate::cookies::args_for(&req.url, s));
    let mut push = |items: &[&str]| a.extend(items.iter().map(|x| x.to_string()));
    if !s.rate_limit.trim().is_empty() {
        push(&["-r", s.rate_limit.trim()]);
    }
    a.extend(proc::split_args(&s.extra_args));
    a.push("--".into());
    a.push(req.url.clone());
    a
}

/// Run yt-dlp to completion. Returns whether it exited successfully.
///
/// The child is parked in `child_slot` so another thread can kill it.
pub fn run(
    req: &DownloadRequest,
    settings: &Settings,
    child_slot: &Mutex<Option<Child>>,
    cancel: &AtomicBool,
    mut on: impl FnMut(DlEvent),
) -> Result<bool> {
    let dir = out_dir(req, settings);
    std::fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;

    let args = build_args(req, settings);
    on(DlEvent::Log(format!("> yt-dlp {}", readable_args(&args))));
    let mut child = proc::command(paths::tool("yt-dlp"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start yt-dlp")?;

    let (tx, rx) = mpsc::channel::<String>();
    let streams: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
    ];
    for stream in streams.into_iter().flatten() {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        for part in String::from_utf8_lossy(&buf).split('\r') {
                            let line = part.trim();
                            if !line.is_empty() && tx.send(line.to_string()).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });
    }
    drop(tx);

    {
        let mut slot = child_slot.lock().unwrap();
        *slot = Some(child);
        if cancel.load(Ordering::SeqCst)
            && let Some(c) = slot.as_mut()
        {
            proc::kill_tree(c);
        }
    }

    for line in rx {
        if !line.starts_with("NERIA_DL ") {
            on(DlEvent::Log(line.clone()));
        }
        if let Some(ev) = parse_line(&line) {
            on(ev);
        }
    }

    let child = child_slot.lock().unwrap().take();
    match child {
        Some(mut c) => Ok(c.wait()?.success()),
        None => Ok(false),
    }
}

/// The command line for reports, without the fixed plumbing (progress
/// templates, prints, encoding) that's the same for every download.
fn readable_args(args: &[String]) -> String {
    const PAIRS: [&str; 5] = ["--progress-template", "--print", "--ffmpeg-location", "--encoding", "--color"];
    const SINGLES: [&str; 4] = ["--newline", "--progress", "--no-mtime", "--no-simulate"];
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if PAIRS.contains(&a.as_str()) {
            it.next();
        } else if !SINGLES.contains(&a.as_str()) {
            out.push(if a.contains(' ') { format!("\"{a}\"") } else { a.clone() });
        }
    }
    out.join(" ")
}

/// `Extractor:id` from yt-dlp's `extractor_key` and `id` ("NA" when missing).
pub fn media_key(extractor: &str, id: &str) -> Option<String> {
    let ok = |v: &str| !v.is_empty() && v != "NA";
    (ok(extractor) && ok(id)).then(|| format!("{extractor}:{id}"))
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

pub fn parse_line(line: &str) -> Option<DlEvent> {
    if let Some(rest) = line.strip_prefix("NERIA_DL ") {
        let f: Vec<&str> = rest.split('|').collect();
        if f.len() < 8 {
            return None;
        }
        let fragment = match (num(f[6]), num(f[7])) {
            (Some(i), Some(n)) if n > 0.0 => Some((i, n)),
            _ => None,
        };
        return Some(DlEvent::Progress {
            finished: f[0] == "finished",
            downloaded: num(f[1]),
            total: num(f[2]).or_else(|| num(f[3])),
            speed: num(f[4]),
            eta: num(f[5]),
            fragment,
        });
    }
    if let Some(rest) = line.strip_prefix("NERIA_PP ") {
        let (name, status) = rest.split_once('|').unwrap_or((rest, ""));
        return (status == "started").then(|| DlEvent::Postprocess(name.to_string()));
    }
    if let Some(rest) = line.strip_prefix("NERIA_INFO ") {
        let (fmt, rest) = rest.split_once('|').unwrap_or(("", rest));
        let (url, rest) = rest.split_once('|').unwrap_or(("", rest));
        let (thumb, rest) = rest.split_once('|').unwrap_or(("", rest));
        let (key, title) = rest.split_once('|').unwrap_or(("", rest));
        let streams = fmt.split('+').count().max(1) as u32;
        let thumbnail = (thumb.starts_with("http")).then(|| thumb.to_string());
        let gif = crate::gif::is_x_gif_url(url);
        let key = media_key(key.split_once(':').map_or("", |k| k.0), key.split_once(':').map_or("", |k| k.1));
        return Some(DlEvent::Info { title: title.to_string(), streams, thumbnail, gif, key });
    }
    if let Some(rest) = line.strip_prefix("NERIA_FILE ") {
        return Some(DlEvent::File(PathBuf::from(rest.trim())));
    }
    if let Some(rest) = line.strip_prefix("ERROR: ") {
        return Some(DlEvent::Error(crate::probe::clean_error(rest)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress() {
        let ev = parse_line("NERIA_DL downloading|1024|NA|4096|512.5|3|NA|NA").unwrap();
        match ev {
            DlEvent::Progress { finished, downloaded, total, speed, eta, fragment } => {
                assert!(!finished);
                assert_eq!(downloaded, Some(1024.0));
                assert_eq!(total, Some(4096.0));
                assert_eq!(speed, Some(512.5));
                assert_eq!(eta, Some(3.0));
                assert!(fragment.is_none());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parses_info_with_pipe_in_title() {
        match parse_line("NERIA_INFO 137+140|NA|https://i.ytimg.com/vi/x/maxresdefault.jpg|Youtube:x|A | B").unwrap() {
            DlEvent::Info { title, streams, thumbnail, gif, key } => {
                assert_eq!(title, "A | B");
                assert_eq!(key.as_deref(), Some("Youtube:x"));
                assert_eq!(streams, 2);
                assert_eq!(thumbnail.as_deref(), Some("https://i.ytimg.com/vi/x/maxresdefault.jpg"));
                assert!(!gif);
            }
            _ => panic!(),
        }
        match parse_line("NERIA_INFO http|https://video.twimg.com/tweet_video/HWyQ.mp4|NA|NA:NA|Cat").unwrap() {
            DlEvent::Info { title, thumbnail, gif, key, .. } => {
                assert_eq!(title, "Cat");
                assert_eq!(key, None);
                assert_eq!(thumbnail, None);
                assert!(gif);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn audio_args() {
        let req = DownloadRequest::new("https://x.y/z", Quality::Audio, "");
        let args = build_args(&req, &Settings::default());
        assert!(args.contains(&"-x".to_string()));
        assert_eq!(args.last().unwrap(), "https://x.y/z");
        assert!(!args.contains(&"--download-sections".to_string()));
    }

    #[test]
    fn report_args_skip_plumbing() {
        let req = DownloadRequest::new("https://x.y/z", Quality::Best, "");
        let line = readable_args(&build_args(&req, &Settings::default()));
        assert!(!line.contains("NERIA_"), "{line}");
        assert!(!line.contains("--newline"));
        assert!(line.contains("-f bv*+ba/b"));
        assert!(line.ends_with("-- https://x.y/z"));
    }

    #[test]
    fn container_args() {
        let req = DownloadRequest::new("https://x.y/z", Quality::P720, "");
        let mut s = Settings { container: "mkv".into(), ..Settings::default() };
        let args = build_args(&req, &s);
        assert!(args.windows(2).any(|w| w[0] == "--merge-output-format" && w[1] == "mkv"));
        assert!(args.windows(2).any(|w| w[0] == "--remux-video" && w[1] == "mkv"));
        s.container = "webm".into();
        let args = build_args(&req, &s);
        assert!(!args.contains(&"--remux-video".to_string()));
        assert!(args.windows(2).any(|w| w[0] == "-S" && w[1] == "res:720,ext:webm:webm"));
        let audio = build_args(&DownloadRequest::new("https://x.y/z", Quality::Audio, ""), &Settings::default());
        assert!(!audio.contains(&"--remux-video".to_string()));
    }

    #[test]
    fn preset_args() {
        let mut req = DownloadRequest::new("https://x.y/z", Quality::Best, "");
        req.preset = Preset::Compatible;
        let s = Settings { container: "mkv".into(), ..Settings::default() };
        let args = build_args(&req, &s);
        assert!(args.windows(2).any(|w| w[0] == "--merge-output-format" && w[1] == "mp4"));
        assert!(args.windows(2).any(|w| w[0] == "-S" && w[1] == "vcodec:h264,acodec:aac"));
        req.preset = Preset::Smallest;
        let args = build_args(&req, &s);
        assert!(args.windows(2).any(|w| w[0] == "-S" && w[1] == "res:480,+size,+br"));
        req.format = Some("mkv".into());
        let args = build_args(&req, &s);
        assert!(args.windows(2).any(|w| w[0] == "--merge-output-format" && w[1] == "mkv"));
    }

    #[test]
    fn section_args() {
        let mut req = DownloadRequest::new("https://x.y/z", Quality::Best, "");
        req.section = Some((3.5, 75.25));
        req.exact_cuts = true;
        let args = build_args(&req, &Settings::default());
        assert!(args.windows(2).any(|w| w[0] == "--download-sections" && w[1] == "*3.500-75.250"));
        assert!(args.contains(&"--force-keyframes-at-cuts".to_string()));
        assert!(args.windows(2).any(|w| w[0] == "-o" && w[1] == "%(title)s [%(id)s] (clip 0m03s-1m15s).%(ext)s"));
    }
}
