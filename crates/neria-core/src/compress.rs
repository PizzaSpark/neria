//! Video compression: hit a target file size (e.g. Discord's 20 MB) or a quality level,
//! using a GPU encoder when one works on this machine. Sound files (no picture) go through
//! the same goals: AAC in .m4a, or Opus, at the bitrate the goal leaves (`plan_sound`).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};

use crate::media::FileInfo;
use crate::sound::{AudioEdit, Level, Loudness};
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

/// A sound file's format: AAC in .m4a (the default, "Plays everywhere" for videos is H.264
/// and AAC), MP3 (plays on anything, ever; needs about a third more bitrate for the same
/// sound), or Opus (sounds as good at about two-thirds of AAC's bitrate; videos' Smaller and
/// Smallest pick it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundCodec {
    Aac,
    Mp3,
    Opus,
    /// Lossless, about 60% of WAV ("Lossless": nothing lost, not smaller).
    Flac,
    /// 16-bit PCM ("For editing": every app opens it).
    Wav,
}

/// The bitrates an MP3 at 44.1 / 48 kHz can have (MPEG-1 Layer III).
const MP3_KBPS: [f64; 14] = [32.0, 40.0, 48.0, 56.0, 64.0, 80.0, 96.0, 112.0, 128.0, 160.0, 192.0, 224.0, 256.0, 320.0];

impl SoundCodec {
    pub fn for_codec(c: Codec) -> Self {
        if c == Codec::H264 { Self::Aac } else { Self::Opus }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Aac => "AAC",
            Self::Mp3 => "MP3",
            Self::Opus => "Opus",
            Self::Flac => "FLAC",
            Self::Wav => "WAV",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Aac => "m4a",
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Flac => "flac",
            Self::Wav => "wav",
        }
    }

    /// Nothing lost: no bitrate to pick (its size is the sound's own).
    pub fn lossless(self) -> bool {
        matches!(self, Self::Flac | Self::Wav)
    }

    /// More than this sounds no better (stereo).
    fn max_kbps(self) -> f64 {
        match self {
            Self::Aac | Self::Mp3 => 192.0,
            Self::Opus => 160.0,
            Self::Flac | Self::Wav => 1411.0,
        }
    }

    /// The least it can be made at all (mono by then).
    fn min_kbps(self) -> f64 {
        match self {
            Self::Aac => 24.0,
            Self::Mp3 => 32.0,
            Self::Opus => 12.0,
            Self::Flac | Self::Wav => 700.0,
        }
    }

    /// Below this, stereo costs more than it gives: mono.
    fn mono_below(self) -> f64 {
        match self {
            Self::Aac => 48.0,
            Self::Mp3 => 80.0,
            Self::Opus => 24.0,
            Self::Flac | Self::Wav => 0.0,
        }
    }

    /// The least that still sounds OK (stereo); `longest_fit` aims here.
    fn ok_kbps(self) -> f64 {
        match self {
            Self::Aac => 48.0,
            Self::Mp3 => 64.0,
            Self::Opus => 24.0,
            Self::Flac | Self::Wav => 700.0,
        }
    }

    /// A bitrate it can really be made at, not above `k`: MP3 has fixed steps.
    fn snap(self, k: f64) -> f64 {
        match self {
            Self::Mp3 => MP3_KBPS.iter().copied().rfind(|&s| s <= k + 0.01).unwrap_or(MP3_KBPS[0]),
            _ => k.floor(),
        }
    }

    /// How it'll sound at `kbps` over `channels` (mono needs about half).
    fn look(self, kbps: f64, channels: u32) -> Look {
        let k = if channels == 1 { kbps * 2.0 } else { kbps };
        let steps = match self {
            Self::Aac => [160.0, 112.0, 80.0, 48.0],
            Self::Mp3 => [192.0, 128.0, 96.0, 64.0],
            Self::Opus => [128.0, 80.0, 48.0, 24.0],
            Self::Flac | Self::Wav => return Look::Same,
        };
        if k >= steps[0] {
            Look::Same
        } else if k >= steps[1] {
            Look::NearlySame
        } else if k >= steps[2] {
            Look::BitSofter
        } else if k >= steps[3] {
            Look::Soft
        } else {
            Look::Blurry
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

/// What the Compress page offers, in plain words. Sending goals are sizes the place takes
/// (checked 2026-11-15: Discord free 20 MB since Aug 2026; Gmail/Outlook 25 MB per message,
/// which base64 makes ~18 MB of file; WhatsApp 16 MB for a video sent as a video, the
/// lowest figure sources give). Keeping goals are a look, and the size follows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Goal {
    Discord,
    Email,
    WhatsApp,
    /// Smaller, still sharp.
    Sharp,
    /// As small as possible.
    Tiny,
    /// Custom size in MB.
    Size(f64),
    /// Sound only, in this format, sounding the same (Compress → Sound only: "Plays
    /// everywhere" M4A, MP3, "Smallest that sounds good" Opus, "Lossless" FLAC, "For
    /// editing" WAV).
    Format(SoundCodec),
}

impl Goal {
    pub fn label(self) -> String {
        match self {
            Self::Discord => "For Discord".into(),
            Self::Email => "For email".into(),
            Self::WhatsApp => "For WhatsApp".into(),
            Self::Sharp => "Smaller, still sharp".into(),
            Self::Tiny => "As small as possible".into(),
            Self::Size(mb) => format!("{} MB", fmt_mb(mb)),
            Self::Format(SoundCodec::Aac) => "Plays everywhere".into(),
            Self::Format(SoundCodec::Mp3) => "MP3".into(),
            Self::Format(SoundCodec::Opus) => "Smallest that sounds good".into(),
            Self::Format(SoundCodec::Flac) => "Lossless".into(),
            Self::Format(SoundCodec::Wav) => "For editing".into(),
        }
    }

    pub fn target(self) -> Target {
        match self {
            Self::Discord => Target::SizeMb(20.0),
            Self::Email => Target::SizeMb(18.0),
            Self::WhatsApp => Target::SizeMb(16.0),
            Self::Sharp => Target::Quality(0.7),
            Self::Tiny => Target::Quality(0.25),
            Self::Size(mb) => Target::SizeMb(mb),
            // AAC 160 / MP3 192 kbps: sounds the same; Opus 96: nearly the same, a third the size.
            Self::Format(SoundCodec::Opus) => Target::Quality(0.7),
            Self::Format(_) => Target::Quality(0.95),
        }
    }

    /// The file is sent to an app that has to play it: only H.264 is safe everywhere.
    pub fn must_play_in(self) -> Option<&'static str> {
        match self {
            Self::Discord => Some("Discord"),
            Self::Email => Some("every mail app"),
            Self::WhatsApp => Some("WhatsApp"),
            _ => None,
        }
    }
}

/// "20", "2.5": MB without a pointless ".0".
pub fn fmt_mb(mb: f64) -> String {
    if (mb - mb.round()).abs() < 0.05 { format!("{:.0}", mb) } else { format!("{:.1}", mb) }
}

/// How the result will look next to the original, from the plan (a guess from the
/// resolution and how much bitrate each pixel gets, not a promise).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Look {
    Same,
    NearlySame,
    BitSofter,
    Soft,
    Blurry,
}

