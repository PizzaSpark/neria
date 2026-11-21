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
//! Sound comes from our own WASAPI (`capture_sound`: ffmpeg on Windows can't record what the
//! PC plays), the save key from `hotkey`. Not yet: a UI, voice ("Hey Neria, clip that"),
//! resizing (1440p is kept as 1440p).

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
    /// A whole monitor, by its place in `capture_pick::monitors()` (0: the first graphics
    /// chip's first screen, usually the main one).
    Monitor(u32),
    /// The window of a program, by its file name (`game.exe`), as Windows Graphics Capture
    /// sees it: works when the window is covered, not in exclusive fullscreen.
    Window(String),
}

impl Source {
    /// Before the input: a monitor is recorded on the graphics chip it's on; a window on
    /// `chip` (the one whose encoder is used, so the frames needn't cross over).
    fn device_args(&self, chip: Option<u32>) -> Vec<String> {
        let on = match self {
            Source::Monitor(i) => crate::capture_pick::monitors().into_iter().find(|m| m.index == *i).map(|m| m.adapter),
            Source::Window(_) => chip,
        };
        match on {
            Some(n) if n != 0 => {
                vec!["-init_hw_device".into(), format!("d3d11va=grab:{n}"), "-filter_hw_device".into(), "grab".into()]
            }
            _ => Vec::new(),
        }
    }

    fn filter(&self, fps: u32) -> String {
        match self {
            Source::Monitor(i) => {
                let output =
                    crate::capture_pick::monitors().into_iter().find(|m| m.index == *i).map(|m| m.output).unwrap_or(*i);
                format!("ddagrab=output_idx={output}:framerate={fps}")
            }
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
        Ok(pick_way(source, fps, Encoder::Auto)?.0)
    }

    /// Half a second through this route into nothing; why not, if it fails.
    pub fn check(self, source: &Source, fps: u32) -> std::result::Result<(), String> {
        try_route(source, fps, self, None)
    }

    /// Whose encoder it is.
    pub fn vendor(self) -> Encoder {
        match self {
            Route::Nvenc | Route::NvencCopy => Encoder::Nvidia,
            Route::Amf => Encoder::Amd,
            Route::Qsv => Encoder::Intel,
            Route::Cpu => Encoder::Cpu,
        }
    }
}

/// Which encoder records (the Capture page's choice).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Encoder {
    /// The fastest there is: a dedicated chip's (NVIDIA, AMD) before the one built into the CPU
    /// (Intel), the CPU itself last.
    #[default]
    Auto,
    Nvidia,
    Amd,
    Intel,
    Cpu,
}

impl Encoder {
    pub fn from_name(s: &str) -> Encoder {
        match s {
            "nvidia" => Encoder::Nvidia,
            "amd" => Encoder::Amd,
            "intel" => Encoder::Intel,
            "cpu" => Encoder::Cpu,
            _ => Encoder::Auto,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Encoder::Auto => "auto",
            Encoder::Nvidia => "nvidia",
            Encoder::Amd => "amd",
            Encoder::Intel => "intel",
            Encoder::Cpu => "cpu",
        }
    }

    /// "NVIDIA", "Intel", "the CPU".
    pub fn label(self) -> &'static str {
        match self {
            Encoder::Auto => "Automatic",
            Encoder::Nvidia => "NVIDIA",
            Encoder::Amd => "AMD",
            Encoder::Intel => "Intel",
            Encoder::Cpu => "CPU",
        }
    }
}

/// The encoders this PC has, for the Capture page (the CPU always).
pub fn encoders() -> Vec<Encoder> {
    let chips = crate::capture_pick::chips();
    let has = |v: crate::capture_pick::Vendor| chips.iter().any(|c| c.vendor == v);
    let mut out = Vec::new();
    use crate::capture_pick::Vendor;
    for (v, e) in [(Vendor::Nvidia, Encoder::Nvidia), (Vendor::Amd, Encoder::Amd), (Vendor::Intel, Encoder::Intel)] {
        if has(v) {
            out.push(e);
        }
    }
    out.push(Encoder::Cpu);
    out
}

