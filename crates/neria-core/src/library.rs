//! The Library: one list of everything Neria saved (`library.json` next to the exe), to find,
//! reopen and do the next thing with. Only what Neria made: no drive scanning, files are
//! never moved or renamed, and a file that went missing stays listed as Gone.
//!
//! Filled once from what was remembered before (`history.json`, Capture's Saved list, the
//! Spotify folders' `.neria-sync.json`), then written wherever something is saved: the
//! download queue (`queue.rs`), instant replays, clips from files, compressed copies, GIFs,
//! transcripts. Mock: `design/library.html` (picks 1A, 2A, 3A, 4A + 4B, 5A).

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::{history, music, paths, settings::Settings};

/// Oldest records are dropped beyond this (history keeps 5,000; songs make the Library bigger).
const MAX: usize = 20_000;

static LOCK: Mutex<()> = Mutex::new(());

/// Told after something was saved into the Library (`add`, `add_all`, `transcribed`,
/// `playlist_synced`, `remove_file`), so an open Library page shows it at once. The page's
/// own changes (Remove, Find it…, lengths, the background check) don't call it.
static ON_SAVED: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

/// The desktop app's Library page: called (from any thread) after each save.
pub fn on_saved(f: impl Fn() + Send + Sync + 'static) {
    let _ = ON_SAVED.set(Box::new(f));
}

fn saved() {
    if let Some(f) = ON_SAVED.get() {
        f();
    }
}

/// What a saved thing is. The tag on its row says it in words (`label`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Video,
    Sound,
    /// A Spotify song (its audio from YouTube Music).
    Song,
    /// A Spotify playlist or album: its folder; songs saved into it hang under it.
    Playlist,
    /// A part of a link or a file (Clip page), a "Clip the last…".
    Clip,
    /// A live stream recorded.
    Recording,
    ReplayPc,
    ReplayPhone,
    Compressed,
    /// GIF, WebP or a GIF-like MP4 (Make a GIF, X GIFs).
    Gif,
    /// Speech to text of a file that isn't in the Library itself.
    Transcript,
    /// Just the captions of a link (Extras → Captions only).
    Captions,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Video => "Video",
            Kind::Sound => "Sound",
            Kind::Song => "Song",
            Kind::Playlist => "Playlist",
            Kind::Clip => "Clip",
            Kind::Recording => "Recording",
            Kind::ReplayPc | Kind::ReplayPhone => "Replay",
            Kind::Compressed => "Compressed",
            Kind::Gif => "GIF",
            Kind::Transcript => "Transcript",
            Kind::Captions => "Captions",
        }
    }

    /// The filter tab it counts under (GIFs and compressed copies under Clips; transcripts
    /// and captions only under All).
    pub fn tab(self) -> Tab {
        match self {
            Kind::Video => Tab::Videos,
            Kind::Sound | Kind::Song | Kind::Playlist => Tab::Sound,
            Kind::Clip | Kind::Compressed | Kind::Gif => Tab::Clips,
            Kind::Recording => Tab::Recordings,
            Kind::ReplayPc | Kind::ReplayPhone => Tab::Replays,
            Kind::Transcript | Kind::Captions => Tab::All,
        }
    }

    /// Played rather than watched ("Play", a square cover).
    pub fn is_sound(self) -> bool {
        matches!(self, Kind::Sound | Kind::Song | Kind::Playlist)
    }

    /// A text file (opened, not watched).
    pub fn is_text(self) -> bool {
        matches!(self, Kind::Transcript | Kind::Captions)
    }
}

/// The filter tabs, in the order they show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    All = 0,
    Videos = 1,
    Sound = 2,
    Clips = 3,
    Recordings = 4,
    Replays = 5,
    Gone = 6,
    /// The same thing saved more than once (only shown when there are any).
    Duplicates = 7,
}

impl Tab {
    pub const ALL: [Tab; 8] =
        [Tab::All, Tab::Videos, Tab::Sound, Tab::Clips, Tab::Recordings, Tab::Replays, Tab::Gone, Tab::Duplicates];

    pub fn from_index(i: i32) -> Tab {
        Tab::ALL.get(i.max(0) as usize).copied().unwrap_or(Tab::All)
    }
}