impl Look {
    pub fn label(self) -> &'static str {
        match self {
            Self::Same => "looks the same",
            Self::NearlySame => "looks nearly the same",
            Self::BitSofter => "a bit softer",
            Self::Soft => "soft",
            Self::Blurry => "blurry",
        }
    }

    /// The same steps for a sound file.
    pub fn sound_label(self) -> &'static str {
        match self {
            Self::Same => "sounds the same",
            Self::NearlySame => "sounds nearly the same",
            Self::BitSofter => "a bit duller",
            Self::Soft => "muffled",
            Self::Blurry => "poor",
        }
    }

    /// The best a picture at `height` can look, coming from `src_height`.
    fn cap_for(height: u32, src_height: u32) -> Self {
        if height + 8 >= src_height {
            Self::Same
        } else if height >= 480 {
            Self::BitSofter
        } else if height >= 360 {
            Self::Soft
        } else {
            Self::Blurry
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Size mode: pick the sharpest resolution the bitrate can carry. Quality mode: keep it,
    /// except at the smallest levels (under 0.4), which go to 720p 30 fps at most.
    Auto,
    Original,
    /// At most this many lines (the short side: "720p" for both landscape and portrait).
    Max(u32),
}

/// The picture without its black bars, in the shown orientation (after turning).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bars {
    pub w: u32,
    pub h: u32,
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub target: Target,
    pub codec: Codec,
    pub use_gpu: bool,
    pub resolution: Resolution,
    /// Cap frames per second (None = keep).
    pub max_fps: Option<f64>,
    /// Audio bitrate in kbps (per track); None removes audio.
    pub audio_kbps: Option<u32>,
    /// Only this part, in seconds (start, end). None = the whole video.
    pub range: Option<(f64, f64)>,
    /// Several parts joined into one file, in this order (2 or more; `range` is then None).
    /// The size goal is for the joined file.
    pub parts: Vec<(f64, f64)>,
    /// One of several parts saved as files of their own: its number, for the name.
    pub part_no: Option<usize>,
    /// Even out the sound to this loudness (LUFS), measured first (two passes, as the Clip
    /// page's Sound does).
    pub even: Option<f32>,
    /// A sound file's format, picked on its own (None: from `codec`, AAC or Opus).
    pub sound_codec: Option<SoundCodec>,
    /// Keep every sound track, the text captions and the chapter marks (else: the first
    /// sound track only, no captions, no chapters).
    pub keep_tracks: bool,
    /// Just the sound of a video, as a sound file (Compress → Sound only; "Extract audio").
    pub sound_only: bool,
    /// Louder or quieter by this many dB (Even out "as an album": one gain for every file,
    /// from their loudness together; `album_gain`). Instead of `even`.
    pub gain: Option<f32>,
    /// Cut these black bars off (from `detect_bars`).
    pub crop: Option<Bars>,
}

impl Options {
    /// How long the result will be: the joined parts, the trimmed part, or the whole video.
    pub fn length(&self, info: &FileInfo) -> f64 {
        if !self.parts.is_empty() {
            return self.parts.iter().map(|&(a, b)| (b.min(info.duration) - a.max(0.0)).max(0.0)).sum();
        }
        match self.range {
            Some((a, b)) => (b.min(info.duration) - a.max(0.0)).max(0.0),
            None => info.duration,
        }
    }

    /// The whole file, untouched: nothing trimmed or joined.
    pub fn whole(&self) -> bool {
        self.range.is_none() && self.parts.is_empty()
    }

    pub fn for_goal(goal: Goal) -> Self {
        Self {
            target: goal.target(),
            codec: Codec::H264,
            use_gpu: true,
            resolution: Resolution::Auto,
            max_fps: None,
            audio_kbps: Some(96),
            range: None,
            parts: Vec::new(),
            part_no: None,
            even: None,
            sound_codec: None,
            keep_tracks: true,
            sound_only: false,
            gain: None,
            crop: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub encoder: Encoder,
    /// Output size, as shown (portrait stays portrait).
    pub width: u32,
    pub height: u32,
    /// The short side ("720p").
    pub lines: u32,
    pub fps: f64,
    /// Size mode: video bitrate. None in quality mode.
    pub video_kbps: Option<f64>,
    /// Per sound track.
    pub audio_kbps: Option<u32>,
    /// How many sound tracks go in.
    pub audio_tracks: u32,
    /// Which caption tracks go in (their number among the file's captions).
    pub captions: Vec<usize>,
    /// Picture captions left out (an .mp4 can't hold them).
    pub captions_dropped: usize,
    pub chapters: bool,
    /// Quality mode: the encoder's quality number (CRF/CQ/QP).
    pub quality: Option<u32>,
    pub target_bytes: Option<u64>,
    /// How it'll look next to the original.
    pub look: Look,
    /// Frames per second were lowered from the original's.
    pub fps_cut: bool,
    /// ffmpeg video filters (crop, scale, fps).
    pub filters: Vec<String>,
    /// The size is tight: spend more time per frame (x264/x265 `slow`, as HandBrake's
    /// small "Social" presets do).
    pub tight: bool,
    /// A sound file: made in this format, no picture (the fields above about the picture
    /// are 0 / empty).
    pub sound: Option<SoundCodec>,
    /// Sound files: 1 mono, 2 stereo.
    pub channels: u32,
    /// Parts joined into one file (from `Options::parts`): no captions or chapters then
    /// (their times would be wrong), the first sound track only.
    pub parts: Vec<(f64, f64)>,
    /// Even out the sound to this loudness (LUFS).
    pub even: Option<f32>,
    /// What the loudness pass measured (`run` fills it in); without it evening out is
    /// one pass, fine for a sample to listen to.
    pub measured: Option<Loudness>,
    /// Louder or quieter by this many dB (an album evened out together).
    pub gain: Option<f32>,
}

impl Plan {
    /// "looks nearly the same" / "sounds nearly the same".
    pub fn look_label(&self) -> &'static str {
        if self.sound.is_some() { self.look.sound_label() } else { self.look.label() }
    }

    /// A sound file's size for `length` seconds: its bitrate (plus ~1% for the container),
    /// never over a size goal. A size goal often leaves more room than sound needs, so this
    /// is what to show, not the goal. None for a video.
    pub fn sound_bytes(&self, length: f64) -> Option<u64> {
        self.sound?;
        let b = (self.audio_kbps.unwrap_or(0) as f64 * 1000.0 / 8.0 * length * 1.01) as u64;
        Some(self.target_bytes.map_or(b, |t| b.min(t)))
    }

    /// The `-af` chain for evening out `dur` seconds of sound (None: not asked). With the
    /// measured loudness it's the second of two passes; without, one pass (samples).
    /// loudnorm works at 192 kHz inside: back to 48 kHz after.
    pub fn audio_filter(&self, dur: f64) -> Option<String> {
        if let Some(g) = self.gain {
            return AudioEdit { level: Level::Gain(g), ..Default::default() }
                .chain(dur, None)
                .map(|c| format!("{c},aresample=48000"));
        }
        let edit = AudioEdit { level: Level::Normalize(self.even?), ..Default::default() };
        edit.chain(dur, self.measured.as_ref()).map(|c| format!("{c},aresample=48000"))
    }

    /// The result's file type.
    pub fn ext(&self) -> &'static str {
        self.sound.map_or("mp4", SoundCodec::ext)
    }

    /// One-line technical summary (Advanced).
    pub fn summary(&self) -> String {
        let mut extra = String::new();
        if let Some(t) = self.even {
            extra.push_str(&format!("  ·  evened out ({t:.0} LUFS)"));
        }
        if !self.parts.is_empty() {
            extra.push_str(&format!("  ·  {} parts joined", self.parts.len()));
        }
        if let Some(sc) = self.sound {
            let ch = if self.channels == 1 { "mono" } else { "stereo" };
            if sc.lossless() {
                return format!("{}  ·  {ch}  ·  .{}{extra}", sc.label(), sc.ext());
            }
            return format!("{} kbps {}  ·  {ch}  ·  .{}{extra}", self.audio_kbps.unwrap_or(0), sc.label(), sc.ext());
        }
        let mut parts = vec![format!("{}×{}", self.width, self.height)];
        if self.fps > 0.0 {
            parts.push(format!("{:.0} fps", self.fps));
        }
        if let Some(v) = self.video_kbps {
            parts.push(format!("{:.0} kbps video", v));
        }
        parts.push(match self.audio_kbps {
            Some(a) if self.audio_tracks > 1 => format!("{} sound tracks at {a} kbps", self.audio_tracks),
            Some(a) => format!("{a} kbps sound"),
            None => "no sound".into(),
        });
        parts.push(self.encoder.label());
        if self.encoder.two_pass() && self.video_kbps.is_some() {
            parts.push("two passes".into());
        }
        parts.join("  ·  ") + &extra
    }

    /// What it'll look like, in plain words: "720p 30 fps, a bit softer".
    pub fn hint(&self) -> String {
        if let Some(sc) = self.sound {
            return match sc {
                SoundCodec::Flac => "FLAC, nothing lost".into(),
                SoundCodec::Wav => "WAV, nothing lost".into(),
                _ => format!("{} kbps {}, {}", self.audio_kbps.unwrap_or(0), sc.label(), self.look.sound_label()),
            };
        }
        let fps = if self.fps_cut { format!(" {:.0} fps", self.fps) } else { String::new() };
        format!("{}p{fps}, {}", self.lines, self.look.label())
    }
}

fn even(x: f64) -> u32 {
    ((x / 2.0).round() as u32).max(1) * 2
}

