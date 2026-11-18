//! Recording live streams (Livestreams milestone).
//!
//! A recording is a list of **parts** in its own folder next to where the file will go
//! (`.Title.recording/`, with `parts.json`): yt-dlp records each part, and when the
//! connection drops a new part starts. **Stop** kills whatever runs and [`save`] makes one file
//! from the parts as they are: a from-now part is MPEG-TS (`p0.mp4.part`, yt-dlp's ffmpeg
//! writes TS for live streams), a from-start YouTube part is a video and an audio
//! fragmented MP4 (`p0.f135.mp4.part` + `p0.f140.mp4.part`), and a part yt-dlp finished
//! itself (the stream ended) is a normal file. The same `save` runs when Neria starts again
//! after a crash, so a recording is never lost with the process.
//!
//! Every part is asked for H.264 + AAC so parts of different kinds join without re-encoding
//! (checked: a from-start part followed by a from-now part plays across the seam).
//!
//! **From the start**: `--live-from-start` (YouTube: DASH, at most the last 120 h; Twitch: the
//! stream's own VOD playlist, which keeps growing). Both catch up at ~6–7× real time and then
//! keep following live. **From now**: yt-dlp hands the stream to ffmpeg.
//!
//! Missed pieces: YouTube's piece addresses carry their number (`/sq/199745/`, `/dur/5.000/`),
//! others often end in one (`2280.mp4`); a jump is a hole. Where nothing is numbered, the
//! gap between two parts is measured on the clock ("about").

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::live::now;
use crate::settings::Settings;
use crate::{paths, proc};

/// What the Download page decided for a live link; kept with the job (and in `queue.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveJob {
    /// Record from the first second the site keeps (else from now).
    pub from_start: bool,
    /// When the stream went live (Unix seconds), if the site says.
    pub started_at: Option<f64>,
    /// When a stream that isn't on yet is due to start.
    pub starts_at: Option<f64>,
    /// The recording's own folder, once it has one (so a restart saves it, not records anew).
    pub work: Option<PathBuf>,
}

/// From the start is offered (and picked by itself) for streams younger than this.
pub const FROM_START_MAX_AGE: f64 = 6.0 * 3600.0;
/// How much faster than real time catching up runs (measured: YouTube ~6×, Twitch ~7×).
pub const CATCH_UP_SPEED: f64 = 6.0;

/// Whether the site can record a live stream from its start (checked on real streams).
pub fn can_record_from_start(url: &str) -> bool {
    let host = url.split("//").nth(1).unwrap_or(url).split('/').next().unwrap_or("").to_ascii_lowercase();
    ["youtube.com", "youtu.be", "twitch.tv"].iter().any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// "40 min", "2 h 14 min", "3 days": how long, in words, for lines about a stream.
pub fn age_words(secs: f64) -> String {
    let m = (secs.max(0.0) / 60.0).round() as u64;
    if secs < 60.0 {
        "under a minute".into()
    } else if m < 60 {
        format!("{m} min")
    } else if m < 48 * 60 {
        let (h, m) = (m / 60, m % 60);
        if m == 0 { format!("{h} h") } else { format!("{h} h {m} min") }
    } else {
        format!("{} days", (m as f64 / 1440.0).round() as u64)
    }
}

/// YouTube keeps at most this much of a live stream (yt-dlp: "the last 120 hours").
pub const KEPT_AT_MOST: f64 = 120.0 * 3600.0;

/// Where a recording of a live stream starts, as the preview says it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StartChoice {
    pub from_start: bool,
    /// "Records from the start" / "Records from now" / "Starts in 42 min".
    pub line: String,
    /// What that means here ("about 20 min to catch up, then it follows live"), and short.
    pub hint: String,
    pub hint_short: String,
    /// The link that flips it ("Record from now instead"), and short; empty: no choice.
    pub switch: String,
    pub switch_short: String,
}

/// The start a live stream gets by itself (`want`: None) or as picked, in plain words.
pub fn start_choice(info: &crate::probe::LiveInfo, want: Option<bool>, now: f64) -> StartChoice {
    if info.upcoming {
        let line = match info.starts_at.map(|t| t - now) {
            Some(d) if d > 60.0 => format!("Starts in {}", age_words(d)),
            Some(_) => "Starts any moment".into(),
            None => "Not live yet".into(),
        };
        let hint = if info.can_rewind {
            "Neria waits and records from the first second"
        } else {
            "Neria waits and records once it's on"
        };
        return StartChoice {
            from_start: info.can_rewind,
            line,
            hint: hint.into(),
            hint_short: "waits for it".into(),
            ..Default::default()
        };
    }
    let age = info.started_at.map(|s| (now - s).max(0.0));
    let reachable = info.can_rewind && age.is_none_or(|a| a <= KEPT_AT_MOST);
    let by_itself = reachable && age.is_some_and(|a| a <= FROM_START_MAX_AGE);
    let from_start = reachable && want.unwrap_or(by_itself);
    let catch_up = age.map(|a| a / CATCH_UP_SPEED).filter(|c| *c >= 60.0);
    let mut c = StartChoice { from_start, ..Default::default() };
    if from_start {
        c.line = "Records from the start".into();
        (c.hint, c.hint_short) = match catch_up {
            Some(t) => (
                format!("about {} to catch up, then it follows live", age_words(t)),
                format!("~{} to catch up", age_words(t)),
            ),
            None => ("catches up, then follows live".into(), "then follows live".into()),
        };
        (c.switch, c.switch_short) = ("Record from now instead".into(), "From now instead".into());
    } else {
        c.line = "Records from now".into();
        if reachable {
            (c.hint, c.hint_short) = match age {
                Some(a) => (format!("it's been live for {}", age_words(a)), format!("live for {}", age_words(a))),
                None => (String::new(), String::new()),
            };
            (c.switch, c.switch_short) = ("Record from the start instead".into(), "From the start".into());
        } else if info.can_rewind {
            let a = age_words(age.unwrap_or(0.0));
            c.hint = format!("it's been live for {a}, longer than the site keeps");
            c.hint_short = format!("live for {a}");
        } else {
            c.hint = "this stream can't be rewound".into();
            c.hint_short = "can't be rewound".into();
        }
    }
    c
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Connecting,
    /// Not on yet: yt-dlp waits for it.
    Waiting,
    CatchingUp,
    Recording,
    /// The connection dropped; trying again.
    Reconnecting,
    Saving,
}

/// A missed part of the stream: where (seconds into the stream) and how long.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hole {
    pub at: f64,
    pub len: f64,
    /// Measured on the clock, not from numbered pieces.
    pub about: bool,
}

/// What the queue row shows while recording.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveStats {
    pub phase: Phase,
    pub started_at: Option<f64>,
    pub starts_at: Option<f64>,
    /// Seconds recorded so far (what the file will hold).
    pub recorded: f64,
    pub bytes: u64,
    /// Bytes a second, lately.
    pub speed: f64,
    /// Where in the stream the recording begins (0 = from its start), when known.
    pub joined: Option<f64>,
    /// Where in the stream the newest recorded second is.
    pub position: f64,
    pub holes: Vec<Hole>,
    pub height: Option<u32>,
    /// "Clip the last…" can cut from what's recorded now (not while catching up).
    pub can_clip: bool,
    /// How many times the connection dropped and came back.
    pub reconnects: u32,
    /// Asked for from the start, but the site wouldn't: recording from now instead.
    pub rewind_failed: bool,
    /// "Also get the part before": stream seconds fetched, of how many (while it runs).
    pub earlier: Option<(f64, f64)>,
    /// It's fetched and will be joined in front; or why it couldn't be.
    pub earlier_done: bool,
    pub earlier_failed: Option<String>,
}

impl LiveStats {
    /// How long the stream has been on (for the tape), else as far as it's known.
    pub fn stream_len(&self, now: f64) -> f64 {
        let age = self.started_at.map(|s| now - s).filter(|a| *a > 0.0);
        age.unwrap_or(0.0).max(self.position)
    }
}

pub enum RecEvent {
    Stats(LiveStats),
    /// A line for "Report a problem" (stream addresses left out).
    Log(String),
}

/// How a recording ended, and what was saved.
#[derive(Clone, Debug, Default)]
pub struct Saved {
    pub files: Vec<PathBuf>,
    /// "the stream ended", "stopped: D: is almost full"…
    pub note: String,
    pub recorded: f64,
    pub holes: Vec<Hole>,
}

pub struct Recording<'a> {
    pub url: &'a str,
    pub settings: &'a Settings,
    pub audio: bool,
    pub max_height: Option<u32>,
    pub live: LiveJob,
    pub title: &'a str,
    pub out_dir: &'a Path,
    pub work: PathBuf,
}

