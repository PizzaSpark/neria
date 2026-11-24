//! Saved workflows: what happens to a file after it's saved. One change (made smaller, its
//! sound only, a GIF, or left as it is), then one place (next to the original, a folder, or
//! in the original's place). Kept in `workflows.json` next to the exe.
//!
//! A workflow runs when it's picked (the Download page's Then button, the Library's Run a
//! workflow…, the palette), or by itself: for every download, for links from some sites, for
//! new uploads of followed channels, or for files put in a watched folder. The more specific
//! wins (followed > sites > every download); a file never gets two.
//!
//! It runs in the download's own queue row, after the download and its other steps; a step
//! that fails leaves the download saved, with a note. The original stays unless the workflow
//! says "Replace the original" (it then goes to the Recycle Bin).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::compress::{self, Encoders, Goal, SoundCodec};
use crate::{clip, gif, library, media, paths};

/// The size goals Make it smaller offers (Compress's sending goals).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    Discord,
    Email,
    WhatsApp,
    Mb(f64),
}

impl Size {
    pub fn goal(self) -> Goal {
        match self {
            Size::Discord => Goal::Discord,
            Size::Email => Goal::Email,
            Size::WhatsApp => Goal::WhatsApp,
            Size::Mb(mb) => Goal::Size(mb),
        }
    }

    pub fn mb(self) -> f64 {
        match self.goal().target() {
            compress::Target::SizeMb(mb) => mb,
            compress::Target::Quality(_) => 0.0,
        }
    }

    /// "for Discord", "for email", "of 50 MB".
    fn words(self) -> String {
        match self {
            Size::Discord => "for Discord".into(),
            Size::Email => "for email".into(),
            Size::WhatsApp => "for WhatsApp".into(),
            Size::Mb(mb) => format!("to {} MB", compress::fmt_mb(mb)),
        }
    }
}

/// Sound only's formats, as Compress names them.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SoundAs {
    Mp3,
    M4a,
    Opus,
    Flac,
    Wav,
}

impl SoundAs {
    pub const ALL: [SoundAs; 5] = [SoundAs::Mp3, SoundAs::M4a, SoundAs::Opus, SoundAs::Flac, SoundAs::Wav];

    pub fn codec(self) -> SoundCodec {
        match self {
            SoundAs::Mp3 => SoundCodec::Mp3,
            SoundAs::M4a => SoundCodec::Aac,
            SoundAs::Opus => SoundCodec::Opus,
            SoundAs::Flac => SoundCodec::Flac,
            SoundAs::Wav => SoundCodec::Wav,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SoundAs::Mp3 => "MP3",
            SoundAs::M4a => "M4A",
            SoundAs::Opus => "Opus",
            SoundAs::Flac => "FLAC",
            SoundAs::Wav => "WAV",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(SoundAs::Mp3)
    }
}

/// What it does to the file.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// Left as it is (only moved).
    Keep,
    Smaller(Size),
    Sound(SoundAs),
    /// A GIF of a short video (a minute at most).
    Gif,
}

/// Where the result goes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Put {
    /// Next to the original (a watched folder's: its Done folder).
    Beside,
    /// In this folder (the original itself, when it's left as it is).
    Folder(PathBuf),
    /// In the original's place; the original goes to the Recycle Bin.
    Replace,
}

/// When it runs without being picked.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// Only when picked (Then, the Library, the palette).
    Picked,
    /// Every download.
    Every,
    /// Links from these sites ("TikTok", as `library::site_of` names them).
    Sites(Vec<String>),
    /// New uploads from followed channels and playlists.
    Followed,
    /// Files put in this folder.
    Folder(PathBuf),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Workflow {
    pub id: u64,
    pub name: String,
    pub change: Change,
    pub put: Put,
    pub when: When,
    /// An automatic one can be paused (Settings → Workflows' switch).
    pub on: bool,
    /// A watched folder's files already done or there when it started watching
    /// (`name|size|modified`), so each is done once.
    pub seen: Vec<String>,
    /// Files a watched folder has done today: (day number, count), for its row.
    pub done_today: (u64, u32),
}