/// The picture as shown: phone footage is stored sideways and turned on playback (ffmpeg
/// turns it before our filters), then the black bars come off.
fn shown_size(info: &FileInfo, crop: Option<Bars>) -> (u32, u32) {
    match crop {
        Some(b) => (b.w, b.h),
        None if info.rotation % 180 == 90 => (info.height, info.width),
        None => (info.width, info.height),
    }
}

/// Work out resolution, fps and bitrates for `info` with `opts`.
pub fn plan(info: &FileInfo, opts: &Options, encoders: &Encoders) -> Result<Plan> {
    let mut p = plan_file(info, opts, encoders)?;
    if !opts.parts.is_empty() {
        p.parts = opts.parts.clone();
        p.captions.clear();
        p.captions_dropped = 0;
        p.chapters = false;
        p.audio_tracks = p.audio_tracks.min(1);
    }
    p.even = opts.even.filter(|_| info.has_audio && p.audio_kbps.is_some());
    p.gain = opts.gain.filter(|_| info.has_audio && p.audio_kbps.is_some());
    if p.gain.is_some() {
        p.even = None;
    }
    Ok(p)
}

fn plan_file(info: &FileInfo, opts: &Options, encoders: &Encoders) -> Result<Plan> {
    if !info.has_video || opts.sound_only {
        if info.has_audio {
            return plan_sound(info, opts);
        }
        bail!(if info.has_video { "This video has no sound." } else { "This file has no picture or sound to compress." });
    }
    if info.duration <= 0.0 {
        bail!("Can't tell how long this video is.");
    }
    let length = opts.length(info);
    if length < 0.1 {
        bail!("The part to keep is empty: move Out after In.");
    }
    let encoder = encoders.pick(opts.codec, opts.use_gpu);
    let src_fps = if info.fps > 0.0 { info.fps } else { 30.0 };
    let mut fps = opts.max_fps.map_or(src_fps, |m| src_fps.min(m));
    let audio_kbps = if info.has_audio { opts.audio_kbps } else { None };
    let audio_tracks = match audio_kbps {
        None => 0,
        Some(_) if opts.keep_tracks => info.audio_tracks.max(1),
        Some(_) => 1,
    };
    let (captions, captions_dropped) = if opts.keep_tracks {
        let text: Vec<usize> = info.subtitles.iter().enumerate().filter(|(_, t)| **t).map(|(i, _)| i).collect();
        let dropped = info.subtitles.len() - text.len();
        (text, dropped)
    } else {
        (Vec::new(), 0)
    };

    let (sw, sh) = shown_size(info, opts.crop);
    let portrait = sh > sw;
    let src_lines = sw.min(sh).max(1);
    let long_per_line = sw.max(sh) as f64 / src_lines as f64;
    let size_at = |lines: u32| {
        let (long, short) = (even(lines as f64 * long_per_line), even(lines as f64));
        if portrait { (short, long) } else { (long, short) }
    };
    let capped = |l: u32| match opts.resolution {
        Resolution::Max(m) => l.min(m),
        _ => l,
    };

    let (lines, video_kbps, quality, target_bytes, audio_kbps, by_quality) = match opts.target {
        Target::Quality(level) => {
            let l = level.clamp(0.0, 1.0);
            let tiny = opts.resolution == Resolution::Auto && l < 0.4;
            let lines = if tiny { src_lines.min(720) } else { capped(src_lines) };
            if tiny && opts.max_fps.is_none() {
                fps = fps.min(30.0);
            }
            let by_level = if l >= 0.65 {
                Look::Same
            } else if l >= 0.45 {
                Look::NearlySame
            } else {
                Look::BitSofter
            };
            // Calibrated on HandBrake's presets (docs/UPSTREAM.md): 0.7 is its default
            // (H.264 22 at 1080p), 0.25 its "Social" ceiling (27); smaller pictures get a
            // slightly better number, as its 720p (21) and 480p (20) presets do.
            let smaller = if lines <= 480 {
                2.0
            } else if lines <= 720 && l >= 0.4 {
                1.0
            } else {
                0.0
            };
            let quality = (match encoder.codec() {
                Codec::H264 => 30.0 - l * 12.0,
                Codec::Hevc => 31.0 - l * 12.0,
                Codec::Av1 => 45.0 - l * 20.0,
            } - smaller)
                .round() as u32;
            (lines, None, Some(quality), None, audio_kbps, by_level)
        }
        Target::SizeMb(mb) => {
            let target_bytes = (mb * 1_000_000.0) as u64;
            // Keep ~4% headroom for container overhead and rate-control wobble.
            let budget_kbps = target_bytes as f64 * 8.0 / 1000.0 / length * 0.96;
            let mut audio_kbps = audio_kbps;
            if let Some(a) = audio_kbps {
                // Don't let sound eat a tiny budget: all tracks together get ~15%.
                let fair = (budget_kbps * 0.15 / audio_tracks.max(1) as f64).max(32.0) as u32;
                audio_kbps = Some(a.min(fair.max(32)));
            }
            let video_kbps = budget_kbps - (audio_kbps.unwrap_or(0) * audio_tracks) as f64;
            if video_kbps < 60.0 {
                bail!(
                    "{} is too long to fit in {} MB. Trim it shorter or pick a bigger size.",
                    fmt::duration(length),
                    fmt_mb(mb)
                );
            }

            let bpp = |w: u32, h: u32, f: f64| video_kbps * 1000.0 / (w as f64 * h as f64 * f);
            let min_bpp = encoder.codec().min_bpp();
            let lines = match opts.resolution {
                Resolution::Original => src_lines,
                Resolution::Max(m) => src_lines.min(m),
                Resolution::Auto => {
                    // Starved bitrate: smooth 30 fps beats choppy 60.
                    if opts.max_fps.is_none() && fps > 31.0 {
                        let (w, h) = size_at(src_lines.min(720));
                        if bpp(w, h, fps) < min_bpp {
                            fps = fps.min(30.0);
                        }
                    }
                    [2160, 1440, 1080, 720, 540, 480, 360, 240]
                        .into_iter()
                        .map(|l: u32| l.min(src_lines))
                        .find(|l| {
                            let (w, h) = size_at(*l);
                            bpp(w, h, fps) >= min_bpp
                        })
                        .unwrap_or(src_lines.min(240))
                }
            };
            let (w, h) = size_at(lines);
            let room = bpp(w, h, fps) / min_bpp;
            let by_room = if room >= 2.5 {
                Look::Same
            } else if room >= 1.4 {
                Look::NearlySame
            } else if room >= 1.0 {
                Look::BitSofter
            } else if room >= 0.6 {
                Look::Soft
            } else {
                Look::Blurry
            };
            (lines, Some(video_kbps), None, Some(target_bytes), audio_kbps, by_room)
        }
    };

    let (width, height) = size_at(lines);
    let look = by_quality.max(Look::cap_for(lines, src_lines));
    let mut filters = Vec::new();
    if info.interlaced {
        // Only the frames that are combed, like HandBrake's Decomb.
        filters.push("bwdif=mode=send_frame:deint=interlaced".to_string());
    }
    if let Some(b) = opts.crop {
        filters.push(format!("crop={}:{}:{}:{}", b.w, b.h, b.x, b.y));
    }
    if (width, height) != (sw, sh) {
        filters.push(format!("scale={width}:{height}:flags=lanczos"));
    }
    if info.fps > 0.0 && fps < info.fps - 0.01 {
        filters.push(format!("fps={fps}"));
    }
    Ok(Plan {
        encoder,
        width,
        height,
        lines,
        fps,
        video_kbps,
        audio_kbps,
        audio_tracks,
        captions,
        captions_dropped,
        chapters: opts.keep_tracks && !info.chapters.is_empty(),
        quality,
        target_bytes,
        look,
        fps_cut: fps < src_fps - 0.5,
        filters,
        tight: target_bytes.is_some() && by_quality >= Look::BitSofter,
        sound: None,
        channels: 0,
        parts: Vec::new(),
        even: None,
        measured: None,
        gain: None,
    })
}

