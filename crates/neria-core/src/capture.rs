//! Instant replay of the screen or a game window (prototype, picture only for now).
//!
//! Medal / ShadowPlay-style: the last minute is always kept, and "save" turns it into a file.
//! No capture or encoder code of our own: the ffmpeg in `bin/` grabs the screen on the GPU
//! (`ddagrab`: Desktop Duplication, a whole monitor; `gfxcapture`: Windows Graphics Capture,
//! one window), hands the frames to the GPU's own encoder without copying them to memory,
//! and writes short pieces that overwrite the oldest, like the live replay buffer
//! (`live::ReplayBuffer`). Saving joins the newest pieces, the one still being written
//! included, so the moment that made you press the key is in it.
//!
//! The way from the capture to the encoder is tried at start (`Route::pick`): on a laptop the
//! screen may belong to the Intel chip while NVENC lives on the NVIDIA one, and a new ffmpeg
//! can ask for a newer NVIDIA driver than the one installed. The first that works wins; the
//! last resort copies frames to memory and encodes on the CPU (the costly way OBS often ends
//! up in).
//!
//! Not yet: sound (ffmpeg on Windows can't record what the PC plays: needs our own WASAPI),
//! a hotkey, voice ("clip that"), resizing (1440p is kept as 1440p).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};

pub use crate::capture_sound::{Sound, mics};
use crate::{capture_sound, paths, proc};

/// Seconds per piece. Short, so a save waits on nothing and starts at most this much early.
pub const PIECE_SECS: u32 = 2;

/// What to record.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A whole monitor (0 = the main one).
    Monitor(u32),
    /// The window of a program, by its file name (`game.exe`), as Windows Graphics Capture
    /// sees it: works when the window is covered, not in exclusive fullscreen.
    Window(String),
}

impl Source {
    fn filter(&self, fps: u32) -> String {
        match self {
            Source::Monitor(i) => format!("ddagrab=output_idx={i}:framerate={fps}"),
            Source::Window(exe) => {
                // A regex inside a quoted filter argument: dots as `[.]`, no backslashes needed.
                let exe: String = exe
                    .chars()
                    .filter(|c| !matches!(c, '\'' | '\\'))
                    .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ' ' { c.to_string() } else { format!("[{c}]") })
                    .collect();
                // Grab as fast as the game draws (up to 240), then keep the frame nearest each
                // 1/fps tick. Capped at `fps`, the capture itself bunched frames (a late one,
                // then one on top of it, ~6 times a second in Valorant): visible stutter.
                format!("gfxcapture=window_exe='(?i){exe}$':max_framerate=240,fps={fps}")
            }
        }
    }
}

/// From the captured frame (on the GPU) to the encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// NVIDIA's encoder, straight from the GPU.
    Nvenc,
    /// AMD's encoder, straight from the GPU.
    Amf,
    /// Intel Quick Sync, the frame mapped across without a copy.
    Qsv,
    /// NVIDIA's encoder, the frame copied through memory (screen on another GPU).
    NvencCopy,
    /// The CPU (x264): works everywhere, costs the most.
    Cpu,
}

impl Route {
    pub const ALL: [Route; 5] = [Route::Nvenc, Route::Amf, Route::Qsv, Route::NvencCopy, Route::Cpu];