/// The folder a recording of `title` keeps its parts in, next to where the file goes.
pub fn work_dir(out_dir: &Path, title: &str) -> PathBuf {
    let name = crate::music::sanitize(title);
    let name = if name.trim().is_empty() { "Live".to_string() } else { name };
    crate::clip::unique_path(out_dir, &format!(".{name}"), "recording")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Manifest {
    title: String,
    url: String,
    audio: bool,
    /// The file type to save as ("mp4", "mkv", or the audio format).
    ext: String,
    started_at: Option<f64>,
    parts: Vec<Part>,
    holes: Vec<Hole>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Part {
    name: String,
    from_start: bool,
    /// Wall clock when it began / last wrote.
    began: f64,
    ended: f64,
    /// Where in the stream it begins.
    offset: f64,
    recorded: f64,
    first_seq: Option<u64>,
    seq_len: Option<f64>,
    /// Which numbered list the numbers are from (the first one seen: video and sound
    /// are numbered apart).
    seq_key: Option<String>,
    /// Seconds to skip at its start (it overlaps the part before).
    skip: f64,
}

/// The live chat read while recording, one message a line (`chat::LiveChat`'s log).
fn chat_log(work: &Path) -> PathBuf {
    work.join("chat.jsonl")
}

/// The chat read while recording, next to each saved file, timed to it: a file's second 0 is
/// `zero` on the clock (the stream's start plus where the file begins in it).
fn save_chat(work: &Path, m: &Manifest, files: &[(PathBuf, f64)]) {
    let log = chat_log(work);
    if !log.is_file() {
        return;
    }
    let all = crate::chat::read_log(&log);
    if all.is_empty() {
        return;
    }
    let sites: Vec<crate::chat::Site> = crate::chat::Site::of(&m.url).into_iter().collect();
    for (file, starts) in files {
        // Where the file's second 0 was on the clock: from the stream's start when it's
        // known, else from when its first part began (a few pieces behind live).
        let zero = match m.started_at {
            Some(s) => s + starts,
            None => m.parts.first().map_or(0.0, |p| p.began - LIVE_LAG + p.skip) + (starts - m.parts.first().map_or(0.0, |p| p.offset + p.skip)),
        };
        let len = crate::media::duration(file).unwrap_or(f64::MAX);
        let msgs: Vec<_> = all.iter().filter(|x| x.t >= zero - 5.0 && x.t <= zero + len + 5.0).cloned().collect();
        if msgs.is_empty() {
            continue;
        }
        let chat = crate::chat::to_file(msgs, zero, sites.clone(), vec![m.title.clone()]);
        let _ = chat.save(&crate::chat::sidecar_of(file));
    }
}

/// How far behind live a from-now recording runs (the pieces the site holds back).
const LIVE_LAG: f64 = 15.0;

fn manifest_path(work: &Path) -> PathBuf {
    work.join("parts.json")
}

fn read_manifest(work: &Path) -> Option<Manifest> {
    serde_json::from_str(&std::fs::read_to_string(manifest_path(work)).ok()?).ok()
}

fn write_manifest(work: &Path, m: &Manifest) {
    if let Ok(text) = serde_json::to_string_pretty(m) {
        let tmp = work.join("parts.json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, manifest_path(work));
        }
    }
}

/// yt-dlp's arguments for one part, read from `source` (the link, or Neria's Twitch relay).
fn part_args(rec: &Recording, source: &str, name: &str, from_start: bool, wait: bool) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(|x| x.to_string()));
    push(&["--encoding", "utf-8", "--newline", "--progress", "--color", "never", "--no-mtime", "--no-playlist"]);
    push(&[
        "--progress-template",
        "download:NERIA_DL %(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s|%(progress.fragment_index)s|%(progress.fragment_count)s",
    ]);
    push(&["--print", "video:NERIA_LIVE %(height)s|%(release_timestamp)s|%(timestamp)s|%(live_status)s"]);
    push(&["--no-quiet", "--no-simulate", "--ffmpeg-location", &paths::bin_dir().to_string_lossy()]);
    push(&["-P", &rec.work.to_string_lossy(), "-o", &format!("{name}.%(ext)s")]);
    if rec.audio {
        push(&["-f", "ba/b", "-S", "acodec:aac"]);
    } else {
        let h = rec.max_height.map(|h| format!("[height<={h}]")).unwrap_or_default();
        push(&["-f", &format!("bv*{h}+ba/b{h}/bv*+ba/b"), "-S", "vcodec:h264,acodec:aac"]);
        push(&["--merge-output-format", "mp4"]);
    }
    if from_start {
        push(&["--live-from-start"]);
    }
    if wait {
        // Not on yet: yt-dlp looks again every 30–60 s until it is.
        push(&["--wait-for-video", "30-60"]);
    }
    a.extend(crate::cookies::args_for(rec.url, rec.settings));
    if !rec.settings.rate_limit.trim().is_empty() {
        a.extend(["-r".to_string(), rec.settings.rate_limit.trim().to_string()]);
    }
    a.extend(proc::split_args(&rec.settings.extra_args));
    a.push("--".into());
    a.push(source.to_string());
    a
}

/// Where a part reads the stream from: a Twitch channel from now goes through the relay
/// that leaves Twitch's ads out (`ttv`), unless you're signed in to Twitch (Turbo and
/// subscribers get no ads, and the relay's address gets no sign-in) or the relay can't
/// reach it; everything else reads the link itself.
fn source_for(rec: &Recording, from_start: bool, wait: bool) -> Option<String> {
    if from_start || wait || std::env::var_os("NERIA_TTV_OFF").is_some() {
        return None;
    }
    crate::ttv::login_of(rec.url)?;
    if !crate::cookies::args_for(rec.url, rec.settings).is_empty() {
        return None;
    }
    crate::ttv::relay_url(rec.url)
}

/// What one line of output says.
#[derive(Debug, PartialEq)]
enum Line {
    /// yt-dlp's own fragment downloader (from-start on YouTube): which stream, bytes, piece i / n.
    Fragment {
        stream: u32,
        bytes: Option<f64>,
        index: Option<f64>,
        count: Option<f64>,
    },
    /// ffmpeg's status line: bytes written and seconds recorded.
    Ffmpeg {
        bytes: Option<f64>,
        time: Option<f64>,
    },
    /// ffmpeg opened a numbered piece (`len`: its length, when the address says).
    Piece {
        key: String,
        seq: u64,
        len: Option<f64>,
    },
    /// A piece yt-dlp gave up on (which one, when it says).
    Skipped(Option<u64>),
    /// Pieces ffmpeg skipped (they left the site's list before it got them) or couldn't get.
    Missed(u64),
    Info {
        height: Option<u32>,
        started: Option<f64>,
    },
    Waiting,
    /// An address or a status line: never logged (it holds the user's IP).
    Quiet,
    Other,
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// "8960KiB" / "3.5MiB" / "512kB" as bytes.
fn size_bytes(s: &str) -> Option<f64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let mult = match unit {
        "" | "B" => 1.0,
        "kB" | "KB" => 1e3,
        "KiB" => 1024.0,
        "MB" => 1e6,
        "MiB" => 1024.0 * 1024.0,
        "GB" => 1e9,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    num(n).map(|v| v * mult)
}

/// "01:02:03.45" as seconds.
fn clock_secs(s: &str) -> Option<f64> {
    let mut total = 0.0;
    for part in s.trim().split(':') {
        total = total * 60.0 + num(part)?;
    }
    (total >= 0.0).then_some(total)
}

fn after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = &line[line.find(key)? + key.len()..];
    let rest = rest.trim_start();
    Some(rest.split_whitespace().next().unwrap_or(""))
}

/// The piece number in a segment address: YouTube's `/sq/N/`, else trailing digits of the
/// file name (`…/2280.mp4`, `seg_123.ts`).
fn piece_of(url: &str) -> Option<(String, u64, Option<f64>)> {
    if let Some(rest) = url.split("/sq/").nth(1) {
        let seq = rest.split('/').next()?.parse().ok()?;
        let len = url.split("/dur/").nth(1).and_then(|r| r.split('/').next()).and_then(num);
        // Video and sound are numbered apart: which one it is.
        let key = url.split("/itag/").nth(1).and_then(|r| r.split('/').next()).unwrap_or("").to_string();
        return Some((key, seq, len));
    }
    let path = url.split(['?', '#']).next()?;
    let file = path.rsplit('/').next()?;
    let (stem, ext) = file.rsplit_once('.')?;
    if !["ts", "mp4", "m4s", "aac", "m4a", "m4v"].contains(&ext) {
        return None;
    }
    let digits: String = stem.chars().rev().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 12 {
        return None;
    }
    let key = path.rsplit_once('/').map(|(d, _)| d.rsplit('/').next().unwrap_or(d)).unwrap_or("").to_string();
    Some((key, digits.chars().rev().collect::<String>().parse().ok()?, None))
}