/// What to try, in order, for an encoder: each route with the chip to grab a window on
/// (`None`: Windows' default chip).
fn ways(encoder: Encoder) -> Vec<(Route, Option<u32>)> {
    use crate::capture_pick::Vendor;
    let chips = crate::capture_pick::chips();
    let chip = |v: Vendor| chips.iter().find(|c| c.vendor == v).map(|c| c.index);
    let nvidia = || match chip(Vendor::Nvidia) {
        Some(n) => vec![(Route::Nvenc, Some(n)), (Route::Nvenc, None), (Route::NvencCopy, None)],
        None => Vec::new(),
    };
    let amd = || match chip(Vendor::Amd) {
        Some(n) => vec![(Route::Amf, Some(n)), (Route::Amf, None)],
        None => Vec::new(),
    };
    let intel = || match chip(Vendor::Intel) {
        Some(n) => vec![(Route::Qsv, Some(n)), (Route::Qsv, None)],
        None => Vec::new(),
    };
    let cpu = || vec![(Route::Cpu, None)];
    let mut v = match encoder {
        Encoder::Nvidia => nvidia(),
        Encoder::Amd => amd(),
        Encoder::Intel => intel(),
        Encoder::Cpu => cpu(),
        Encoder::Auto => [nvidia(), amd(), intel(), cpu()].concat(),
    };
    v.dedup();
    v
}

/// The first way `encoder` gets frames from `source` here: the route and the chip a window
/// is grabbed on. One picked by name that doesn't work here says why.
pub fn pick_way(source: &Source, fps: u32, encoder: Encoder) -> Result<(Route, Option<u32>)> {
    let mut last = String::new();
    for (r, chip) in ways(encoder) {
        match try_route(source, fps, r, chip) {
            Ok(()) => return Ok((r, chip)),
            Err(e) => last = e,
        }
    }
    if encoder == Encoder::Auto || encoder == Encoder::Cpu {
        bail!("Couldn't record this: {last}")
    }
    bail!("{}'s encoder couldn't record this ({last}). Pick Automatic.", encoder.label())
}

fn try_route(source: &Source, fps: u32, route: Route, chip: Option<u32>) -> std::result::Result<(), String> {
    let graph = format!("{}{}", source.filter(fps), route.chain());
    let dev = source.device_args(chip);
    // A screen on another graphics chip: ddagrab only takes that chip inside -filter_complex.
    let input: Vec<String> = if dev.is_empty() {
        vec!["-f".into(), "lavfi".into(), "-i".into(), graph]
    } else {
        vec!["-filter_complex".into(), format!("{graph}[v]"), "-map".into(), "[v]".into()]
    };
    let out = proc::command(paths::tool("ffmpeg"))
        .args(["-hide_banner", "-v", "error"])
        .args(dev)
        .args(input)
        .args(["-t", "0.5"])
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
    /// `None`: tried at start (`pick_way` with `encoder`).
    pub route: Option<Route>,
    /// The chip a window is grabbed on (with `route`; `None`: Windows' default).
    pub chip: Option<u32>,
    pub encoder: Encoder,
    /// Sound recorded, in order (AAC, named after each: "Game & PC (no Discord)", "Discord", "Mic").
    pub sounds: Vec<Sound>,
    /// What each of `sounds` is for (in the "Everything" mix, a track of its own, both) and
    /// its track's name. Empty: each is both, named after itself.
    pub tracks: Vec<Track>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            source: Source::Monitor(0),
            fps: 60,
            keep: 60,
            mbps: 16,
            route: None,
            chip: None,
            encoder: Encoder::Auto,
            sounds: default_sounds(None),
            tracks: Vec::new(),
        }
    }
}

impl Options {
    /// The saved file's sound tracks, in order.
    pub fn titles(&self) -> Vec<String> {
        titles_for(&self.sounds, &self.tracks)
    }
}

/// Where a recorded sound goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// In the "Everything" mix and a track of its own (the mic, an app when everything is
    /// built app by app).
    Both,
    /// Only in the mix (what the PC plays: the apps' own tracks would repeat it).
    MixOnly,
    /// Only a track of its own (an app also heard in "what the PC plays").
    ApartOnly,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub role: Role,
    /// The track's name ("VALORANT", "Discord", "Mic").
    pub name: String,
}