/// A sound file: the bitrate the goal leaves (a size goal), or one for the look (a keeping
/// goal), never more than the file carries now. Mono when the bitrate is low.
fn plan_sound(info: &FileInfo, opts: &Options) -> Result<Plan> {
    if info.duration <= 0.0 {
        bail!("Can't tell how long this file is.");
    }
    let length = opts.length(info);
    if length < 0.1 {
        bail!("The part to keep is empty: move Out after In.");
    }
    let sc = opts.sound_codec.unwrap_or(SoundCodec::for_codec(opts.codec));
    if sc.lossless() {
        // CD-like 16-bit sound: 1411 kbps stereo as WAV, FLAC about 60% of that.
        let channels = if info.audio_channels == 1 { 1 } else { 2 };
        let pcm = 705.6 * channels as f64;
        let kbps = if sc == SoundCodec::Flac { pcm * 0.6 } else { pcm };
        return Ok(Plan {
            encoder: Encoder::Cpu(opts.codec),
            width: 0,
            height: 0,
            lines: 0,
            fps: 0.0,
            video_kbps: None,
            audio_kbps: Some(kbps as u32),
            audio_tracks: 1,
            captions: Vec::new(),
            captions_dropped: 0,
            chapters: opts.keep_tracks && !info.chapters.is_empty() && sc == SoundCodec::Flac,
            quality: None,
            target_bytes: None,
            look: Look::Same,
            fps_cut: false,
            filters: Vec::new(),
            tight: false,
            sound: Some(sc),
            channels,
            parts: Vec::new(),
            even: None,
            measured: None,
        gain: None,
        });
    }
    let (kbps, target_bytes) = match opts.target {
        Target::Quality(level) => {
            // 0.7 (Smaller, still clear): AAC 128 / MP3 160 / Opus 96; 0.25: AAC 72 /
            // MP3 96 / Opus 48.
            let l = level.clamp(0.0, 1.0) as f64;
            let k = match sc {
                SoundCodec::Aac => 40.0 + l * 125.0,
                SoundCodec::Mp3 => 64.0 + l * 137.0,
                SoundCodec::Opus => 24.0 + l * 103.0,
                SoundCodec::Flac | SoundCodec::Wav => unreachable!("lossless is planned above"),
            };
            ((k / 8.0).round() * 8.0, None)
        }
        Target::SizeMb(mb) => {
            let target_bytes = (mb * 1_000_000.0) as u64;
            // ~3% headroom for the container and the encoder's wobble.
            let budget = target_bytes as f64 * 8.0 / 1000.0 / length * 0.97;
            if budget < sc.min_kbps() {
                bail!(
                    "{} is too long to fit in {} MB. Trim it shorter or pick a bigger size.",
                    fmt::duration(length),
                    fmt_mb(mb)
                );
            }
            (budget.floor(), Some(target_bytes))
        }
    };
    // What the file carries now (its whole size over its length): more can't sound better.
    // (A video's size is mostly picture: no cap from it.)
    let now = if info.has_video { f64::MAX } else { info.size_bytes as f64 * 8.0 / 1000.0 / info.duration };
    // Mono sounds the same at half of stereo's bitrate.
    let max = if info.audio_channels == 1 { sc.max_kbps() / 2.0 } else { sc.max_kbps() };
    let kbps = sc.snap(kbps.min(max).min(now.floor().max(sc.min_kbps())).max(sc.min_kbps()));
    let channels = if info.audio_channels == 1 || kbps < sc.mono_below() { 1 } else { 2 };
    Ok(Plan {
        encoder: Encoder::Cpu(opts.codec),
        width: 0,
        height: 0,
        lines: 0,
        fps: 0.0,
        video_kbps: None,
        audio_kbps: Some(kbps as u32),
        audio_tracks: 1,
        captions: Vec::new(),
        captions_dropped: 0,
        chapters: opts.keep_tracks && !info.chapters.is_empty(),
        quality: None,
        target_bytes,
        look: sc.look(kbps, channels),
        fps_cut: false,
        filters: Vec::new(),
        tight: false,
        sound: Some(sc),
        channels,
        parts: Vec::new(),
        even: None,
        measured: None,
        gain: None,
    })
}

/// Even out "as an album": the one gain (dB) that brings these files, together, to `target`
/// LUFS, from each one's loudness and length (louder parts weigh by their energy), so a quiet
/// song stays quieter than a loud one. Within ±20 dB.
pub fn album_gain(files: &[(f64, f64)], target: f32) -> Option<f32> {
    let total: f64 = files.iter().map(|(_, len)| len).sum();
    if total <= 0.0 {
        return None;
    }
    let energy: f64 = files.iter().map(|(i, len)| len * 10f64.powf(i / 10.0)).sum::<f64>() / total;
    let album = 10.0 * energy.log10();
    Some(((target as f64 - album) as f32).clamp(-20.0, 20.0))
}

/// A size goal the whole video can't look OK in (the plan fails, or comes out blurry):
/// how long a part would fit at 360p 30 fps with just enough bitrate. None when it fits.
pub fn longest_fit(info: &FileInfo, opts: &Options, encoders: &Encoders) -> Option<f64> {
    let Target::SizeMb(mb) = opts.target else { return None };
    if let Ok(p) = plan(info, opts, encoders)
        && p.look < Look::Blurry
    {
        return None;
    }
    if !info.has_video {
        // A sound file: as long as just-OK sound (as `plan_sound` counts the room) fits.
        let sc = opts.sound_codec.unwrap_or(SoundCodec::for_codec(opts.codec));
        let ok = if info.audio_channels == 1 { sc.ok_kbps() / 2.0 } else { sc.ok_kbps() };
        let secs = (mb * 1e6 * 8.0 / 1000.0 * 0.97 / ok).floor();
        return (secs >= 1.0 && secs < opts.length(info)).then_some(secs);
    }
    let (sw, sh) = shown_size(info, opts.crop);
    let lines = sw.min(sh).clamp(1, 360) as f64;
    let long = lines * sw.max(sh) as f64 / sw.min(sh).max(1) as f64;
    let fps = if info.fps > 0.0 { info.fps.min(30.0) } else { 30.0 };
    let video = encoders.pick(opts.codec, opts.use_gpu).codec().min_bpp() * long * lines * fps / 1000.0;
    let tracks = if !info.has_audio || opts.audio_kbps.is_none() {
        0
    } else if opts.keep_tracks {
        info.audio_tracks.max(1)
    } else {
        1
    };
    // Sound takes ~15% of a small budget (at least 32 kbps a track), as in `plan`; 5% spare.
    let total = (video / 0.85).max(video + 32.0 * tracks as f64) * 1.05;
    let secs = (mb * 1e6 * 8.0 / 1000.0 * 0.96 / total).floor();
    (secs >= 1.0 && secs < opts.length(info)).then_some(secs)
}