fn parse(line: &str) -> Line {
    // yt-dlp prefixes each stream's lines with its number when it downloads two at once.
    let body = match line.split_once(": ") {
        Some((n, rest)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => line,
    };
    let stream = line.split_once(": ").and_then(|(n, _)| n.parse::<u32>().ok()).unwrap_or(1);
    if let Some(rest) = body.strip_prefix("NERIA_DL ") {
        let f: Vec<&str> = rest.split('|').collect();
        if f.len() >= 8 {
            return Line::Fragment { stream, bytes: num(f[1]), index: num(f[6]), count: num(f[7]) };
        }
        return Line::Quiet;
    }
    if let Some(rest) = body.strip_prefix("NERIA_LIVE ") {
        let f: Vec<&str> = rest.split('|').collect();
        let height = f.first().and_then(|h| h.trim().parse().ok());
        let started = f.get(1).and_then(|v| num(v)).or_else(|| f.get(2).and_then(|v| num(v)));
        return Line::Info { height, started };
    }
    if body.contains("Opening '") {
        let url = body.split("Opening '").nth(1).unwrap_or("");
        let url = url.split('\'').next().unwrap_or("");
        if url.contains("/sq/") {
            return piece_of(url).map(|(key, seq, len)| Line::Piece { key, seq, len }).unwrap_or(Line::Quiet);
        }
        if url.contains(".m3u8") || url.contains("/playlist") {
            return Line::Quiet;
        }
        return piece_of(url).map(|(key, seq, len)| Line::Piece { key, seq, len }).unwrap_or(Line::Quiet);
    }
    if body.contains("time=") && body.contains("size=") {
        let bytes = after(body, "size=").and_then(size_bytes);
        let time = after(body, "time=").and_then(clock_secs);
        return Line::Ffmpeg { bytes, time };
    }
    if let Some(rest) = body.split("kipping fragment ").nth(1) {
        let n = rest.split(|c: char| !c.is_ascii_digit()).next().and_then(|n| n.parse().ok());
        return Line::Skipped(n);
    }
    // ffmpeg's HLS reader: "skipping 3 segments ahead, expired from playlists",
    // "Failed to open segment 1234 of playlist 0".
    if let Some(rest) = body.split("skipping ").nth(1).filter(|r| r.contains("segments ahead"))
        && let Some(n) = rest.split_whitespace().next().and_then(|n| n.parse().ok())
    {
        return Line::Missed(n);
    }
    if body.contains("Failed to open segment") {
        return Line::Missed(1);
    }
    if body.starts_with("[wait]") || body.contains("Waiting for") && body.contains("Press Ctrl+C") {
        return Line::Waiting;
    }
    // ffmpeg's own notes about each piece ("[hls @ 0x…] …", "[mov,mp4… @ 0x…] Found
    // duplicated MOOV Atom") repeat for hours: only its errors are worth keeping.
    if body.starts_with('[') && body.contains(" @ ") && !body.to_ascii_lowercase().contains("error") {
        return Line::Quiet;
    }
    if body.contains("http://") || body.contains("https://") {
        // Stream addresses carry the user's IP address; "Report a problem" never needs them.
        return Line::Quiet;
    }
    // yt-dlp's own lines ("[youtube] …", "ERROR: …"); not ffmpeg's stream-info dump.
    let keep = body.starts_with('[') || body.starts_with("ERROR") || body.starts_with("WARNING");
    if keep { Line::Other } else { Line::Quiet }
}

/// The state of the part being recorded.
#[derive(Default)]
struct Running {
    /// Bytes per stream, and pieces per stream (from-start).
    bytes: [f64; 4],
    index: [Option<f64>; 4],
    count: Option<f64>,
    ffmpeg_time: Option<f64>,
    ffmpeg_bytes: Option<f64>,
    /// Pieces yt-dlp skipped, not counted yet: one near the newest is usually not made yet
    /// (ytarchive: a miss within 2 of the end isn't a hole). Counted once both streams are
    /// 2 pieces past it.
    skipped: Vec<u64>,
    waiting: bool,
    any_data: bool,
}

impl Running {
    fn bytes(&self) -> f64 {
        self.ffmpeg_bytes.unwrap_or(0.0).max(self.bytes.iter().sum())
    }
}

/// Record until stopped, cancelled or the stream ends, then save. `slot` holds the yt-dlp
/// running now, so another thread can kill it; `stop` saves what's there, `cancel` throws
/// it away.
pub fn record(
    rec: &Recording,
    slot: &Mutex<Option<Child>>,
    stop: &AtomicBool,
    cancel: &AtomicBool,
    mut on: impl FnMut(RecEvent),
) -> Result<Saved> {
    std::fs::create_dir_all(&rec.work).with_context(|| format!("can't create {}", rec.work.display()))?;
    let mut m = read_manifest(&rec.work).unwrap_or_else(|| Manifest {
        title: rec.title.to_string(),
        url: rec.url.to_string(),
        audio: rec.audio,
        ext: if rec.audio {
            rec.settings.audio_format.clone()
        } else if rec.settings.container == "mkv" {
            "mkv".into()
        } else {
            "mp4".into()
        },
        started_at: rec.live.started_at,
        ..Manifest::default()
    });
    write_manifest(&rec.work, &m);
    let mut stats = LiveStats {
        started_at: m.started_at,
        starts_at: rec.live.starts_at.filter(|t| *t > now()),
        holes: m.holes.clone(),
        recorded: m.parts.iter().map(|p| p.recorded).sum(),
        joined: m.parts.first().map(|p| p.offset),
        ..LiveStats::default()
    };
    let mut failures = 0u32;
    let mut note = String::new();
    let mut ended = false;
    // Its chat, all along (into the recording's folder line by line, so a crash keeps it):
    // saved next to the file as `.chat.json`.
    let _chat = (rec.settings.chat_save && crate::chat::supported(rec.url))
        .then(|| crate::chat::LiveChat::start(rec.url, Some(chat_log(&rec.work))))
        .flatten();

    while !stop.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst) {
        // The first part goes from the start when asked; a from-start part that broke is
        // picked up where it was (yt-dlp keeps its place in `.ytdl`); otherwise from now.
        let resume = m.parts.last().filter(|p| p.from_start && has_ytdl(&rec.work, &p.name)).cloned();
        let first = m.parts.is_empty();
        let from_start = resume.is_some() || (first && rec.live.from_start && !stats.rewind_failed);
        let wait = first && rec.live.starts_at.is_some_and(|t| t > now() - 60.0);
        let mut part = resume.clone().unwrap_or_else(|| Part {
            name: format!("p{}", m.parts.len()),
            from_start,
            began: now(),
            ..Part::default()
        });
        if resume.is_some() {
            m.parts.pop();
        }
        let earlier: f64 = m.parts.iter().map(|p| p.recorded).sum();
        let earlier_bytes = part_bytes(&rec.work, &m.parts);
        let prev = m.parts.last().cloned();
        if !from_start {
            // Where in the stream this part starts: by the clock (a few pieces behind live).
            part.offset = m.started_at.map(|s| (now() - s - 15.0).max(0.0)).unwrap_or(stats.position);
        }
        stats.phase = if wait {
            Phase::Waiting
        } else if prev.is_some() {
            Phase::Reconnecting
        } else {
            Phase::Connecting
        };
        on(RecEvent::Stats(stats.clone()));

        let relay = source_for(rec, from_start, wait);
        // Through the relay yt-dlp doesn't say when the stream began: Twitch's playlist does.
        if relay.is_some()
            && m.started_at.is_none()
            && let Some(s) = crate::ttv::started_at(rec.url)
        {
            m.started_at = Some(s);
            stats.started_at = Some(s);
            part.offset = (now() - s - 15.0).max(0.0);
        }
        let args = part_args(rec, relay.as_deref().unwrap_or(rec.url), &part.name, from_start, wait);
        on(RecEvent::Log(format!(
            "> yt-dlp (live part {}{}{})",
            part.name,
            if from_start { ", from the start" } else { "" },
            if relay.is_some() { ", through Neria's Twitch relay: no ads" } else { "" }
        )));
        let started = Instant::now();
        let mut run = Running::default();
        let mut last_emit = Instant::now() - Duration::from_secs(5);
        let mut last_disk = Instant::now();
        let mut speed_mark = (Instant::now(), 0.0f64);
        let debug = std::env::var_os("NERIA_REC_DEBUG").is_some();
        let pi = m.parts.len();
        m.parts.push(part.clone());
        write_manifest(&rec.work, &m);
        let mut last_write = Instant::now();
        let ok = run_part(&args, slot, stop, cancel, |line| {
            if debug && !line.is_empty() {
                match parse(line) {
                    Line::Piece { key, seq, len } => eprintln!("PIECE {key} {seq} {len:?}"),
                    _ => eprintln!("RAW {}", line.split("http").next().unwrap_or(line)),
                }
            }
            match if line.is_empty() { Line::Quiet } else { parse(line) } {
                Line::Fragment { stream, bytes, index, count } => {
                    let s = (stream as usize).min(3);
                    if let Some(b) = bytes {
                        run.bytes[s] = b;
                    }
                    run.index[s] = index.or(run.index[s]);
                    run.count = count.or(run.count);
                    run.any_data = true;
                }
                Line::Ffmpeg { bytes, time } => {
                    run.ffmpeg_bytes = bytes.or(run.ffmpeg_bytes);
                    run.ffmpeg_time = time.or(run.ffmpeg_time);
                    run.any_data |= time.is_some_and(|t| t > 0.0);
                }
                Line::Piece { key, seq, len } => {
                    if part.seq_key.is_none() {
                        part.seq_key = Some(key.clone());
                    }
                    if part.seq_key.as_deref() != Some(key.as_str()) {
                        return;
                    }
                    if let Some(len) = len {
                        part.seq_len = Some(len);
                    }
                    // Only the first: ffmpeg doesn't log pieces it fetches on a kept-open
                    // connection, so a jump in these numbers isn't a missed piece.
                    if part.first_seq.is_none() {
                        part.first_seq = Some(seq);
                        // Where it starts against the part before: trim an overlap now, so
                        // the count stays right while it records.
                        if let Some(p) = prev.as_ref().filter(|_| resume.is_none())
                            && let Some(hole) = seam(p, &mut part)
                        {
                            m.holes.push(hole);
                            stats.holes.push(hole);
                        }
                    }
                }
                Line::Missed(n) => {
                    let len = part.seq_len.unwrap_or(5.0);
                    let hole = Hole {
                        at: part.offset + part_time(&m, &run),
                        len: n as f64 * len,
                        about: part.seq_len.is_none(),
                    };
                    m.holes.push(hole);
                    stats.holes.push(hole);
                }
                Line::Skipped(Some(n)) => run.skipped.push(n),
                Line::Skipped(None) => {}
                Line::Info { height, started } => {
                    stats.height = height.or(stats.height);
                    if m.started_at.is_none() {
                        m.started_at = started;
                        stats.started_at = started;
                        // Now it's known where in the stream "now" is.
                        if let (Some(s), false) = (started, from_start) {
                            part.offset = (now() - s - 15.0 - part_time(&m, &run)).max(0.0);
                        }
                    }
                }
                Line::Waiting => run.waiting = true,
                Line::Quiet => {}
                Line::Other => on(RecEvent::Log(line.to_string())),
            }
            if last_emit.elapsed() < Duration::from_millis(500) {
                return;
            }
            last_emit = Instant::now();
            // Skipped pieces both streams are well past: really missed.
            let behind = run.index.iter().flatten().fold(f64::MAX, |a, b| a.min(*b));
            if behind < f64::MAX {
                let len = frag_len(&m, &run);
                let (done, wait): (Vec<u64>, Vec<u64>) = run.skipped.iter().partition(|n| (**n as f64) + 2.0 < behind);
                run.skipped = wait;
                for n in done {
                    let hole = Hole { at: part.offset + n as f64 * len, len, about: false };
                    m.holes.push(hole);
                    stats.holes.push(hole);
                }
            }
            let t = part_time(&m, &run);
            part.recorded = (t - part.skip).max(0.0);
            part.ended = now();
            stats.recorded = earlier + part.recorded;
            stats.position = part.offset + t;
            stats.bytes = (earlier_bytes + run.bytes()) as u64;
            let (at, b) = speed_mark;
            if at.elapsed() >= Duration::from_secs(8) {
                stats.speed = (run.bytes() - b).max(0.0) / at.elapsed().as_secs_f64();
                speed_mark = (Instant::now(), run.bytes());
            }
            stats.joined = Some(m.parts.first().map_or(part.offset, |p| p.offset));
            let behind = stats.started_at.map(|s| now() - s - stats.position).unwrap_or(0.0);
            stats.phase = if run.waiting && !run.any_data {
                Phase::Waiting
            } else if !run.any_data {
                if prev.is_some() { Phase::Reconnecting } else { Phase::Connecting }
            } else if from_start && behind > 60.0 {
                Phase::CatchingUp
            } else {
                Phase::Recording
            };
            stats.can_clip = stats.phase == Phase::Recording && part.recorded >= 5.0;
            on(RecEvent::Stats(stats.clone()));
            // Kept fresh: "Clip the last" and "End clip" cut from what it says.
            if last_write.elapsed() >= Duration::from_secs(2) {
                last_write = Instant::now();
                m.parts[pi] = part.clone();
                write_manifest(&rec.work, &m);
            }
            // Saving needs the space again (the parts are joined into a new file).
            if last_disk.elapsed() >= Duration::from_secs(5) {
                last_disk = Instant::now();
                let need = stats.bytes + crate::disk::MARGIN;
                if crate::disk::free_space(&rec.work).is_some_and(|f| f < need) && !stop.load(Ordering::SeqCst) {
                    note = format!("stopped: {} is almost full", crate::disk::drive_name(&rec.work));
                    stop.store(true, Ordering::SeqCst);
                    if let Some(c) = slot.lock().unwrap().as_mut() {
                        proc::kill_tree(c);
                    }
                }
            }
        })?;
        let given_up = stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst);
        if from_start && resume.is_none() && !run.any_data && !ok && !given_up && !run.waiting {
            // The site wouldn't give it from the start (rewinding off, no saved broadcast):
            // record from now instead, and say so.
            for f in files_of(&rec.work, &part.name) {
                let _ = std::fs::remove_file(f);
            }
            m.parts.pop();
            write_manifest(&rec.work, &m);
            stats.rewind_failed = true;
            on(RecEvent::Log("Couldn't record from the start: recording from now".into()));
            continue;
        }
        // Ad stretches the relay had to leave out (every source had the ad): holes.
        if relay.is_some()
            && let Some(start) = m.started_at
        {
            for (at, len) in crate::ttv::gaps(rec.url) {
                let pos = at - start;
                let seen = m.holes.iter().any(|h| (h.at - pos).abs() < 1.0);
                if at >= part.began - 30.0 && !seen {
                    let hole = Hole { at: pos, len, about: false };
                    m.holes.push(hole);
                    stats.holes.push(hole);
                }
            }
        }
        // Keep the part (even an empty one is dropped at saving).
        let t = part_time(&m, &run);
        part.recorded = (t - part.skip).max(0.0);
        part.ended = now();
        part.recorded = (t - part.skip).max(0.0);
        let numbered = prev.as_ref().is_some_and(|p| p.seq_key.is_some() && p.seq_key == part.seq_key);
        if numbered {
            // Done when its first piece came in (`seam`).
        } else if let Some(p) = prev.as_ref().filter(|_| resume.is_none() && run.any_data) {
            // Nothing numbered: the gap on the clock, roughly.
            let gap = part.offset - (p.offset + p.recorded + p.skip);
            if gap > 2.0 {
                let hole = Hole { at: p.offset + p.recorded + p.skip, len: gap, about: true };
                m.holes.push(hole);
                stats.holes.push(hole);
            }
        }
        m.parts[pi] = part;
        write_manifest(&rec.work, &m);
        stats.recorded = m.parts.iter().map(|p| p.recorded).sum();

        if stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst) {
            break;
        }
        if ok {
            // yt-dlp finished by itself: the stream ended.
            ended = true;
            break;
        }
        // It stopped: the stream ended, or the connection dropped.
        match still_live(rec.url, rec.settings) {
            Some(false) => {
                ended = true;
                break;
            }
            _ => {
                let long = started.elapsed() > Duration::from_secs(60);
                failures = if long { 0 } else { failures + 1 };
                if failures >= 5 {
                    note = "the connection kept dropping".into();
                    break;
                }
                stats.reconnects += 1;
                stats.phase = Phase::Reconnecting;
                on(RecEvent::Stats(stats.clone()));
                let wait = [2, 5, 10, 20, 30][failures.min(4) as usize];
                for _ in 0..wait * 10 {
                    if stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    if cancel.load(Ordering::SeqCst) {
        discard(&rec.work);
        bail!("Cancelled");
    }
    if note.is_empty() && ended {
        note = "the stream ended".into();
    }
    stats.phase = Phase::Saving;
    on(RecEvent::Stats(stats.clone()));
    // Missed bits not fetched yet: now (the stream is still there to fetch them from).
    if !cancel.load(Ordering::SeqCst) && read_manifest(&rec.work).is_some_and(|m| !m.holes.is_empty()) {
        let never = AtomicBool::new(false);
        match repair_holes(rec, &never) {
            Ok(n) if n > 0 => on(RecEvent::Log(format!("Fetched {n} missed bit(s) again"))),
            Err(e) => on(RecEvent::Log(format!("Couldn't fetch the missed bits again: {e}"))),
            _ => {}
        }
    }
    let mut saved = save(&rec.work, rec.out_dir)?;
    saved.note = [note, saved.note].into_iter().filter(|n| !n.is_empty()).collect::<Vec<_>>().join(" · ");
    Ok(saved)
}

/// Where `part` starts against `p`, the part before, when both are numbered the same way
/// (YouTube): an overlap becomes `part.skip` (trimmed when saving), a gap is a hole.
fn seam(p: &Part, part: &mut Part) -> Option<Hole> {
    if p.seq_key.is_none() || p.seq_key != part.seq_key {
        return None;
    }
    let (p_first, first, len) = (p.first_seq?, part.first_seq?, part.seq_len.or(p.seq_len)?);
    let gap = (first as f64 - p_first as f64) * len - (p.recorded + p.skip);
    if gap < -0.5 {
        part.skip = -gap;
        None
    } else if gap > len * 0.5 {
        Some(Hole { at: p.offset + p.recorded + p.skip, len: gap, about: false })
    } else {
        None
    }
}

/// Seconds the running part holds: ffmpeg's clock, or (from-start) pieces × their length,
/// counted on the stream that's furthest behind (video and audio come down separately).
fn part_time(m: &Manifest, run: &Running) -> f64 {
    if let Some(t) = run.ffmpeg_time {
        return t;
    }
    let idx = run.index.iter().flatten().fold(f64::MAX, |a, b| a.min(*b));
    if idx == f64::MAX {
        return 0.0;
    }
    idx * frag_len(m, run)
}

/// The length of one from-start piece: the stream's age over the newest piece's number
/// (YouTube numbers them from the start), else YouTube's usual 5 s.
fn frag_len(m: &Manifest, run: &Running) -> f64 {
    match (m.started_at, run.count) {
        (Some(s), Some(n)) if n > 0.0 => {
            let len = (now() - s) / n;
            if (0.5..=12.0).contains(&len) { len } else { 5.0 }
        }
        _ => 5.0,
    }
}

fn has_ytdl(work: &Path, name: &str) -> bool {
    files_of(work, name).iter().any(|f| f.extension().is_some_and(|e| e == "ytdl"))
}

/// The files a part left (`p0.mp4.part`, `p0.f135.mp4.part`, `p0.mp4`…).
fn files_of(work: &Path, name: &str) -> Vec<PathBuf> {
    let prefix = format!("{name}.");
    let mut out: Vec<PathBuf> = std::fs::read_dir(work)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix)))
        .collect();
    out.sort();
    out
}