/// What to record for sound, worked out from the Capture page's choices.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SoundPlan {
    pub sounds: Vec<Sound>,
    pub tracks: Vec<Track>,
    /// Two or more apps are never recorded: "Everything" is the other apps one by one, so an
    /// app that starts later isn't in it until a restart.
    pub by_app: bool,
}

fn same_exe(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Instant replay's choices (the Capture page), saved in `settings.json`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Turn instant replay on when Neria starts.
    pub on_at_start: bool,
    /// "monitor" or "window".
    pub kind: String,
    /// The program's file name (`VALORANT-Win64-Shipping.exe`) when `kind` is "window".
    pub exe: String,
    /// The monitor's name ("DELL U2720Q") and its number when picked (names win: numbers
    /// change when a screen is plugged in).
    pub monitor: String,
    pub monitor_index: u32,
    /// What it's called in the status and the saved file's name ("VALORANT", "DELL U2720Q").
    pub label: String,
    /// Seconds kept (`KEEPS`).
    pub keep: u32,
    /// A track for each app playing sound (track 1 is always everything mixed).
    pub per_app: bool,
    /// Apps never recorded, on any track (file names, `Spotify.exe`).
    pub never: Vec<String>,
    pub mic: bool,
    /// Part of the mic's name; empty for Windows' default.
    pub mic_name: String,
    /// The save key (`hotkey::Hotkey::parse`).
    pub key: String,
    /// Which encoder records: "auto", "nvidia", "amd", "intel", "cpu" (`Encoder`).
    pub encoder: String,
    pub fps: u32,
    pub mbps: u32,
    /// The clips saved, newest first (up to `SAVED_MAX`).
    pub saved: Vec<PathBuf>,
}

/// How much can be kept: the Capture page's chips, in seconds.
pub const KEEPS: [u32; 4] = [30, 60, 120, 300];
/// Saved clips listed on the Capture page.
pub const SAVED_MAX: usize = 8;
/// The voice-chat app split off onto its own track.
pub const CHAT_APP: &str = "Discord.exe";

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            on_at_start: false,
            kind: "monitor".into(),
            exe: String::new(),
            monitor: String::new(),
            monitor_index: 0,
            label: String::new(),
            keep: 60,
            per_app: true,
            never: Vec::new(),
            mic: true,
            mic_name: String::new(),
            key: crate::hotkey::DEFAULT.into(),
            encoder: "auto".into(),
            fps: 60,
            mbps: 16,
            saved: Vec::new(),
        }
    }
}

impl Prefs {
    /// Recording whatever game is in front (the desktop decides what that is right now).
    pub fn automatic(&self) -> bool {
        self.kind == "auto"
    }

    /// The source, the monitor found by its name when it's still there (Automatic: the
    /// picked monitor, until a game is in front).
    pub fn source(&self, monitors: &[crate::capture_pick::Monitor]) -> Source {
        if self.kind == "window" && !self.exe.is_empty() {
            return Source::Window(self.exe.clone());
        }
        let i = monitors
            .iter()
            .find(|m| !self.monitor.is_empty() && m.name == self.monitor)
            .map(|m| m.index)
            .unwrap_or(self.monitor_index);
        Source::Monitor(i)
    }

    pub fn is_never(&self, exe: &str) -> bool {
        self.never.iter().any(|n| same_exe(n, exe))
    }

    /// The sound to record now: the apps playing sound asked Windows for.
    pub fn sound_plan(&self, game: Option<(&str, &str)>) -> SoundPlan {
        self.plan_from(game, &capture_sound::playing_apps())
    }