    pub fn label(self) -> &'static str {
        match self {
            Route::Nvenc => "NVIDIA (NVENC), no copy",
            Route::Amf => "AMD (AMF), no copy",
            Route::Qsv => "Intel (Quick Sync), no copy",
            Route::NvencCopy => "NVIDIA (NVENC), copied through memory",
            Route::Cpu => "CPU (x264), copied through memory",
        }
    }

    /// The filter graph after the source.
    fn chain(self) -> &'static str {
        match self {
            Route::Nvenc | Route::Amf => "",
            Route::Qsv => ",hwmap=derive_device=qsv,format=qsv",
            Route::NvencCopy | Route::Cpu => ",hwdownload,format=bgra",
        }
    }

    fn encoder(self, fps: u32, mbps: u32) -> Vec<String> {
        let rate = |b: u32| format!("{b}M");
        let mut a: Vec<String> = match self {
            Route::Nvenc | Route::NvencCopy => {
                ["-c:v", "h264_nvenc", "-preset", "p2", "-tune", "ll", "-rc", "vbr"].map(String::from).to_vec()
            }
            Route::Amf => ["-c:v", "h264_amf", "-usage", "lowlatency", "-quality", "speed"].map(String::from).to_vec(),
            Route::Qsv => ["-c:v", "h264_qsv", "-preset", "veryfast"].map(String::from).to_vec(),
            Route::Cpu => ["-c:v", "libx264", "-preset", "superfast", "-tune", "zerolatency", "-pix_fmt", "yuv420p"]
                .map(String::from)
                .to_vec(),
        };
        // A keyframe at every piece, so any piece can start a clip; no B-frames (latency).
        let gop = (fps * PIECE_SECS).to_string();
        a.extend(["-b:v".into(), rate(mbps), "-maxrate".into(), rate(mbps * 3 / 2), "-bufsize".into(), rate(mbps * 2)]);
        a.extend(["-g".into(), gop, "-bf".into(), "0".into()]);
        a
    }

    /// The first route that gets frames from `source` into an encoder on this machine.
    pub fn pick(source: &Source, fps: u32) -> Result<Route> {
        let mut last = String::new();
        for r in Route::ALL {
            match r.check(source, fps) {
                Ok(()) => return Ok(r),
                Err(e) => last = e,
            }
        }
        bail!("Couldn't record this: {last}")
    }

    /// Half a second through this route into nothing; why not, if it fails.
    pub fn check(self, source: &Source, fps: u32) -> std::result::Result<(), String> {
        try_route(source, fps, self)
    }
}

fn try_route(source: &Source, fps: u32, route: Route) -> std::result::Result<(), String> {
    let graph = format!("{}{}", source.filter(fps), route.chain());
    let out = proc::command(paths::tool("ffmpeg"))
        .args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i", &graph, "-t", "0.5"])
        .args(["-fps_mode", "cfr", "-r", &fps.to_string()])
        .args(route.encoder(fps, 8))
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    // The line that says why (driver too old, no window…), not the generic last one.
    let why = err
        .lines()
        .find(|l| l.contains("driver") || l.contains("No window") || l.contains("not found") || l.contains("Failed"))
        .or_else(|| err.lines().last())
        .unwrap_or("ffmpeg failed");
    Err(why.trim().to_string())
}

#[derive(Clone, Debug)]
pub struct Options {
    pub source: Source,
    pub fps: u32,
    /// How much is kept, in seconds (60 by default).
    pub keep: u32,
    /// Video bitrate in Mbit/s.
    pub mbps: u32,
    /// `None`: tried at start (`Route::pick`).
    pub route: Option<Route>,
    /// Sound tracks, in order (AAC, named after each: "Game & PC (no Discord)", "Discord", "Mic").
    pub sounds: Vec<Sound>,
}

impl Default for Options {
    fn default() -> Self {
        Options { source: Source::Monitor(0), fps: 60, keep: 60, mbps: 16, route: None, sounds: default_sounds(None) }
    }
}

/// The usual tracks: the game and PC, a voice-chat app split off when it's running
/// (Discord by default), and the mic (`None`: Windows' default).
pub fn default_sounds(mic: Option<String>) -> Vec<Sound> {
    let chat = "Discord.exe";
    if capture_sound::app_running(chat) {
        vec![Sound::PcExcept(chat.into()), Sound::App(chat.into()), Sound::Mic(mic)]
    } else {
        vec![Sound::Pc, Sound::Mic(mic)]
    }
}

/// How the recording keeps up, from ffmpeg's progress lines.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub frames: u64,
    pub fps: f64,
    /// Frames the encoder couldn't take in time (`drop_frames`).
    pub dropped: u64,
    /// Frames repeated because the capture gave none in time (`dup_frames`): a still screen
    /// repeats too, so this only means trouble while something moves.
    pub repeated: u64,
    /// 1.0 = keeping up with real time.
    pub speed: f64,
}