impl Default for Workflow {
    fn default() -> Self {
        Workflow {
            id: 0,
            name: String::new(),
            change: Change::Smaller(Size::Discord),
            put: Put::Beside,
            when: When::Picked,
            on: true,
            seen: Vec::new(),
            done_today: (0, 0),
        }
    }
}

/// The sites "For links from" offers, in this order.
pub const SITES: &[&str] =
    &["YouTube", "TikTok", "Instagram", "X", "Twitch", "Reddit", "Facebook", "Vimeo", "SoundCloud", "Bluesky"];

impl Workflow {
    /// Runs by itself (not just when picked), and isn't paused.
    pub fn automatic(&self) -> bool {
        self.on && self.when != When::Picked
    }

    /// Does nothing at all (left as it is, put next to itself or in its own place).
    pub fn is_empty(&self) -> bool {
        self.change == Change::Keep && !matches!(self.put, Put::Folder(_))
    }

    /// What it makes, in a few words: "A copy for Discord (under 20 MB)", "Moved to D:\Clips".
    pub fn what(&self) -> String {
        let made = match self.change {
            Change::Keep => {
                return match &self.put {
                    Put::Folder(dir) => format!("Moved to {}", dir.display()),
                    _ => "Left as it is".into(),
                };
            }
            Change::Smaller(Size::Mb(mb)) => format!("A copy under {} MB", compress::fmt_mb(mb)),
            Change::Smaller(size) => {
                format!("A copy {} (under {} MB)", size.words(), compress::fmt_mb(size.mb()))
            }
            Change::Sound(s) => format!("Its sound as {}", s.label()),
            Change::Gif => "A GIF of it".into(),
        };
        let place = match (&self.put, &self.when) {
            (Put::Beside, When::Folder(_)) => "in the Done folder".to_string(),
            (Put::Beside, _) => "next to the original".to_string(),
            (Put::Folder(dir), _) => format!("in {}", dir.display()),
            (Put::Replace, _) => "in place of the original".to_string(),
        };
        format!("{made}, {place}")
    }

    /// When, in a few words: "When I pick it", "TikTok links", "Files in D:\Drop".
    pub fn when_label(&self) -> String {
        match &self.when {
            When::Picked => "When I pick it".into(),
            When::Every => "Every download".into(),
            When::Sites(s) if s.is_empty() => "No sites picked".into(),
            When::Sites(s) => format!("{} links", join_and(s)),
            When::Followed => "Channels I follow".into(),
            When::Folder(_) => "Folder".into(),
        }
    }

    /// The whole thing said back in a sentence (the editor's last line).
    pub fn sentence(&self) -> String {
        let who = match &self.when {
            When::Picked => "When you pick it".to_string(),
            When::Every => "Every download".to_string(),
            When::Sites(s) if s.is_empty() => "Links from the sites you tick".to_string(),
            When::Sites(s) => format!("{} links", join_and(s)),
            When::Followed => "New uploads from channels you follow".to_string(),
            When::Folder(dir) if dir.as_os_str().is_empty() => "Files put in the folder you pick".to_string(),
            When::Folder(dir) => format!("Files put in {}", dir.display()),
        };
        let mut what = self.what();
        if let Some(first) = what.get(..1) {
            what = first.to_lowercase() + &what[1..];
        }
        let mut s = format!("{who}: {what}.");
        if self.put == Put::Replace && self.change != Change::Keep {
            s.push_str(" The original goes to the Recycle Bin.");
        }
        s
    }
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

// ---------------------------------------------------------------------------------------
// The list

#[derive(Serialize, Deserialize, Default)]
struct Store {
    next_id: u64,
    items: Vec<Workflow>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn file() -> PathBuf {
    paths::app_dir().join("workflows.json")
}

/// The three to start with: Discord-ready, Sound only, Into a folder.
fn starters() -> Store {
    let into = folders_default().unwrap_or_default();
    let items = vec![
        Workflow { id: 1, name: "Discord-ready".into(), ..Workflow::default() },
        Workflow { id: 2, name: "Sound only".into(), change: Change::Sound(SoundAs::Mp3), ..Workflow::default() },
        Workflow { id: 3, name: "Into a folder".into(), change: Change::Keep, put: Put::Folder(into), ..Workflow::default() },
    ];
    Store { next_id: 4, items }
}

/// "Into a folder"'s folder until it's changed: Videos\Neria\Sorted.
fn folders_default() -> Option<PathBuf> {
    dirs::video_dir().map(|d| d.join("Neria").join("Sorted"))
}

fn load_store() -> Store {
    match std::fs::read(file()) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
        Err(_) => starters(),
    }
}

fn store(s: &Store) {
    if let Ok(json) = serde_json::to_vec_pretty(s) {
        let tmp = file().with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, file());
        }
    }
}