/// One thing Neria saved.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Item {
    /// Stable while the record lives (selection, actions); given by `add`.
    pub id: u64,
    pub kind: Kind,
    pub title: String,
    /// "YouTube", "Twitch", "Spotify"; "" for things made in Neria (replays, compressed…).
    pub site: String,
    /// Where it came from ("Download again", "Copy link"); "" when there's none.
    pub url: String,
    /// The saved file (a playlist: its folder).
    pub file: PathBuf,
    /// The file it was made from (a clip of a file, a compressed copy, a GIF, a transcript).
    pub source: Option<PathBuf>,
    /// Seconds, when known.
    pub length: Option<f64>,
    /// Bytes (a playlist: its songs together).
    pub size: Option<u64>,
    /// When it was saved (Unix seconds); a playlist: when it was last updated.
    pub at: u64,
    /// Files next to it that belong to it: `.chat.json`, transcripts, captions, lyrics.
    pub attached: Vec<PathBuf>,
    /// A link clip's In / Out in the source (seconds).
    pub section: Option<(f64, f64)>,
    /// A few quiet words for its line: "3 tracks", "was 212 MB", "recorded from the start".
    pub note: String,
    /// `Extractor:id`, as history keeps it.
    pub key: String,
    /// The site's picture for it (a link), for when the file has no frame (songs).
    pub thumbnail: String,
    /// A recording's holes, while it has some it could get back (Get the missed bits back).
    pub gaps: Option<crate::record::Gaps>,
}

impl Item {
    pub fn new(kind: Kind, file: impl Into<PathBuf>, title: impl Into<String>) -> Self {
        Item { kind, file: file.into(), title: title.into(), at: now(), ..Item::default() }
    }

