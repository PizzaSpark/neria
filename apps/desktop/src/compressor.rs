//! Compress page (Compress track): goals in plain words (`compress::Goal`), what each will
//! look like for this file, before / after from a sample encode, several files at once,
//! trimming, and Advanced (format, graphics card, picture, sound, tracks, black bars).
//! The state the UI shows lives in the `Compress` global; this side owns the files.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use neria_core::compress::{self, Bars, Codec, Encoder, Encoders, Goal, Look, Options, Resolution, Target};
use neria_core::fmt;
use neria_core::media::{self, FileInfo};
use slint::{ComponentHandle, Image, ModelRc, SharedString, VecModel};

use crate::{AppWindow, CmpFile, CmpGoal, CmpPart, Compress};

const VIDEO_EXTENSIONS: &[&str] =
    &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "flv", "wmv", "mts", "m2ts", "3gp"];
/// Sound files go through the same goals (AAC or Opus, see `compress::plan`).
const SOUND_EXTENSIONS: &[&str] = &["mp3", "m4a", "aac", "wav", "flac", "opus", "ogg", "wma", "aiff", "aif"];
const GOALS: [Goal; 5] = [Goal::Discord, Goal::Email, Goal::WhatsApp, Goal::Sharp, Goal::Tiny];

// File states, as the UI reads them (CmpFile.state).
const WAITING: i32 = 0;
const RUNNING: i32 = 1;
const SAVED: i32 = 2;
const SKIPPED: i32 = 3;
const CANT: i32 = 4;
const FAILED: i32 = 5;
const CANCELLED: i32 = 6;

#[derive(Default)]
struct Item {
    path: PathBuf,
    info: Option<FileInfo>,
    error: Option<String>,
    thumb: Option<Image>,
    bars: Option<Bars>,
    state: i32,
    tag: String,
    detail: String,
    progress: f32,
    output: Option<PathBuf>,
}

#[derive(Default)]
pub struct Compressor {
    items: Vec<Item>,
    /// Detected once, in the background, the first time a file is opened.
    encoders: Arc<Mutex<Option<Encoders>>>,
    detecting: bool,
    /// Bumped when the list is cleared, so late reads of old files are dropped.
    list_gen: u64,
    /// The preview being made (key, generation); a newer one cancels it.
    preview_key: String,
    preview_gen: Arc<AtomicU64>,
    preview_cancel: Option<Arc<AtomicBool>>,
    /// Estimated sizes of the "keeping" goals, by preview key.
    estimates: HashMap<String, u64>,
    /// Preview keys whose size couldn't be worked out (shown blank, not "working…").
    no_estimate: std::collections::HashSet<String>,
    /// Trim strip frames made for this file.
    strip_for: Option<PathBuf>,
    /// Parts kept on the one file (Trim → Add this part), in time order.
    parts: Vec<(f64, f64)>,
    running: Option<Arc<AtomicBool>>,
    last_output: Option<PathBuf>,
    autostarted: bool,
    /// Files waiting to be read (`read_later`).
    reader: Option<std::sync::mpsc::Sender<(PathBuf, u64)>>,
}

thread_local! {
    static CMP: RefCell<Option<Rc<RefCell<Compressor>>>> = const { RefCell::new(None) };
}

/// Run `f` on the page's state from the UI thread (background work reports back this way).
fn with(f: impl FnOnce(&mut Compressor)) {
    if let Some(c) = CMP.with(|c| c.borrow().clone()) {
        f(&mut c.borrow_mut());
    }
}

fn goal_of(c: &Compress, index: i32) -> Result<Goal> {
    Ok(match index {
        i @ 0..=4 => GOALS[i as usize],
        _ => Goal::Size(
            c.get_custom_mb()
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .filter(|v| *v >= 0.5)
                .ok_or_else(|| anyhow!("Type a size in MB (at least 0.5)."))?,
        ),
    })
}

/// A video or a sound file (by its name: what a folder holds that's worth reading).
fn is_media(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        let e = e.to_ascii_lowercase();
        VIDEO_EXTENSIONS.contains(&e.as_str()) || SOUND_EXTENSIONS.contains(&e.as_str())
    })
}

/// Where a size goal is going, for sentences: "too long for WhatsApp".
fn place(goal: Goal) -> String {
    match goal {
        Goal::Discord => "Discord".into(),
        Goal::Email => "email".into(),
        Goal::WhatsApp => "WhatsApp".into(),
        Goal::Size(mb) => format!("{} MB", compress::fmt_mb(mb)),
        g => g.label(),
    }
}

/// A sound file's format chip (Advanced → Format): M4A, MP3 or Opus.
fn sound_codec_of(c: &Compress) -> compress::SoundCodec {
    match c.get_sound_format() {
        1 => compress::SoundCodec::Mp3,
        2 => compress::SoundCodec::Opus,
        _ => compress::SoundCodec::Aac,
    }
}

/// The goal's name for this file: a sound file is "still clear", not "still sharp".
fn goal_name(goal: Goal, info: &FileInfo) -> String {
    match goal {
        Goal::Sharp if !info.has_video => "Smaller, still clear".into(),
        g => g.label(),
    }
}

/// What a part of the file would weigh as it is: to tell "already fits".
fn part_bytes(info: &FileInfo, opts: &Options) -> u64 {
    (info.size_bytes as f64 * (opts.length(info) / info.duration.max(0.1)).min(1.0)) as u64
}

/// The file can be sent as it is: the whole of it (a trimmed part isn't a file yet), already
/// under `limit`, in a kind of file the apps play (an .mkv, .avi or .wma isn't, so it's
/// still made), and nothing else asked of it (evening out the sound).
fn fits_as_is(path: &Path, info: &FileInfo, opts: &Options, limit: u64) -> bool {
    if opts.even.is_some() {
        return false;
    }
    let playable = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["mp4", "mov", "m4v", "webm", "mp3", "m4a", "ogg", "opus", "wav", "flac"]
            .contains(&e.to_ascii_lowercase().as_str())
    });
    opts.whole() && playable && info.size_bytes <= limit
}

/// The file card's line: "1920×1080 · 60 fps · 2:14 · 312 MB · h264", or for a sound file
/// "30:00 · 317 MB · pcm_s16le · stereo".
fn info_line(info: &FileInfo) -> String {
    let mut parts = Vec::new();
    if info.has_video {
        parts.push(format!("{}×{}", info.width, info.height));
        if info.fps > 0.0 {
            parts.push(format!("{:.0} fps", info.fps));
        }
    }
    parts.push(fmt::duration(info.duration));
    parts.push(fmt::mb(info.size_bytes as f64));
    if info.has_video {
        if !info.video_codec.is_empty() {
            parts.push(info.video_codec.clone());
        }
    } else {
        if !info.audio_codec.is_empty() {
            parts.push(info.audio_codec.clone());
        }
        match info.audio_channels {
            1 => parts.push("mono".into()),
            2 => parts.push("stereo".into()),
            n if n > 2 => parts.push(format!("{n} channels")),
            _ => {}
        }
    }
    parts.join(" · ")
}

fn image(path: &Path) -> Option<Image> {
    Image::load_from_path(path).ok()
}

impl Compressor {
    fn encoders(&self) -> Encoders {
        self.encoders.lock().unwrap().clone().unwrap_or_default()
    }