    /// Track 1 "Everything": what the PC plays (but the one app never recorded), mixed with
    /// the mic; then the game (`(exe, name)`) and every other app playing sound, each on its
    /// own; the mic last. Two or more apps never recorded: Windows can leave out only one, so
    /// "Everything" is built from the other apps one by one.
    pub fn plan_from(&self, game: Option<(&str, &str)>, playing: &[capture_sound::AppSound]) -> SoundPlan {
        let mut apps: Vec<(String, String)> = Vec::new();
        if let Some((exe, name)) = game {
            if !self.is_never(exe) {
                apps.push((exe.to_string(), name.to_string()));
            }
        }
        for a in playing {
            if !self.is_never(&a.exe) && !apps.iter().any(|(e, _)| same_exe(e, &a.exe)) {
                apps.push((a.exe.clone(), a.name.clone()));
            }
        }
        let by_app = self.never.len() >= 2;
        let mut plan = SoundPlan { by_app, ..Default::default() };
        if !by_app {
            plan.sounds.push(match self.never.first() {
                Some(n) => Sound::PcExcept(n.clone()),
                None => Sound::Pc,
            });
            plan.tracks.push(Track { role: Role::MixOnly, name: "Everything".into() });
        }
        if self.per_app || by_app {
            for (exe, name) in apps {
                let role = match (by_app, self.per_app) {
                    (true, true) => Role::Both,
                    (true, false) => Role::MixOnly,
                    _ => Role::ApartOnly,
                };
                plan.sounds.push(Sound::App(exe));
                plan.tracks.push(Track { role, name });
            }
        }
        if self.mic {
            plan.sounds.push(Sound::Mic(Some(self.mic_name.clone()).filter(|m| !m.is_empty())));
            plan.tracks.push(Track { role: Role::Both, name: "Mic".into() });
        }
        plan
    }

    /// What's recorded, for the status ("Valorant", "DELL U2720Q").
    pub fn what(&self) -> String {
        if !self.label.is_empty() {
            return self.label.clone();
        }
        if self.kind == "window" && !self.exe.is_empty() {
            return self.exe.trim_end_matches(".exe").to_string();
        }
        "your main screen".into()
    }