fn edit<T>(f: impl FnOnce(&mut Store) -> T) -> T {
    let _guard = LOCK.lock().unwrap();
    let mut s = load_store();
    let out = f(&mut s);
    store(&s);
    out
}

/// Every workflow, in the order they're listed.
pub fn list() -> Vec<Workflow> {
    let _guard = LOCK.lock().unwrap();
    load_store().items
}

pub fn get(id: u64) -> Option<Workflow> {
    list().into_iter().find(|w| w.id == id)
}

/// Save a new one (id 0) or changes to one; returns its id. A watched folder that's new or
/// changed starts from what's in it now (those files are left alone).
pub fn save(mut wf: Workflow) -> u64 {
    edit(|s| {
        let old = s.items.iter().find(|w| w.id == wf.id && wf.id != 0).cloned();
        let folder_changed = match (&wf.when, old.as_ref().map(|o| &o.when)) {
            (When::Folder(a), Some(When::Folder(b))) => a != b,
            (When::Folder(_), _) => true,
            _ => false,
        };
        if folder_changed {
            wf.seen = match &wf.when {
                When::Folder(dir) => waiting_in(dir).iter().map(|p| seen_key(p)).flatten().collect(),
                _ => Vec::new(),
            };
        } else if let Some(o) = &old {
            wf.seen = o.seen.clone();
            wf.done_today = o.done_today;
        }
        if !matches!(wf.when, When::Folder(_)) {
            wf.seen.clear();
        }
        match s.items.iter_mut().find(|w| w.id == wf.id && wf.id != 0) {
            Some(w) => *w = wf.clone(),
            None => {
                s.next_id = s.next_id.max(s.items.iter().map(|w| w.id).max().unwrap_or(0) + 1).max(1);
                wf.id = s.next_id;
                s.next_id += 1;
                s.items.push(wf.clone());
            }
        }
        wf.id
    })
}

pub fn delete(id: u64) {
    edit(|s| s.items.retain(|w| w.id != id));
}

/// Pause or resume an automatic one.
pub fn set_on(id: u64, on: bool) {
    edit(|s| {
        if let Some(w) = s.items.iter_mut().find(|w| w.id == id) {
            w.on = on;
        }
    });
}

/// A watched folder's files already there: none of them are skipped from now on (the
/// editor's "do them too").
pub fn forget_seen(id: u64) {
    edit(|s| {
        if let Some(w) = s.items.iter_mut().find(|w| w.id == id) {
            w.seen.clear();
        }
    });
}

// ---------------------------------------------------------------------------------------
// Which one runs

/// What a download gets: the one picked (Then), none, or the automatic one for its link.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    /// Not said: the automatic one for its link, if any (decided when it's queued).
    #[default]
    Auto,
    Nothing,
    Use(Box<Workflow>),
}

impl Pick {
    pub fn workflow(&self) -> Option<&Workflow> {
        match self {
            Pick::Use(w) => Some(w),
            _ => None,
        }
    }
}

/// The automatic one for a link: one for its site, else one for every download.
pub fn auto_for(url: &str) -> Option<Workflow> {
    let all = list();
    let site = library::site_of(url);
    let on = |w: &&Workflow| w.on && !w.is_empty();
    all.iter()
        .filter(on)
        .find(|w| matches!(&w.when, When::Sites(s) if s.iter().any(|x| x.eq_ignore_ascii_case(&site))))
        .or_else(|| all.iter().filter(on).find(|w| w.when == When::Every))
        .cloned()
}

/// A followed channel's new upload: the one for followed channels, else as any link.
pub fn for_followed(url: &str) -> Option<Workflow> {
    list().into_iter().find(|w| w.on && !w.is_empty() && w.when == When::Followed).or_else(|| auto_for(url))
}