/// Black bars around the picture (films in a 16:9 frame, 4:3 in widescreen), found by
/// looking at 2 s in five places; their union, so a dark scene doesn't cut real picture.
/// None when there are none worth cutting (under 8 px a side).
pub fn detect_bars(path: &Path, info: &FileInfo) -> Option<Bars> {
    if !info.has_video || info.duration <= 0.0 {
        return None;
    }
    let (sw, sh) = shown_size(info, None);
    let jobs: Vec<_> = [0.15, 0.35, 0.5, 0.65, 0.85]
        .into_iter()
        .map(|f| {
            let (path, at) = (path.to_path_buf(), (info.duration * f - 1.0).max(0.0));
            thread::spawn(move || {
                let out = proc::command(paths::tool("ffmpeg"))
                    .args(["-hide_banner", "-nostats", "-v", "info", "-ss", &format!("{at:.2}"), "-t", "2", "-i"])
                    .arg(&path)
                    .args(["-an", "-sn", "-vf", "cropdetect=limit=24:round=2:reset=0", "-f", "null", "-"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .output()
                    .ok()?;
                let text = String::from_utf8_lossy(&out.stderr);
                let last = text.lines().rev().find_map(|l| l.split("crop=").nth(1))?;
                let n: Vec<u32> = last.trim().split(':').filter_map(|v| v.parse().ok()).collect();
                (n.len() == 4).then(|| (n[2], n[3], n[2] + n[0], n[3] + n[1]))
            })
        })
        .collect();
    let boxes: Vec<_> = jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect();
    if boxes.len() < 3 {
        return None;
    }
    let (x0, y0) = (boxes.iter().map(|b| b.0).min()?, boxes.iter().map(|b| b.1).min()?);
    let (x1, y1) = (boxes.iter().map(|b| b.2).max()?.min(sw), boxes.iter().map(|b| b.3).max()?.min(sh));
    bars_worth_cutting(sw, sh, x0, y0, x1, y1)
}

fn bars_worth_cutting(sw: u32, sh: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> Option<Bars> {
    let (w, h) = (x1.saturating_sub(x0) / 2 * 2, y1.saturating_sub(y0) / 2 * 2);
    let side = |a: u32, b: u32| a >= 8 || b >= 8;
    let cut = side(x0, sw.saturating_sub(x1)) || side(y0, sh.saturating_sub(y1));
    // Too much gone means a dark video, not bars.
    let sane = w * 2 >= sw && h * 2 >= sh;
    (cut && sane).then_some(Bars { w, h, x: x0, y: y0 })
}

pub fn output_path(input: &Path, opts: &Options, plan: &Plan) -> PathBuf {
    let dir = input.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "video".into());
    // Converted to another kind of sound file (a video's sound, an .mp3 as .flac): just the
    // new extension, as a converter names it.
    let same_ext = input.extension().is_some_and(|e| e.eq_ignore_ascii_case(plan.ext()));
    if plan.sound.is_some() && !same_ext && opts.whole() && matches!(opts.target, Target::Quality(_)) {
        return clip::unique_path(&dir, &stem, plan.ext());
    }
    let mut tag = match opts.target {
        Target::SizeMb(mb) => format!("compressed {mb}MB"),
        Target::Quality(_) => "compressed".into(),
    };
    if let Some(n) = opts.part_no {
        tag.push_str(&format!(", part {n}"));
    } else if !opts.parts.is_empty() {
        tag.push_str(&format!(", {} parts", opts.parts.len()));
    }
    clip::unique_path(&dir, &format!("{stem} ({tag})"), plan.ext())
}

/// Pass: None = single pass, Some(1) / Some(2) = two-pass stage.
/// `range`: only that part (the same seek in both passes, so their stats line up); a sample
/// passes its own. Without one, `plan.parts` (2 or more) are trimmed and joined in one
/// filter graph.
pub fn build_args(
    input: &Path,
    out: &Path,
    info: &FileInfo,
    plan: &Plan,
    range: Option<(f64, f64)>,
    pass: Option<u8>,
) -> Vec<String> {
    let enc = plan.encoder;
    let mut a: Vec<String> =
        ["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y"].map(String::from).to_vec();
    let joined = range.is_none() && !plan.parts.is_empty();
    let dur = match range {
        Some((start, end)) => (end - start).max(0.0),
        None if joined => plan.parts.iter().map(|&(s, e)| (e - s).max(0.0)).sum(),
        None => info.duration,
    };
    if let Some((start, end)) = range {
        // Before -i: a fast seek, still frame-exact since everything is re-encoded.
        a.extend([
            "-ss".into(),
            format!("{:.3}", start.max(0.0)),
            "-t".into(),
            format!("{:.3}", (end - start).max(0.0)),
        ]);
    }
    a.push("-i".into());
    a.push(input.to_string_lossy().into_owned());
    let with_video = plan.sound.is_none();
    let with_audio = plan.audio_kbps.is_some() && info.has_audio && pass != Some(1);
    let af = if with_audio { plan.audio_filter(dur) } else { None };
    if joined {
        // Each part trimmed from the file, then joined; the picture's filters and the
        // sound's evening out apply to the joined whole.
        let mut graph = Vec::new();
        let mut inputs = String::new();
        for (i, &(s, e)) in plan.parts.iter().enumerate() {
            if with_video {
                graph.push(format!("[0:V:0]trim=start={s:.3}:end={e:.3},setpts=PTS-STARTPTS[v{i}]"));
                inputs.push_str(&format!("[v{i}]"));
            }
            if with_audio {
                graph.push(format!("[0:a:0]atrim=start={s:.3}:end={e:.3},asetpts=PTS-STARTPTS[a{i}]"));
                inputs.push_str(&format!("[a{i}]"));
            }
        }
        let outs = format!("{}{}", if with_video { "[vj]" } else { "" }, if with_audio { "[aj]" } else { "" });
        graph.push(format!(
            "{inputs}concat=n={}:v={}:a={}{outs}",
            plan.parts.len(),
            with_video as u8,
            with_audio as u8
        ));
        if with_video {
            let vf = if plan.filters.is_empty() { "null".to_string() } else { plan.filters.join(",") };
            graph.push(format!("[vj]{vf}[vo]"));
        }
        if with_audio {
            graph.push(format!("[aj]{}[ao]", af.as_deref().unwrap_or("anull")));
        }
        a.extend(["-filter_complex".into(), graph.join(";")]);
        if with_video {
            a.extend(["-map", "[vo]"].map(String::from));
        }
        if with_audio {
            a.extend(["-map", "[ao]"].map(String::from));
        }
    }
    if let Some(sc) = plan.sound {
        // A sound file: its first sound track, re-encoded; no cover art (an .opus can't
        // hold one), the tags and chapters stay.
        if !joined {
            a.extend(["-map", "0:a:0"].map(String::from));
            if let Some(f) = &af {
                a.extend(["-af".into(), f.clone()]);
            }
        }
        a.extend(["-vn", "-sn", "-dn"].map(String::from));
        match sc {
            SoundCodec::Aac => a.extend(["-c:a", "aac"].map(String::from)),
            // Constant bitrate: the size is exactly what it says.
            SoundCodec::Mp3 => a.extend(["-c:a", "libmp3lame"].map(String::from)),
            SoundCodec::Opus => a.extend(["-c:a", "libopus", "-vbr", "on"].map(String::from)),
            SoundCodec::Flac => a.extend(["-c:a", "flac"].map(String::from)),
            SoundCodec::Wav => a.extend(["-c:a", "pcm_s16le"].map(String::from)),
        }
        if !sc.lossless() {
            a.extend(["-b:a".into(), format!("{}k", plan.audio_kbps.unwrap_or(96))]);
        }
        a.extend(["-ac".into(), plan.channels.max(1).to_string()]);
        if !plan.chapters {
            a.extend(["-map_chapters", "-1"].map(String::from));
        }
        if sc == SoundCodec::Aac {
            a.extend(["-movflags", "+faststart"].map(String::from));
        }
        a.push(out.to_string_lossy().into_owned());
        return a;
    }
    if !joined {
        a.extend(["-map", "0:V:0"].map(String::from));
        if !plan.filters.is_empty() {
            a.extend(["-vf".into(), plan.filters.join(",")]);
        }
    }

    a.extend(["-c:v".into(), enc.ffmpeg_name()]);
    let pix = if matches!(enc, Encoder::Gpu(_, Gpu::Intel)) { "nv12" } else { "yuv420p" };
    a.extend(["-pix_fmt".into(), pix.into()]);

    let k = |v: f64| format!("{}k", v.round() as u64);
    match (enc, plan.video_kbps, plan.quality) {
        // ── Target size ──
        (Encoder::Cpu(Codec::H264 | Codec::Hevc), Some(v), _) => {
            let preset = if plan.tight { "slow" } else { "medium" };
            a.extend(["-preset".into(), preset.into(), "-b:v".into(), k(v)]);
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
        a.extend(["-an", "-sn", "-f", "null", if cfg!(windows) { "NUL" } else { "/dev/null" }].map(String::from));
        return a;
    }

    match plan.audio_kbps {
        Some(kbps) => {
            if !joined {
                let map = if plan.audio_tracks > 1 { "0:a?" } else { "0:a:0?" };
                a.extend(["-map".into(), map.into()]);
                if let Some(f) = &af {
                    a.extend(["-af".into(), f.clone()]);
                }
            }
            // Stereo: 5.1 at a small bitrate sounds worse and wastes the budget.
            a.extend(["-c:a", "aac", "-ac", "2", "-b:a"].map(String::from));
            a.push(format!("{kbps}k"));
        }
        None => a.push("-an".into()),
    }
    for i in &plan.captions {
        a.extend(["-map".into(), format!("0:s:{i}")]);
    }
    if !plan.captions.is_empty() {
        a.extend(["-c:s", "mov_text"].map(String::from));
    }
    if !plan.chapters {
        a.extend(["-map_chapters", "-1"].map(String::from));
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
    let out = output_path(input, opts, &plan);
    let work = paths::cache_dir();
    let length = opts.length(info);
    // Room for the result (quality mode: assume up to the original's size for that length).
    let needed = match opts.target {
        Target::SizeMb(mb) => (mb * 1e6) as u64,
        Target::Quality(_) => (info.size_bytes as f64 * (length / info.duration).min(1.0)) as u64,
    };
    if let Some(dir) = out.parent() {
        crate::disk::check(dir, needed).map_err(|m| anyhow::anyhow!("{m} Free some up and try again."))?;
    }
    // Evening out: how loud the part(s) are first, so the encode can set it in one go.
    if let Some(target) = plan.even {
        on(Progress { fraction: 0.0, stage: "Measuring the loudness…".into() });
        plan.measured = measure(input, info, opts, target, cancel, |f| {
            on(Progress { fraction: f, stage: "Measuring the loudness…".into() })
        })?;
        if plan.measured.is_none() {
            // Only silence: nothing to even out.
            plan.even = None;
        }
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
            let args = build_args(input, &out, info, &plan, opts.range, pass);
            encode(&args, &work, length, cancel, |f| {
                on(Progress { fraction: lo + (hi - lo) * f, stage: stage.clone() })
            })
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&out);
            })?;
        }

        let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        match plan.target_bytes {
            Some(target) if bytes > target && attempt < ATTEMPTS => {
                let over = fmt::mb(bytes as f64);
                if let Some(sc) = plan.sound {
                    // Sound: its bitrate, by how far over, plus a margin.
                    let factor = target as f64 / bytes as f64 * 0.95;
                    plan.audio_kbps = plan.audio_kbps.map(|k| sc.snap((k as f64 * factor).max(8.0)) as u32);
                    on(Progress { fraction: 0.0, stage: format!("{over} is a bit over, redoing smaller…") });
                } else if let Encoder::Gpu(codec, _) = plan.encoder {
                    // Graphics cards keep their own idea of the bitrate (Intel's came out
                    // bigger on every redo); the processor's two passes hit it exactly.
                    plan.encoder = Encoder::Cpu(codec);
                    plan.video_kbps = plan.video_kbps.map(|v| v * 0.97);
                    on(Progress {
                        fraction: 0.0,
                        stage: format!("{over} is a bit over, redoing it on the processor for an exact size…"),
                    });
                } else {
                    // Scale the video bitrate by how far over we went, plus a margin.
                    let factor = target as f64 / bytes as f64 * 0.95;
                    plan.video_kbps = plan.video_kbps.map(|v| v * factor);
                    on(Progress { fraction: 0.0, stage: format!("{over} is a bit over, redoing smaller…") });
                }
            }
            Some(target) if bytes > target => {
                let _ = std::fs::remove_file(&out);
                bail!(
                    "Couldn't get under {} MB. Try a lower resolution or a shorter part.",
                    fmt_mb(target as f64 / 1e6)
                );
            }
            _ => {
                on(Progress { fraction: 1.0, stage: "Done".into() });
                return Ok(Outcome { path: out, bytes, plan });
            }
        }
    }
    unreachable!()
}