    /// The options for `item` with `goal`, from the page.
    fn options(&self, c: &Compress, goal: Goal, item: &Item) -> Options {
        let mut o = Options::for_goal(goal);
        o.codec = match c.get_format() {
            1 => Codec::Hevc,
            2 => Codec::Av1,
            _ => Codec::H264,
        };
        o.use_gpu = c.get_use_gpu();
        o.resolution = match c.get_picture() {
            1 => Resolution::Original,
            2 => Resolution::Max(1080),
            3 => Resolution::Max(720),
            4 => Resolution::Max(480),
            _ => Resolution::Auto,
        };
        o.audio_kbps = match c.get_sound() {
            0 => Some(128),
            2 => Some(64),
            3 => None,
            _ => Some(96),
        };
        o.keep_tracks = c.get_keep_tracks();
        o.crop = if c.get_cut_bars() { item.bars } else { None };
        if c.get_volume() == 1 {
            o.even = Some(if c.get_loudness() == 1 { -16.0 } else { -14.0 });
        }
        // One sound file picks its own format; in a list, sound files follow Format (AAC for
        // Plays everywhere, else Opus).
        if self.items.len() == 1 && item.info.as_ref().is_some_and(|i| !i.has_video) {
            o.sound_codec = Some(sound_codec_of(c));
        }
        if self.items.len() == 1 && c.get_trim_open() && !self.parts.is_empty() {
            // Parts kept: one is a plain trim; joined, they're one file; one file each,
            // the longest stands for them in the hints (each file is planned at Start).
            match self.parts.len() {
                1 => o.range = Some(self.parts[0]),
                _ if c.get_split() == 0 => o.parts = self.parts.clone(),
                _ => o.range = self.parts.iter().copied().max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0))),
            }
            return o;
        }
        if self.items.len() == 1 && c.get_trim_open() {
            let (a, b) = (c.get_trim_in() as f64, c.get_trim_out() as f64);
            if let Some(info) = &item.info
                && (a > 0.05 || b < info.duration - 0.05)
            {
                o.range = Some((a, b));
            }
        }
        o
    }

    /// Two or more parts kept, saved as a file each.
    fn each_part(&self, c: &Compress) -> bool {
        self.items.len() == 1 && c.get_trim_open() && self.parts.len() > 1 && c.get_split() == 1
    }

    /// The parts list on the page, and what they add up to.
    fn show_parts(&self, ui: &AppWindow) {
        let c = ui.global::<Compress>();
        let rows: Vec<CmpPart> = self
            .parts
            .iter()
            .map(|&(a, b)| CmpPart {
                start: a as f32,
                end: b as f32,
                text: format!("{} → {}  ·  {}", fmt::duration(a), fmt::duration(b), fmt::duration(b - a)).into(),
            })
            .collect();
        c.set_parts(ModelRc::from(Rc::new(VecModel::from(rows))));
        let total: f64 = self.parts.iter().map(|(a, b)| b - a).sum();
        c.set_parts_text(
            match self.parts.len() {
                0 => String::new(),
                1 => format!("1 part · {}", fmt::duration(total)),
                n => format!("{n} parts · {} in all", fmt::duration(total)),
            }
            .into(),
        );
    }

    fn preview_key(&self, item: &Item, opts: &Options, goal: Goal) -> String {
        format!("{}|{goal:?}|{opts:?}", item.path.display())
    }

    // ── Files ──

    fn add(&mut self, ui: &AppWindow, paths: Vec<PathBuf>) {
        if self.running.is_some() {
            crate::show_toast(ui, "Wait for the files being compressed to finish, then add more.");
            return;
        }
        let mut found = Vec::new();
        for p in paths {
            if p.is_dir() {
                let mut inside: Vec<PathBuf> = std::fs::read_dir(&p)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|f| f.is_file() && is_media(f))
                    .collect();
                inside.sort();
                if inside.is_empty() {
                    crate::show_toast(ui, "There are no videos or sound files in that folder.");
                }
                found.extend(inside);
            } else {
                found.push(p);
            }
        }
        // Finished files go when new ones come: the list is what's about to be done.
        if self.items.iter().all(|i| i.state != WAITING) && !self.items.is_empty() && !found.is_empty() {
            self.items.retain(|i| i.state == WAITING);
        }
        // Parts belong to the file they were picked on.
        if !found.is_empty() {
            self.parts.clear();
            self.show_parts(ui);
        }
        let c = ui.global::<Compress>();
        for path in found {
            if self.items.iter().any(|i| i.path == path) {
                continue;
            }
            self.items.push(Item { path: path.clone(), ..Default::default() });
            self.read_later(ui, path);
        }
        if self.items.len() != 1 {
            c.set_trim_open(false);
        }
        c.set_status("".into());
        c.set_has_output(false);
        ui.set_page(2);
        self.detect_encoders(ui);
        self.refresh(ui);
    }

    /// Read a file (length, size, a thumbnail, black bars) in the background, three at a time:
    /// each read runs several ffmpeg processes, and a folder can hold hundreds of videos.
    fn read_later(&mut self, ui: &AppWindow, path: PathBuf) {
        let reader = self.reader.get_or_insert_with(|| {
            let (tx, rx) = std::sync::mpsc::channel::<(PathBuf, u64)>();
            let rx = Arc::new(Mutex::new(rx));
            for _ in 0..3 {
                let (rx, weak) = (rx.clone(), ui.as_weak());
                std::thread::spawn(move || {
                    loop {
                        let job = rx.lock().unwrap().recv();
                        let Ok((path, list_gen)) = job else { break };
                        let result = media::file_info(&path);
                        let (thumb, bars) = match &result {
                            Ok(i) if i.has_video => (
                                media::thumbnails(&path, i.duration * 0.6, 1, 180).into_iter().next(),
                                compress::detect_bars(&path, i),
                            ),
                            _ => (None, None),
                        };
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            with(|cmp| {
                                if cmp.list_gen != list_gen {
                                    return;
                                }
                                if let Some(it) = cmp.items.iter_mut().find(|i| i.path == path) {
                                    match result {
                                        Ok(info) if !info.has_video && !info.has_audio => {
                                            it.error = Some("This file has no picture or sound to compress.".into())
                                        }
                                        Ok(info) => it.info = Some(info),
                                        Err(e) => it.error = Some(e.to_string()),
                                    }
                                    it.thumb = thumb.as_deref().and_then(image);
                                    it.bars = bars;
                                }
                                cmp.refresh(&ui);
                            });
                        });
                    }
                });
            }
            tx
        });
        let _ = reader.send((path, self.list_gen));
    }

    fn detect_encoders(&mut self, ui: &AppWindow) {
        if self.detecting || self.encoders.lock().unwrap().is_some() {
            return;
        }
        self.detecting = true;
        let (slot, weak) = (self.encoders.clone(), ui.as_weak());
        std::thread::spawn(move || {
            let found = Encoders::detect();
            *slot.lock().unwrap() = Some(found);
            let _ = weak.upgrade_in_event_loop(|ui| with(|c| c.refresh(&ui)));
        });
    }

    fn clear(&mut self, ui: &AppWindow) {
        self.items.clear();
        self.list_gen += 1;
        self.strip_for = None;
        self.preview_key.clear();
        self.parts.clear();
        self.show_parts(ui);
        crate::cmp_player::stop(ui);
        let c = ui.global::<Compress>();
        c.set_trim_open(false);
        c.set_status("".into());
        c.set_has_output(false);
        self.refresh(ui);
    }

    // ── What the page shows ──

    fn refresh(&mut self, ui: &AppWindow) {
        let c = ui.global::<Compress>();
        let encoders = self.encoders.lock().unwrap().clone();
        let enc = encoders.clone().unwrap_or_default();
        let running = self.running.is_some();
        c.set_count(self.items.len() as i32);
        c.set_running(running);
        c.set_reading(self.items.iter().any(|i| i.info.is_none() && i.error.is_none()));

        // The graphics card tick: what this machine can do for the picked format.
        let codec = match c.get_format() {
            1 => Codec::Hevc,
            2 => Codec::Av1,
            _ => Codec::H264,
        };
        let gpu_ok = encoders.as_ref().is_some_and(|e| e.has_gpu_for(codec));
        c.set_gpu_available(gpu_ok);
        c.set_gpu_text(
            match (&encoders, enc.pick(codec, true)) {
                (None, _) => "Use my graphics card (checking it…)".to_string(),
                (Some(_), Encoder::Gpu(_, g)) => format!("Use my graphics card (faster) · {}", g.label()),
                _ => "Use my graphics card: it can't make this format, the processor will".to_string(),
            }
            .into(),
        );

        let goal = goal_of(&c, c.get_goal());
        // One sound file: Advanced is only its format.
        let sound_file = self.items.len() == 1 && self.items[0].info.as_ref().is_some_and(|i| !i.has_video);
        c.set_sound_file(sound_file);
        let format_names = ["Plays everywhere", "Smaller", "Smallest"];
        let picture_names = ["picture as needed", "picture kept", "1080p at most", "720p at most", "480p at most"];
        let sound_names = ["sound 128 kbps", "sound 96 kbps", "sound 64 kbps", "no sound"];
        let format = format_names[c.get_format().clamp(0, 2) as usize].to_string();
        let line = if sound_file {
            let sc = sound_codec_of(&c);
            let mut line = vec![format!("{} (.{})", sc.label(), sc.ext())];
            if c.get_volume() == 1 {
                line.push("evened out".into());
            }
            line
        } else {
            let mut line = vec![
                format,
                if c.get_use_gpu() && gpu_ok { "graphics card".into() } else { "processor".into() },
                picture_names[c.get_picture().clamp(0, 4) as usize].into(),
                sound_names[c.get_sound().clamp(0, 3) as usize].into(),
            ];
            if !c.get_keep_tracks() {
                line.push("first sound track only".into());
            }
            line
        };
        c.set_advanced_line(line.join(" · ").into());
        c.set_format_warning(
            match goal.as_ref().ok().and_then(|g| g.must_play_in()) {
                Some(place) if sound_file && sound_codec_of(&c) == compress::SoundCodec::Opus => format!(
                    "Opus may not play in {place} on every phone or computer. M4A or MP3 are the safe picks for sending."
                ),
                Some(_) if sound_file => String::new(),
                Some(place) if c.get_format() != 0 => format!(
                    "{} may not play in {place} on every phone or computer. Plays everywhere is the safe pick for sending.",
                    format_names[c.get_format() as usize]
                ),
                _ => String::new(),
            }
            .into(),
        );

        // Files list (several) and the one file's card.
        let files: Vec<CmpFile> = self
            .items
            .iter()
            .map(|it| {
                let meta = it.info.as_ref().map_or_else(
                    || if it.error.is_some() { String::new() } else { "Reading…".into() },
                    |i| format!("{} · {}", fmt::duration(i.duration), fmt::mb(i.size_bytes as f64)),
                );
                let (tag, detail) = if it.state != WAITING {
                    (it.tag.clone(), it.detail.clone())
                } else if let Some(e) = &it.error {
                    ("Can't".into(), e.clone())
                } else {
                    match (&it.info, &goal) {
                        (Some(info), Ok(g)) => self
                            .verdict_for(&c, *g, it, info, &enc)
                            .map_or_else(|(tag, why)| (tag, why), |hint| (String::new(), hint)),
                        _ => (String::new(), String::new()),
                    }
                };
                CmpFile {
                    name: it.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into(),
                    meta: meta.into(),
                    thumb: it.thumb.clone().unwrap_or_default(),
                    state: if it.state == WAITING && it.error.is_some() { CANT } else { it.state },
                    tag: tag.into(),
                    detail: detail.into(),
                    progress: it.progress,
                }
            })
            .collect();
        c.set_files(ModelRc::from(Rc::new(VecModel::from(files))));

        let one = (self.items.len() == 1).then(|| &self.items[0]);
        let mut goals = Vec::new();
        let (mut verdict, mut sizes, mut problem, mut fix, mut summary) =
            (String::new(), String::new(), String::new(), String::new(), String::new());
        let mut can_start = !running && encoders.is_some() && !self.items.is_empty();
        let mut start_text = match self.items.len() {
            0 | 1 => "Compress".to_string(),
            n => format!("Compress {n} files"),
        };

        match one {
            Some(it) => {
                c.set_name(it.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into());
                c.set_thumb(it.thumb.clone().unwrap_or_default());
                match (&it.info, &it.error) {
                    (_, Some(e)) => {
                        c.set_info(e.as_str().into());
                        can_start = false;
                    }
                    (None, _) => {
                        c.set_info("Reading the file…".into());
                        can_start = false;
                    }
                    (Some(info), _) => {
                        c.set_info(info_line(info).into());
                        c.set_duration(info.duration as f32);

                        // Every goal, worked out for this file.
                        for g in GOALS.iter() {
                            let opts = self.options(&c, *g, it);
                            let row = match self.verdict_for(&c, *g, it, info, &enc) {
                                Ok(hint) => {
                                    let size = match g.target() {
                                        Target::SizeMb(mb) => format!("{} MB", compress::fmt_mb(mb)),
                                        Target::Quality(_) => {
                                            self.estimates.get(&self.preview_key(it, &opts, *g)).map_or_else(
                                                || {
                                                    // Still being worked out (a few seconds per goal).
                                                    let key = self.preview_key(it, &opts, *g);
                                                    if self.no_estimate.contains(&key) {
                                                        String::new()
                                                    } else {
                                                        "working…".into()
                                                    }
                                                },
                                                |b| format!("about {}", fmt::mb(*b as f64)),
                                            )
                                        }
                                    };
                                    CmpGoal {
                                        name: goal_name(*g, info).into(),
                                        hint: hint.into(),
                                        size: size.into(),
                                        warn: false,
                                    }
                                }
                                Err((_, why)) => CmpGoal {
                                    name: goal_name(*g, info).into(),
                                    hint: why.into(),
                                    size: match g.target() {
                                        Target::SizeMb(mb) => format!("{} MB", compress::fmt_mb(mb)).into(),
                                        _ => self
                                            .estimates
                                            .get(&self.preview_key(it, &opts, *g))
                                            .map_or(String::new(), |b| format!("about {}", fmt::mb(*b as f64)))
                                            .into(),
                                    },
                                    warn: true,
                                },
                            };
                            goals.push(row);
                        }
                        let custom = goal_of(&c, 5);
                        goals.push(match &custom {
                            Ok(g) => {
                                let mb = format!("{} MB", c.get_custom_mb().trim());
                                match self.verdict_for(&c, *g, it, info, &enc) {
                                    Ok(hint) => CmpGoal {
                                        name: "Custom size".into(),
                                        hint: hint.into(),
                                        size: mb.into(),
                                        warn: false,
                                    },
                                    Err((_, why)) => CmpGoal {
                                        name: "Custom size".into(),
                                        hint: why.into(),
                                        size: mb.into(),
                                        warn: true,
                                    },
                                }
                            }
                            Err(_) => CmpGoal {
                                name: "Custom size".into(),
                                hint: "type a size".into(),
                                size: "".into(),
                                warn: false,
                            },
                        });

                        // The picked goal: verdict, sizes, problem, the technical line.
                        match &goal {
                            Err(e) => {
                                problem = e.to_string();
                                can_start = false;
                            }
                            Ok(g) => {
                                let opts = self.options(&c, *g, it);
                                let length = opts.length(info);
                                if self.each_part(&c) {
                                    start_text = format!("Compress {} parts", self.parts.len());
                                } else if !opts.whole() {
                                    start_text = format!("Compress {}", fmt::duration(length));
                                }
                                match compress::plan(info, &opts, &enc) {
                                    Err(e) => {
                                        problem = e.to_string();
                                        can_start = false;
                                    }
                                    Ok(plan) => {
                                        verdict = plan.hint();
                                        summary = plan.summary();
                                        if plan.captions_dropped > 0 {
                                            summary.push_str(&format!(
                                                "  ·  {} picture caption track{} left out (an .mp4 can't hold them)",
                                                plan.captions_dropped,
                                                if plan.captions_dropped == 1 { "" } else { "s" }
                                            ));
                                        }
                                        let now = part_bytes(info, &opts);
                                        let after = match (plan.sound_bytes(length), plan.target_bytes) {
                                            // Sound: its bitrate says the size, often well under the goal.
                                            (Some(b), _) => Some((b, "about ")),
                                            (None, Some(t)) => Some((t, "up to ")),
                                            (None, None) => self
                                                .estimates
                                                .get(&self.preview_key(it, &opts, *g))
                                                .map(|b| (*b, "about ")),
                                        };
                                        sizes = match after {
                                            Some((b, how)) => {
                                                let pct = 100.0 - b as f64 / now.max(1) as f64 * 100.0;
                                                let tail = if pct >= 1.0 {
                                                    format!(" · {pct:.0}% smaller")
                                                } else {
                                                    String::new()
                                                };
                                                // A size goal is the limit's own number ("up to 20 MB").
                                                let to = match plan.target_bytes {
                                                    Some(t) if plan.sound.is_none() => {
                                                        format!("{} MB", compress::fmt_mb(t as f64 / 1e6))
                                                    }
                                                    _ => fmt::mb(b as f64),
                                                };
                                                format!("{} → {how}{to}{tail}", fmt::mb(now as f64))
                                            }
                                            None => format!("{} → working out the size…", fmt::mb(now as f64)),
                                        };
                                        let est = self.estimates.get(&self.preview_key(it, &opts, *g)).copied();
                                        if let Some(t) = plan.target_bytes
                                            && fits_as_is(&it.path, info, &opts, t)
                                        {
                                            sizes = format!(
                                                "{} now: under {} MB already",
                                                fmt::mb(now as f64),
                                                compress::fmt_mb(t as f64 / 1e6)
                                            );
                                            problem = format!(
                                                "It's already small enough for {}: send it as it is. Compressing would only lose quality.",
                                                place(*g)
                                            );
                                            can_start = false;
                                            start_text = "Already fits".into();
                                        } else if let Some(s) = compress::longest_fit(info, &opts, &enc) {
                                            problem = format!(
                                                "Too long for {}: in {} MB it would be {}.",
                                                place(*g),
                                                compress::fmt_mb(plan.target_bytes.unwrap_or(0) as f64 / 1e6),
                                                plan.look_label()
                                            );
                                            fix = format!("Keep the first {}", fmt::duration(s));
                                        } else if let Some(b) = est
                                            && plan.target_bytes.is_none()
                                            && opts.even.is_none()
                                            && b as f64 >= now as f64 * 0.95
                                        {
                                            problem = format!(
                                                "This one won't get smaller with \"{}\": it's already packed tight. Pick a size, or As small as possible.",
                                                goal_name(*g, info)
                                            );
                                            can_start = false;
                                            start_text = "Won't get smaller".into();
                                        }
                                        if plan.sound.is_none() {
                                            c.set_after_label(format!("After · {}p", plan.lines).into());
                                        }
                                    }
                                }
                                if problem.is_empty()
                                    && let Some(s) = compress::longest_fit(info, &opts, &enc)
                                {
                                    problem = format!("{} is too long for {}.", fmt::duration(length), place(*g));
                                    fix = format!("Keep the first {}", fmt::duration(s));
                                }
                            }
                        }
                    }
                }
                // What gets kept, for the Advanced tick.
                let tracks = it.info.as_ref().map(|i| {
                    let mut t = Vec::new();
                    if i.audio_tracks > 1 {
                        t.push(format!("all {} sound tracks", i.audio_tracks));
                    }
                    let text = i.subtitles.iter().filter(|s| **s).count();
                    if text > 0 {
                        t.push(if text == 1 { "the captions".into() } else { format!("{text} caption tracks") });
                    }
                    if !i.chapters.is_empty() {
                        t.push("the chapters".into());
                    }
                    match t.len() {
                        0 => String::new(),
                        1 => t.remove(0),
                        _ => {
                            let last = t.pop().unwrap();
                            format!("{} and {last}", t.join(", "))
                        }
                    }
                });
                c.set_tracks_text(tracks.unwrap_or_default().into());
                c.set_bars_text(it.bars.map(|b| format!("{}×{} kept", b.w, b.h)).unwrap_or_default().into());
                // Chapters for the trim.
                let chapters: Vec<SharedString> = it
                    .info
                    .as_ref()
                    .map(|i| i.chapters.iter().map(|ch| SharedString::from(ch.title.as_str())).collect())
                    .unwrap_or_default();
                c.set_chapters(ModelRc::from(Rc::new(VecModel::from(chapters))));
            }
            None => {
                // Several (or none): one goal for all, each file says what it'll get.
                for g in GOALS {
                    let size = match g.target() {
                        Target::SizeMb(mb) => format!("{} MB each", compress::fmt_mb(mb)),
                        Target::Quality(_) => "the size follows".into(),
                    };
                    goals.push(CmpGoal { name: g.label().into(), hint: "".into(), size: size.into(), warn: false });
                }
                goals.push(CmpGoal {
                    name: "Custom size".into(),
                    hint: "".into(),
                    size: format!("{} MB each", c.get_custom_mb().trim()).into(),
                    warn: false,
                });
                c.set_tracks_text("all sound tracks, captions and chapters".into());
                c.set_bars_text("".into());
                if goal.is_err() {
                    problem = "Type a size in MB (at least 0.5).".into();
                    can_start = false;
                }
                // Count what will really be made: not the ones that fit already or can't.
                let todo = match &goal {
                    Ok(g) => self
                        .items
                        .iter()
                        .filter(|i| matches!(i.state, WAITING | CANCELLED | FAILED))
                        .filter(|i| {
                            i.info.as_ref().is_some_and(|info| match self.verdict_for(&c, *g, i, info, &enc) {
                                Ok(_) => true,
                                Err((tag, _)) => tag != "Already fits" && tag != "Can't fit",
                            })
                        })
                        .count(),
                    Err(_) => 0,
                };
                if !self.items.is_empty() && !running {
                    start_text = match todo {
                        0 => "Nothing to compress".into(),
                        1 => "Compress 1 file".into(),
                        n => format!("Compress {n} files"),
                    };
                    if todo == 0 {
                        can_start = false;
                    }
                }
                if self.items.iter().any(|i| i.info.is_none() && i.error.is_none()) {
                    can_start = false;
                }
            }
        }
        c.set_goals(ModelRc::from(Rc::new(VecModel::from(goals))));
        c.set_verdict(verdict.into());
        c.set_sizes(sizes.into());
        c.set_problem(problem.into());
        c.set_fix_text(fix.into());
        c.set_summary(summary.into());
        c.set_can_start(can_start);
        c.set_start_text(start_text.into());

        // Dev hook for scripted checks: start once everything is read and ready.
        if !self.autostarted && can_start && !c.get_reading() && std::env::var_os("NERIA_COMPRESS_START").is_some() {
            self.autostarted = true;
            self.start(ui);
            return;
        }
        self.schedule_preview(ui);
    }

    /// The hint for `goal` on this file, or (tag, why not).
    fn verdict_for(
        &self,
        c: &Compress,
        goal: Goal,
        it: &Item,
        info: &FileInfo,
        enc: &Encoders,
    ) -> std::result::Result<String, (String, String)> {
        let opts = self.options(c, goal, it);
        match compress::plan(info, &opts, enc) {
            Err(_) => Err(("Can't fit".into(), format!("too long for {}", place(goal)))),
            Ok(plan) => {
                let now = part_bytes(info, &opts);
                if let Some(t) = plan.target_bytes
                    && fits_as_is(&it.path, info, &opts, t)
                {
                    return Err((
                        "Already fits".into(),
                        format!("already under {} MB", compress::fmt_mb(t as f64 / 1e6)),
                    ));
                }
                if plan.look == Look::Blurry {
                    return Err(if plan.sound.is_some() {
                        ("Poor".into(), format!("{}: too long to sound OK", plan.hint()))
                    } else {
                        ("Blurry".into(), format!("{}: too long to look OK", plan.hint()))
                    });
                }
                // A look goal on a file that's already packed tight comes out no smaller.
                // (Evening out the sound is worth doing even then.)
                if plan.target_bytes.is_none()
                    && opts.even.is_none()
                    && let Some(b) = self.estimates.get(&self.preview_key(it, &opts, goal))
                    && *b as f64 >= now as f64 * 0.95
                {
                    return Err(("No smaller".into(), format!("{}: won't get smaller", plan.hint())));
                }
                Ok(plan.hint())
            }
        }
    }

    // ── Before / after ──

    /// A moment after the options settle, encode samples of the open file with them: the
    /// before / after frames (and the size, for the "keeping" goals). Never blocks Compress.
    fn schedule_preview(&mut self, ui: &AppWindow) {
        let c = ui.global::<Compress>();
        let Some(it) = (self.items.len() == 1).then(|| &self.items[0]) else { return };
        let (Some(info), Ok(goal)) = (it.info.clone(), goal_of(&c, c.get_goal())) else { return };
        if self.encoders.lock().unwrap().is_none() || self.running.is_some() {
            return;
        }
        let opts = self.options(&c, goal, it);
        let key = self.preview_key(it, &opts, goal);
        if key == self.preview_key {
            return;
        }
        self.preview_key = key.clone();
        // A sample made for other choices isn't this one any more.
        crate::cmp_player::stop_unless(ui, &key);
        if let Some(old) = self.preview_cancel.take() {
            old.store(true, Ordering::SeqCst);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.preview_cancel = Some(cancel.clone());
        let generation = self.preview_gen.fetch_add(1, Ordering::SeqCst) + 1;
        // The other "keeping" goals' sizes, while at it (they're in the goal list too).
        let others: Vec<(Goal, Options, String)> = GOALS
            .iter()
            .filter(|g| matches!(g.target(), Target::Quality(_)) && **g != goal)
            .map(|g| {
                let o = self.options(&c, *g, it);
                let k = self.preview_key(it, &o, *g);
                (*g, o, k)
            })
            .filter(|(_, _, k)| !self.estimates.contains_key(k))
            .collect();
        c.set_preview_busy(true);
        let (path, enc, gen_ref, weak) = (it.path.clone(), self.encoders(), self.preview_gen.clone(), ui.as_weak());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(350));
            if gen_ref.load(Ordering::SeqCst) != generation {
                return;
            }
            let name = format!("p{generation}");
            // Older previews' samples and pictures (their images are already shown or gone).
            let dir = neria_core::paths::cache_dir().join("compress-preview");
            for f in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                if !f.file_name().to_string_lossy().starts_with(&format!("{name}-")) {
                    let _ = std::fs::remove_file(f.path());
                }
            }
            let result = compress::preview(&path, &info, &opts, &enc, &name, 540, &cancel);
            let (w2, g2) = (weak.clone(), gen_ref.clone());
            let _ = weak.upgrade_in_event_loop(move |ui| {
                with(|cmp| {
                    let current = gen_ref.load(Ordering::SeqCst) == generation;
                    let c = ui.global::<Compress>();
                    match result {
                        Ok(p) => {
                            if p.estimated {
                                cmp.estimates.insert(key, p.bytes);
                            }
                            if current {
                                c.set_before(image(&p.before).unwrap_or_default());
                                c.set_after(image(&p.after).unwrap_or_default());
                            }
                        }
                        Err(_) if current => {
                            cmp.no_estimate.insert(key);
                            c.set_before(Image::default());
                            c.set_after(Image::default());
                        }
                        Err(_) => {}
                    }
                    if current {
                        c.set_preview_busy(false);
                        // The size may have arrived: show it (same key: no new preview).
                        cmp.refresh(&ui);
                    }
                });
            });
            // Then the other "keeping" goals' sizes, for the goal list.
            for (i, (_, o, k)) in others.into_iter().enumerate() {
                if cancel.load(Ordering::SeqCst) || g2.load(Ordering::SeqCst) != generation {
                    break;
                }
                let result = compress::preview(&path, &info, &o, &enc, &format!("{name}-e{i}"), 120, &cancel);
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let _ = w2.upgrade_in_event_loop(move |ui| {
                    with(|cmp| {
                        if let Ok(p) = result {
                            cmp.estimates.insert(k, p.bytes);
                        } else {
                            cmp.no_estimate.insert(k);
                        }
                        cmp.refresh(&ui);
                    })
                });
            }
        });
    }

    /// The player's button: play or pause the sample made for the page as it is now, or make
    /// it first (10 s from the middle of the part kept, like the before / after frames).
    fn play_pause(&mut self, ui: &AppWindow) {
        let c = ui.global::<Compress>();
        let Some(it) = (self.items.len() == 1).then(|| &self.items[0]) else { return };
        let (Some(info), Ok(goal)) = (it.info.clone(), goal_of(&c, c.get_goal())) else { return };
        let opts = self.options(&c, goal, it);
        let key = self.preview_key(it, &opts, goal);
        if crate::cmp_player::toggle(ui, &key) {
            return;
        }
        let enc = self.encoders();
        let Ok(plan) = compress::plan(&info, &opts, &enc) else { return };
        let (from, to) = compress::sample_span(&info, &opts);
        let at = from + ((to - from) - 10.0).max(0.0) / 2.0;
        let len = (to - from).min(10.0);
        c.set_player_busy(true);
        let (path, weak) = (it.path.clone(), ui.as_weak());
        let has_video = info.has_video;
        std::thread::spawn(move || {
            // Earlier samples go (one still open in a player stays until the next time).
            for f in std::fs::read_dir(neria_core::paths::cache_dir()).into_iter().flatten().flatten() {
                if f.file_name().to_string_lossy().starts_with("Neria sample ") {
                    let _ = std::fs::remove_file(f.path());
                }
            }
            let out = neria_core::paths::cache_dir().join(format!(
                "Neria sample {}.{}",
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                plan.ext()
            ));
            let result = compress::sample(&path, &info, &plan, at, len, &out, &AtomicBool::new(false));
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.global::<Compress>().set_player_busy(false);
                match result {
                    Ok(s) => crate::cmp_player::ready(
                        &ui,
                        crate::cmp_player::Sample { key, file: path, at, len: s.secs, after: s.path, has_video },
                    ),
                    Err(e) => crate::show_toast(&ui, &format!("Couldn't make a sample: {e}")),
                }
            });
        });
    }

    // ── Trim ──

    fn trim_opened(&mut self, ui: &AppWindow) {
        let c = ui.global::<Compress>();
        let Some(it) = self.items.first() else { return };
        let Some(info) = it.info.clone() else { return };
        if c.get_trim_out() <= c.get_trim_in() || c.get_trim_out() as f64 > info.duration + 0.01 {
            c.set_trim_in(0.0);
            c.set_trim_out(info.duration as f32);
        }
        if self.strip_for.as_ref() != Some(&it.path) {
            self.strip_for = Some(it.path.clone());
            c.set_strip(ModelRc::default());
            c.set_strip_wave(Image::default());
            let (path, weak) = (it.path.clone(), ui.as_weak());
            if !info.has_video {
                // A sound file: its waveform (the Clip page's sound levels, cached per file).
                std::thread::spawn(move || {
                    let Ok(peaks) = media::peaks(&path) else { return };
                    let view = neria_core::timeline::View::whole(info.duration.max(peaks.duration()));
                    let (w, h) = (1000, 88);
                    let px = peaks.render(view, w, h, crate::timeline_ui::WAVE_RGB);
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        // Still the file whose strip it is.
                        let mut still = false;
                        with(|c| still = c.strip_for.as_ref() == Some(&path));
                        if still {
                            ui.global::<Compress>().set_strip_wave(crate::timeline_ui::image(&px, w, h));
                        }
                    });
                });
                self.trim_text(ui);
                return;
            }
            std::thread::spawn(move || {
                let frames = media::thumbnails(&path, info.duration, 10, 72);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let images: Vec<Image> = frames.iter().filter_map(|p| image(p)).collect();
                    ui.global::<Compress>().set_strip(ModelRc::from(Rc::new(VecModel::from(images))));
                });
            });
        }
        self.trim_text(ui);
    }

    fn trim_text(&self, ui: &AppWindow) {
        let c = ui.global::<Compress>();
        let (a, b, d) = (c.get_trim_in() as f64, c.get_trim_out() as f64, c.get_duration() as f64);
        c.set_trim_text(
            format!("{} → {}  ·  {} of {}", fmt::duration(a), fmt::duration(b), fmt::duration(b - a), fmt::duration(d))
                .into(),
        );
    }

    // ── Compressing ──

    fn start(&mut self, ui: &AppWindow) {
        let c = ui.global::<Compress>();
        if self.running.is_some() || !c.get_can_start() {
            return;
        }
        let Ok(goal) = goal_of(&c, c.get_goal()) else { return };
        let enc = self.encoders();
        // Again: one file can be made again (another goal, say); in a list, the rows that
        // were cancelled, failed or skipped get another go (saved ones stay saved).
        let one = self.items.len() == 1;
        for it in &mut self.items {
            if one || matches!(it.state, CANCELLED | FAILED | SKIPPED | CANT) {
                it.state = WAITING;
                it.tag.clear();
                it.detail.clear();
                it.progress = 0.0;
            }
        }
        // Work out each file's options now; files that can't or needn't go say why.
        let mut plan = Vec::new();
        let each = self.each_part(&c);
        let parts = self.parts.clone();
        for i in 0..self.items.len() {
            let it = &self.items[i];
            if it.state != WAITING {
                continue;
            }
            let Some(info) = it.info.clone() else { continue };
            let path = it.path.clone();
            let opts = self.options(&c, goal, it);
            let verdict = self.verdict_for(&c, goal, it, &info, &enc);
            let it = &mut self.items[i];
            match verdict {
                // (One file that fits can't be started: the page says so instead.)
                Err((tag, why)) if tag == "Already fits" => {
                    it.state = SKIPPED;
                    it.tag = "Skipped".into();
                    it.detail = why;
                }
                Err((tag, why)) if tag == "Can't fit" => {
                    it.state = CANT;
                    it.tag = tag;
                    it.detail = why;
                }
                // One file each: a job per part, each fitting the goal on its own.
                _ if each => {
                    for (n, &part) in parts.iter().enumerate() {
                        let o = Options { range: Some(part), part_no: Some(n + 1), ..opts.clone() };
                        plan.push((i, path.clone(), info.clone(), o));
                    }
                }
                _ => plan.push((i, path, info, opts)),
            }
        }
        crate::cmp_player::stop(ui);
        if plan.is_empty() {
            self.refresh(ui);
            return;
        }
        // The preview's samples are no use while the encode runs.
        if let Some(p) = self.preview_cancel.take() {
            p.store(true, Ordering::SeqCst);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.running = Some(cancel.clone());
        c.set_running(true);
        c.set_progress(0.0);
        c.set_has_output(false);
        c.set_status("Starting…".into());
        let total = plan.len();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let started = Instant::now();
            let (mut saved, mut failed) = (0usize, 0usize);
            for (n, (i, path, info, opts)) in plan.into_iter().enumerate() {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let w = weak.clone();
                let _ = w.upgrade_in_event_loop(move |_| {
                    with(|cmp| {
                        if let Some(it) = cmp.items.get_mut(i) {
                            it.state = RUNNING;
                            it.tag = "Compressing".into();
                            it.progress = 0.0;
                        }
                    })
                });
                let mut last = Instant::now() - Duration::from_secs(1);
                let result = compress::run(&path, &info, &opts, &enc, &cancel, |p| {
                    if last.elapsed() >= Duration::from_millis(120) || p.fraction >= 1.0 {
                        last = Instant::now();
                        let stage = if total > 1 { format!("{} of {total}: {}", n + 1, p.stage) } else { p.stage };
                        let _ = w.upgrade_in_event_loop(move |ui| {
                            let c = ui.global::<Compress>();
                            c.set_progress((n as f32 + p.fraction) / total as f32);
                            c.set_status(stage.into());
                            with(|cmp| {
                                if let Some(it) = cmp.items.get_mut(i) {
                                    it.progress = p.fraction;
                                }
                                if cmp.items.len() > 1 {
                                    cmp.refresh_files_only(&ui);
                                }
                            });
                        });
                    }
                });
                let cancelled = cancel.load(Ordering::SeqCst);
                match &result {
                    Ok(_) => saved += 1,
                    Err(_) if !cancelled => failed += 1,
                    _ => {}
                }
                let _ = weak.upgrade_in_event_loop(move |_| {
                    with(|cmp| {
                        let Some(it) = cmp.items.get_mut(i) else { return };
                        match result {
                            Ok(out) => {
                                let before = part_bytes(&info, &opts);
                                let pct = 100.0 - out.bytes as f64 / before.max(1) as f64 * 100.0;
                                it.state = SAVED;
                                it.tag = format!("Saved · {}", fmt::mb(out.bytes as f64));
                                it.detail = format!("{} · {pct:.0}% smaller", out.plan.hint());
                                it.output = Some(out.path.clone());
                                cmp.last_output = Some(out.path);
                            }
                            Err(_) if cancelled => {
                                it.state = CANCELLED;
                                it.tag = "Cancelled".into();
                                it.detail.clear();
                            }
                            Err(e) => {
                                it.state = FAILED;
                                it.tag = "Failed".into();
                                it.detail = e.to_string();
                            }
                        }
                    })
                });
            }
            let secs = started.elapsed().as_secs_f32();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                with(|cmp| {
                    cmp.running = None;
                    let c = ui.global::<Compress>();
                    let one = cmp.items.len() == 1;
                    let status = if one && saved > 1 {
                        // One file, its parts saved a file each (next to it).
                        let mut s = format!("{saved} parts saved");
                        if failed > 0 {
                            s.push_str(&format!(" · {failed} failed"));
                        }
                        format!("{s} · {secs:.0}s")
                    } else if one {
                        match cmp.items.first() {
                            Some(it) if it.state == SAVED => format!(
                                "Saved · {} ({}) in {secs:.0}s",
                                it.output
                                    .as_ref()
                                    .and_then(|p| p.file_name())
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                                it.tag.trim_start_matches("Saved · ")
                            ),
                            Some(it) if it.state == CANCELLED => "Cancelled.".into(),
                            Some(it) => it.detail.clone(),
                            None => String::new(),
                        }
                    } else {
                        let mut s = format!("{saved} saved");
                        let skipped = cmp.items.iter().filter(|i| i.state == SKIPPED).count();
                        if skipped > 0 {
                            s.push_str(&format!(" · {skipped} already fit"));
                        }
                        if failed > 0 {
                            s.push_str(&format!(" · {failed} failed"));
                        }
                        if cmp.items.iter().any(|i| i.state == CANCELLED) {
                            s.push_str(" · cancelled");
                        }
                        format!("{s} · {secs:.0}s")
                    };
                    c.set_status(status.into());
                    c.set_has_output(one && cmp.items.first().is_some_and(|i| i.state == SAVED));
                    let note = if failed > 0 && saved == 0 {
                        crate::background_ui::Note {
                            title: "Couldn't compress it".into(),
                            body: cmp
                                .items
                                .iter()
                                .find(|i| i.state == FAILED)
                                .map(|i| i.detail.clone())
                                .unwrap_or_default(),
                            image: None,
                            file: None,
                            failed: true,
                        }
                    } else {
                        crate::background_ui::Note {
                            title: if saved > 1 { format!("Compressed {saved} files") } else { "Compressed".into() },
                            body: c.get_status().to_string(),
                            image: None,
                            file: cmp.last_output.clone(),
                            failed: false,
                        }
                    };
                    if saved > 0 || failed > 0 {
                        crate::background_ui::notify(&ui, note);
                    }
                    // Fresh preview next time the options change.
                    cmp.preview_key.clear();
                    cmp.refresh(&ui);
                })
            });
        });
        self.refresh(ui);
    }

    /// While a list runs: just the rows (the meters), not every goal again.
    fn refresh_files_only(&self, ui: &AppWindow) {
        let c = ui.global::<Compress>();
        let model = c.get_files();
        for (i, it) in self.items.iter().enumerate() {
            if let Some(mut row) = slint::Model::row_data(&model, i) {
                row.state = it.state;
                row.tag = it.tag.clone().into();
                row.progress = it.progress;
                slint::Model::set_row_data(&model, i, row);
            }
        }
    }
}