    /// A clip just saved goes to the top of the list.
    pub fn add_saved(&mut self, file: PathBuf) {
        self.saved.retain(|p| p != &file && p.is_file());
        self.saved.insert(0, file);
        self.saved.truncate(SAVED_MAX);
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
    pub fn start(mut options: Options) -> Result<ScreenBuffer> {
        let route = match options.route {
            Some(r) => r,
            None => {
                let (r, chip) = pick_way(&options.source, options.fps, options.encoder)?;
                options.chip = chip;
                r
            }
        };
        options.route = Some(route);
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
        save_from(&self.dir, seconds, out_dir, &self.options.titles(), "Replay")
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
    titles_for(sounds, &[])
}

/// Which sounds are in the mix and which are tracks of their own, as indices into `sounds`.
fn layout(sounds: &[Sound], tracks: &[Track]) -> (Vec<usize>, Vec<usize>) {
    let role = |i: usize| tracks.get(i).map(|t| t.role).unwrap_or(Role::Both);
    let mix: Vec<usize> = (0..sounds.len()).filter(|&i| role(i) != Role::ApartOnly).collect();
    let apart: Vec<usize> = (0..sounds.len()).filter(|&i| role(i) != Role::MixOnly).collect();
    // One sound that's a track of its own anyway: no mix of it alone.
    let mixed = mix.len() >= 2 || mix.iter().any(|&i| role(i) == Role::MixOnly);
    (if mixed { mix } else { Vec::new() }, apart)
}

fn titles_for(sounds: &[Sound], tracks: &[Track]) -> Vec<String> {
    let (mix, apart) = layout(sounds, tracks);
    let mut t = Vec::new();
    if !mix.is_empty() {
        t.push("Everything".to_string());
    }
    for i in apart {
        t.push(tracks.get(i).map(|t| t.name.clone()).unwrap_or_else(|| sounds[i].title()));
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

/// [`ScreenBuffer::save`] from its folder, to run off the UI thread: `name (last 1m00s).mp4`.
pub fn save_from(dir: &Path, seconds: u32, out_dir: &Path, titles: &[String], name: &str) -> Result<PathBuf> {
    // Absolute: ffmpeg reads relative names in the list from the list's own folder.
    let pieces: Vec<PathBuf> =
        newest_pieces(dir, seconds).into_iter().map(|p| std::path::absolute(&p).unwrap_or(p)).collect();
    if pieces.is_empty() {
        bail!("Nothing kept yet: give it a few seconds.");
    }
    std::fs::create_dir_all(out_dir)?;
    let name = crate::music::sanitize(name);
    let out = crate::clip::unique_path(out_dir, &format!("{name} (last {})", crate::clip::stamp(seconds as f64)), "mp4");
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
    let dev = o.source.device_args(o.chip);
    // A screen on another graphics chip: ddagrab only takes that chip inside -filter_complex,
    // so the picture is made there instead of as input 0.
    let in_graph = !dev.is_empty();
    cmd.args(dev);
    if !in_graph {
        cmd.args(["-f", "lavfi", "-i", &graph]);
    }
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
    let first = if in_graph { 0 } else { 1 };
    let mut graphs = Vec::new();
    if in_graph {
        graphs.push(format!("{graph}[v]"));
    }
    // Most players (and Discord, browsers) play only the first track: make it everything
    // mixed, the separate ones after it for editing. A sound used twice (in the mix and on
    // its own) is split first.
    let (mix, apart) = layout(&o.sounds, &o.tracks);
    let twice = |i: usize| mix.contains(&i) && apart.contains(&i);
    for i in 0..feeds.len() {
        if twice(i) {
            graphs.push(format!("[{}:a]asplit=2[s{i}m][s{i}t]", first + i));
        }
    }
    if !mix.is_empty() {
        let ins: String =
            mix.iter().map(|&i| if twice(i) { format!("[s{i}m]") } else { format!("[{}:a]", first + i) }).collect();
        graphs.push(format!(
            "{ins}amix=inputs={}:normalize=0:dropout_transition=0,alimiter=limit=0.97[mix]",
            mix.len()
        ));
    }
    if !graphs.is_empty() {
        cmd.args(["-filter_complex", &graphs.join(";")]);
    }
    cmd.args(["-map", if in_graph { "[v]" } else { "0:v" }]);
    if !mix.is_empty() {
        cmd.args(["-map", "[mix]"]);
    }
    for &i in &apart {
        let m = if twice(i) { format!("[s{i}t]") } else { format!("{}:a", first + i) };
        cmd.args(["-map", &m]);
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
    fn sound_plans() {
        use capture_sound::AppSound;
        let app = |e: &str| AppSound { exe: e.into(), name: e.trim_end_matches(".exe").into(), peak: 0.1 };
        let playing = [app("Discord.exe"), app("Spotify.exe")];
        let mut p = Prefs::default();
        // Everything (the PC, mixed with the mic), the game, each app, the mic.
        let plan = p.plan_from(Some(("VALORANT.exe", "VALORANT")), &playing);
        assert!(!plan.by_app);
        let o = Options { sounds: plan.sounds.clone(), tracks: plan.tracks.clone(), ..Options::default() };
        assert_eq!(o.titles(), ["Everything", "VALORANT", "Discord", "Spotify", "Mic"]);
        assert_eq!(plan.sounds[0], Sound::Pc);
        // One app never recorded: left out of what the PC plays.
        p.never = vec!["spotify.exe".into()];
        let plan = p.plan_from(None, &playing);
        assert_eq!(plan.sounds[0], Sound::PcExcept("spotify.exe".into()));
        assert!(!plan.sounds.contains(&Sound::App("Spotify.exe".into())));
        // Two: everything built app by app.
        p.never.push("Discord.exe".into());
        let plan = p.plan_from(Some(("g.exe", "Game")), &playing);
        assert!(plan.by_app && !plan.sounds.contains(&Sound::Pc));
        let o = Options { sounds: plan.sounds.clone(), tracks: plan.tracks.clone(), ..Options::default() };
        assert_eq!(o.titles(), ["Everything", "Game", "Mic"]);
        // No mic, no apps apart: the PC alone is still "Everything".
        let q = Prefs { mic: false, per_app: false, ..Prefs::default() };
        let plan = q.plan_from(None, &playing);
        let o = Options { sounds: plan.sounds, tracks: plan.tracks, ..Options::default() };
        assert_eq!(o.titles(), ["Everything"]);
    }

    #[test]
    fn keyframe_every_piece() {
        let a = Route::Qsv.encoder(60, 16);
        let g = a.iter().position(|x| x == "-g").unwrap();
        assert_eq!(a[g + 1], (60 * PIECE_SECS).to_string());
    }
}
