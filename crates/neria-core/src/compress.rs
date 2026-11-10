//! Video compression: hit a target file size (e.g. Discord's 20 MB) or a quality level,
//! using a GPU encoder when one works on this machine.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};

use crate::media::FileInfo;
use crate::{clip, fmt, paths, proc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "H.265",
            Self::Av1 => "AV1",
        }
    }

    fn software(self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::Hevc => "libx265",
            Self::Av1 => "libsvtav1",
        }
    }

    /// Bits per pixel per frame below which this codec starts to look bad
    /// (e.g. H.264 720p30 holds up at ~1.1 Mbps ≈ 0.04 bpp).
    fn min_bpp(self) -> f64 {
        match self {
            Self::H264 => 0.035,
            Self::Hevc => 0.024,
            Self::Av1 => 0.02,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gpu {
    Nvidia,
    Intel,
    Amd,
}

impl Gpu {
    pub fn label(self) -> &'static str {
        match self {
            Self::Nvidia => "NVIDIA NVENC",
            Self::Intel => "Intel Quick Sync",
            Self::Amd => "AMD AMF",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::Nvidia => "nvenc",
            Self::Intel => "qsv",
            Self::Amd => "amf",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    Cpu(Codec),
    Gpu(Codec, Gpu),
}

impl Encoder {
    pub fn codec(self) -> Codec {
        match self {
            Self::Cpu(c) | Self::Gpu(c, _) => c,
        }
    }

    pub fn ffmpeg_name(self) -> String {
        match self {
            Self::Cpu(c) => c.software().to_string(),
            Self::Gpu(c, g) => {
                let base = match c {
                    Codec::H264 => "h264",
                    Codec::Hevc => "hevc",
                    Codec::Av1 => "av1",
                };
                format!("{base}_{}", g.suffix())
            }
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Cpu(c) => format!("{} on CPU", c.label()),
            Self::Gpu(c, g) => format!("{} on {}", c.label(), g.label()),
        }
    }

    /// Two-pass gives the most accurate size; only worth it on CPU x264/x265.
    fn two_pass(self) -> bool {
        matches!(self, Self::Cpu(Codec::H264) | Self::Cpu(Codec::Hevc))
    }
}

/// GPU encoders that actually work here (listed by ffmpeg *and* pass a test encode).
#[derive(Clone, Debug, Default)]
pub struct Encoders {
    pub gpu: Vec<(Codec, Gpu)>,
}

impl Encoders {
    pub fn detect() -> Self {
        let jobs: Vec<_> = [Codec::H264, Codec::Hevc, Codec::Av1]
            .into_iter()
            .flat_map(|c| [Gpu::Nvidia, Gpu::Intel, Gpu::Amd].map(move |g| (c, g)))
            .map(|(c, g)| {
                thread::spawn(move || {
                    let name = Encoder::Gpu(c, g).ffmpeg_name();
                    let ok = proc::command(paths::tool("ffmpeg"))
                        .args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i", "color=black:s=320x240:r=30:d=0.2"])
                        .args(["-c:v", &name, "-f", "null", "-"])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .is_ok_and(|s| s.success());
                    ok.then_some((c, g))
                })
            })
            .collect();
        Self { gpu: jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect() }
    }

    pub fn pick(&self, codec: Codec, use_gpu: bool) -> Encoder {
        if use_gpu {
            for g in [Gpu::Nvidia, Gpu::Amd, Gpu::Intel] {
                if self.gpu.contains(&(codec, g)) {
                    return Encoder::Gpu(codec, g);
                }
            }
        }
        Encoder::Cpu(codec)
    }

    pub fn has_gpu_for(&self, codec: Codec) -> bool {
        self.gpu.iter().any(|(c, _)| *c == codec)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    /// Fit in this many megabytes (1 MB = 1,000,000 bytes, which is what upload limits use).
    SizeMb(f64),
    /// 0.0 = smallest, 1.0 = best looking.
    Quality(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Size mode: pick the sharpest resolution the bitrate can carry.
    Auto,
    Original,
    Max(u32),
}

#[derive(Clone, Debug)]
pub struct Options {
    pub target: Target,
    pub codec: Codec,
    pub use_gpu: bool,
    pub resolution: Resolution,
    /// Cap frames per second (None = keep).
    pub max_fps: Option<f64>,
    /// Audio bitrate in kbps; None removes audio.
    pub audio_kbps: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub encoder: Encoder,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Size mode: video bitrate. None in quality mode.
    pub video_kbps: Option<f64>,
    pub audio_kbps: Option<u32>,
    /// Quality mode: the encoder's quality number (CRF/CQ/QP).
    pub quality: Option<u32>,
    pub target_bytes: Option<u64>,
}

impl Plan {
    /// One-line summary for the UI.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{}×{}", self.width, self.height)];
        if self.fps > 0.0 {
            parts.push(format!("{:.0} fps", self.fps));
        }
        if let Some(v) = self.video_kbps {
            parts.push(format!("{:.0} kbps video", v));
        }
        parts.push(match self.audio_kbps {
            Some(a) => format!("{a} kbps audio"),
            None => "no audio".into(),
        });
        parts.push(self.encoder.label());
        parts.join("  ·  ")
    }
}

fn even(x: f64) -> u32 {
    ((x / 2.0).round() as u32).max(1) * 2
}

/// Work out resolution, fps and bitrates for `info` with `opts`.
pub fn plan(info: &FileInfo, opts: &Options, encoders: &Encoders) -> Result<Plan> {
    if !info.has_video {
        bail!("This file has no video to compress.");
    }
    if info.duration <= 0.0 {
        bail!("Can't tell how long this video is.");
    }
    let encoder = encoders.pick(opts.codec, opts.use_gpu);
    let src_fps = if info.fps > 0.0 { info.fps } else { 30.0 };
    let mut fps = opts.max_fps.map_or(src_fps, |m| src_fps.min(m));
    let audio_kbps = if info.has_audio { opts.audio_kbps } else { None };
    let aspect = info.width as f64 / info.height.max(1) as f64;

    let size_at = |h: u32| (even(h as f64 * aspect), even(h as f64));
    let capped = |h: u32| match opts.resolution {
        Resolution::Max(m) => h.min(m),
        _ => h,
    };

    match opts.target {
        Target::Quality(level) => {
            let h = capped(info.height);
            let (width, height) = size_at(h);
            let l = level.clamp(0.0, 1.0);
            let quality = match encoder.codec() {
                Codec::H264 => 30.0 - l * 12.0,
                Codec::Hevc => 33.0 - l * 12.0,
                Codec::Av1 => 45.0 - l * 20.0,
            }
            .round() as u32;
            Ok(Plan {
                encoder,
                width,
                height,
                fps,
                video_kbps: None,
                audio_kbps,
                quality: Some(quality),
                target_bytes: None,
            })
        }
        Target::SizeMb(mb) => {
            let target_bytes = (mb * 1_000_000.0) as u64;
            // Keep ~4% headroom for container overhead and rate-control wobble.
            let budget_kbps = target_bytes as f64 * 8.0 / 1000.0 / info.duration * 0.96;
            let mut audio_kbps = audio_kbps;
            if let Some(a) = audio_kbps {
                // Don't let audio eat a tiny budget.
                let fair = (budget_kbps * 0.15).max(48.0) as u32;
                audio_kbps = Some(a.min(fair.max(48)));
            }
            let video_kbps = budget_kbps - audio_kbps.unwrap_or(0) as f64;
            if video_kbps < 60.0 {
                bail!(
                    "{} is too long to fit in {} MB. Clip it shorter or pick a bigger size.",
                    fmt::duration(info.duration),
                    mb
                );
            }

            let bpp = |w: u32, h: u32, f: f64| video_kbps * 1000.0 / (w as f64 * h as f64 * f);
            let height = match opts.resolution {
                Resolution::Original => info.height,
                Resolution::Max(m) => info.height.min(m),
                Resolution::Auto => {
                    // Starved bitrate: smooth 30 fps beats choppy 60.
                    if opts.max_fps.is_none() && fps > 31.0 {
                        let (w, h) = size_at(info.height.min(720));
                        if bpp(w, h, fps) < encoder.codec().min_bpp() {
                            fps = fps.min(30.0);
                        }
                    }
                    let candidates = [2160, 1440, 1080, 720, 540, 480, 360, 240];
                    candidates
                        .iter()
                        .copied()
                        .filter(|h| *h <= info.height)
                        .find(|h| {
                            let (w, hh) = size_at(*h);
                            bpp(w, hh, fps) >= encoder.codec().min_bpp()
                        })
                        .unwrap_or(info.height.min(240))
                }
            };
            let (width, height) = size_at(height);
            Ok(Plan {
                encoder,
                width,
                height,
                fps,
                video_kbps: Some(video_kbps),
                audio_kbps,
                quality: None,
                target_bytes: Some(target_bytes),
            })
        }
    }
}

pub fn output_path(input: &Path, opts: &Options) -> PathBuf {
    let dir = input.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "video".into());
    let tag = match opts.target {
        Target::SizeMb(mb) => format!("compressed {mb}MB"),
        Target::Quality(_) => "compressed".into(),
    };
    clip::unique_path(&dir, &format!("{stem} ({tag})"), "mp4")
}

/// Pass: None = single pass, Some(1) / Some(2) = two-pass stage.
pub fn build_args(input: &Path, out: &Path, info: &FileInfo, plan: &Plan, pass: Option<u8>) -> Vec<String> {
    let enc = plan.encoder;
    let mut a: Vec<String> =
        ["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y", "-i"].map(String::from).to_vec();
    a.push(input.to_string_lossy().into_owned());
    a.extend(["-map", "0:V:0"].map(String::from));

    let mut filters = Vec::new();
    if plan.height != info.height || plan.width != info.width {
        filters.push(format!("scale={}:{}:flags=lanczos", plan.width, plan.height));
    }
    if info.fps > 0.0 && plan.fps < info.fps - 0.01 {
        filters.push(format!("fps={}", plan.fps));
    }
    if !filters.is_empty() {
        a.extend(["-vf".into(), filters.join(",")]);
    }

    a.extend(["-c:v".into(), enc.ffmpeg_name()]);
    let pix = if matches!(enc, Encoder::Gpu(_, Gpu::Intel)) { "nv12" } else { "yuv420p" };
    a.extend(["-pix_fmt".into(), pix.into()]);

    let k = |v: f64| format!("{}k", v.round() as u64);
    match (enc, plan.video_kbps, plan.quality) {
        // ── Target size ──
        (Encoder::Cpu(Codec::H264), Some(v), _) => {
            a.extend(["-preset", "medium", "-b:v"].map(String::from));
            a.push(k(v));
        }
        (Encoder::Cpu(Codec::Hevc), Some(v), _) => {
            a.extend(["-preset", "medium", "-b:v"].map(String::from));
            a.push(k(v));
        }
        (Encoder::Cpu(Codec::Av1), Some(v), _) => {
            a.extend(["-preset", "8", "-b:v"].map(String::from));
            a.push(k(v));
        }
        (Encoder::Gpu(_, Gpu::Nvidia), Some(v), _) => {
            a.extend(["-preset", "p6", "-tune", "hq", "-rc", "vbr", "-multipass", "fullres", "-b:v"].map(String::from));
            a.extend([k(v), "-maxrate".into(), k(v * 1.5), "-bufsize".into(), k(v * 2.0)]);
        }
        (Encoder::Gpu(_, Gpu::Intel), Some(v), _) => {
            a.extend(["-preset", "slower", "-b:v"].map(String::from));
            a.extend([k(v), "-maxrate".into(), k(v * 1.5), "-bufsize".into(), k(v * 2.0)]);
        }
        (Encoder::Gpu(_, Gpu::Amd), Some(v), _) => {
            a.extend(["-quality", "quality", "-rc", "vbr_peak", "-b:v"].map(String::from));
            a.extend([k(v), "-maxrate".into(), k(v * 1.5), "-bufsize".into(), k(v * 2.0)]);
        }
        // ── Quality ──
        (Encoder::Cpu(c), None, Some(q)) => {
            let preset = if c == Codec::Av1 { "8" } else { "medium" };
            a.extend(["-preset".into(), preset.into(), "-crf".into(), q.to_string()]);
        }
        (Encoder::Gpu(_, Gpu::Nvidia), None, Some(q)) => {
            a.extend(["-preset", "p6", "-tune", "hq", "-rc", "vbr", "-b:v", "0", "-cq"].map(String::from));
            a.push(q.to_string());
        }
        (Encoder::Gpu(_, Gpu::Intel), None, Some(q)) => {
            a.extend(["-preset".into(), "slower".into(), "-global_quality".into(), q.to_string()]);
        }
        (Encoder::Gpu(_, Gpu::Amd), None, Some(q)) => {
            a.extend(["-rc".into(), "cqp".into(), "-qp_i".into(), q.to_string(), "-qp_p".into(), q.to_string()]);
        }
        _ => {}
    }
    if enc.codec() == Codec::Hevc {
        // Lets Apple devices / QuickTime play it.
        a.extend(["-tag:v", "hvc1"].map(String::from));
    }

    if let Some(p) = pass {
        // Relative stats file: the process runs in the cache dir (x265's
        // option parser chokes on Windows drive-letter colons).
        if enc.codec() == Codec::Hevc {
            a.extend(["-x265-params".into(), format!("pass={p}:stats=neria-2pass.log")]);
        } else {
            // `:v` so the audio encoder doesn't also look for pass stats.
            a.extend(["-pass:v".into(), p.to_string(), "-passlogfile:v".into(), "neria-2pass".into()]);
        }
    }

    if pass == Some(1) {
        a.extend(["-an", "-f", "null", if cfg!(windows) { "NUL" } else { "/dev/null" }].map(String::from));
        return a;
    }

    match plan.audio_kbps {
        Some(kbps) => {
            // Stereo: 5.1 at a small bitrate sounds worse and wastes the budget.
            a.extend(["-map", "0:a:0?", "-c:a", "aac", "-ac", "2", "-b:a"].map(String::from));
            a.push(format!("{kbps}k"));
        }
        None => a.push("-an".into()),
    }
    a.extend(["-movflags", "+faststart"].map(String::from));
    a.push(out.to_string_lossy().into_owned());
    a
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub fraction: f32,
    pub stage: String,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub path: PathBuf,
    pub bytes: u64,
    pub plan: Plan,
}

/// Compress `input`. In size mode, re-encodes smaller (up to twice) if the
/// result overshoots, so the file is guaranteed to fit when this returns Ok.
pub fn run(
    input: &Path,
    info: &FileInfo,
    opts: &Options,
    encoders: &Encoders,
    cancel: &AtomicBool,
    mut on: impl FnMut(Progress),
) -> Result<Outcome> {
    let mut plan = plan(info, opts, encoders)?;
    let out = output_path(input, opts);
    let work = paths::cache_dir();
    // Room for the result (quality mode: assume up to the original's size).
    let needed = match opts.target {
        Target::SizeMb(mb) => (mb * 1e6) as u64,
        Target::Quality(_) => info.size_bytes,
    };
    if let Some(dir) = out.parent() {
        crate::disk::check(dir, needed).map_err(|m| anyhow::anyhow!("{m} Free some up and try again."))?;
    }

    const ATTEMPTS: usize = 3;
    for attempt in 1..=ATTEMPTS {
        let retry = if attempt > 1 { format!(" (try {attempt})") } else { String::new() };
        let passes: Vec<Option<u8>> =
            if plan.encoder.two_pass() && plan.video_kbps.is_some() { vec![Some(1), Some(2)] } else { vec![None] };
        let n = passes.len();
        for (i, pass) in passes.into_iter().enumerate() {
            let stage = match (pass, n) {
                (Some(1), _) => format!("Analyzing{retry}…"),
                _ => format!("Compressing{retry}…"),
            };
            let (lo, hi) = if n == 2 { if i == 0 { (0.0, 0.35) } else { (0.35, 1.0) } } else { (0.0, 1.0) };
            let args = build_args(input, &out, info, &plan, pass);
            encode(&args, &work, info.duration, cancel, |f| {
                on(Progress { fraction: lo + (hi - lo) * f, stage: stage.clone() })
            })
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&out);
            })?;
        }

        let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        match plan.target_bytes {
            Some(target) if bytes > target && attempt < ATTEMPTS => {
                // Scale the video bitrate by how far over we went, plus a margin.
                let factor = target as f64 / bytes as f64 * 0.95;
                plan.video_kbps = plan.video_kbps.map(|v| v * factor);
                on(Progress {
                    fraction: 0.0,
                    stage: format!("{} is a bit over, redoing smaller…", fmt::bytes(bytes as f64)),
                });
            }
            Some(target) if bytes > target => {
                let _ = std::fs::remove_file(&out);
                bail!("Couldn't get under {} MB. Try a lower resolution or a shorter clip.", target as f64 / 1e6);
            }
            _ => {
                on(Progress { fraction: 1.0, stage: "Done".into() });
                return Ok(Outcome { path: out, bytes, plan });
            }
        }
    }
    unreachable!()
}

