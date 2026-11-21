//! Clipping live streams (Clip track 8).
//!
//! **Rewind**: sites like YouTube keep the last part of a live stream seekable (its HLS
//! playlist is a "DVR" window, an hour on YouTube). The clipper plays that window; a mark on
//! its timeline is pinned to the wall clock (`anchor + position`), because the window keeps
//! sliding. When the download starts, the mark is turned back into seconds from the start of
//! the window *as it is then* (`section_now`) and handed to yt-dlp's `--download-sections`.
//!
//! **Replay buffer**: any live stream, rewindable or not: ffmpeg records it into short
//! pieces that overwrite the oldest (`ReplayBuffer`), so disk use stays fixed, and "Clip the
//! last…" joins the newest pieces into a file.

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::settings::Settings;
use crate::{paths, proc};

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// How much of the live stream the site keeps, in seconds (YouTube: 3600), read from the
/// stream's own playlist address (`…/playlist_duration/3600/…`), or by adding up its pieces.
pub fn window_len(url: &str, settings: &Settings) -> Result<f64> {
    let mut cmd = proc::command(paths::tool("yt-dlp"));
    cmd.args(["--no-warnings", "--no-playlist", "-g", "-f", "bv*[protocol^=m3u8]/b[protocol^=m3u8]/b"]);
    cmd.args(crate::cookies::args_for(url, settings));
    let out = cmd.arg("--").arg(url).stdout(Stdio::piped()).stderr(Stdio::null()).output()?;
    let stream = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").trim().to_string();
    if stream.is_empty() {
        bail!("Couldn't reach the live stream");
    }
    if let Some(len) = playlist_duration(&stream) {
        return Ok(len);
    }
    // Add up the pieces of the playlist itself.
    let text = crate::web::get_text(&stream).context("couldn't read the stream's playlist")?;
    let total = playlist_total(&text);
    if total <= 0.0 {
        bail!("This live stream can't be rewound");
    }
    Ok(total)
}

fn playlist_duration(stream_url: &str) -> Option<f64> {
    let rest = stream_url.split("/playlist_duration/").nth(1)?;
    rest.split(['/', '?', '&']).next()?.parse().ok()
}

fn playlist_total(m3u8: &str) -> f64 {
    m3u8.lines()
        .filter_map(|l| l.strip_prefix("#EXTINF:"))
        .filter_map(|l| l.split(',').next()?.trim().parse::<f64>().ok())
        .sum()
}

/// A part pinned to the wall clock (`from`–`to`, Unix seconds) as seconds from the start
/// of the site's window right now. Fails in plain words once it has scrolled out.
pub fn section_now(url: &str, settings: &Settings, from: f64, to: f64) -> Result<(f64, f64)> {
    let window = window_len(url, settings)?;
    to_window(window, now(), from, to)
}

fn to_window(window: f64, now: f64, from: f64, to: f64) -> Result<(f64, f64)> {
    let start = now - window;
    // Not yet in the window (the PC's clock is off?): say so, don't hand ffmpeg a bad range.
    if from >= now || to <= start {
        bail!("Couldn't find that part of the live stream (is the PC's clock right?). Mark it again in the clipper.");
    }
    if from < start {
        bail!(
            "That part of the live stream is gone: the site keeps only the last {}. Mark it again in the clipper.",
            crate::fmt::duration(window)
        );
    }
    Ok(((from - start).max(0.0), (to - start).min(window)))
}

/// The newest `seconds` of a recording made by `ReplayBuffer`, out of its folder.
fn newest_pieces(dir: &Path, seconds: f64, piece_secs: f64) -> Vec<PathBuf> {
    let mut pieces: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "ts"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    // Oldest first; the newest is still being written, so it's left out.
    pieces.sort_by_key(|(t, _)| *t);
    pieces.pop();
    let want = (seconds / piece_secs).ceil() as usize;
    let skip = pieces.len().saturating_sub(want);
    pieces.into_iter().skip(skip).map(|(_, p)| p).collect()
}