/// How loud what `opts` keeps is (the trimmed part, the joined parts or the whole file):
/// the first pass of evening out. None for silence.
fn measure(
    input: &Path,
    info: &FileInfo,
    opts: &Options,
    target: f32,
    cancel: &AtomicBool,
    on: impl FnMut(f32),
) -> Result<Option<Loudness>> {
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-progress", "pipe:1", "-y"].map(String::from).to_vec();
    if let Some((s, e)) = opts.range {
        a.extend(["-ss".into(), format!("{:.3}", s.max(0.0)), "-t".into(), format!("{:.3}", (e - s).max(0.0))]);
    }
    a.extend(["-i".into(), input.to_string_lossy().into_owned()]);
    let measure = format!("loudnorm=I={target}:TP=-1.5:LRA=11:print_format=json");
    if opts.parts.is_empty() {
        a.extend(["-map", "0:a:0", "-vn", "-sn", "-dn", "-af"].map(String::from));
        a.push(measure);
    } else {
        let mut graph: Vec<String> = opts
            .parts
            .iter()
            .enumerate()
            .map(|(i, &(s, e))| format!("[0:a:0]atrim=start={s:.3}:end={e:.3},asetpts=PTS-STARTPTS[a{i}]"))
            .collect();
        let inputs: String = (0..opts.parts.len()).map(|i| format!("[a{i}]")).collect();
        graph.push(format!("{inputs}concat=n={}:v=0:a=1,{measure}[m]", opts.parts.len()));
        a.extend(["-filter_complex".into(), graph.join(";"), "-map".into(), "[m]".into()]);
    }
    a.extend(["-f", "null", "-"].map(String::from));
    let log = crate::sound::run_ffmpeg(&a, opts.length(info), cancel, on)
        .map_err(|e| anyhow::anyhow!("Couldn't measure the loudness: {e}"))?;
    Ok(crate::sound::parse_loudness(&log))
}

/// A short piece encoded with the real settings, to look at or measure.
pub struct Sample {
    pub path: PathBuf,
    pub bytes: u64,
    pub secs: f64,
}

/// Encode `secs` from `at` with the plan for the whole thing (so the bitrate is the real one),
/// single pass, into `out`.
pub fn sample(
    input: &Path,
    info: &FileInfo,
    plan: &Plan,
    at: f64,
    secs: f64,
    out: &Path,
    cancel: &AtomicBool,
) -> Result<Sample> {
    let at = at.clamp(0.0, (info.duration - 0.5).max(0.0));
    let secs = secs.min(info.duration - at).max(0.1);
    let args = build_args(input, out, info, plan, Some((at, at + secs)), None);
    encode(&args, &paths::cache_dir(), secs, cancel, |_| {})?;
    let bytes = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    Ok(Sample { path: out.to_path_buf(), bytes, secs })
}

/// What the Compress page shows before you start: the size (measured from three 2 s samples
/// when the goal is a look, not a size) and the same moment before and after.
pub struct Preview {
    /// Estimated result size; for a size goal, the size itself.
    pub bytes: u64,
    pub estimated: bool,
    pub before: PathBuf,
    pub after: PathBuf,
}

/// `key` names the cache files (one set per file on the page). Picture height of the frames:
/// `frame_h` (the after frame is scaled up to it, so softness shows as it would full screen).
pub fn preview(
    input: &Path,
    info: &FileInfo,
    opts: &Options,
    encoders: &Encoders,
    key: &str,
    frame_h: u32,
    cancel: &AtomicBool,
) -> Result<Preview> {
    let plan = plan(info, opts, encoders)?;
    let dir = paths::cache_dir().join("compress-preview");
    std::fs::create_dir_all(&dir)?;
    // Samples come from the part kept (joined parts: the longest); the size is for all of it.
    let (from, to) = sample_span(info, opts);
    let length = (to - from).max(0.1);
    let total = opts.length(info).max(0.1);
    if let Some(bytes) = plan.sound_bytes(total) {
        // A sound file: no pictures, and the size is its bitrate over its length, no
        // samples needed.
        return Ok(Preview { bytes, estimated: true, before: PathBuf::new(), after: PathBuf::new() });
    }
    let spots: &[f64] = if plan.target_bytes.is_some() { &[0.5] } else { &[0.2, 0.5, 0.8] };
    let piece = 2.0_f64.min(length / spots.len() as f64);
    let (mut bytes, mut secs, mut middle) = (0u64, 0.0, None);
    for (i, f) in spots.iter().enumerate() {
        let at = from + (length - piece) * f;
        let s = sample(input, info, &plan, at, piece, &dir.join(format!("{key}-{i}.mp4")), cancel)?;
        bytes += s.bytes;
        secs += s.secs;
        if (*f - 0.5).abs() < 0.01 {
            middle = Some((at, s.path));
        }
    }
    let (at, after_clip) = middle.context("no middle sample")?;
    // A frame a second in: the first one after a seek is a keyframe, sharper than the rest.
    let mid = (piece / 2.0).min(1.0);
    let before = dir.join(format!("{key}-before.jpg"));
    let after = dir.join(format!("{key}-after.jpg"));
    frame(input, at + mid, frame_h, opts.crop, &before)?;
    frame(&after_clip, mid, frame_h, None, &after)?;
    let (estimated, total) = match plan.target_bytes {
        // A size goal ends at most there; the encode aims a little under.
        Some(t) => (false, t),
        None => (true, (bytes as f64 / secs.max(0.1) * total) as u64),
    };
    Ok(Preview { bytes: total, estimated, before, after })
}

/// Where samples (the preview's, the one you play) come from: the trimmed part, the longest
/// of joined parts, or the whole file.
pub fn sample_span(info: &FileInfo, opts: &Options) -> (f64, f64) {
    let longest = opts.parts.iter().copied().max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)));
    opts.range.or(longest).unwrap_or((0.0, info.duration))
}