    /// The title, else the file's name without its extension.
    pub fn name(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.clone();
        }
        self.file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Store {
    items: Vec<Item>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn file() -> PathBuf {
    paths::app_dir().join("library.json")
}

/// Frames for the rows, cached next to `library.json` (portable, like everything else).
pub fn thumbs_dir() -> PathBuf {
    paths::app_dir().join("library-thumbs")
}

fn read(path: &Path) -> Option<Vec<Item>> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<Store>(&bytes).ok().map(|s| s.items)
}

fn write(path: &Path, items: &[Item]) {
    let Ok(json) = serde_json::to_vec(&Store { items: items.to_vec() }) else { return };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Everything in the Library, oldest first (as saved).
pub fn load() -> Vec<Item> {
    let _guard = LOCK.lock().unwrap();
    read(&file()).unwrap_or_default()
}

/// Change the Library under its lock and save it.
fn edit<R>(f: impl FnOnce(&mut Vec<Item>) -> R) -> R {
    let _guard = LOCK.lock().unwrap();
    let path = file();
    let mut items = read(&path).unwrap_or_default();
    let r = f(&mut items);
    write(&path, &items);
    r
}

/// Remember something just saved. The same file again replaces its record (keeping its id
/// and what the new one doesn't know). Size, site and attached files are filled in here.
/// Returns its id.
pub fn add(item: Item) -> u64 {
    let item = complete(item);
    let id = edit(|items| put(items, item));
    saved();
    id
}

/// `add` for several at once (one write).
pub fn add_all(new: Vec<Item>) {
    let new: Vec<Item> = new.into_iter().map(complete).collect();
    edit(|items| {
        for it in new {
            put(items, it);
        }
    });
    saved();
}

fn complete(mut item: Item) -> Item {
    if item.at == 0 {
        item.at = now();
    }
    if item.site.is_empty() && !item.url.is_empty() {
        item.site = site_of(&item.url);
    }
    if item.kind == Kind::Playlist {
        if item.size.is_none() {
            item.size = Some(folder_size(&item.file));
        }
    } else {
        if item.size.is_none() {
            item.size = std::fs::metadata(&item.file).ok().filter(|m| m.is_file()).map(|m| m.len());
        }
        item.attached = attached_of(&item.file);
    }
    item
}

fn put(items: &mut Vec<Item>, mut item: Item) -> u64 {
    // The same file again; else the same thing whose file is gone (Download again): this
    // takes its place instead of a second row.
    let same = items.iter().position(|o| same_path(&o.file, &item.file)).or_else(|| {
        items.iter().position(|o| {
            let same_thing = (!item.key.is_empty() && o.key == item.key) || (!item.url.is_empty() && o.url == item.url);
            same_thing && o.kind == item.kind && o.section == item.section && !o.file.exists()
        })
    });
    if let Some(i) = same {
        let old = items.remove(i);
        item.id = old.id;
        if item.url.is_empty() {
            item.url = old.url;
            item.site = if item.site.is_empty() { old.site } else { item.site };
        }
        if item.source.is_none() {
            item.source = old.source;
        }
        if item.length.is_none() {
            item.length = old.length;
        }
        if item.thumbnail.is_empty() {
            item.thumbnail = old.thumbnail;
        }
        if item.title.is_empty() {
            item.title = old.title;
        }
    } else {
        item.id = items.iter().map(|o| o.id).max().unwrap_or(0) + 1;
    }
    let id = item.id;
    items.push(item);
    if items.len() > MAX {
        items.drain(..items.len() - MAX);
    }
    id
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

/// Forget these (Remove from Library); the files stay.
pub fn remove(ids: &[u64]) {
    edit(|items| items.retain(|i| !ids.contains(&i.id)));
}

/// A file Neria just deleted again (Undo after a save): its record goes too.
pub fn remove_file(path: &Path) {
    edit(|items| items.retain(|i| !same_path(&i.file, path)));
    saved();
}

/// The same thing saved more than once: groups of ids (newest first) with the same link or
/// site id, kind and part (a song downloaded twice, a video in two folders). Playlists and
/// things made from files (no link) aren't counted; nor are files already gone (`here`).
pub fn duplicates(items: &[Item], here: impl Fn(&Item) -> bool) -> Vec<Vec<u64>> {
    let mut groups: Vec<(String, Vec<&Item>)> = Vec::new();
    for it in items.iter().filter(|i| i.kind != Kind::Playlist && here(i)) {
        // A song is its Spotify link (its key is whichever YouTube Music match it got).
        let what = if it.kind == Kind::Song && !it.url.is_empty() || it.key.is_empty() { &it.url } else { &it.key };
        if what.is_empty() {
            continue;
        }
        let key = format!("{what}|{:?}|{:?}", it.kind, it.section.map(|(a, b)| ((a * 10.0) as i64, (b * 10.0) as i64)));
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.push(it),
            None => groups.push((key, vec![it])),
        }
    }
    groups
        .into_iter()
        .filter(|(_, g)| g.len() > 1)
        .map(|(_, mut g)| {
            g.sort_by(|a, b| b.at.cmp(&a.at));
            g.into_iter().map(|i| i.id).collect()
        })
        .collect()
}

/// These records' files are now here (Find it…), with what's attached there.
pub fn relocate(moves: &[(u64, PathBuf)]) {
    edit(|items| {
        for (id, to) in moves {
            if let Some(it) = items.iter_mut().find(|i| i.id == *id) {
                if let Some(src) = &it.source {
                    // A source in the same folder moved with it.
                    if let (Some(from_dir), Some(name)) = (it.file.parent(), src.file_name()) {
                        if src.parent() == Some(from_dir) {
                            let moved = to.with_file_name(name);
                            if moved.is_file() {
                                it.source = Some(moved);
                            }
                        }
                    }
                }
                it.file = to.clone();
                it.attached = attached_of(to);
            }
        }
    });
}

/// A recording's holes after "Get the missed bits back": what's still missing (none: the
/// record forgets them), its size and length now. Returns the record as it is now.
pub fn set_gaps(id: u64, gaps: Option<crate::record::Gaps>) -> Option<Item> {
    edit(|items| {
        let it = items.iter_mut().find(|i| i.id == id)?;
        it.gaps = gaps.filter(|g| !g.holes.is_empty());
        it.size = std::fs::metadata(&it.file).ok().map(|m| m.len()).or(it.size);
        it.length = crate::media::duration(&it.file).or(it.length);
        Some(it.clone())
    })
}

/// Lengths worked out later (the Library's thumbnails probe each file once).
pub fn set_lengths(found: &[(u64, f64)]) {
    if found.is_empty() {
        return;
    }
    edit(|items| {
        for (id, secs) in found {
            if let Some(it) = items.iter_mut().find(|i| i.id == *id) {
                it.length = Some(*secs);
            }
        }
    });
}

/// Refresh what the background check found: attached files and sizes (they change: a
/// transcript made later, a playlist updated).
pub fn refresh(updates: &[(u64, Vec<PathBuf>, Option<u64>)]) {
    if updates.is_empty() {
        return;
    }
    edit(|items| {
        for (id, attached, size) in updates {
            if let Some(it) = items.iter_mut().find(|i| i.id == *id) {
                it.attached = attached.clone();
                if size.is_some() {
                    it.size = *size;
                }
            }
        }
    });
}

// ───────────── Where a file is ─────────────

/// Is a record's file still there?
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Where {
    Here,
    /// Moved or deleted.
    Gone,
    /// Its drive isn't connected ("E:"): not gone, just not here now.
    Offline(String),
}

pub fn where_is(path: &Path) -> Where {
    if path.exists() {
        return Where::Here;
    }
    match path.components().next() {
        Some(Component::Prefix(p)) => {
            let root = PathBuf::from(format!("{}\\", p.as_os_str().to_string_lossy()));
            if root.exists() {
                Where::Gone
            } else {
                let name = p.as_os_str().to_string_lossy().into_owned();
                Where::Offline(name.trim_start_matches(r"\\?\").to_string())
            }
        }
        _ => Where::Gone,
    }
}

/// What the background check says about one record.
#[derive(Clone, Debug)]
pub struct Checked {
    pub id: u64,
    pub place: Where,
    pub attached: Vec<PathBuf>,
    pub size: Option<u64>,
}

/// Check every record's file (off the UI thread: a sleeping network drive can take a while).
pub fn check(items: &[Item]) -> Vec<Checked> {
    items
        .iter()
        .map(|it| {
            let place = where_is(&it.file);
            let (attached, size) = match (&place, it.kind) {
                (Where::Here, Kind::Playlist) => (Vec::new(), Some(folder_size(&it.file))),
                (Where::Here, _) => {
                    (attached_of(&it.file), std::fs::metadata(&it.file).ok().map(|m| m.len()).or(it.size))
                }
                _ => (it.attached.clone(), it.size),
            };
            Checked { id: it.id, place, attached, size }
        })
        .collect()
}

/// Find it…: the user found `id`'s file at `found`. Returns it and every other gone file
/// from the same old folder that is in the new folder too (by name), so a moved folder is
/// fixed in one go. The first entry is always `id` itself.
pub fn find_with(items: &[Item], id: u64, found: &Path) -> Vec<(u64, PathBuf)> {
    let Some(it) = items.iter().find(|i| i.id == id) else { return Vec::new() };
    let mut out = vec![(id, found.to_path_buf())];
    let (Some(old_dir), Some(new_dir)) = (it.file.parent(), found.parent()) else { return out };
    for other in items.iter().filter(|o| o.id != id && o.file.parent() == Some(old_dir)) {
        if where_is(&other.file) != Where::Gone {
            continue;
        }
        let Some(name) = other.file.file_name() else { continue };
        let there = new_dir.join(name);
        if there.exists() {
            out.push((other.id, there));
        }
    }
    out
}

// ───────────── What belongs to a file ─────────────

/// What an attached file is, in a word ("Chat", "Transcript", "Captions", "Lyrics").
pub fn attached_label(path: &Path) -> &'static str {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if name.ends_with(".chat.json") {
        "Chat"
    } else if name.ends_with(".txt") {
        "Transcript"
    } else if name.ends_with(".lrc") {
        "Lyrics"
    } else {
        "Captions"
    }
}

/// Files next to `file` that belong to it: `name.chat.json`, `name.txt`, `name.srt`,
/// `name.en.vtt`, `name.lrc`…
pub fn attached_of(file: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (file.parent(), file.file_stem()) else { return Vec::new() };
    let stem = stem.to_string_lossy().to_lowercase();
    let own = file.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if name == own || !name.starts_with(&stem) {
                return false;
            }
            let rest = &name[stem.len()..];
            if rest == ".chat.json" {
                return true;
            }
            // ".txt", ".srt", or ".en.srt" / ".en-US.vtt" (one language part, no more).
            let Some((middle, ext)) = rest.rsplit_once('.') else { return false };
            matches!(ext, "txt" | "srt" | "vtt" | "ass" | "lrc")
                && (middle.is_empty()
                    || (middle.starts_with('.')
                        && middle.len() <= 9
                        && middle[1..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-')))
        })
        .collect();
    out.sort();
    out
}

fn folder_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

/// "YouTube", "Twitch", "X"… from a link; the host without `www.` otherwise.
pub fn site_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_lowercase();
    let host = host.rsplit_once('@').map_or(host.as_str(), |(_, h)| h).split(':').next().unwrap_or("");
    let host = host.trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music.");
    let known = [
        ("youtube.com", "YouTube"),
        ("youtu.be", "YouTube"),
        ("twitch.tv", "Twitch"),
        ("x.com", "X"),
        ("twitter.com", "X"),
        ("spotify.com", "Spotify"),
        ("tiktok.com", "TikTok"),
        ("instagram.com", "Instagram"),
        ("reddit.com", "Reddit"),
        ("soundcloud.com", "SoundCloud"),
        ("kick.com", "Kick"),
        ("vimeo.com", "Vimeo"),
        ("facebook.com", "Facebook"),
        ("bsky.app", "Bluesky"),
        ("bilibili.com", "Bilibili"),
    ];
    for (domain, name) in known {
        if host == domain || host.ends_with(&format!(".{domain}")) {
            return name.to_string();
        }
    }
    host.to_string()
}

const AUDIO: &[&str] = &["mp3", "m4a", "aac", "wav", "flac", "opus", "ogg", "wma"];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| AUDIO.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// A Spotify playlist's songs: the records saved in its folder.
pub fn songs_of<'a>(items: &'a [Item], playlist: &Item) -> Vec<&'a Item> {
    items.iter().filter(|i| i.kind != Kind::Playlist && i.file.parent() == Some(playlist.file.as_path())).collect()
}