fn part_bytes(work: &Path, parts: &[Part]) -> f64 {
    parts
        .iter()
        .flat_map(|p| files_of(work, &p.name))
        .filter_map(|f| std::fs::metadata(f).ok())
        .map(|m| m.len() as f64)
        .sum()
}

/// Is the stream still on? `None` when it couldn't be asked (no connection).
fn still_live(url: &str, settings: &Settings) -> Option<bool> {
    let mut cmd = proc::command(paths::tool("yt-dlp"));
    cmd.args(["--no-warnings", "--no-playlist", "--ignore-no-formats-error", "--print", "%(live_status)s"]);
    cmd.args(crate::cookies::args_for(url, settings));
    let out = cmd.arg("--").arg(url).stdout(Stdio::piped()).stderr(Stdio::piped()).output().ok()?;
    let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
    if status == "is_live" {
        return Some(true);
    }
    if !status.is_empty() && status != "NA" {
        return Some(false);
    }
    // Twitch says so in words once a channel is offline.
    if err.contains("not currently live") || err.contains("is offline") {
        return Some(false);
    }
    None
}

/// Run one yt-dlp to its end, handing every output line to `on`. Returns whether it
/// finished by itself with success.
fn run_part(
    args: &[String],
    slot: &Mutex<Option<Child>>,
    stop: &AtomicBool,
    cancel: &AtomicBool,
    mut on: impl FnMut(&str),
) -> Result<bool> {
    let mut child = proc::command(paths::tool("yt-dlp"))
        .args(args)
        .env("PYTHONUNBUFFERED", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start yt-dlp")?;
    // A recording never ends by itself: if Neria goes, it goes too (its parts are saved at
    // the next start).
    proc::end_with_neria(&child);
    let (tx, rx) = mpsc::channel::<String>();
    let streams: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
    ];
    for stream in streams.into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            // ffmpeg ends its status line with a carriage return only, so lines end at either.
            let mut reader = BufReader::new(stream);
            let mut chunk = [0u8; 8192];
            let mut line: Vec<u8> = Vec::new();
            loop {
                let n = match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                for &b in &chunk[..n] {
                    if b == 10 || b == 13 {
                        let text = String::from_utf8_lossy(&line).trim().to_string();
                        line.clear();
                        if !text.is_empty() && tx.send(text).is_err() {
                            return;
                        }
                    } else {
                        line.push(b);
                    }
                }
            }
            let text = String::from_utf8_lossy(&line).trim().to_string();
            if !text.is_empty() {
                let _ = tx.send(text);
            }
        });
    }
    drop(tx);
    {
        let mut guard = slot.lock().unwrap();
        *guard = Some(child);
        if (stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst))
            && let Some(c) = guard.as_mut()
        {
            proc::kill_tree(c);
        }
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(line) => on(&line),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Keep the numbers moving even when nothing prints (and see a Stop).
                on("");
                if (stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst))
                    && let Some(c) = slot.lock().unwrap().as_mut()
                {
                    proc::kill_tree(c);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let child = slot.lock().unwrap().take();
    match child {
        Some(mut c) => Ok(c.wait()?.success()),
        None => Ok(false),
    }
}