/// Why an automatic one was picked, for the Then menu ("For TikTok links").
pub fn why(wf: &Workflow) -> String {
    match &wf.when {
        When::Every => "For every download".into(),
        When::Sites(s) => format!("For {} links", join_and(s)),
        When::Followed => "For channels I follow".into(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------------------
// Running one

/// What a run left behind.
#[derive(Clone, Debug, PartialEq)]
pub struct Done {
    /// The file to show: the new one, or the original where it now is.
    pub file: PathBuf,
    /// A new file was made (else the original was only moved, or nothing was needed).
    pub made: bool,
    /// The original went to the Recycle Bin (`file` is in its place).
    pub replaced: bool,
    /// The original, moved here (left as it is, put in a folder).
    pub moved: Option<PathBuf>,
    /// For the queue row: "a copy, 9.6 MB", "already under 20 MB".
    pub said: String,
    pub bytes: u64,
    /// The Library's kind for a new file.
    pub kind: library::Kind,
}

fn encoders() -> &'static Encoders {
    static E: OnceLock<Encoders> = OnceLock::new();
    E.get_or_init(Encoders::detect)
}

/// Longest video Make a GIF takes in a workflow (seconds).
pub const GIF_MAX: f64 = 60.0;

/// Run `wf` on `file`. `done_dir`: a watched folder's Done folder (instead of next to the
/// original). `on` gets 0..1 and what it's doing ("Making it smaller for Discord…").
pub fn run(
    wf: &Workflow,
    file: &Path,
    done_dir: Option<&Path>,
    cancel: &AtomicBool,
    mut on: impl FnMut(f32, &str),
) -> Result<Done> {
    if !file.is_file() {
        bail!("the file isn't there any more");
    }
    let info = media::file_info(file)?;
    let mut said = String::new();
    let mut kind = library::Kind::Compressed;
    let made: Option<PathBuf> = match wf.change {
        Change::Keep => None,
        Change::Smaller(size) => {
            let goal = size.goal();
            let limit = (size.mb() * 1_000_000.0) as u64;
            if info.size_bytes <= limit {
                said = format!("already under {} MB", compress::fmt_mb(size.mb()));
                None
            } else {
                let stage = format!("Making it smaller {}…", size.words());
                let opts = compress::Options::for_goal(goal);
                let out = compress::run(file, &info, &opts, encoders(), cancel, |p| on(p.fraction, &stage))?;
                said = format!("a copy, {}", crate::fmt::mb(out.bytes as f64));
                Some(out.path)
            }
        }
        Change::Sound(s) => {
            if !info.has_audio {
                bail!("it has no sound");
            }
            let same = file.extension().is_some_and(|e| e.eq_ignore_ascii_case(s.codec().ext()));
            kind = library::Kind::Sound;
            if !info.has_video && same {
                said = format!("already {}", s.label());
                None
            } else {
                let stage = format!("Saving its sound as {}…", s.label());
                let mut opts = compress::Options::for_goal(Goal::Format(s.codec()));
                opts.sound_codec = Some(s.codec());
                opts.sound_only = true;
                let out = compress::run(file, &info, &opts, encoders(), cancel, |p| on(p.fraction, &stage))?;
                said = format!("{}, {}", s.label(), crate::fmt::mb(out.bytes as f64));
                Some(out.path)
            }
        }
        Change::Gif => {
            if !info.has_video {
                bail!("a sound file has no picture to make a GIF of");
            }
            if info.duration > GIF_MAX {
                bail!("too long for a GIF ({} s; a minute at most)", info.duration.round());
            }
            kind = library::Kind::Gif;
            let spec = gif::GifSpec::default();
            let dir = file.parent().unwrap_or(Path::new("."));
            let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "gif".into());
            let out = clip::unique_path(dir, &stem, spec.kind.ext());
            let stage = "Making a GIF…";
            if let Err(e) = crate::sound::run_ffmpeg(
                &gif::build_args(file, 0.0, 0.0, &spec, &out),
                info.duration,
                cancel,
                |f| on(f, stage),
            ) {
                let _ = std::fs::remove_file(&out);
                return Err(e);
            }
            let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            said = format!("a GIF, {}", crate::fmt::mb(bytes as f64));
            Some(out)
        }
    };

    let mut done = Done {
        file: file.to_path_buf(),
        made: made.is_some(),
        replaced: false,
        moved: None,
        said,
        bytes: 0,
        kind,
    };
    match made {
        // Nothing new: the original may still be moved.
        None => {
            if wf.change == Change::Keep
                && let Put::Folder(dir) = &wf.put
            {
                on(1.0, "Moving it…");
                let to = move_with_attached(file, dir)?;
                done.said = format!("moved to {}", dir.display());
                done.moved = Some(to.clone());
                done.file = to;
            }
        }
        Some(out) => {
            let to = match &wf.put {
                Put::Beside => match done_dir {
                    Some(dir) => move_into(&out, dir)?,
                    None => out,
                },
                Put::Folder(dir) => move_into(&out, dir)?,
                Put::Replace => {
                    // The original to the Recycle Bin, the new file under its name.
                    if trash::delete(file).is_err() {
                        bail!("made it, but couldn't put the original in the Recycle Bin (open somewhere?)");
                    }
                    done.replaced = true;
                    let ext = out.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
                    let own = file.with_extension(&ext);
                    let to = match done_dir {
                        Some(dir) => {
                            let name = own.file_name().map(PathBuf::from).unwrap_or_default();
                            dir.join(name)
                        }
                        None => own,
                    };
                    if let Some(parent) = to.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if !to.exists() && move_file(&out, &to).is_ok() { to } else { out }
                }
            };
            done.bytes = std::fs::metadata(&to).map(|m| m.len()).unwrap_or(0);
            done.file = to;
        }
    }
    Ok(done)
}

/// Move `file` into `dir` (made if missing), never over another file.
fn move_into(file: &Path, dir: &Path) -> Result<PathBuf> {
    if file.parent() == Some(dir) {
        return Ok(file.to_path_buf());
    }
    std::fs::create_dir_all(dir)?;
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = file.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let to = clip::unique_path(dir, &stem, &ext);
    move_file(file, &to)?;
    Ok(to)
}

/// The original moved into `dir`, with its chat, captions, transcript and lyrics.
fn move_with_attached(file: &Path, dir: &Path) -> Result<PathBuf> {
    let attached = library::attached_of(file);
    let old_stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let to = move_into(file, dir)?;
    let new_stem = to.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    for a in attached {
        let name = a.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let rest = name.get(old_stem.len()..).unwrap_or("");
        let _ = move_file(&a, &dir.join(format!("{new_stem}{rest}")));
    }
    Ok(to)
}

/// Rename, or copy and remove when it's another drive.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The watched folder

const MEDIA: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "flv", "wmv", "mts", "m2ts", "3gp", "mp3", "m4a", "aac", "wav",
    "flac", "opus", "ogg", "wma",
];