/// The last `keep` seconds of the screen, always.
pub struct ScreenBuffer {
    pub dir: PathBuf,
    pub route: Route,
    pub options: Options,
    pub started: Instant,
    child: Arc<Mutex<Option<Child>>>,
    stopped: Arc<AtomicBool>,
    pub stats: Arc<Mutex<Stats>>,
    /// Why it stopped by itself ("" while running).
    pub problem: Arc<Mutex<String>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ScreenBuffer {
    pub fn start(options: Options) -> Result<ScreenBuffer> {
        let route = match options.route {
            Some(r) => r,
            None => Route::pick(&options.source, options.fps)?,
        };
        let dir = paths::app_dir().join("capture");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let buffer = ScreenBuffer {
            dir: dir.clone(),
            route,
            options: options.clone(),
            started: Instant::now(),
            child: Arc::new(Mutex::new(None)),
            stopped: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(Mutex::new(Stats::default())),
            problem: Arc::new(Mutex::new(String::new())),
            worker: Mutex::new(None),
        };
        let (child, stopped, stats, problem) =
            (buffer.child.clone(), buffer.stopped.clone(), buffer.stats.clone(), buffer.problem.clone());
        let worker = std::thread::spawn(move || {
            let mut failures = 0;
            while !stopped.load(Ordering::SeqCst) {
                let began = Instant::now();
                if let Err(e) = record(&options, route, &dir, &child, &stopped, &stats) {
                    *problem.lock().unwrap() = e.to_string();
                }
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                // Desktop Duplication is lost on a resolution change, a game going fullscreen,
                // a UAC prompt: start again. Three quick failures in a row: give up.
                failures = if began.elapsed() > Duration::from_secs(20) { 0 } else { failures + 1 };
                if failures >= 3 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            stopped.store(true, Ordering::SeqCst);
        });
        *buffer.worker.lock().unwrap() = Some(worker);
        Ok(buffer)
    }

    pub fn running(&self) -> bool {
        !self.stopped.load(Ordering::SeqCst)
    }

    /// Seconds kept so far and the space they take.
    pub fn kept(&self) -> (f64, u64) {
        let pieces = newest_pieces(&self.dir, u32::MAX);
        let bytes = pieces.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        let secs = (pieces.len().saturating_sub(1) as u32 * PIECE_SECS) as f64;
        (secs.min(self.options.keep as f64), bytes)
    }

    /// The last `seconds` (up to a piece more) as an MP4 in `out_dir`, while recording goes on.
    pub fn save(&self, seconds: u32, out_dir: &Path) -> Result<PathBuf> {
        save_from(&self.dir, seconds, out_dir, &track_titles(&self.options.sounds))
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            proc::kill_tree(c);
        }
        if let Some(w) = self.worker.lock().unwrap().take() {
            let _ = w.join();
        }
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for ScreenBuffer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The saved file's sound tracks, in order: "Everything" (the mix) first when there are two
/// or more, then each on its own.
pub fn track_titles(sounds: &[Sound]) -> Vec<String> {
    let mut t: Vec<String> = sounds.iter().map(Sound::title).collect();
    if t.len() >= 2 {
        t.insert(0, "Everything".into());
    }
    t
}

/// Pieces left by a crash.
pub fn clean_leftovers() {
    let _ = std::fs::remove_dir_all(paths::app_dir().join("capture"));
}

/// The newest pieces covering `seconds`, oldest first, the one still being written included.
fn newest_pieces(dir: &Path, seconds: u32) -> Vec<PathBuf> {
    let mut pieces: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "ts"))
        .filter(|e| e.metadata().is_ok_and(|m| m.len() > 0))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    pieces.sort_by_key(|(t, _)| *t);
    // The newest is partial: one more piece makes up for it.
    let want = (seconds / PIECE_SECS).saturating_add(1) as usize;
    let skip = pieces.len().saturating_sub(want);
    pieces.into_iter().skip(skip).map(|(_, p)| p).collect()
}

/// [`ScreenBuffer::save`] from its folder, to run off the UI thread.
pub fn save_from(dir: &Path, seconds: u32, out_dir: &Path, titles: &[String]) -> Result<PathBuf> {
    let pieces = newest_pieces(dir, seconds);
    if pieces.is_empty() {
        bail!("Nothing kept yet: give it a few seconds.");
    }
    std::fs::create_dir_all(out_dir)?;
    let out = crate::clip::unique_path(out_dir, &format!("Replay (last {})", crate::clip::stamp(seconds as f64)), "mp4");
    let list = dir.join(format!("save-{}.txt", std::process::id()));
    let lines: Vec<String> =
        pieces.iter().map(|p| format!("file '{}'", p.to_string_lossy().replace('\'', "'\\''"))).collect();
    std::fs::write(&list, lines.join("\n"))?;
    let mut cmd = proc::command(paths::tool("ffmpeg"));
    cmd.args(["-hide_banner", "-v", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .args(["-map", "0", "-c", "copy", "-movflags", "+faststart"]);
    for (i, t) in titles.iter().enumerate() {
        // MP4 keeps a track's name as its handler name; players show either.
        cmd.arg(format!("-metadata:s:a:{i}")).arg(format!("title={t}"));
        cmd.arg(format!("-metadata:s:a:{i}")).arg(format!("handler_name={t}"));
    }
    let result = cmd
        .arg(&out)
        .stdin(Stdio::null())
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

/// One ffmpeg run, until it ends or `stop` kills it.
fn record(
    o: &Options,
    route: Route,
    dir: &Path,
    slot: &Mutex<Option<Child>>,
    stopped: &AtomicBool,
    stats: &Mutex<Stats>,
) -> Result<()> {
    let wrap = o.keep / PIECE_SECS + 3;
    let graph = format!("{}{}", o.source.filter(o.fps), route.chain());
    let mut cmd = proc::command(paths::tool("ffmpeg"));
    cmd.args(["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y"]);
    cmd.args(["-f", "lavfi", "-i", &graph]);
    // Each sound track: opened now (a missing app fails here), sent when ffmpeg connects.
    let feeds_stop = Arc::new(AtomicBool::new(false));
    // However this run ends, its tracks stop sending.
    struct StopOnDrop(Arc<AtomicBool>);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let _stop_feeds = StopOnDrop(feeds_stop.clone());
    let mut feeds = Vec::new();
    for s in &o.sounds {
        match capture_sound::feed(s, feeds_stop.clone()) {
            Ok(f) => feeds.push(f),
            Err(e) => {
                feeds_stop.store(true, Ordering::SeqCst);
                bail!("Couldn't record {}: {e}", s.title());
            }
        }
    }
    for f in &feeds {
        cmd.args(capture_sound::input_args(f.port));
    }
    cmd.args(["-map", "0:v"]);
    // Most players (and Discord, browsers) play only the first track: make it all of them
    // mixed, the separate ones after it for editing.
    if feeds.len() >= 2 {
        let ins: String = (1..=feeds.len()).map(|i| format!("[{i}:a]")).collect();
        let mix = format!("{ins}amix=inputs={}:normalize=0:dropout_transition=0,alimiter=limit=0.97[mix]", feeds.len());
        cmd.args(["-filter_complex", &mix, "-map", "[mix]"]);
    }
    for i in 0..feeds.len() {
        cmd.args(["-map", &format!("{}:a", i + 1)]);
    }
    if feeds.is_empty() {
        cmd.arg("-an");
    } else {
        cmd.args(["-c:a", "aac", "-b:a", "160k"]);
    }
    cmd.args(["-fps_mode", "cfr", "-r", &o.fps.to_string()]);
    cmd.args(route.encoder(o.fps, o.mbps));
    cmd.args(["-f", "segment", "-segment_time", &PIECE_SECS.to_string()]);
    cmd.args(["-segment_wrap", &wrap.to_string(), "-reset_timestamps", "1", "-flush_packets", "1"]);
    cmd.arg(dir.join("piece%03d.ts"));
    let stdout;
    let stderr;
    {
        let mut guard = slot.lock().unwrap();
        if stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to start ffmpeg")?;
        proc::end_with_neria(&child);
        stdout = child.stdout.take();
        stderr = child.stderr.take();
        *guard = Some(child);
    }
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    if let Some(out) = stdout {
        let mut now = Stats::default();
        for line in BufReader::new(out).lines().map_while(|l| l.ok()) {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k {
                "frame" => now.frames = v.parse().unwrap_or(0),
                "fps" => now.fps = v.parse().unwrap_or(0.0),
                "drop_frames" => now.dropped = v.parse().unwrap_or(0),
                "dup_frames" => now.repeated = v.parse().unwrap_or(0),
                "speed" => now.speed = v.trim_end_matches('x').parse().unwrap_or(0.0),
                "progress" => *stats.lock().unwrap() = now.clone(),
                _ => {}
            }
        }
    }
    if let Some(mut c) = slot.lock().unwrap().take() {
        let _ = c.wait();
    }
    let err = errors.join().unwrap_or_default();
    if !stopped.load(Ordering::SeqCst) {
        if let Some(line) = err.lines().rev().find(|l| !l.trim().is_empty()) {
            bail!("Recording stopped: {}", line.trim());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_names_become_safe_patterns() {
        assert_eq!(
            Source::Window("Game-1.exe".into()).filter(60),
            "gfxcapture=window_exe='(?i)Game-1[.]exe$':max_framerate=240,fps=60"
        );
        // Quotes and backslashes can't break out of the filter argument.
        assert!(!Source::Window("a'b\\c.exe".into()).filter(30).contains("a'"));
    }

    #[test]
    fn keyframe_every_piece() {
        let a = Route::Qsv.encoder(60, 16);
        let g = a.iter().position(|x| x == "-g").unwrap();
        assert_eq!(a[g + 1], (60 * PIECE_SECS).to_string());
    }
}