/// "Also get the part before": a from-now recording's earlier part, fetched from the start
/// next to it (`fetch_earlier`) and joined in front of it on saving. Where the two meet is
/// found by their sound: the clock is a few seconds off (11 s on YouTube, 5 s on Twitch in
/// tests), and Twitch starts a from-now recording with its own ad, which the earlier part
/// doesn't have; so the first stretch of the recording that matches is where it takes over.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Early {
    /// It got as far as where the recording began (else it was stopped short: a hole).
    pub whole: bool,
    /// Seconds of the earlier part to keep, and seconds at the start of the recording to
    /// leave out (the ad, or what the earlier part already has).
    pub until: f64,
    pub skip: f64,
    /// No stretch matched: joined where the clock says.
    pub by_clock: bool,
    /// How much of the stream the earlier part holds.
    pub got: f64,
}

fn early_path(work: &Path) -> PathBuf {
    work.join("early.json")
}

/// The earlier part is being fetched (saving waits for it to be cut and matched).
fn early_busy(work: &Path) -> PathBuf {
    work.join("early.busy")
}

/// The earlier part of a recording, once fetched.
pub fn early_of(work: &Path) -> Option<Early> {
    serde_json::from_str(&std::fs::read_to_string(early_path(work)).ok()?).ok()
}

/// Fetch the part of the stream before a from-now recording began, from the start, until it
/// has passed that point (or `stop`); then find where the two meet. `on(got, of)`: stream
/// seconds fetched so far, of how many. Runs next to `record` (its own yt-dlp in `slot`).
pub fn fetch_earlier(
    rec: &Recording,
    slot: &Mutex<Option<Child>>,
    stop: &AtomicBool,
    cancel: &AtomicBool,
    mut on: impl FnMut(f64, f64),
) -> Result<Early> {
    let m = read_manifest(&rec.work).context("Nothing recorded yet")?;
    let first = m.parts.first().cloned().context("Nothing recorded yet")?;
    if first.from_start {
        bail!("It's recorded from the start already");
    }
    if m.started_at.is_none() || first.offset < 30.0 {
        bail!("Neria doesn't know when the stream began");
    }
    let _ = std::fs::write(early_busy(&rec.work), "");
    let result = fetch_earlier_inner(rec, &m, &first, slot, stop, cancel, &mut on);
    let _ = std::fs::remove_file(early_busy(&rec.work));
    let early = result?;
    if let Ok(text) = serde_json::to_string_pretty(&early) {
        std::fs::write(early_path(&rec.work), text)?;
    }
    Ok(early)
}

fn fetch_earlier_inner(
    rec: &Recording,
    m: &Manifest,
    first: &Part,
    slot: &Mutex<Option<Child>>,
    stop: &AtomicBool,
    cancel: &AtomicBool,
    on: &mut impl FnMut(f64, f64),
) -> Result<Early> {
    // A minute and a half past where the recording began: room for the sound to match,
    // even after Twitch's ad at its start.
    let want = first.offset + 90.0;
    let mut target = want;
    let mut reached: bool;
    let mut got = 0.0;
    let mut file = None;
    let mut last = Instant::now() - Duration::from_secs(1);
    // YouTube's piece count only estimates how far it is (it said 53 min where the file
    // held 52): measure the file, and go on where it was (yt-dlp keeps its place in
    // `.ytdl`) until it really has passed that point. Twitch's count is ffmpeg's own clock.
    for _ in 0..4 {
        let args = part_args(rec, rec.url, "early", true, false);
        let mut run = Running::default();
        reached = false;
        run_part(&args, slot, stop, cancel, |line| {
            match if line.is_empty() { Line::Quiet } else { parse(line) } {
                Line::Fragment { stream, bytes, index, count } => {
                    let i = (stream as usize).min(3);
                    if let Some(b) = bytes {
                        run.bytes[i] = b;
                    }
                    run.index[i] = index.or(run.index[i]);
                    run.count = count.or(run.count);
                    run.any_data = true;
                }
                Line::Ffmpeg { bytes, time } => {
                    run.ffmpeg_bytes = bytes.or(run.ffmpeg_bytes);
                    run.ffmpeg_time = time.or(run.ffmpeg_time);
                    run.any_data |= time.is_some_and(|t| t > 0.0);
                }
                _ => {}
            }
            let t = part_time(m, &run);
            if last.elapsed() >= Duration::from_millis(500) {
                last = Instant::now();
                on(t.min(first.offset), first.offset);
                // Still moving: saving waits for it.
                let _ = std::fs::write(early_busy(&rec.work), "");
            }
            if t >= target && !reached {
                reached = true;
                if let Some(c) = slot.lock().unwrap().as_mut() {
                    proc::kill_tree(c);
                }
            }
        })?;
        if cancel.load(Ordering::SeqCst) {
            bail!("Cancelled");
        }
        if file.is_none() && !run.any_data {
            let _ = files_of(&rec.work, "early").iter().map(std::fs::remove_file).count();
            bail!("The site wouldn't give the earlier part");
        }
        let estimate = part_time(m, &run);
        let _ = std::fs::remove_file(rec.work.join("early.ready.mp4"));
        let part = Part { name: "early".into(), from_start: true, recorded: estimate, ..Part::default() };
        file = part_file(&rec.work, &part)?;
        got = file.as_deref().and_then(crate::media::duration).unwrap_or(0.0);
        let resumable = has_ytdl(&rec.work, "early");
        if !reached || got >= want - 30.0 || !resumable || stop.load(Ordering::SeqCst) {
            break;
        }
        // Short of it: ask for what's missing on top of where the count says it is.
        target = estimate + (want - got) + 15.0;
    }
    let file = file.context("The earlier part came out empty")?;
    on(got.min(first.offset), first.offset);
    if got < first.offset - 2.0 {
        // Stopped short (Stop, or the site): keep what there is; the rest is a hole.
        return Ok(Early { whole: false, until: got, skip: 0.0, by_clock: true, got });
    }
    let (until, skip, by_clock) = match_seam(&rec.work, first, &file).unwrap_or((first.offset, 0.0, true));
    Ok(Early { whole: true, until: until.min(got), skip, by_clock, got })
}

/// Where the recording's own part (`first`, from now) takes over from the earlier part
/// (`early`): (seconds of the earlier part to keep, seconds of `first` to leave out). Tries a
/// stretch at 0, 30, 60 s into `first` (Twitch's ad at the start matches nothing).
fn match_seam(work: &Path, first: &Part, early: &Path) -> Option<(f64, f64, bool)> {
    use crate::fingerprint::{align, fingerprint_range};
    let own = files_of(work, &first.name).into_iter().find(|f| {
        let n = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        !n.ends_with(".ytdl") && !n.contains(".ready.") && !is_format_piece(n)
    })?;
    for at in [0.0, 30.0, 60.0] {
        let Ok(clip) = fingerprint_range(own.as_os_str(), at, 25.0) else { continue };
        // Around where the clock puts it: its guess is off by seconds, not minutes.
        let from = (first.offset + at - 150.0).max(0.0);
        let Ok(song) = fingerprint_range(early.as_os_str(), from, 300.0) else { continue };
        if let Some(a) = align(&clip, &song).filter(|a| a.similarity >= 0.8) {
            return Some((from + a.offset_secs, at, false));
        }
    }
    None
}

// ── Missed bits, got back ──
//
// Where the connection dropped or slowed, the site still has that stretch (YouTube keeps the
// last hour seekable while it's live and the whole video after; Twitch's broadcast copy, when
// it's public). `repair_holes` fetches each missed bit with a margin either side, once it's a
// minute old and the recording is going steadily; saving splices it in where the sound says
// the recording broke off and where it picks up again (a hole's place is only known to a few
// seconds).

/// A missed bit fetched again: the hole (`at`, `len`, stream seconds), the file, and where
/// in the stream the file begins.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fill {
    pub at: f64,
    pub len: f64,
    pub file: String,
    pub from: f64,
}

fn fills_path(work: &Path) -> PathBuf {
    work.join("fills.json")
}