/// A file Neria would take from a watched folder: a video or sound file, not hidden, not one
/// still being written by a browser or a sync app.
fn takes(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if name.starts_with('.') || name.starts_with('~') {
        return false;
    }
    p.extension().is_some_and(|e| MEDIA.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// The video and sound files in `dir` itself (not its folders, so Done is never looked in).
pub fn waiting_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && takes(p)).collect();
    out.sort();
    out
}

/// How a file is known: its name, size and time, so a new file of the same name counts again.
pub fn seen_key(p: &Path) -> Option<String> {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(format!("{}|{}|{t}", p.file_name()?.to_string_lossy(), m.len()))
}

/// The Done folder of a watched folder.
pub fn done_dir(folder: &Path) -> PathBuf {
    folder.join("Done")
}

/// Watches the folders: files that stopped growing and weren't done yet.
#[derive(Default)]
pub struct Watcher {
    /// Last look: size and time per file; a file is taken once a look finds them unchanged.
    last: HashMap<PathBuf, (u64, SystemTime)>,
}

impl Watcher {
    /// One look at every watched folder (call every few seconds): the files ready to run,
    /// with their workflow. They're marked seen right away, so a look never hands one out
    /// twice.
    pub fn look(&mut self) -> Vec<(Workflow, PathBuf)> {
        let all = list();
        let mut ready = Vec::new();
        let mut now: HashMap<PathBuf, (u64, SystemTime)> = HashMap::new();
        for wf in all.iter().filter(|w| w.on) {
            let When::Folder(dir) = &wf.when else { continue };
            for p in waiting_in(dir) {
                let Ok(m) = std::fs::metadata(&p) else { continue };
                let stamp = (m.len(), m.modified().unwrap_or(UNIX_EPOCH));
                now.insert(p.clone(), stamp);
                let Some(key) = seen_key(&p) else { continue };
                if wf.seen.contains(&key) {
                    continue;
                }
                // Stopped growing since the last look, and not written in the last 2 s.
                let settled = self.last.get(&p) == Some(&stamp)
                    && stamp.1.elapsed().map_or(true, |e| e.as_secs() >= 2)
                    && stamp.0 > 0;
                if settled && openable(&p) {
                    ready.push((wf.clone(), p));
                }
            }
        }
        self.last = now;
        if !ready.is_empty() {
            let today = day();
            edit(|s| {
                for (wf, p) in &ready {
                    if let (Some(w), Some(key)) = (s.items.iter_mut().find(|w| w.id == wf.id), seen_key(p)) {
                        w.seen.push(key);
                        w.done_today = if w.done_today.0 == today { (today, w.done_today.1 + 1) } else { (today, 1) };
                    }
                }
                // Forget files no longer in the folder.
                for w in s.items.iter_mut() {
                    if let When::Folder(dir) = &w.when {
                        let here: Vec<String> = waiting_in(dir).iter().filter_map(|p| seen_key(p)).collect();
                        w.seen.retain(|k| here.contains(k));
                    }
                }
            });
        }
        ready
    }
}