fn encode(args: &[String], cwd: &Path, duration: f64, cancel: &AtomicBool, mut on: impl FnMut(f32)) -> Result<()> {
    if std::env::var_os("NERIA_DEBUG_ARGS").is_some() {
        eprintln!("ffmpeg (in {}) {:?}", cwd.display(), args);
    }
    let mut child = proc::command(paths::tool("ffmpeg"))
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg")?;

    let mut stderr = child.stderr.take().expect("piped");
    let errors = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    for line in BufReader::new(child.stdout.take().expect("piped")).lines() {
        if cancel.load(Ordering::SeqCst) {
            proc::kill_tree(&mut child);
            break;
        }
        let Ok(line) = line else { break };
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
            on(((us / 1e6) / duration.max(0.01)).clamp(0.0, 1.0) as f32);
        }
    }
    let status = child.wait()?;
    let errors = errors.join().unwrap_or_default();
    if cancel.load(Ordering::SeqCst) {
        bail!("Cancelled");
    }
    if !status.success() {
        bail!("ffmpeg failed: {}", errors.lines().last().unwrap_or("unknown error"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> FileInfo {
        FileInfo {
            duration: 120.0,
            width: 1920,
            height: 1080,
            fps: 60.0,
            video_codec: "h264".into(),
            size_bytes: 300_000_000,
            has_video: true,
            has_audio: true,
        }
    }

    fn opts(target: Target) -> Options {
        Options {
            target,
            codec: Codec::H264,
            use_gpu: false,
            resolution: Resolution::Auto,
            max_fps: None,
            audio_kbps: Some(128),
        }
    }

    #[test]
    fn ten_mb_two_minutes_downscales() {
        let p = plan(&info(), &opts(Target::SizeMb(10.0)), &Encoders::default()).unwrap();
        // 10 MB over 120 s ≈ 640 kbps total: should drop to 30 fps and a small resolution.
        let total = p.video_kbps.unwrap() + p.audio_kbps.unwrap() as f64;
        assert!(total <= 10_000_000.0 * 8.0 / 1000.0 / 120.0, "{total}");
        assert!(p.height < 1080 && p.fps <= 30.0, "{p:?}");
        assert!(p.audio_kbps.unwrap() < 128);
        assert_eq!(p.width % 2, 0);
    }

    #[test]
    fn roomy_budget_keeps_resolution() {
        let p = plan(&info(), &opts(Target::SizeMb(500.0)), &Encoders::default()).unwrap();
        assert_eq!((p.width, p.height), (1920, 1080));
        assert_eq!(p.fps, 60.0);
    }

    #[test]
    fn impossible_target_errors() {
        let mut i = info();
        i.duration = 3.0 * 3600.0;
        assert!(plan(&i, &opts(Target::SizeMb(10.0)), &Encoders::default()).is_err());
    }

    #[test]
    fn two_pass_args() {
        let i = info();
        let o = opts(Target::SizeMb(25.0));
        let p = plan(&i, &o, &Encoders::default()).unwrap();
        let first = build_args(Path::new("in.mp4"), Path::new("out.mp4"), &i, &p, Some(1));
        assert!(first.windows(2).any(|w| w[0] == "-pass:v" && w[1] == "1"));
        assert!(first.contains(&"-an".to_string()));
        let second = build_args(Path::new("in.mp4"), Path::new("out.mp4"), &i, &p, Some(2));
        assert_eq!(second.last().unwrap(), "out.mp4");
        assert!(second.contains(&"aac".to_string()));
    }

    #[test]
    fn gpu_pick_falls_back_to_cpu() {
        let e = Encoders { gpu: vec![(Codec::H264, Gpu::Nvidia)] };
        assert_eq!(e.pick(Codec::H264, true), Encoder::Gpu(Codec::H264, Gpu::Nvidia));
        assert_eq!(e.pick(Codec::Av1, true), Encoder::Cpu(Codec::Av1));
        assert_eq!(e.pick(Codec::H264, false), Encoder::Cpu(Codec::H264));
    }
}