/// One picture of `path` at `at`, `height` px tall (as shown), cropped first if asked.
fn frame(path: &Path, at: f64, height: u32, crop: Option<Bars>, out: &Path) -> Result<()> {
    let mut vf = String::new();
    if let Some(b) = crop {
        vf.push_str(&format!("crop={}:{}:{}:{},", b.w, b.h, b.x, b.y));
    }
    vf.push_str(&format!("scale=-2:{height}:flags=bicubic"));
    let status = proc::command(paths::tool("ffmpeg"))
        .args(["-hide_banner", "-v", "error", "-y", "-ss", &format!("{at:.3}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &vf, "-q:v", "3"])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to start ffmpeg")?;
    if !status.success() || !out.is_file() {
        bail!("Couldn't take a picture from the video.");
    }
    Ok(())
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
            audio_tracks: 1,
            audio_titles: Vec::new(),
            audio_codec: "aac".into(),
            audio_channels: 2,
            interlaced: false,
            subtitles: Vec::new(),
            chapters: Vec::new(),
            rotation: 0,
        }
    }

    /// A 30 minute WAV recording (1411 kbps, stereo).
    fn song() -> FileInfo {
        FileInfo {
            duration: 1800.0,
            width: 0,
            height: 0,
            fps: 0.0,
            video_codec: String::new(),
            size_bytes: 317_500_000,
            has_video: false,
            audio_codec: "pcm_s16le".into(),
            ..info()
        }
    }

    #[test]
    fn sound_file_fits_a_size() {
        let p = plan(&song(), &opts(Target::SizeMb(20.0)), &Encoders::default()).unwrap();
        assert_eq!(p.sound, Some(SoundCodec::Aac));
        // 20 MB over 30 min: ~86 kbps, kept stereo.
        let k = p.audio_kbps.unwrap();
        assert!((80..=90).contains(&k), "{k}");
        assert_eq!(p.channels, 2);
        assert_eq!(p.ext(), "m4a");
        let args = build_args(Path::new("in.wav"), Path::new("out.m4a"), &song(), &p, None, None);
        assert!(args.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert!(args.windows(2).any(|w| w == ["-b:a".to_string(), format!("{k}k")]));
        assert!(!args.iter().any(|a| a == "0:V:0"));
    }

    #[test]
    fn sound_file_goes_mono_then_refuses() {
        let mut o = opts(Target::SizeMb(5.0));
        o.codec = Codec::Hevc; // "Smaller": Opus
        let p = plan(&song(), &o, &Encoders::default()).unwrap();
        assert_eq!(p.sound, Some(SoundCodec::Opus));
        assert_eq!(p.channels, 1); // ~21 kbps
        assert_eq!(p.ext(), "opus");
        assert!(plan(&song(), &opts(Target::SizeMb(2.0)), &Encoders::default()).is_err());
        // And the part that would fit sounding OK: 2 MB at 48 kbps ≈ 5 min.
        let fit = longest_fit(&song(), &opts(Target::SizeMb(2.0)), &Encoders::default()).unwrap();
        assert!((300.0..330.0).contains(&fit), "{fit}");
    }

    #[test]
    fn sound_file_never_grows() {
        // A 64 kbps mp3: "Smaller, still sharp" (AAC 128) stays at what it has.
        let small = FileInfo { size_bytes: 14_400_000, audio_codec: "mp3".into(), ..song() };
        let p = plan(&small, &opts(Target::Quality(0.7)), &Encoders::default()).unwrap();
        assert_eq!(p.audio_kbps, Some(64));
        let p = plan(&song(), &opts(Target::Quality(0.7)), &Encoders::default()).unwrap();
        assert_eq!(p.audio_kbps, Some(128));
        assert_eq!(p.hint(), "128 kbps AAC, sounds nearly the same");
    }

    #[test]
    fn mp3_takes_its_steps() {
        let mut o = opts(Target::SizeMb(20.0));
        o.sound_codec = Some(SoundCodec::Mp3);
        // 20 MB over 30 min leaves ~86 kbps: MP3 has 80, and that's mono-or-stereo's edge.
        let p = plan(&song(), &o, &Encoders::default()).unwrap();
        assert_eq!((p.sound, p.audio_kbps, p.channels, p.ext()), (Some(SoundCodec::Mp3), Some(80), 2, "mp3"));
        let args = build_args(Path::new("in.wav"), Path::new("out.mp3"), &song(), &p, None, None);
        assert!(args.windows(2).any(|w| w == ["-c:a", "libmp3lame"]));
        assert!(!args.iter().any(|a| a == "+faststart"));
        // "Smaller, still clear": 160; never off the steps.
        o.target = Target::Quality(0.7);
        assert_eq!(plan(&song(), &o, &Encoders::default()).unwrap().audio_kbps, Some(160));
        o.target = Target::SizeMb(5.0); // ~22 kbps: under MP3's least
        assert!(plan(&song(), &o, &Encoders::default()).is_err());
    }

    #[test]
    fn mono_sound_needs_half() {
        // A mono voice recording for Discord: 96 kbps sounds the same, so not 192.
        let voice = FileInfo { audio_channels: 1, duration: 760.0, ..song() };
        let p = plan(&voice, &opts(Target::SizeMb(20.0)), &Encoders::default()).unwrap();
        assert_eq!((p.audio_kbps, p.channels, p.look), (Some(96), 1, Look::Same));
        assert!(p.sound_bytes(760.0).unwrap() < 9_300_000);
        // 30 min of it: the 20 MB goal leaves less room (~86 kbps), and that wins.
        let long = FileInfo { duration: 1800.0, ..voice };
        let p = plan(&long, &opts(Target::SizeMb(20.0)), &Encoders::default()).unwrap();
        assert_eq!(p.audio_kbps, Some(86));
    }

    fn opts(target: Target) -> Options {
        Options {
            target,
            codec: Codec::H264,
            use_gpu: false,
            resolution: Resolution::Auto,
            max_fps: None,
            audio_kbps: Some(128),
            range: None,
            parts: Vec::new(),
            part_no: None,
            even: None,
            sound_codec: None,
            keep_tracks: false,
            sound_only: false,
            gain: None,
            crop: None,
        }
    }

    #[test]
    fn an_album_gets_one_gain() {
        // A loud single (-8 LUFS) and a quiet ballad (-18), as long as each other: together
        // about -10.6, so both go down ~3.4 dB to -14 and the ballad stays 10 dB quieter.
        let g = album_gain(&[(-8.0, 200.0), (-18.0, 200.0)], -14.0).unwrap();
        assert!((g + 3.4).abs() < 0.1, "{g}");
        assert_eq!(album_gain(&[], -14.0), None);
        let mut p = plan(&song(), &Options { gain: Some(g), even: Some(-14.0), ..opts(Target::Quality(0.7)) }, &Encoders::default()).unwrap();
        assert_eq!(p.even, None);
        p.measured = None;
        assert_eq!(p.audio_filter(60.0).as_deref(), Some("volume=-3.4dB,aresample=48000"));
    }

    #[test]
    fn a_videos_sound_in_each_format() {
        let opts_for = |sc: SoundCodec| Options {
            sound_codec: Some(sc),
            sound_only: true,
            ..opts(Goal::Format(sc).target())
        };
        // Plays everywhere: AAC that sounds the same, the picture left out.
        let p = plan(&info(), &opts_for(SoundCodec::Aac), &Encoders::default()).unwrap();
        assert_eq!((p.sound, p.audio_kbps, p.ext()), (Some(SoundCodec::Aac), Some(160), "m4a"));
        let args = build_args(Path::new("in.mp4"), Path::new("out.m4a"), &info(), &p, None, None);
        assert!(args.iter().any(|a| a == "-vn") && args.windows(2).any(|w| w == ["-map", "0:a:0"]));
        // Smallest that sounds good: Opus, nearly the same.
        let p = plan(&info(), &opts_for(SoundCodec::Opus), &Encoders::default()).unwrap();
        assert_eq!((p.audio_kbps, p.look), (Some(96), Look::NearlySame));
        // Lossless and For editing: no bitrate asked of the encoder; WAV about 10 MB a minute.
        for (sc, codec) in [(SoundCodec::Flac, "flac"), (SoundCodec::Wav, "pcm_s16le")] {
            let p = plan(&info(), &opts_for(sc), &Encoders::default()).unwrap();
            assert_eq!(p.look, Look::Same);
            let args = build_args(Path::new("in.mp4"), Path::new("out"), &info(), &p, None, None);
            assert!(args.windows(2).any(|w| w == ["-c:a", codec]));
            assert!(!args.iter().any(|a| a == "-b:a"));
        }
        let wav = plan(&song(), &opts_for(SoundCodec::Wav), &Encoders::default()).unwrap();
        let mb_a_minute = wav.sound_bytes(60.0).unwrap() as f64 / 1e6;
        assert!((10.0..11.0).contains(&mb_a_minute), "{mb_a_minute}");
        assert_eq!(wav.hint(), "WAV, nothing lost");
        // A video without its sound option is planned as a video.
        assert!(plan(&info(), &opts(Target::Quality(0.7)), &Encoders::default()).unwrap().sound.is_none());
    }

    #[test]
    fn joined_parts_one_graph() {
        let mut o = opts(Target::SizeMb(10.0));
        o.parts = vec![(10.0, 20.0), (60.0, 75.0)];
        o.even = Some(-14.0);
        let p = plan(&info(), &o, &Encoders::default()).unwrap();
        assert_eq!(o.length(&info()), 25.0);
        assert_eq!(p.even, Some(-14.0));
        let args = build_args(Path::new("in.mp4"), Path::new("out.mp4"), &info(), &p, None, None);
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("[0:V:0]trim=start=60.000:end=75.000"), "{graph}");
        assert!(graph.contains("concat=n=2:v=1:a=1[vj][aj]"), "{graph}");
        assert!(graph.contains("[aj]loudnorm=I=-14"), "{graph}");
        assert!(args.windows(2).any(|w| w == ["-map", "[vo]"]));
        assert!(!args.iter().any(|a| a == "0:V:0"));
        // The first of two passes: picture only.
        let p1 = build_args(Path::new("in.mp4"), Path::new("out.mp4"), &info(), &p, None, Some(1));
        let g1 = &p1[p1.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(g1.contains("concat=n=2:v=1:a=0[vj]") && !g1.contains("atrim"), "{g1}");
        // A sample of it is a plain range of the file.
        let s = build_args(Path::new("in.mp4"), Path::new("s.mp4"), &info(), &p, Some((60.0, 62.0)), None);
        assert!(!s.iter().any(|a| a == "-filter_complex"));
        assert!(s.iter().any(|a| a.starts_with("loudnorm=I=-14:TP=-1.5:LRA=11,aresample")));
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
        let first = build_args(Path::new("in.mp4"), Path::new("out.mp4"), &i, &p, None, Some(1));
        assert!(first.windows(2).any(|w| w[0] == "-pass:v" && w[1] == "1"));
        assert!(first.contains(&"-an".to_string()));
        let second = build_args(Path::new("in.mp4"), Path::new("out.mp4"), &i, &p, None, Some(2));
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

    #[test]
    fn hints_in_plain_words() {
        let e = Encoders::default();
        // 2 minutes of 1080p60 in 20 MB: downscaled and 30 fps.
        let p = plan(&info(), &opts(Goal::Discord.target()), &e).unwrap();
        assert!(p.fps_cut && p.height < 1080, "{p:?}");
        assert!(p.look >= Look::BitSofter);
        assert!(p.hint().starts_with(&format!("{}p 30 fps, ", p.height)), "{}", p.hint());
        // Plenty of room: the same picture.
        let p = plan(&info(), &opts(Target::SizeMb(500.0)), &e).unwrap();
        assert_eq!(p.look, Look::Same);
        assert_eq!(p.hint(), "1080p, looks the same");
        // Keeping goals: sharp keeps 1080p60, tiny goes to 720p30.
        let p = plan(&info(), &opts(Goal::Sharp.target()), &e).unwrap();
        assert_eq!(p.hint(), "1080p, looks the same");
        let p = plan(&info(), &opts(Goal::Tiny.target()), &e).unwrap();
        assert_eq!(p.hint(), "720p 30 fps, a bit softer");
    }

    #[test]
    fn a_shorter_part_gets_a_sharper_picture() {
        let e = Encoders::default();
        let whole = plan(&info(), &opts(Goal::Discord.target()), &e).unwrap();
        let mut o = opts(Goal::Discord.target());
        o.range = Some((10.0, 40.0));
        let part = plan(&info(), &o, &e).unwrap();
        assert!(part.video_kbps.unwrap() > whole.video_kbps.unwrap() * 3.0);
        assert!(part.height >= whole.height);
        let args = build_args(Path::new("in.mp4"), Path::new("out.mp4"), &info(), &part, o.range, None);
        let at = |k: &str| args.iter().position(|a| a == k).unwrap();
        assert_eq!(args[at("-ss") + 1], "10.000");
        assert_eq!(args[at("-t") + 1], "30.000");
        assert!(at("-ss") < at("-i"));
        o.range = Some((40.0, 40.0));
        assert!(plan(&info(), &o, &e).is_err());
    }

    #[test]
    fn mb_without_trailing_zero() {
        assert_eq!(fmt_mb(20.0), "20");
        assert_eq!(fmt_mb(2.5), "2.5");
        assert_eq!(Goal::Size(7.5).label(), "7.5 MB");
    }

    #[test]
    fn portrait_phone_video_stays_portrait() {
        // Stored 1920×1080, shown turned: 1080×1920.
        let mut i = info();
        i.rotation = 90;
        let p = plan(&i, &opts(Goal::Discord.target()), &Encoders::default()).unwrap();
        assert!(p.height > p.width, "{p:?}");
        assert!(p.hint().starts_with(&format!("{}p", p.width)));
        assert_eq!(p.lines, p.width);
    }

    #[test]
    fn black_bars_come_off_first() {
        let mut o = opts(Target::SizeMb(500.0));
        o.crop = Some(Bars { w: 1920, h: 800, x: 0, y: 140 });
        let p = plan(&info(), &o, &Encoders::default()).unwrap();
        assert_eq!((p.width, p.height), (1920, 800));
        assert_eq!(p.filters, vec!["crop=1920:800:0:140".to_string()]);
        // Bars: 140 px top and bottom count; 4 px don't; a mostly black frame isn't bars.
        assert!(bars_worth_cutting(1920, 1080, 0, 140, 1920, 940).is_some());
        assert!(bars_worth_cutting(1920, 1080, 0, 4, 1920, 1076).is_none());
        assert!(bars_worth_cutting(1920, 1080, 800, 400, 1100, 600).is_none());
    }

    #[test]
    fn keeps_every_track_it_can() {
        let mut i = info();
        i.audio_tracks = 2;
        i.subtitles = vec![true, false, true];
        let mut o = opts(Goal::Discord.target());
        o.keep_tracks = true;
        let p = plan(&i, &o, &Encoders::default()).unwrap();
        assert_eq!((p.audio_tracks, p.captions.clone(), p.captions_dropped), (2, vec![0, 2], 1));
        let args = build_args(Path::new("in.mkv"), Path::new("out.mp4"), &i, &p, None, None);
        let joined = args.join(" ");
        assert!(joined.contains("-map 0:a?") && joined.contains("-map 0:s:0") && joined.contains("-map 0:s:2"));
        assert!(joined.contains("-c:s mov_text"));
        // Two sound tracks share the sound budget.
        let one = plan(&i, &opts(Goal::Discord.target()), &Encoders::default()).unwrap();
        assert!(p.video_kbps.unwrap() <= one.video_kbps.unwrap());
        let args = build_args(Path::new("in.mkv"), Path::new("out.mp4"), &i, &one, None, None);
        assert!(args.join(" ").contains("-map 0:a:0? ") && args.join(" ").contains("-map_chapters -1"));
    }

    #[test]
    fn says_how_long_would_fit() {
        let mut i = info();
        i.duration = 3.0 * 3600.0;
        let secs = longest_fit(&i, &opts(Goal::Discord.target()), &Encoders::default()).unwrap();
        assert!(secs > 60.0 && secs < 600.0, "{secs}");
        let mut o = opts(Goal::Discord.target());
        o.range = Some((0.0, secs));
        let p = plan(&i, &o, &Encoders::default()).unwrap();
        assert!(p.look < Look::Blurry, "{p:?}");
        // Fits fine: nothing to say.
        assert!(longest_fit(&info(), &opts(Target::SizeMb(500.0)), &Encoders::default()).is_none());
    }
}