/// Folders of the Spotify playlists in the Library (their songs hang under them, not in the
/// list by themselves).
pub fn playlist_dirs(items: &[Item]) -> HashSet<PathBuf> {
    items.iter().filter(|i| i.kind == Kind::Playlist).map(|i| i.file.clone()).collect()
}

// ───────────── Filters ─────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Newest,
    Oldest,
    Biggest,
    Name,
}

impl Sort {
    pub const LABELS: [&'static str; 4] = ["Newest first", "Oldest first", "Biggest first", "Name A–Z"];
    pub fn from_index(i: i32) -> Sort {
        match i {
            1 => Sort::Oldest,
            2 => Sort::Biggest,
            3 => Sort::Name,
            _ => Sort::Newest,
        }
    }
}

/// Date menu: Any time, Today, Last 7 days, Last 30 days, This year.
pub const DATES: [&str; 5] = ["Any time", "Today", "Last 7 days", "Last 30 days", "Last 12 months"];

/// The oldest time a Date pick lets through (`today_start`: local midnight, Unix seconds).
pub fn since(date: i32, now: u64, today_start: u64) -> u64 {
    match date {
        1 => today_start,
        2 => now.saturating_sub(7 * 86_400),
        3 => now.saturating_sub(30 * 86_400),
        4 => now.saturating_sub(365 * 86_400),
        _ => 0,
    }
}

/// The site a record counts under in the Site menu ("Made in Neria" for things with none).
pub fn site_name(it: &Item) -> String {
    if it.site.is_empty() { "Made in Neria".to_string() } else { it.site.clone() }
}

/// Does it match what's typed? Every word must be in the title, the site, the file's name
/// or its note (case doesn't matter).
pub fn matches(it: &Item, query: &str) -> bool {
    let hay = format!(
        "{} {} {} {} {}",
        it.title,
        site_name(it),
        it.file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
        it.note,
        it.kind.label()
    )
    .to_lowercase();
    query.to_lowercase().split_whitespace().all(|w| hay.contains(w))
}

/// Order the shown records.
pub fn sort(items: &mut [&Item], how: Sort) {
    match how {
        Sort::Newest => items.sort_by(|a, b| b.at.cmp(&a.at).then(b.id.cmp(&a.id))),
        Sort::Oldest => items.sort_by(|a, b| a.at.cmp(&b.at).then(a.id.cmp(&b.id))),
        Sort::Biggest => items.sort_by(|a, b| b.size.unwrap_or(0).cmp(&a.size.unwrap_or(0))),
        Sort::Name => items.sort_by_key(|a| a.name().to_lowercase()),
    }
}

// ───────────── Filled once from what was remembered before ─────────────

/// The first time the Library opens (no `library.json` yet): bring in what Neria remembered
/// before. Returns how many things came in, or None when the Library already existed.
pub fn fill_once(settings: &Settings) -> Option<usize> {
    let path = file();
    {
        let _guard = LOCK.lock().unwrap();
        if path.exists() {
            return None;
        }
    }
    let mut found = from_history(&history_records());
    found.extend(from_replays(&settings.capture.saved));
    found.extend(from_spotify(&settings.output_dir));
    let n = found.len();
    let new: Vec<Item> = found.into_iter().map(complete).collect();
    let _guard = LOCK.lock().unwrap();
    if path.exists() {
        return None;
    }
    let mut items = Vec::new();
    // Oldest first, so the newest of one file wins.
    let mut new = new;
    new.sort_by_key(|i| i.at);
    for it in new {
        put(&mut items, it);
    }
    write(&path, &items);
    Some(n)
}

fn history_records() -> Vec<history::Record> {
    std::fs::read(paths::app_dir().join("history.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn from_history(records: &[history::Record]) -> Vec<Item> {
    records
        .iter()
        .map(|r| {
            // Instant replays went into history with no link.
            let kind = if r.url.is_empty() {
                Kind::ReplayPc
            } else if is_audio_file(&r.file) {
                Kind::Sound
            } else if is_gif_file(&r.file) {
                Kind::Gif
            } else if is_recording_name(&r.file) {
                Kind::Recording
            } else {
                Kind::Video
            };
            Item { kind, url: r.url.clone(), key: r.key.clone(), at: r.at, ..Item::new(kind, &r.file, &r.title) }
        })
        .collect()
}

fn is_gif_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif") || e.eq_ignore_ascii_case("webp"))
}

/// A live recording's name has its day in it and a chat file next to it.
fn is_recording_name(path: &Path) -> bool {
    let chat = path.with_extension("chat.json");
    chat.is_file() && path.file_stem().is_some_and(|s| s.to_string_lossy().contains(" (live"))
}

fn from_replays(saved: &[PathBuf]) -> Vec<Item> {
    saved
        .iter()
        .map(|f| {
            let at = std::fs::metadata(f)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            Item { at, ..Item::new(Kind::ReplayPc, f, "") }
        })
        .collect()
}

/// Spotify playlists and albums: folders with a `.neria-sync.json` in the download folder,
/// its Audio folder, or one level under either. Each is a Playlist record, and each of its
/// songs that's there a Song record.
fn from_spotify(root: &Path) -> Vec<Item> {
    let mut dirs = Vec::new();
    for base in [root.to_path_buf(), root.join("Audio")] {
        let Ok(rd) = std::fs::read_dir(&base) else { continue };
        dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    let mut out = Vec::new();
    for dir in dirs {
        let Some(m) = music::read_manifest(&dir) else { continue };
        out.extend(playlist_items(&dir, &m));
    }
    out
}

/// The records for a Spotify folder: the playlist, then its songs that are there.
pub fn playlist_items(dir: &Path, m: &music::Manifest) -> Vec<Item> {
    let modified = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs())
    };
    let mut out = Vec::new();
    let mut length = 0.0;
    let m3u = dir.join(format!("{}.m3u8", music::sanitize(&m.name)));
    let durations: Vec<(String, f64, String)> = music::read_m3u(&m3u);
    for t in &m.tracks {
        let Some(file) = music::existing_file(dir, &t.stem) else { continue };
        let secs = durations
            .iter()
            .find(|(_, _, f)| Path::new(f).file_stem().is_some_and(|s| s.to_string_lossy() == t.stem))
            .map(|(_, s, _)| *s)
            .filter(|s| *s > 0.0);
        length += secs.unwrap_or(0.0);
        out.push(Item {
            site: "Spotify".into(),
            url: if t.id.is_empty() { String::new() } else { format!("https://open.spotify.com/track/{}", t.id) },
            length: secs,
            at: modified(&file),
            ..Item::new(Kind::Song, &file, &t.title)
        });
    }
    let kind = if m.spotify_kind.is_empty() { "playlist" } else { m.spotify_kind.as_str() };
    let songs = out.len();
    out.insert(
        0,
        Item {
            site: "Spotify".into(),
            url: if m.spotify_id.is_empty() {
                String::new()
            } else {
                format!("https://open.spotify.com/{kind}/{}", m.spotify_id)
            },
            length: (length > 0.0).then_some(length),
            note: format!("{songs} song{}", if songs == 1 { "" } else { "s" }),
            at: modified(&dir.join(".neria-sync.json")),
            ..Item::new(Kind::Playlist, dir, &m.name)
        },
    );
    out
}

/// A Spotify playlist or album was just synced into `dir`: its record (and its songs that
/// are already there) come in or are brought up to date.
pub fn playlist_synced(dir: &Path, m: &music::Manifest) {
    let mut items = playlist_items(dir, m);
    if let Some(p) = items.first_mut() {
        p.at = now();
    }
    add_all(items);
}

/// Speech to text wrote `outputs` for `input`: attached to its record when it has one,
/// else a Transcript record of its own.
pub fn transcribed(input: &Path, outputs: &[PathBuf]) {
    if outputs.is_empty() {
        return;
    }
    let has_record = edit(|items| {
        let Some(it) = items.iter_mut().find(|i| same_path(&i.file, input)) else { return false };
        it.attached = attached_of(input);
        true
    });
    if has_record {
        saved();
        return;
    }
    // The .txt first if there is one: it's what people open.
    let main = outputs.iter().find(|p| p.extension().is_some_and(|e| e == "txt")).unwrap_or(&outputs[0]);
    let title = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    add(Item { source: Some(input.to_path_buf()), ..Item::new(Kind::Transcript, main, title) });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("neria-library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sites_from_links() {
        assert_eq!(site_of("https://www.youtube.com/watch?v=x"), "YouTube");
        assert_eq!(site_of("https://youtu.be/x"), "YouTube");
        assert_eq!(site_of("https://music.youtube.com/watch?v=x"), "YouTube");
        assert_eq!(site_of("https://m.twitch.tv/videos/1"), "Twitch");
        assert_eq!(site_of("https://twitter.com/a/status/1"), "X");
        assert_eq!(site_of("https://open.spotify.com/track/1"), "Spotify");
        assert_eq!(site_of("https://example.org:8080/a"), "example.org");
    }

    #[test]
    fn same_file_replaces_its_record_and_keeps_the_id() {
        let mut items = Vec::new();
        let a = put(&mut items, Item { url: "https://youtu.be/a".into(), ..Item::new(Kind::Video, "C:/v/a.mp4", "A") });
        let b = put(&mut items, Item::new(Kind::Clip, "C:/v/b.mp4", "B"));
        assert_ne!(a, b);
        // Saved again (Download again): same id, the link kept, now the newest.
        let again = put(&mut items, Item { at: 99, ..Item::new(Kind::Video, "c:/V/A.mp4", "A") });
        assert_eq!(again, a);
        assert_eq!(items.len(), 2);
        assert_eq!(items.last().unwrap().url, "https://youtu.be/a");
        assert_eq!(items.last().unwrap().at, 99);
    }

    #[test]
    fn the_same_thing_saved_twice() {
        let link = |id, u: &str, kind, file: &str, at| Item { id, url: u.into(), at, ..Item::new(kind, file, "x") };
        let items = vec![
            link(1, "https://youtu.be/a", Kind::Video, "C:/1/a.mp4", 1),
            link(2, "https://youtu.be/a", Kind::Video, "C:/2/a.mp4", 5),
            // A clip of the same link is another thing; so is a file with no link.
            Item { section: Some((1.0, 2.0)), ..link(3, "https://youtu.be/a", Kind::Clip, "C:/c.mp4", 2) },
            link(4, "", Kind::Clip, "C:/d.mp4", 3),
            link(5, "", Kind::Clip, "C:/e.mp4", 4),
        ];
        let g = duplicates(&items, |_| true);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0], vec![2, 1]); // newest first
        // Two songs from different YouTube Music matches of the same Spotify song.
        let song = |id, key: &str, file: &str| Item {
            key: key.into(),
            ..link(id, "https://open.spotify.com/track/x", Kind::Song, file, id)
        };
        assert_eq!(duplicates(&[song(7, "Youtube:a", "C:/s1.mp3"), song(8, "Youtube:b", "C:/s2.mp3")], |_| true), vec![vec![8, 7]]);
        // One of the two gone: no longer a pair.
        assert!(duplicates(&items, |i| i.file != Path::new("C:/1/a.mp4")).is_empty());
    }

    #[test]
    fn download_again_takes_the_gone_ones_place() {
        let dir = temp("again");
        let mut items = Vec::new();
        let link = "https://youtu.be/a";
        let gone = put(&mut items, Item { url: link.into(), ..Item::new(Kind::Video, dir.join("moved.mp4"), "A") });
        let clip = put(
            &mut items,
            Item { url: link.into(), section: Some((1.0, 2.0)), ..Item::new(Kind::Clip, dir.join("part.mp4"), "A") },
        );
        // Saved somewhere else: the gone record becomes this one (its id kept); the clip of
        // the same link is another thing and stays.
        std::fs::write(dir.join("new.mp4"), b"x").unwrap();
        let again = put(&mut items, Item { url: link.into(), ..Item::new(Kind::Video, dir.join("new.mp4"), "A") });
        assert_eq!(again, gone);
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|i| i.id == clip));
        // While the file is still there, another copy is a row of its own.
        std::fs::write(dir.join("copy.mp4"), b"x").unwrap();
        let copy = put(&mut items, Item { url: link.into(), ..Item::new(Kind::Video, dir.join("copy.mp4"), "A") });
        assert_ne!(copy, gone);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tabs_and_search() {
        assert_eq!(Kind::Gif.tab(), Tab::Clips);
        assert_eq!(Kind::Compressed.tab(), Tab::Clips);
        assert_eq!(Kind::Song.tab(), Tab::Sound);
        assert_eq!(Kind::ReplayPhone.tab(), Tab::Replays);
        assert_eq!(Kind::Transcript.tab(), Tab::All);
        let it = Item { site: "YouTube".into(), ..Item::new(Kind::Recording, "D:/v/Day 3 in Tokyo.mp4", "IShowSpeed · Day 3") };
        assert!(matches(&it, "tokyo youtube"));
        assert!(matches(&it, "SPEED"));
        assert!(matches(&it, "recording"));
        assert!(!matches(&it, "tokyo twitch"));
        let made = Item::new(Kind::Compressed, "D:/v/x (Discord).mp4", "");
        assert_eq!(site_name(&made), "Made in Neria");
        assert!(matches(&made, "neria discord"));
    }

    #[test]
    fn sorting() {
        let a = Item { id: 1, at: 10, size: Some(5), ..Item::new(Kind::Video, "a", "beta") };
        let b = Item { id: 2, at: 20, size: Some(50), ..Item::new(Kind::Video, "b", "Alpha") };
        let ids = |how| {
            let mut v = vec![&a, &b];
            sort(&mut v, how);
            v.iter().map(|i| i.id).collect::<Vec<_>>()
        };
        assert_eq!(ids(Sort::Newest), vec![2, 1]);
        assert_eq!(ids(Sort::Oldest), vec![1, 2]);
        assert_eq!(ids(Sort::Biggest), vec![2, 1]);
        assert_eq!(ids(Sort::Name), vec![2, 1]);
        assert_eq!(since(0, 1000, 900), 0);
        assert_eq!(since(1, 1000, 900), 900);
    }

    #[test]
    fn attached_files_by_name() {
        let dir = temp("attached");
        let video = dir.join("Talk.mp4");
        for f in ["Talk.mp4", "Talk.chat.json", "Talk.txt", "Talk.en.srt", "Talk.en-US.vtt", "Talk 2.mp4", "Talk 2.txt", "Talky.srt", "Talk.jpg"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let names: Vec<String> =
            attached_of(&video).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["Talk.chat.json", "Talk.en-US.vtt", "Talk.en.srt", "Talk.txt"]);
        assert_eq!(attached_label(&dir.join("Talk.chat.json")), "Chat");
        assert_eq!(attached_label(&dir.join("Talk.txt")), "Transcript");
        assert_eq!(attached_label(&dir.join("Talk.en.srt")), "Captions");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gone_and_a_missing_drive() {
        let dir = temp("where");
        let here = dir.join("here.mp4");
        std::fs::write(&here, b"x").unwrap();
        assert_eq!(where_is(&here), Where::Here);
        assert_eq!(where_is(&dir.join("gone.mp4")), Where::Gone);
        // A drive letter that isn't there: not gone, just not connected.
        let free = ('D'..='Z').rev().find(|l| !Path::new(&format!("{l}:\\")).exists());
        if let Some(l) = free {
            assert_eq!(where_is(Path::new(&format!("{l}:\\Videos\\a.mp4"))), Where::Offline(format!("{l}:")));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_it_also_finds_the_neighbours() {
        let dir = temp("find");
        let (old, new) = (dir.join("old"), dir.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        for f in ["a.mp4", "b.mp4", "c.mp4"] {
            std::fs::write(new.join(f), b"x").unwrap();
        }
        // d.mp4 stayed behind in the old folder: it isn't gone, so it isn't moved.
        std::fs::write(old.join("d.mp4"), b"x").unwrap();
        let mut items = Vec::new();
        for f in ["a.mp4", "b.mp4", "d.mp4", "e.mp4"] {
            put(&mut items, Item::new(Kind::ReplayPc, old.join(f), ""));
        }
        put(&mut items, Item::new(Kind::ReplayPc, dir.join("elsewhere").join("c.mp4"), ""));
        let a = items[0].id;
        let found = find_with(&items, a, &new.join("a.mp4"));
        let names: Vec<String> =
            found.iter().map(|(_, p)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        // a itself first, then b (gone from old, in new); not d (still there), e (not in new),
        // or c (another old folder).
        assert_eq!(names, vec!["a.mp4", "b.mp4"]);
        assert_eq!(found[0].0, a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_and_replays_come_in() {
        let dir = temp("fill");
        let rec = |url: &str, file: &str| history::Record {
            key: String::new(),
            url: url.into(),
            file: dir.join(file),
            title: "t".into(),
            at: 5,
        };
        let items = from_history(&[
            rec("https://youtu.be/a", "a.mp4"),
            rec("https://youtu.be/b", "b.mp3"),
            rec("", "VALORANT 14.32.mp4"),
        ]);
        let kinds: Vec<Kind> = items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, vec![Kind::Video, Kind::Sound, Kind::ReplayPc]);
        assert_eq!(from_replays(&[dir.join("x.mp4")])[0].kind, Kind::ReplayPc);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_spotify_folder_is_a_playlist_with_its_songs() {
        let dir = temp("spotify");
        let pl = dir.join("Audio").join("Chill Mix");
        std::fs::create_dir_all(&pl).unwrap();
        std::fs::write(pl.join("01 A - One.mp3"), b"xx").unwrap();
        std::fs::write(pl.join("02 B - Two.mp3"), b"xxx").unwrap();
        let m = music::Manifest {
            spotify_kind: "playlist".into(),
            spotify_id: "37i9".into(),
            name: "Chill Mix".into(),
            tracks: vec![
                music::ManifestTrack { id: "t1".into(), stem: "01 A - One".into(), title: "A - One".into() },
                music::ManifestTrack { id: "t2".into(), stem: "02 B - Two".into(), title: "B - Two".into() },
                music::ManifestTrack { id: "t3".into(), stem: "03 C - Three".into(), title: "C - Three".into() },
            ],
        };
        music::write_manifest(&pl, &m).unwrap();
        let items: Vec<Item> = from_spotify(&dir).into_iter().map(complete).collect();
        assert_eq!(items.len(), 3, "the playlist and the two songs that are there");
        assert_eq!(items[0].kind, Kind::Playlist);
        assert_eq!(items[0].url, "https://open.spotify.com/playlist/37i9");
        assert_eq!(items[0].note, "2 songs");
        assert_eq!(items[1].url, "https://open.spotify.com/track/t1");
        assert_eq!(songs_of(&items, &items[0]).len(), 2);
        assert!(playlist_dirs(&items).contains(&pl));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
