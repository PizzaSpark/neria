//! Running a single yt-dlp download and turning its output into events.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;

use anyhow::{Context, Result};

use crate::music::MusicJob;
use crate::settings::{Quality, Settings};
use crate::{paths, proc};

#[derive(Clone, Debug)]
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
}

impl DownloadRequest {
    pub fn new(url: impl Into<String>, quality: Quality, title_hint: impl Into<String>) -> Self {
        Self { url: url.into(), quality, title_hint: title_hint.into(), section: None, exact_cuts: false, music: None }
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
    Info { title: String, streams: u32, thumbnail: Option<String> },
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

pub fn build_args(req: &DownloadRequest, s: &Settings) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(|x| x.to_string()));

    push(&["--encoding", "utf-8", "--newline", "--progress", "--color", "never", "--no-mtime", "--no-simulate"]);
    push(&["--progress-template", DL_TEMPLATE, "--progress-template", PP_TEMPLATE]);
    // URLs never contain '|' in practice, titles can: keep the title last.
    push(&["--print", "video:NERIA_INFO %(format_id)s|%(thumbnail)s|%(title)s"]);
    push(&["--print", "after_move:NERIA_FILE %(filepath)s"]);
    // Where it will land, so leftovers can be removed if the drive fills up.
    push(&["--print", "video:NERIA_DEST %(filename)s"]);
    push(&["--ffmpeg-location", &paths::bin_dir().to_string_lossy()]);
    let out_dir = req.music.as_ref().map_or(s.output_dir.clone(), |m| m.dir.clone());
    push(&["-P", &out_dir.to_string_lossy()]);
    let template = if let Some(m) = &req.music {
        // Fixed, predictable name so resume/M3U/sync can find the file. `%` must be escaped.
        format!("{}.%(ext)s", m.stem.replace('%', "%%"))
    } else if s.filename_template.trim().is_empty() {
        "%(title)s [%(id)s].%(ext)s".to_string()
    } else {
        s.filename_template.trim().to_string()
    };
    let template = match req.section {
        Some((a, b)) => {
            let tag = format!(" (clip {}-{})", crate::clip::stamp(a), crate::clip::stamp(b));
            match template.strip_suffix(".%(ext)s") {
                Some(base) => format!("{base}{tag}.%(ext)s"),
                None => format!("{template}{tag}"),
            }
        }
        None => template,
    };
    push(&["-o", &template]);
    if req.music.as_ref().is_some_and(|m| m.forced.is_some()) {
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
    push(&[if s.allow_playlists { "--yes-playlist" } else { "--no-playlist" }]);

    match req.quality {
        Quality::Audio => {
            push(&["-f", "ba/b", "-x", "--audio-format", &s.audio_format, "--audio-quality", "0"]);
        }
        q => {
            push(&["-f", "bv*+ba/b"]);
            let mut sort = Vec::new();
            if s.prefer_h264 {
                sort.push("vcodec:h264".to_string());
                sort.push("acodec:aac".to_string());
            }
            if let Some(h) = q.max_height() {
                sort.insert(0, format!("res:{h}"));
            }
            if !sort.is_empty() {
                push(&["-S", &sort.join(",")]);
            }
            push(&["--merge-output-format", &s.container]);
        }
    }

    // webm can't hold cover art; yt-dlp would just warn, so skip it.
    let thumb_ok = req.quality == Quality::Audio || s.container != "webm";
    if s.embed_thumbnail && thumb_ok {
        push(&["--embed-thumbnail"]);
    }
    if s.embed_metadata {
        push(&["--embed-metadata"]);
    }
    if s.embed_subs && req.quality != Quality::Audio {
        push(&["--embed-subs", "--sub-langs", "all,-live_chat"]);
    }
    // Not for Spotify tracks: YouTube Music audio has no sponsor segments, and
    // cutting "music_offtopic" would shorten songs.
    if s.sponsorblock && req.music.is_none() {
        push(&["--sponsorblock-remove", "sponsor,selfpromo,interaction"]);
    }
    if !s.cookies_browser.is_empty() {
        push(&["--cookies-from-browser", &s.cookies_browser]);
    }
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
    std::fs::create_dir_all(&settings.output_dir)
        .with_context(|| format!("can't create {}", settings.output_dir.display()))?;

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
        if cancel.load(Ordering::SeqCst) {
            if let Some(c) = slot.as_mut() {
                proc::kill_tree(c);
            }
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
        let (thumb, title) = rest.split_once('|').unwrap_or(("", rest));
        let streams = fmt.split('+').count().max(1) as u32;
        let thumbnail = (thumb.starts_with("http")).then(|| thumb.to_string());
        return Some(DlEvent::Info { title: title.to_string(), streams, thumbnail });
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
        match parse_line("NERIA_INFO 137+140|https://i.ytimg.com/vi/x/maxresdefault.jpg|A | B").unwrap() {
            DlEvent::Info { title, streams, thumbnail } => {
                assert_eq!(title, "A | B");
                assert_eq!(streams, 2);
                assert_eq!(thumbnail.as_deref(), Some("https://i.ytimg.com/vi/x/maxresdefault.jpg"));
            }
            _ => panic!(),
        }
        match parse_line("NERIA_INFO 18|NA|Title").unwrap() {
            DlEvent::Info { title, thumbnail, .. } => {
                assert_eq!(title, "Title");
                assert_eq!(thumbnail, None);
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