pub fn install(ui: &AppWindow, file_for_job: impl Fn(u64) -> Option<PathBuf> + 'static) -> Rc<RefCell<Compressor>> {
    let cmp = Rc::new(RefCell::new(Compressor::default()));
    CMP.with(|c| *c.borrow_mut() = Some(cmp.clone()));
    let c = ui.global::<Compress>();

    c.on_open_file({
        let weak = ui.as_weak();
        move || {
            if let Some(paths) = rfd::FileDialog::new()
                .add_filter("Videos and sound", &[VIDEO_EXTENSIONS, SOUND_EXTENSIONS].concat())
                .add_filter("Videos", VIDEO_EXTENSIONS)
                .add_filter("Sound", SOUND_EXTENSIONS)
                .add_filter("All files", &["*"])
                .pick_files()
            {
                let ui = weak.unwrap();
                with(|c| c.add(&ui, paths));
            }
        }
    });
    c.on_open_folder({
        let weak = ui.as_weak();
        move || {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                let ui = weak.unwrap();
                with(|c| c.add(&ui, vec![dir]));
            }
        }
    });
    c.on_open_path({
        let weak = ui.as_weak();
        move |path| {
            let ui = weak.unwrap();
            with(|c| c.add(&ui, vec![PathBuf::from(path.as_str())]));
        }
    });
    ui.on_cmp_job({
        let weak = ui.as_weak();
        move |id| {
            if let Some(path) = file_for_job(id as u64).filter(|p| p.is_file()) {
                let ui = weak.unwrap();
                with(|c| c.add(&ui, vec![path]));
            }
        }
    });
    c.on_remove({
        let weak = ui.as_weak();
        move |i| {
            let ui = weak.unwrap();
            with(|c| {
                if (i as usize) < c.items.len() {
                    c.items.remove(i as usize);
                    c.preview_key.clear();
                    c.parts.clear();
                    c.show_parts(&ui);
                    crate::cmp_player::stop(&ui);
                }
                c.refresh(&ui);
            });
        }
    });
    c.on_clear({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            with(|c| c.clear(&ui));
        }
    });
    c.on_changed({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            with(|c| {
                // Another goal: rows skipped or refused for the old one say what they'd get now.
                if c.running.is_none() {
                    for it in &mut c.items {
                        if matches!(it.state, SKIPPED | CANT) {
                            it.state = WAITING;
                            it.tag.clear();
                            it.detail.clear();
                        }
                    }
                }
                c.refresh(&ui)
            });
        }
    });
    c.on_fix({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            with(|c| {
                let g = ui.global::<Compress>();
                let Some(it) = c.items.first() else { return };
                let (Some(info), Ok(goal)) = (it.info.clone(), goal_of(&g, g.get_goal())) else { return };
                let mut opts = c.options(&g, goal, it);
                opts.range = None;
                if let Some(secs) = compress::longest_fit(&info, &opts, &c.encoders()) {
                    g.set_trim_open(true);
                    c.trim_opened(&ui);
                    g.set_trim_in(0.0);
                    g.set_trim_out(secs as f32);
                    g.set_chapter(-1);
                    c.trim_text(&ui);
                }
                c.refresh(&ui);
            });
        }
    });
    c.on_play_pause({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            with(|c| c.play_pause(&ui));
        }
    });
    c.on_hear({
        let weak = ui.as_weak();
        move || crate::cmp_player::switch(&weak.unwrap())
    });
    c.on_add_part({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            with(|c| {
                let g = ui.global::<Compress>();
                let (a, b) = (g.get_trim_in() as f64, g.get_trim_out() as f64);
                if b - a < 0.5 {
                    crate::show_toast(&ui, "That part is too short: move Out further from In.");
                    return;
                }
                if c.parts.iter().any(|&(x, y)| a < y - 0.05 && b > x + 0.05) {
                    crate::show_toast(&ui, "That part overlaps one you kept. Move it, or take the other one off.");
                    return;
                }
                // Kept in time order (joined that way too).
                c.parts.push((a, b));
                c.parts.sort_by(|x, y| x.0.total_cmp(&y.0));
                c.show_parts(&ui);
                c.refresh(&ui);
            });
        }
    });
    c.on_remove_part({
        let weak = ui.as_weak();
        move |i| {
            let ui = weak.unwrap();
            with(|c| {
                if (i as usize) < c.parts.len() {
                    c.parts.remove(i as usize);
                }
                c.show_parts(&ui);
                c.refresh(&ui);
            });
        }
    });
    c.on_trim_changed({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            with(|c| {
                let g = ui.global::<Compress>();
                if g.get_trim_open() {
                    c.trim_opened(&ui);
                }
                g.set_chapter(-1);
                c.trim_text(&ui);
                c.refresh(&ui);
            });
        }
    });
    c.on_trim_whole({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            with(|c| {
                let g = ui.global::<Compress>();
                g.set_trim_in(0.0);
                g.set_trim_out(g.get_duration());
                g.set_chapter(-1);
                c.trim_text(&ui);
                c.refresh(&ui);
            });
        }
    });
    c.on_trim_chapter({
        let weak = ui.as_weak();
        move |i| {
            let ui = weak.unwrap();
            with(|c| {
                let g = ui.global::<Compress>();
                if let Some(ch) =
                    c.items.first().and_then(|it| it.info.as_ref()).and_then(|info| info.chapters.get(i as usize))
                {
                    g.set_trim_in(ch.start as f32);
                    g.set_trim_out(ch.end as f32);
                }
                c.trim_text(&ui);
                c.refresh(&ui);
            });
        }
    });
    c.on_start({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            with(|c| c.start(&ui));
        }
    });
    c.on_cancel(|| {
        with(|c| {
            if let Some(x) = &c.running {
                x.store(true, Ordering::SeqCst);
            }
        })
    });
    c.on_show(|i| {
        with(|c| {
            let path =
                if i < 0 { c.last_output.clone() } else { c.items.get(i as usize).and_then(|it| it.output.clone()) };
            if let Some(p) = path {
                crate::reveal(&p);
            }
        })
    });

    cmp
}