/// "Keep the last 10 minutes" of a live stream, OBS-style: ffmpeg records it into short
/// pieces that overwrite the oldest, in Neria's own folder (`replay/`), so the space used
/// stays the same however long it runs.
pub struct ReplayBuffer {
    child: Arc<Mutex<Option<Child>>>,
    pub dir: PathBuf,
    pub minutes: u32,
    pub started: f64,
    stopped: Arc<AtomicBool>,
    /// Why it stopped by itself ("" while running).
    pub problem: Arc<Mutex<String>>,
    /// The thread that keeps ffmpeg going; `stop` waits for it.
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

pub const PIECE_SECS: f64 = 6.0;

impl ReplayBuffer {
    /// Start keeping the last `minutes` of `url`. The stream address is looked up again
    /// whenever ffmpeg stops (they expire), until `stop`.
    pub fn start(url: &str, settings: &Settings, minutes: u32) -> Result<ReplayBuffer> {
        let dir = paths::app_dir().join("replay");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let buffer = ReplayBuffer {
            child: Arc::new(Mutex::new(None)),
            dir: dir.clone(),
            minutes,
            started: now(),
            stopped: Arc::new(AtomicBool::new(false)),
            problem: Arc::new(Mutex::new(String::new())),
            worker: Mutex::new(None),
        };
        let (child, stopped, problem) = (buffer.child.clone(), buffer.stopped.clone(), buffer.problem.clone());
        let (url, settings) = (url.to_string(), settings.clone());
        let wrap = ((minutes as f64 * 60.0) / PIECE_SECS).ceil() as u32 + 2;
        let worker = std::thread::spawn(move || {
            let mut failures = 0;
            while !stopped.load(Ordering::SeqCst) {
                let started = std::time::Instant::now();
                match record(&url, &settings, &dir, wrap, &child, &stopped) {
                    Ok(()) => {}
                    Err(e) => *problem.lock().unwrap() = e.to_string(),
                }
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                // Ran for a while: the address expired or the stream hiccuped; go again.
                failures = if started.elapsed() > Duration::from_secs(60) { 0 } else { failures + 1 };
                if failures >= 3 {
                    let mut p = problem.lock().unwrap();
                    if p.is_empty() {
                        *p = "The live stream stopped".into();
                    }
                    break;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            stopped.store(true, Ordering::SeqCst);
        });
        *buffer.worker.lock().unwrap() = Some(worker);
        Ok(buffer)
    }

    pub fn running(&self) -> bool {
        !self.stopped.load(Ordering::SeqCst)
    }

    /// How much is kept so far, in seconds, and the space it takes.
    pub fn kept(&self) -> (f64, u64) {
        let pieces = newest_pieces(&self.dir, f64::MAX, PIECE_SECS);
        let bytes = pieces.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        (pieces.len() as f64 * PIECE_SECS, bytes)
    }

    /// The last `seconds` as one file in `out_dir` (`Title (last 1m00s).mp4`).
    pub fn clip_last(&self, seconds: f64, out_dir: &Path, title: &str) -> Result<PathBuf> {
        clip_last_in(&self.dir, seconds, out_dir, title)
    }

    /// Stop recording and throw the pieces away.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            proc::kill_tree(c);
        }
        // Wait for the worker (it starts no new ffmpeg once stopped), then throw the pieces away.
        if let Some(w) = self.worker.lock().unwrap().take() {
            for _ in 0..60 {
                if w.is_finished() {
                    break;
                }
                if let Some(c) = self.child.lock().unwrap().as_mut() {
                    proc::kill_tree(c);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        // Windows keeps the last piece locked for a moment after ffmpeg ends.
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A replay buffer's pieces left by a crash: thrown away when Neria starts.
pub fn clean_leftovers() {
    let _ = std::fs::remove_dir_all(paths::app_dir().join("replay"));
}

/// [`ReplayBuffer::clip_last`] from its folder (`buffer.dir`), so it can run off the UI thread.
pub fn clip_last_in(dir: &Path, seconds: f64, out_dir: &Path, title: &str) -> Result<PathBuf> {
    {
        let pieces = newest_pieces(dir, seconds, PIECE_SECS);
        if pieces.is_empty() {
            bail!("Nothing kept yet: give it a few seconds.");
        }
        std::fs::create_dir_all(out_dir)?;
        let name = crate::music::sanitize(title);
        let name = if name.trim().is_empty() { "Live".to_string() } else { name };
        let out = crate::clip::unique_path(out_dir, &format!("{name} (last {})", crate::clip::stamp(seconds)), "mp4");
        let list = dir.join(format!("clip-{}.txt", now() as u64));
        let lines: Vec<String> =
            pieces.iter().map(|p| format!("file '{}'", p.to_string_lossy().replace('\'', "'\\''"))).collect();
        std::fs::write(&list, lines.join("\n"))?;
        let result = proc::command(paths::tool("ffmpeg"))
            .args(["-hide_banner", "-v", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list)
            .args(["-map", "0", "-c", "copy", "-bsf:a", "aac_adtstoasc", "-movflags", "+faststart"])
            .arg(&out)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        let _ = std::fs::remove_file(&list);
        let result = result?;
        if !result.status.success() || !out.is_file() {
            let _ = std::fs::remove_file(&out);
            let err = String::from_utf8_lossy(&result.stderr);
            bail!("Couldn't save it: {}", err.lines().last().unwrap_or("ffmpeg failed"));
        }
        Ok(out)
    }
}

impl Drop for ReplayBuffer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One ffmpeg run: the stream into `piece000.ts`… wrapping after `wrap` pieces.
fn record(
    url: &str,
    settings: &Settings,
    dir: &Path,
    wrap: u32,
    slot: &Mutex<Option<Child>>,
    stopped: &AtomicBool,
) -> Result<()> {
    // The stream's own addresses (video + audio, or one with both), at a sane size.
    let mut cmd = proc::command(paths::tool("yt-dlp"));
    cmd.args(["--no-warnings", "--no-playlist", "-g", "-f", "bv*[height<=1080]+ba/b[height<=1080]/b"]);
    cmd.args(crate::cookies::args_for(url, settings));
    let out = cmd.arg("--").arg(url).stdout(Stdio::piped()).stderr(Stdio::piped()).output()?;
    let urls: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    if urls.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("Couldn't reach the live stream: {}", err.lines().last().unwrap_or("no stream"));
    }
    let mut cmd = proc::command(paths::tool("ffmpeg"));
    cmd.args(["-hide_banner", "-v", "error", "-y"]);
    for u in &urls {
        cmd.args(["-i", u]);
    }
    for i in 0..urls.len() {
        cmd.args(["-map", &format!("{i}:v?"), "-map", &format!("{i}:a?")]);
    }
    cmd.args(["-c", "copy", "-f", "segment", "-segment_time", &PIECE_SECS.to_string()]);
    cmd.args(["-segment_wrap", &wrap.to_string(), "-reset_timestamps", "1"]);
    cmd.arg(dir.join("piece%03d.ts"));
    // Under the lock: `stop` either sees this ffmpeg (and kills it) or stops it starting.
    {
        let mut guard = slot.lock().unwrap();
        if stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start ffmpeg")?;
        // It never ends by itself: if Neria goes, it goes too.
        proc::end_with_neria(&child);
        *guard = Some(child);
    }
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let mut guard = slot.lock().unwrap();
        let Some(c) = guard.as_mut() else { return Ok(()) };
        if c.try_wait()?.is_some() {
            *guard = None;
            return Ok(());
        }
    }
}

/// The watch screen's tape for a live stream (it takes the timeline's place there): the
/// stream from its start, or from a little before what matters on a long one, to now; what
/// the player can go back to; where it is; what a recording of it has. Positions are 0..1
/// along the tape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tape {
    /// Stream seconds at its left and right ends (the right is now).
    pub from: f64,
    pub to: f64,
    /// What the player can go back to (`seek_to <= seek_from`: nothing), and where it is
    /// (`None`: not on the tape).
    pub seek_from: f32,
    pub seek_to: f32,
    pub play: Option<f32>,
    /// A recording: before it began (`joined`), recorded up to `done`, the part before in up
    /// to `early` (all of it: `early_in`), missed bits.
    pub recording: bool,
    pub joined: f32,
    pub done: f32,
    pub early: f32,
    pub early_in: bool,
    pub holes: Vec<f32>,
    pub catching_up: bool,
    /// Under it: "started 1:06:25 ago" (or how far back it goes) · "now".
    pub left: String,
    pub right: String,
}

/// What the tape is drawn from, in seconds of the stream (from when it went live; when
/// that isn't known, from when the player opened it: `origin_known` false).
#[derive(Clone, Debug, Default)]
pub struct TapeInput<'a> {
    pub now: f64,
    pub went_live: f64,
    pub origin_known: bool,
    pub seek: Option<(f64, f64)>,
    pub play: Option<f64>,
    pub rec: Option<&'a crate::record::LiveStats>,
}

impl Tape {
    pub fn new(i: &TapeInput) -> Tape {
        let to = (i.now - i.went_live).max(1.0);
        // What matters on it: where the player can go back to, where a recording began, the
        // start itself when the part before is being got.
        let mut lo = to - 60.0;
        if let Some((a, _)) = i.seek {
            lo = lo.min(a);
        }
        if let Some(r) = i.rec {
            let joined = r.joined.unwrap_or(0.0);
            lo = lo.min(if r.earlier.is_some() || r.earlier_done { 0.0 } else { joined });
        }
        let lo = lo.max(0.0);
        // All of it, unless it's been on for long and what matters is only lately: then a
        // little before that (11 days of a 24/7 channel would leave an hour a sliver).
        let from = if to > 8.0 * 3600.0 && lo > (to - lo) * 2.0 { (lo - (to - lo) * 0.25).max(0.0) } else { 0.0 };
        let f = |s: f64| (((s - from) / (to - from)).clamp(0.0, 1.0)) as f32;
        let mut t = Tape { from, to, ..Default::default() };
        if let Some((a, b)) = i.seek {
            (t.seek_from, t.seek_to) = (f(a), f(b));
        }
        t.play = i.play.filter(|p| *p >= from - 1.0).map(f);
        if let Some(r) = i.rec {
            let joined = r.joined.unwrap_or(0.0);
            t.recording = true;
            t.joined = f(joined);
            t.done = f(r.position.max(joined));
            if let Some((got, _)) = r.earlier {
                t.early = f(from.max(got.min(joined)));
            } else if r.earlier_done {
                (t.early, t.early_in) = (t.joined, true);
            }
            t.holes = r.holes.iter().map(|h| f(h.at)).collect();
            t.catching_up = r.phase == crate::record::Phase::CatchingUp;
        }
        let ago = crate::fmt::duration((to - from).max(0.0));
        t.left = match (from < 1.0, i.origin_known) {
            (true, true) => format!("started {ago} ago"),
            (true, false) => format!("opened {ago} ago"),
            (false, _) => format!("{ago} ago"),
        };
        t.right = "now".into();
        t
    }

    /// Stream seconds at a point along it (0..1).
    pub fn at(&self, x: f64) -> f64 {
        self.from + x.clamp(0.0, 1.0) * (self.to - self.from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tape_shows_what_matters() {
        use crate::record::{LiveStats, Phase};
        let now = 100_000.0;
        // On for an hour, the player keeps the last 10 min, nothing recorded: the whole hour.
        let t = Tape::new(&TapeInput {
            now,
            went_live: now - 3600.0,
            origin_known: true,
            seek: Some((3000.0, 3590.0)),
            play: Some(3500.0),
            rec: None,
        });
        assert_eq!(t.from, 0.0);
        assert!((t.seek_from - 3000.0 / 3600.0).abs() < 1e-4);
        assert!((t.play.unwrap() - 3500.0 / 3600.0).abs() < 1e-4);
        assert_eq!(t.left, "started 1:00:00 ago");
        // On for 11 days, the player keeps an hour: a little before that hour, not 11 days.
        let t = Tape::new(&TapeInput {
            now,
            went_live: now - 950_400.0,
            origin_known: true,
            seek: Some((946_800.0, 950_390.0)),
            ..Default::default()
        });
        assert!((t.from - (946_800.0 - 900.0)).abs() < 1.0, "{}", t.from);
        assert!(t.seek_from > 0.15 && t.seek_from < 0.25, "{}", t.seek_from);
        assert_eq!(t.left, "1:15:00 ago");
        // Recording from now with the part before coming: from the very start.
        let st = LiveStats {
            phase: Phase::Recording,
            joined: Some(3230.0),
            position: 3285.0,
            earlier: Some((720.0, 3230.0)),
            ..LiveStats::default()
        };
        let t = Tape::new(&TapeInput { now, went_live: now - 3300.0, origin_known: true, rec: Some(&st), ..Default::default() });
        assert_eq!(t.from, 0.0);
        assert!(t.recording && !t.early_in);
        assert!((t.early - 720.0 / 3300.0).abs() < 1e-4);
        assert!((t.joined - 3230.0 / 3300.0).abs() < 1e-4);
        assert!((t.at(0.5) - 1650.0).abs() < 1e-6);
    }

    #[test]
    fn reads_the_window() {
        assert_eq!(
            playlist_duration(
                "https://x/api/manifest/hls_playlist/expire/1/playlist_duration/3600/manifest_duration/3600/sig"
            ),
            Some(3600.0)
        );
        assert_eq!(playlist_duration("https://x/index.m3u8"), None);
        assert_eq!(playlist_total("#EXTM3U\n#EXTINF:5.0,\na.ts\n#EXTINF:4.5,\nb.ts\n"), 9.5);
    }

    #[test]
    fn marks_follow_the_window() {
        // Window: the last hour up to now = 10_000. A mark from 9_000 to 9_030.
        assert_eq!(to_window(3600.0, 10_000.0, 9_000.0, 9_030.0).unwrap(), (2600.0, 2630.0));
        // Ten minutes later the same mark is 600 s nearer the start.
        assert_eq!(to_window(3600.0, 10_600.0, 9_000.0, 9_030.0).unwrap(), (2000.0, 2030.0));
        // Scrolled out.
        assert!(
            to_window(3600.0, 12_000.0, 8_000.0, 9_030.0).unwrap_err().to_string().contains("only the last 1:00:00")
        );
        // In the future (a clock that's off).
        assert!(to_window(3600.0, 10_000.0, 12_000.0, 12_030.0).unwrap_err().to_string().contains("clock"));
    }
}