/// The missed bits fetched again so far.
pub fn fills_of(work: &Path) -> Vec<Fill> {
    std::fs::read_to_string(fills_path(work)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Seconds fetched before a missed bit (its place is a few seconds off) and after it: more
/// after, as a reconnect to Twitch without the relay starts with its ~30 s ad, which matches
/// nothing.
const FILL_MARGIN: f64 = 20.0;
const FILL_AFTER: f64 = 60.0;

/// Fetch the stretch `from`–`to` (stream seconds) of the stream as `<stem>.mp4` in `work`.
fn fetch_stretch(rec: &Recording, started_at: f64, from: f64, to: f64, stem: &str) -> Result<PathBuf> {
    let host = rec.url.split("//").nth(1).unwrap_or(rec.url).split('/').next().unwrap_or("").to_ascii_lowercase();
    let youtube = host.ends_with("youtube.com") || host == "youtu.be";
    let twitch = host.ends_with("twitch.tv");
    let mut a: Vec<String> = Vec::new();
    let (s, e) = if youtube && still_live(rec.url, rec.settings) != Some(false) {
        // Live: the last hour, counted from the start of what YouTube keeps right now.
        crate::live::section_now(rec.url, rec.settings, started_at + from, started_at + to)?
    } else if youtube {
        (from, to)
    } else if twitch {
        // The broadcast copy while it's live (from the start, cut).
        a.push("--live-from-start".into());
        (from, to)
    } else {
        bail!("This site keeps nothing to fetch it again from");
    };
    a.extend(["--no-warnings", "--no-playlist", "--no-mtime", "--no-part"].map(String::from));
    a.extend(["--ffmpeg-location".into(), paths::bin_dir().to_string_lossy().into_owned()]);
    if rec.audio {
        a.extend(["-f", "ba/b", "-S", "acodec:aac"].map(String::from));
    } else {
        let h = rec.max_height.map(|h| format!("[height<={h}]")).unwrap_or_default();
        a.extend(["-f".into(), format!("bv*{h}+ba/b{h}/bv*+ba/b"), "-S".into(), "vcodec:h264,acodec:aac".into()]);
        a.extend(["--merge-output-format", "mp4"].map(String::from));
    }
    a.extend(["--download-sections".into(), format!("*{s:.1}-{e:.1}")]);
    a.extend(["-P".into(), s_(&rec.work), "-o".into(), format!("{stem}.%(ext)s")]);
    a.extend(crate::cookies::args_for(rec.url, rec.settings));
    a.push("--".into());
    a.push(rec.url.to_string());
    let out = proc::command(paths::tool("yt-dlp")).args(&a).stdout(Stdio::null()).stderr(Stdio::piped()).output()?;
    let file = files_of(&rec.work, stem).into_iter().find(|f| {
        let n = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        !n.ends_with(".part") && !n.ends_with(".ytdl") && std::fs::metadata(f).is_ok_and(|m| m.len() > 0)
    });
    match file {
        Some(f) if out.status.success() => Ok(f),
        _ => bail!(
            "{}",
            String::from_utf8_lossy(&out.stderr).lines().rfind(|l| l.contains("ERROR")).unwrap_or("couldn't fetch it")
        ),
    }
}

fn s_(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Fetch the recording's missed bits again (those a minute old or more, not fetched yet).
/// Returns how many were got back now. `stop`: give up between bits.
pub fn repair_holes(rec: &Recording, stop: &AtomicBool) -> Result<usize> {
    let m = read_manifest(&rec.work).context("Nothing recorded yet")?;
    let started = m.started_at.context("Neria doesn't know when the stream began")?;
    let mut fills = fills_of(&rec.work);
    let age_now = now() - started;
    let mut got = 0;
    for (i, h) in m.holes.iter().enumerate() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let done = fills.iter().any(|f| (f.at - h.at).abs() < 1.0);
        // A minute old: the site has it by then.
        if done || h.at + h.len + FILL_AFTER > age_now - 30.0 || h.len < 0.5 {
            continue;
        }
        let from = (h.at - FILL_MARGIN).max(0.0);
        let to = h.at + h.len + FILL_AFTER;
        let stem = format!("fill{i}");
        let file = fetch_stretch(rec, started, from, to, &stem)?;
        fills.push(Fill {
            at: h.at,
            len: h.len,
            file: file.file_name().unwrap_or_default().to_string_lossy().into(),
            from,
        });
        if let Ok(text) = serde_json::to_string_pretty(&fills) {
            std::fs::write(fills_path(&rec.work), text)?;
        }
        got += 1;
    }
    Ok(got)
}

/// Where a fill takes over in a recording, by sound: (seconds into the recording where it
/// broke off, the fill's own seconds to keep `a`–`b`, seconds into the recording where it
/// picks up again). `expect`: about where in the recording the hole is.
pub fn splice_points(recording: &Path, fill: &Path, expect: f64, hole: f64) -> Option<(f64, f64, f64, f64)> {
    use crate::fingerprint::{ITEM_SECS, align, fingerprint_range};
    let fill_len = crate::media::duration(fill)?;
    let around = 90.0 + hole;
    let from = (expect - around).max(0.0);
    let rec_fp = fingerprint_range(recording.as_os_str(), from, 2.0 * around + fill_len).ok()?;
    let fill_fp = fingerprint_range(fill.as_os_str(), 0.0, fill_len).ok()?;
    // Each 6 s of the fill: where it is in the recording (if anywhere). `align` wants 40
    // items (5 s) at least.
    const WIN: f64 = 6.0;
    let win = (WIN / ITEM_SECS) as usize;
    let step = (1.0 / ITEM_SECS) as usize;
    let mut hits: Vec<(f64, Option<f64>)> = Vec::new();
    let mut k = 0;
    while k + win <= fill_fp.len() {
        let t = k as f64 * ITEM_SECS;
        let at = align(&fill_fp[k..k + win], &rec_fp).filter(|a| a.similarity >= 0.8).map(|a| from + a.offset_secs - t);
        hits.push((t, at));
        k += step;
    }
    if std::env::var_os("NERIA_REC_DEBUG").is_some() {
        eprintln!(
            "SPLICE expect {expect:.1} hole {hole:.1} fill {fill_len:.1} search from {from:.1} ({} / {} items)",
            rec_fp.len(),
            fill_fp.len()
        );
        let keep = std::env::temp_dir().join("neria-splice");
        let _ = std::fs::create_dir_all(&keep);
        let _ = std::fs::copy(recording, keep.join("recording.mp4"));
        let _ = std::fs::copy(fill, keep.join("fill.mp4"));
        for (t, d) in &hits {
            eprintln!("  fill {t:6.1} -> {}", d.map_or("-".to_string(), |d| format!("{:.2}", d + t)));
        }
    }
    // The first run of matches (before the hole) and the last (after it): each keeps the same
    // difference between the fill's clock and the recording's.
    let same = |a: f64, b: f64| (a - b).abs() < 0.6;
    let first = hits.iter().position(|(_, d)| d.is_some())?;
    let d1 = hits[first].1?;
    let mut end1 = first;
    while end1 + 1 < hits.len() && hits[end1 + 1].1.is_some_and(|d| same(d, d1)) {
        end1 += 1;
    }
    let last = hits.iter().rposition(|(_, d)| d.is_some())?;
    let d2 = hits[last].1?;
    let mut start2 = last;
    while start2 > end1 + 1 && hits[start2 - 1].1.is_some_and(|d| same(d, d2)) {
        start2 -= 1;
    }
    if start2 <= end1 || same(d1, d2) {
        // No jump between them: nothing is missing here (or it can't be told).
        return None;
    }
    let a = hits[end1].0 + WIN - 1.0; // the end of the last matching stretch, a little in
    let b = hits[start2].0 + 1.0; // into the first one after
    let cut = a + d1;
    let resume = b + d2;
    (b > a && resume > cut - 1.0).then_some((cut, a, b, resume))
}

/// Splice the fetched bits into the saved `file` (in place). How many went in.
fn splice_fills(work: &Path, file: &Path, starts_at: f64) -> usize {
    let mut fills = fills_of(work);
    fills.sort_by(|a, b| b.at.total_cmp(&a.at)); // last first: earlier places don't move
    let mut done = 0;
    for f in fills {
        let fill = work.join(&f.file);
        let Some((cut, a, b, resume)) = splice_points(file, &fill, f.at - starts_at, f.len) else { continue };
        let pieces =
            [(file.to_path_buf(), 0.0, Some(cut)), (fill.clone(), a, Some(b - a)), (file.to_path_buf(), resume, None)];
        let mut parts = Vec::new();
        for (i, (src, ss, t)) in pieces.iter().enumerate() {
            let out = work.join(format!("splice{i}.mp4"));
            let mut args = vec!["-ss".to_string(), format!("{ss:.3}"), "-i".into(), s(src)];
            if let Some(t) = t {
                args.extend(["-t".into(), format!("{t:.3}")]);
            }
            args.extend(["-map", "0", "-c", "copy", "-avoid_negative_ts", "make_zero"].map(String::from));
            if ffmpeg(&args, &out).is_err() {
                break;
            }
            parts.push(out);
        }
        if parts.len() != 3 {
            continue;
        }
        let list = work.join("splice.txt");
        let lines: Vec<String> = parts.iter().map(|p| format!("file '{}'", s(p).replace('\'', "'\\''"))).collect();
        if std::fs::write(&list, lines.join("\n")).is_err() {
            continue;
        }
        let joined = work.join("spliced.mp4");
        let args: Vec<String> =
            ["-f", "concat", "-safe", "0", "-i", &s(&list), "-map", "0", "-c", "copy", "-movflags", "+faststart"]
                .iter()
                .map(|x| x.to_string())
                .collect();
        if ffmpeg(&args, &joined).is_ok() && std::fs::rename(&joined, file).is_ok() {
            done += 1;
        }
    }
    done
}

/// Throw a recording away (Cancel).
pub fn discard(work: &Path) {
    for _ in 0..20 {
        if std::fs::remove_dir_all(work).is_ok() || !work.exists() {
            return;
        }
        // Windows keeps the files locked for a moment after the tools end.
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// ffmpeg, quietly; Ok when it exited well and wrote something that has a length.
fn ffmpeg(args: &[String], out: &Path) -> Result<f64> {
    let result = proc::command(paths::tool("ffmpeg"))
        .args(["-hide_banner", "-nostdin", "-v", "error", "-y"])
        .args(args)
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("failed to start ffmpeg")?;
    let len = crate::media::duration(out).unwrap_or(0.0);
    if !result.status.success() || len <= 0.0 {
        let _ = std::fs::remove_file(out);
        let err = String::from_utf8_lossy(&result.stderr);
        bail!("ffmpeg: {}", err.lines().last().unwrap_or("failed"));
    }
    Ok(len)
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// One playable file for a part (in the work folder), from whatever it left.
fn part_file(work: &Path, part: &Part) -> Result<Option<PathBuf>> {
    let files: Vec<PathBuf> =
        files_of(work, &part.name).into_iter().filter(|f| std::fs::metadata(f).is_ok_and(|m| m.len() > 0)).collect();
    let name = |f: &PathBuf| f.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let out = work.join(format!("{}.ready.mp4", part.name));
    if out.is_file() && crate::media::duration(&out).is_some_and(|d| d > 0.0) {
        return Ok(Some(out));
    }
    // yt-dlp finished it (the stream ended): a normal file. Kept as it is, unless it's from
    // now (its timestamps may need mending, see `TS_FIX`) or starts with a bit to leave out.
    let done = files.iter().find(|f| {
        let n = name(f);
        !n.ends_with(".part")
            && !n.ends_with(".ytdl")
            && !n.contains(".ready.")
            && !n.contains(".part-Frag")
            && !is_format_piece(&n)
    });
    if let Some(done) = done
        && part.from_start
        && part.skip <= 0.0
    {
        return Ok(Some(done.clone()));
    }
    let parts: Vec<&PathBuf> = files.iter().filter(|f| name(f).ends_with(".part")).chain(done).collect();
    let pieces: Vec<&PathBuf> = parts.iter().copied().filter(|f| is_format_piece(&name(f))).collect();
    let mut args: Vec<String> = Vec::new();
    if pieces.len() >= 2 {
        // From the start, stopped: video and audio apart; end where both have data.
        for p in &pieces {
            args.extend(["-i".into(), s(p)]);
        }
        args.extend(["-map", "0:v?", "-map", "1:a?", "-map", "0:a?", "-map", "1:v?"].map(String::from));
        args.extend(["-c", "copy", "-shortest", "-avoid_negative_ts", "make_zero"].map(String::from));
    } else if let Some(p) = parts.first().or(files.iter().find(|f| is_format_piece(&name(f))).as_ref()) {
        args.extend(["-i".into(), s(p), "-map".into(), "0".into(), "-dn".into(), "-ignore_unknown".into()]);
        args.extend(
            ["-c", "copy", "-bsf:a", "aac_adtstoasc", "-bsf:v", TS_FIX, "-avoid_negative_ts", "make_zero"]
                .map(String::from),
        );
    } else {
        return Ok(None);
    }
    if part.skip > 0.0 {
        // Starts with pieces the part before already has.
        args.splice(0..0, ["-ss".to_string(), format!("{:.3}", part.skip)]);
    }
    args.extend(["-movflags", "+faststart"].map(String::from));
    match ffmpeg(&args, &out) {
        Ok(_) => Ok(Some(out)),
        Err(e) => {
            // Nothing usable in it (it broke before the first piece).
            if part.recorded < 1.0 { Ok(None) } else { Err(e) }
        }
    }
}

/// A from-now part's frames that show at the stream's own clock instead of the part's: Twitch
/// stitches its ad in front for viewers who aren't signed in, and where the ad ends two
/// frames come out ~5000 s ahead of where they're decoded (shown: a saved file that claimed
/// 7878 s for 4517 s, and a player that stalls there). Normal frames show within a second of
/// being decoded, so a frame more than 5 s ahead is shown where it's decoded.
const TS_FIX: &str = "setts=pts='if(gt(PTS-DTS,5/TB),DTS,PTS)'";

/// The earlier part, cut where the recording takes over.
fn early_file(work: &Path, e: &Early) -> Option<PathBuf> {
    let whole = work.join("early.ready.mp4");
    if !whole.is_file() || e.until < 1.0 {
        return None;
    }
    let out = work.join("early.cut.mp4");
    if out.is_file() && crate::media::duration(&out).is_some_and(|d| d > 0.0) {
        return Some(out);
    }
    let args: Vec<String> = ["-i", &s(&whole), "-t", &format!("{:.3}", e.until), "-map", "0", "-c", "copy"]
        .iter()
        .map(|x| x.to_string())
        .chain(["-avoid_negative_ts", "make_zero", "-movflags", "+faststart"].map(String::from))
        .collect();
    ffmpeg(&args, &out).ok().map(|_| out)
}

/// `p0.f135.mp4.part`: one stream of a two-stream download.
fn is_format_piece(name: &str) -> bool {
    name.split('.')
        .nth(1)
        .is_some_and(|f| f.len() > 1 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Make the recording in `work` into its file in `out_dir` (one file; numbered files when
/// the parts can't be joined), then remove `work`. Runs again safely after a crash midway.
pub fn save(work: &Path, out_dir: &Path) -> Result<Saved> {
    // The earlier part is still coming in (the stream ended while it was) or being cut and
    // matched (Stop ends its download too): wait while it's moving (it touches its marker).
    while std::fs::metadata(early_busy(work))
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(90))
    {
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut m = read_manifest(work).context("The recording's list of parts is missing")?;
    let early = early_of(work);
    if let (Some(e), Some(first)) = (early.as_ref(), m.parts.first_mut()) {
        first.skip = first.skip.max(e.skip);
        first.recorded = (first.recorded - e.skip).max(0.0);
        if !e.whole && first.offset > e.got + 2.0 {
            m.holes.push(Hole { at: e.got, len: first.offset - e.got, about: true });
        }
    }
    let need = part_bytes(work, &m.parts) as u64 + crate::disk::MARGIN / 4;
    if let Err(e) = crate::disk::check(out_dir, need.saturating_sub(crate::disk::MARGIN)) {
        bail!("{e}. Free some space, then press Retry to save the recording.");
    }
    let mut ready = Vec::new();
    if let Some(e) = early.as_ref()
        && let Some(f) = early_file(work, e)
    {
        ready.push(f);
    }
    for p in &m.parts {
        if let Some(f) = part_file(work, p)? {
            ready.push(f);
        }
    }
    if ready.is_empty() {
        discard(work);
        bail!("Nothing was recorded");
    }
    let name = crate::music::sanitize(&m.title);
    let name = if name.trim().is_empty() { "Live".to_string() } else { name };
    let ext = if m.ext.is_empty() { "mp4".to_string() } else { m.ext.clone() };
    std::fs::create_dir_all(out_dir)?;
    let mut files = Vec::new();
    let joined = work.join(format!("joined.{}", if m.audio { "mp4" } else { &ext }));
    let one = if ready.len() == 1 {
        Some(ready[0].clone())
    } else {
        let list = work.join("join.txt");
        let lines: Vec<String> = ready.iter().map(|p| format!("file '{}'", s(p).replace('\'', "'\\''"))).collect();
        std::fs::write(&list, lines.join("\n"))?;
        let args: Vec<String> =
            ["-f", "concat", "-safe", "0", "-i", &s(&list), "-map", "0", "-c", "copy", "-movflags", "+faststart"]
                .iter()
                .map(|x| x.to_string())
                .collect();
        ffmpeg(&args, &joined).ok().map(|_| joined.clone())
    };
    // Missed bits fetched again go in where the sound says (the file begins at `starts`).
    let mut got_back = 0.0;
    let mut holes = m.holes.clone();
    if let Some(file) = one.as_ref() {
        let starts = if early.as_ref().is_some_and(|e| e.until >= 1.0) {
            0.0
        } else {
            m.parts.first().map_or(0.0, |p| p.offset + p.skip)
        };
        let before = fills_of(work);
        if !before.is_empty() && splice_fills(work, file, starts) > 0 {
            // The holes that went in (spliced ones are those whose bit the file now holds).
            for f in before {
                if let Some(i) = holes.iter().position(|h| (h.at - f.at).abs() < 1.0) {
                    got_back += holes[i].len;
                    holes.remove(i);
                }
            }
        }
    }
    match one {
        Some(file) => files.push(finish_one(&file, out_dir, &name, &ext, m.audio, work)?),
        None => {
            // Parts that won't join (the stream changed size or format midway): one file each.
            for (i, f) in ready.iter().enumerate() {
                files.push(finish_one(f, out_dir, &format!("{name} (part {})", i + 1), &ext, m.audio, work)?);
            }
        }
    }
    // What the file really holds (the count while recording is an estimate).
    let recorded = files.iter().filter_map(|f| crate::media::duration(f)).sum::<f64>();
    let recorded = if recorded > 0.0 { recorded } else { m.parts.iter().map(|p| p.recorded).sum() };
    // Its chat, timed to each file: where in the stream each one begins.
    let file_starts: Vec<f64> = if files.len() == 1 {
        vec![if early.as_ref().is_some_and(|e| e.until >= 1.0) { 0.0 } else { m.parts.first().map_or(0.0, |p| p.offset + p.skip) }]
    } else {
        early.iter().map(|_| 0.0).chain(m.parts.iter().map(|p| p.offset + p.skip)).collect()
    };
    let pairs: Vec<(PathBuf, f64)> = files.iter().cloned().zip(file_starts).collect();
    save_chat(work, &m, &pairs);
    discard(work);
    let note =
        if got_back > 0.0 { format!("got back {} missed", crate::fmt::duration(got_back)) } else { String::new() };
    Ok(Saved { files, note, recorded, holes })
}

/// Move (or, for another type, convert) a finished file into place under a free name.
fn finish_one(file: &Path, out_dir: &Path, name: &str, ext: &str, audio: bool, work: &Path) -> Result<PathBuf> {
    let out = crate::clip::unique_path(out_dir, name, ext);
    let same_type = file.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext));
    if same_type && !audio {
        if std::fs::rename(file, &out).is_err() {
            std::fs::copy(file, &out)?;
        }
        return Ok(out);
    }
    let mut args = vec!["-i".to_string(), s(file)];
    if audio {
        args.extend(["-vn".into(), "-map".into(), "0:a".into()]);
        let enc = crate::sound::encoder_for(ext);
        if ext == "m4a" || ext == "aac" || enc.is_empty() {
            args.extend(["-c:a".into(), "copy".into()]);
        } else {
            args.extend(enc);
        }
    } else {
        args.extend(["-map", "0", "-c", "copy"].map(String::from));
    }
    if ext == "mp4" || ext == "m4a" {
        args.extend(["-movflags", "+faststart"].map(String::from));
    }
    // Written in the recording's folder, then moved: a crash midway leaves nothing half
    // done next to the user's files.
    let tmp = work.join(format!("final.{ext}"));
    ffmpeg(&args, &tmp)?;
    std::fs::rename(&tmp, &out)?;
    Ok(out)
}

/// "Clip the last…": the newest `seconds` of what's recorded, as its own file in `out_dir`,
/// while the recording goes on.
pub fn clip_last(work: &Path, seconds: f64, out_dir: &Path, title: &str) -> Result<PathBuf> {
    clip_last_as(work, seconds, out_dir, title, &format!("last {}", crate::clip::stamp(seconds)))
}

/// A clip marked while recording ("Start clip" … "End clip"): it ends now, so it's the last
/// `seconds`, named by where it sits in the recording ("clip 12m30s-15m10s").
pub fn clip_marked(work: &Path, from: f64, to: f64, out_dir: &Path, title: &str) -> Result<PathBuf> {
    let tag = format!("clip {}-{}", crate::clip::stamp(from), crate::clip::stamp(to));
    clip_last_as(work, (to - from).max(1.0), out_dir, title, &tag)
}

fn clip_last_as(work: &Path, seconds: f64, out_dir: &Path, title: &str, tag: &str) -> Result<PathBuf> {
    let m = read_manifest(work).context("The recording isn't there")?;
    let part = m.parts.last().context("Nothing recorded yet")?;
    let files: Vec<PathBuf> = files_of(work, &part.name)
        .into_iter()
        .filter(|f| {
            let n = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !n.ends_with(".ytdl") && !n.contains(".ready.") && std::fs::metadata(f).is_ok_and(|m| m.len() > 0)
        })
        .collect();
    let name = crate::music::sanitize(title);
    let name = if name.trim().is_empty() { "Live".to_string() } else { name };
    std::fs::create_dir_all(out_dir)?;
    let ext = if m.audio { "m4a" } else { "mp4" };
    let out = crate::clip::unique_path(out_dir, &format!("{name} ({tag})"), ext);
    // Not longer than this part holds (the connection dropped since the clip started).
    let seconds = if part.recorded >= 1.0 { seconds.min(part.recorded) } else { seconds };
    let pieces: Vec<&PathBuf> =
        files.iter().filter(|f| is_format_piece(f.file_name().and_then(|n| n.to_str()).unwrap_or(""))).collect();
    let mut args: Vec<String> = Vec::new();
    if pieces.len() >= 2 {
        // Video and sound come down apart, each starting at the same piece: cut both from
        // what both hold (the part's count, kept every few seconds). Their own lengths can't
        // be read while they grow (ffprobe gives the stream's clock for them).
        if part.recorded < 1.0 {
            bail!("Nothing recorded yet: give it a few seconds.");
        }
        let from = (part.recorded - seconds).max(0.0);
        for p in &pieces {
            args.extend(["-ss".into(), format!("{from:.3}"), "-i".into(), s(p)]);
        }
        args.extend(["-t".into(), format!("{seconds:.3}")]);
        args.extend(["-map", "0:v?", "-map", "1:a?", "-map", "0:a?", "-map", "1:v?"].map(String::from));
    } else if let Some(f) = files.first() {
        args.extend(["-sseof".into(), format!("-{seconds:.3}"), "-i".into(), s(f), "-map".into(), "0".into()]);
        args.extend(["-dn".into(), "-bsf:a".into(), "aac_adtstoasc".into(), "-bsf:v".into(), TS_FIX.into()]);
    } else {
        bail!("Nothing recorded yet");
    }
    if m.audio {
        args.push("-vn".into());
    }
    args.extend(["-c", "copy", "-avoid_negative_ts", "make_zero", "-movflags", "+faststart"].map(String::from));
    ffmpeg(&args, &out)?;
    Ok(out)
}

/// The recordings left in `out_dir`-style folders by a crash are saved from the queue
/// (it knows their folders); this says whether `work` still holds one.
pub fn has_recording(work: &Path) -> bool {
    read_manifest(work).is_some_and(|m| part_bytes(work, &m.parts) > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fragment_lines_with_their_stream() {
        assert_eq!(
            parse("2: NERIA_DL downloading|3687107|NA|NA|137670.3|0.0|45|461243"),
            Line::Fragment { stream: 2, bytes: Some(3687107.0), index: Some(45.0), count: Some(461243.0) }
        );
        assert_eq!(
            parse("NERIA_DL downloading|100|NA|NA|NA|NA|NA|NA"),
            Line::Fragment { stream: 1, bytes: Some(100.0), index: None, count: None }
        );
    }

    #[test]
    fn reads_ffmpeg_status() {
        let l = "frame= 1560 fps= 34 q=-1.0 size=    8960KiB time=00:00:51.98 bitrate=1411.8kbits/s speed=1.13x";
        assert_eq!(parse(l), Line::Ffmpeg { bytes: Some(8960.0 * 1024.0), time: Some(51.98) });
        let a = "size=     512kB time=01:00:02.00 bitrate= 128.0kbits/s speed=1x";
        assert_eq!(parse(a), Line::Ffmpeg { bytes: Some(512_000.0), time: Some(3602.0) });
        assert_eq!(parse("size=N/A time=N/A bitrate=N/A"), Line::Ffmpeg { bytes: None, time: None });
    }

    #[test]
    fn numbers_pieces_and_hides_addresses() {
        let yt = "[https @ 0000] Opening 'https://rr7.googlevideo.com/videoplayback/id/x.1/itag/234/ip/1.2.3.4/playlist/index.m3u8/sq/199745/goap/lmt%3D57/dur/5.000/file/seg.ts' for reading";
        assert_eq!(parse(yt), Line::Piece { key: "234".into(), seq: 199745, len: Some(5.0) });
        let tw =
            "[hls @ 0000] Opening 'https://d2nvs.cloudfront.net/abc_shroud_1794705616/480p30/2280.mp4' for reading";
        assert_eq!(parse(tw), Line::Piece { key: "480p30".into(), seq: 2280, len: None });
        let opaque = "[hls @ 0000] Opening 'https://x.ttvnw.net/v1/segment/CoADgbzujXmrXOlDPzP' for reading";
        assert_eq!(parse(opaque), Line::Quiet);
        let playlist = "[https @ 0000] Opening 'https://manifest.googlevideo.com/api/manifest/hls_playlist/ip/1.2.3.4/playlist/index.m3u8' for reading";
        assert_eq!(parse(playlist), Line::Quiet);
        assert_eq!(parse("[download] Destination: https://x/y"), Line::Quiet);
        assert_eq!(
            parse("[mov,mp4,m4a,3gp,3g2,mj2 @ 000002577468e200] Found duplicated MOOV Atom. Skipped it"),
            Line::Quiet
        );
        assert_eq!(parse("[hls @ 0000] Error when loading first segment"), Line::Other);
        assert_eq!(parse("[hls @ 0000] skipping 3 segments ahead, expired from playlists"), Line::Missed(3));
        assert_eq!(parse("[hls @ 0000] Failed to open segment 1234 of playlist 0"), Line::Missed(1));
        assert_eq!(parse("2: [download] fragment not found; Skipping fragment 57 ..."), Line::Skipped(Some(57)));
    }

    #[test]
    fn reads_info_and_waiting() {
        assert_eq!(
            parse("NERIA_LIVE 480|1794696101|NA|is_live"),
            Line::Info { height: Some(480), started: Some(1794696101.0) }
        );
        assert_eq!(
            parse("NERIA_LIVE NA|NA|1794696101|is_live"),
            Line::Info { height: None, started: Some(1794696101.0) }
        );
        assert_eq!(parse("[wait] Remaining time until next attempt: 00:00:30"), Line::Waiting);
    }

    #[test]
    fn knows_two_stream_pieces() {
        assert!(is_format_piece("p0.f135.mp4.part"));
        assert!(is_format_piece("p0.f140.mp4"));
        assert!(!is_format_piece("p0.mp4.part"));
        assert!(!is_format_piece("p0.ready.mp4"));
    }

    #[test]
    fn sites_that_rewind() {
        assert!(can_record_from_start("https://www.youtube.com/watch?v=x"));
        assert!(can_record_from_start("https://www.twitch.tv/shroud"));
        assert!(!can_record_from_start("https://kick.com/x"));
        assert!(!can_record_from_start("https://notyoutube.com/x"));
    }

    #[test]
    fn says_where_it_starts() {
        use crate::probe::LiveInfo;
        let now = 1_000_000.0;
        let young =
            LiveInfo { started_at: Some(now - 2.0 * 3600.0 - 14.0 * 60.0), can_rewind: true, ..Default::default() };
        let c = start_choice(&young, None, now);
        assert!(c.from_start);
        assert_eq!(c.hint, "about 22 min to catch up, then it follows live");
        assert_eq!(c.switch, "Record from now instead");
        let c = start_choice(&young, Some(false), now);
        assert!(!c.from_start);
        assert_eq!(c.switch, "Record from the start instead");
        // Long ago: from now by itself, from the start still there to pick.
        let day = LiveInfo { started_at: Some(now - 30.0 * 3600.0), can_rewind: true, ..Default::default() };
        assert!(!start_choice(&day, None, now).from_start);
        assert!(start_choice(&day, Some(true), now).from_start);
        // Longer than the site keeps: no choice.
        let radio = LiveInfo { started_at: Some(now - 11.0 * 86400.0), can_rewind: true, ..Default::default() };
        let c = start_choice(&radio, Some(true), now);
        assert!(!c.from_start && c.switch.is_empty());
        assert_eq!(c.hint, "it's been live for 11 days, longer than the site keeps");
        let flat = LiveInfo { started_at: Some(now - 600.0), can_rewind: false, ..Default::default() };
        assert_eq!(start_choice(&flat, Some(true), now).hint, "this stream can't be rewound");
        let soon =
            LiveInfo { upcoming: true, starts_at: Some(now + 42.0 * 60.0), can_rewind: true, ..Default::default() };
        let c = start_choice(&soon, None, now);
        assert_eq!((c.line.as_str(), c.from_start), ("Starts in 42 min", true));
    }

    #[test]
    fn ages_in_words() {
        assert_eq!(age_words(30.0), "under a minute");
        assert_eq!(age_words(40.0 * 60.0), "40 min");
        assert_eq!(age_words(2.0 * 3600.0 + 14.0 * 60.0), "2 h 14 min");
        assert_eq!(age_words(3.0 * 3600.0), "3 h");
        assert_eq!(age_words(11.0 * 86400.0), "11 days");
    }

    #[test]
    fn stream_length_for_the_tape() {
        let s = LiveStats { started_at: Some(1000.0), position: 50.0, ..Default::default() };
        assert_eq!(s.stream_len(1600.0), 600.0);
        let s = LiveStats { started_at: None, position: 50.0, ..Default::default() };
        assert_eq!(s.stream_len(1600.0), 50.0);
    }
}