/// Another program still writing it (OBS, a copy) keeps it locked on Windows.
fn openable(p: &Path) -> bool {
    std::fs::OpenOptions::new().read(true).write(true).open(p).is_ok()
}

fn day() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) / 86_400
}

/// A watched folder's row: "3 done today".
pub fn done_today(wf: &Workflow) -> u32 {
    if wf.done_today.0 == day() { wf.done_today.1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_it_does() {
        let wf = Workflow { name: "Discord-ready".into(), ..Workflow::default() };
        assert_eq!(wf.what(), "A copy for Discord (under 20 MB), next to the original");
        let tik = Workflow { when: When::Sites(vec!["TikTok".into()]), ..wf.clone() };
        assert_eq!(tik.sentence(), "TikTok links: a copy for Discord (under 20 MB), next to the original.");
        let moved =
            Workflow { change: Change::Keep, put: Put::Folder(PathBuf::from(r"D:\Clips")), ..Workflow::default() };
        assert_eq!(moved.what(), r"Moved to D:\Clips");
        let rep = Workflow { change: Change::Sound(SoundAs::Mp3), put: Put::Replace, when: When::Every, ..wf };
        assert_eq!(rep.sentence(), "Every download: its sound as MP3, in place of the original. The original goes to the Recycle Bin.");
        assert_eq!(
            Workflow { when: When::Sites(vec!["TikTok".into(), "Instagram".into(), "X".into()]), ..Workflow::default() }
                .when_label(),
            "TikTok, Instagram and X links"
        );
    }

    #[test]
    fn empty_ones_never_run() {
        let keep = Workflow { change: Change::Keep, put: Put::Beside, ..Workflow::default() };
        assert!(keep.is_empty());
        let mv = Workflow { change: Change::Keep, put: Put::Folder("x".into()), ..Workflow::default() };
        assert!(!mv.is_empty());
    }

    #[test]
    fn watched_folder_takes_media_only() {
        assert!(takes(Path::new(r"D:\a\clip.mp4")));
        assert!(takes(Path::new(r"D:\a\song.FLAC")));
        assert!(!takes(Path::new(r"D:\a\clip.mp4.part")));
        assert!(!takes(Path::new(r"D:\a\clip.crdownload")));
        assert!(!takes(Path::new(r"D:\a\.hidden.mp4")));
        assert!(!takes(Path::new(r"D:\a\notes.txt")));
    }

    #[test]
    fn pick_round_trips() {
        let p = Pick::Use(Box::new(Workflow { id: 7, name: "x".into(), ..Workflow::default() }));
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Pick>(&json).unwrap(), p);
        assert_eq!(serde_json::from_str::<Pick>("\"auto\"").unwrap(), Pick::Auto);
    }
}