/// Dev hooks for scripted checks (see CLAUDE.md): `NERIA_COMPRESS` (files or folders,
/// separated by `;`), `NERIA_COMPRESS_GOAL` (`discord`, `email`, `whatsapp`, `sharp`, `tiny`
/// or a size in MB), `NERIA_COMPRESS_TRIM` (`in,out` seconds), `NERIA_COMPRESS_ADVANCED`
/// (open the fold).
pub fn dev_hooks(ui: &AppWindow) {
    let c = ui.global::<Compress>();
    if let Ok(goal) = std::env::var("NERIA_COMPRESS_GOAL") {
        let names = ["discord", "email", "whatsapp", "sharp", "tiny"];
        match names.iter().position(|n| goal.eq_ignore_ascii_case(n)) {
            Some(i) => c.set_goal(i as i32),
            None => {
                c.set_goal(5);
                c.set_custom_mb(goal.into());
            }
        }
    }
    if std::env::var_os("NERIA_COMPRESS_ADVANCED").is_some() {
        c.set_advanced_open(true);
    }
    if let Ok(v) = std::env::var("NERIA_COMPRESS_EVEN") {
        c.set_volume(1);
        c.set_loudness(if v.trim() == "16" || v.trim() == "-16" { 1 } else { 0 });
    }
    if std::env::var_os("NERIA_COMPRESS_EACH").is_some() {
        c.set_split(1);
    }
    // A sound file's format: `m4a`, `mp3` or `opus`.
    if let Ok(f) = std::env::var("NERIA_COMPRESS_FORMAT") {
        c.set_sound_format(match f.trim().to_ascii_lowercase().as_str() {
            "mp3" => 1,
            "opus" => 2,
            _ => 0,
        });
    }
    if let Some(v) = std::env::var_os("NERIA_COMPRESS") {
        let paths: Vec<PathBuf> =
            v.to_string_lossy().split(';').filter(|s| !s.trim().is_empty()).map(PathBuf::from).collect();
        with(|cmp| cmp.add(ui, paths));
    }
    // `NERIA_COMPRESS_PARTS=10-20,30-40`: those parts kept (Trim open on the last).
    let parts: Vec<(f64, f64)> = std::env::var("NERIA_COMPRESS_PARTS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|p| p.split_once('-'))
        .filter_map(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
        .collect();
    let trim = std::env::var("NERIA_COMPRESS_TRIM")
        .ok()
        .and_then(|t| t.split_once(',').map(|(a, b)| (a.trim().parse::<f32>(), b.trim().parse::<f32>())))
        .and_then(|(a, b)| Some((a.ok()?, b.ok()?)))
        .or_else(|| parts.last().map(|&(a, b)| (a as f32, b as f32)));
    let play = std::env::var_os("NERIA_COMPRESS_PLAY").is_some();
    if trim.is_some() || play {
        let (a, b) = trim.unwrap_or((-1.0, -1.0));
        // Once the file is read (its length), so the strip and the hints use it.
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            for _ in 0..100 {
                std::thread::sleep(Duration::from_millis(200));
                let done = Arc::new(AtomicBool::new(false));
                let d = done.clone();
                let parts = parts.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    with(|cmp| {
                        if cmp.items.first().is_some_and(|i| i.info.is_some()) {
                            let c = ui.global::<Compress>();
                            if a >= 0.0 {
                                c.set_trim_open(true);
                                cmp.trim_opened(&ui);
                                c.set_trim_in(a);
                                c.set_trim_out(b);
                                cmp.trim_text(&ui);
                            }
                            if !parts.is_empty() {
                                cmp.parts = parts;
                                cmp.show_parts(&ui);
                            }
                            cmp.refresh(&ui);
                            // NERIA_COMPRESS_PLAY: press the player's button (makes the sample).
                            if play {
                                cmp.play_pause(&ui);
                            }
                            d.store(true, Ordering::SeqCst);
                        }
                    })
                });
                std::thread::sleep(Duration::from_millis(50));
                if done.load(Ordering::SeqCst) {
                    break;
                }
            }
        });
    }
}
