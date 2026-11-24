//! Follow a channel or playlist: its new uploads are downloaded on a schedule (Download
//! track 12). Kept in `following.json` next to the exe.
//!
//! Following only gets *new* uploads: what's there when you follow counts as seen. A check
//! lists the channel's newest entries (channels list newest first) or the whole playlist
//! (playlists usually grow at the end), queues what wasn't seen as one group card, and
//! saves into a folder named after the channel or playlist, so every check lands in the
//! same place. An upload only counts as seen once it downloaded (or failed three checks in
//! a row), so a video that wasn't ready yet is tried again next time.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::download::DownloadRequest;
use crate::post::Item;
use crate::queue::Queue;
use crate::settings::{Preset, Quality, Settings};
use crate::{folders, history, music, paths, probe, spotify};

/// How many of a channel's newest uploads a check looks at.
const CHANNEL_ENTRIES: usize = 40;
/// Checks an upload may fail before it's given up on.
const TRIES: u32 = 3;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Every {
    Hourly,
    #[default]
    Daily,
    Weekly,
}

impl Every {
    pub const ALL: [Every; 3] = [Every::Hourly, Every::Daily, Every::Weekly];

    pub fn secs(self) -> u64 {
        match self {
            Every::Hourly => 3_600,
            Every::Daily => 86_400,
            Every::Weekly => 7 * 86_400,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Every::Hourly => "hourly",
            Every::Daily => "daily",
            Every::Weekly => "weekly",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|e| *e == self).unwrap_or(1)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }
}

/// One followed channel or playlist.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Followed {
    /// What's listed at each check (a channel's Videos tab).
    pub url: String,
    /// The channel or playlist name: the card's title and the folder's name.
    pub name: String,
    /// A channel (newest first: only the newest entries are looked at).
    pub channel: bool,
    pub every: Every,
    pub paused: bool,
    pub thumbnail: Option<String>,
    /// The kind of file, as the chips were when you followed it.
    pub quality: Quality,
    pub preset: Preset,
    /// Unix seconds; 0 = never checked.
    pub last_check: u64,
    /// New uploads found by the last check.
    pub last_new: usize,
    /// What went wrong at the last check (the channel is still followed).
    pub error: Option<String>,
    /// Links of uploads seen (there when you followed, downloaded, or given up on).
    pub seen: Vec<String>,
    /// Queued by a check, not downloaded yet.
    pub pending: Vec<String>,
    /// Failed checks per pending link.
    pub tries: HashMap<String, u32>,
    /// A Spotify playlist: songs taken off it that are still in its folder, waiting for
    /// "Review removed" (a check never deletes anything).
    pub removed: Vec<music::Removed>,
}

impl Followed {
    /// Due for a check at `now` (paused ones never are).
    pub fn due(&self, now: u64) -> bool {
        !self.paused && now >= self.last_check.saturating_add(self.every.secs())
    }

    /// A Spotify playlist (its songs are kept by track id, not by link).
    pub fn is_spotify(&self) -> bool {
        spotify::parse_link(&self.url).is_some()
    }

    /// Removed songs whose file is still there.
    pub fn removed_now(&self) -> Vec<music::Removed> {
        self.removed.iter().filter(|r| r.file.is_file()).cloned().collect()
    }
}

static LOCK: Mutex<()> = Mutex::new(());

fn file() -> std::path::PathBuf {
    paths::app_dir().join("following.json")
}

fn load() -> Vec<Followed> {
    std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn store(all: &[Followed]) {
    if let Ok(json) = serde_json::to_vec_pretty(all) {
        let tmp = file().with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, file());
        }
    }
}

/// Change the followed list under the lock.
fn edit<T>(f: impl FnOnce(&mut Vec<Followed>) -> T) -> T {
    let _guard = LOCK.lock().unwrap();
    let mut all = load();
    let out = f(&mut all);
    store(&all);
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The link a check lists for `url` (a channel's Videos tab, a Spotify playlist's plain
/// link), also the key it's kept by.
pub fn list_url(url: &str) -> String {
    if let Some(link) = spotify::parse_link(url) {
        return spotify_url(&link.id);
    }
    probe::channel_videos_url(url).unwrap_or_else(|| url.trim().to_string())
}

fn spotify_url(playlist_id: &str) -> String {
    format!("https://open.spotify.com/playlist/{playlist_id}")
}

/// A Spotify link that can be followed: playlists (albums don't change, an artist's page
/// only lists the top 10).
pub fn spotify_followable(url: &str) -> bool {
    spotify::parse_link(url).is_some_and(|l| l.kind == spotify::Kind::Playlist)
}

/// Follow a Spotify playlist: the songs in it now count as seen (by track id).
pub fn follow_spotify(col: &spotify::Collection) {
    let items: Vec<Item> = col.tracks.iter().map(song_item).collect();
    follow(&spotify_url(&col.id), &col.name, col.cover_url.clone(), &items, Quality::Audio, Preset::default());
}

/// A Spotify song as a check sees it: its track id where a video has its link.
fn song_item(t: &spotify::Track) -> Item {
    Item {
        kind: crate::post::ItemKind::Video,
        title: format!("{} - {}", t.artist_line(), t.title),
        duration: None,
        thumbnail: None,
        url: t.id.clone(),
        playlist_index: None,
        direct_url: None,
        ext: String::new(),
    }
}

/// The review is done (songs moved to the Recycle Bin or kept): stop asking about them.
pub fn forget_removed(files: &[std::path::PathBuf]) {
    edit(|all| {
        for f in all.iter_mut() {
            f.removed.retain(|r| !files.contains(&r.file));
        }
    });
}

pub fn list() -> Vec<Followed> {
    let _guard = LOCK.lock().unwrap();
    load()
}

pub fn find(url: &str) -> Option<Followed> {
    let key = list_url(url);
    list().into_iter().find(|f| f.url == key)
}

/// Follow `url`: everything in `items` (what it holds now) counts as seen.
pub fn follow(url: &str, name: &str, thumbnail: Option<String>, items: &[Item], quality: Quality, preset: Preset) {
    let key = list_url(url);
    let channel = probe::channel_videos_url(url).is_some();
    // A channel's tab is named "<channel> - Videos".
    let name = name.strip_suffix(" - Videos").unwrap_or(name).trim().to_string();
    edit(|all| {
        all.retain(|f| f.url != key);
        all.push(Followed {
            url: key,
            name,
            channel,
            thumbnail,
            quality,
            preset,
            last_check: now_secs(),
            seen: items.iter().map(|i| i.url.clone()).collect(),
            ..Default::default()
        });
    });
}

pub fn unfollow(url: &str) {
    edit(|all| all.retain(|f| f.url != url));
}

pub fn set_every(url: &str, every: Every) {
    edit(|all| all.iter_mut().filter(|f| f.url == url).for_each(|f| f.every = every));
}

pub fn set_paused(url: &str, paused: bool) {
    edit(|all| all.iter_mut().filter(|f| f.url == url).for_each(|f| f.paused = paused));
}

/// Followed lists due for a check, longest overdue first.
pub fn due(now: u64) -> Vec<String> {
    let mut d: Vec<Followed> = list().into_iter().filter(|f| f.due(now)).collect();
    d.sort_by_key(|f| f.last_check);
    d.into_iter().map(|f| f.url).collect()
}

/// What a check does with what it found, apart from the listing (tested on its own).
/// `done(link)`: it downloaded; `waiting(link)`: it's still in the queue.
fn settle(f: &mut Followed, items: &[Item], done: impl Fn(&str) -> bool, waiting: impl Fn(&str) -> bool) -> Vec<Item> {
    let pending = std::mem::take(&mut f.pending);
    for link in pending {
        if done(&link) {
            f.tries.remove(&link);
            f.seen.push(link);
        } else if waiting(&link) {
            f.pending.push(link);
        } else {
            // Failed or cancelled: try again, up to a point.
            let n = f.tries.entry(link.clone()).or_insert(0);
            *n += 1;
            if *n >= TRIES {
                f.tries.remove(&link);
                f.seen.push(link);
            }
        }
    }
    let mut new: Vec<Item> = Vec::new();
    for it in items {
        if f.seen.contains(&it.url) || f.pending.contains(&it.url) {
            continue;
        }
        // Downloaded by hand already: nothing to fetch.
        if done(&it.url) {
            f.seen.push(it.url.clone());
        } else {
            new.push(it.clone());
        }
    }
    // Channels list newest first: download the oldest of the new ones first.
    if f.channel {
        new.reverse();
    }
    f.pending.extend(new.iter().map(|i| i.url.clone()));
    // Keep the lists from growing forever: a channel only shows its newest entries.
    if f.seen.len() > 5_000 {
        let cut = f.seen.len() - 5_000;
        f.seen.drain(..cut);
    }
    new
}

/// Check one followed list now: queue its new uploads (one card). Returns how many.
pub fn check(url: &str, settings: &Settings, queue: &Queue) -> anyhow::Result<usize> {
    let Some(f) = list().into_iter().find(|f| f.url == url) else { return Ok(0) };
    if f.is_spotify() {
        return check_spotify(&f, settings, queue);
    }
    let listed = probe::list_entries(&f.url, settings, f.channel.then_some(CHANNEL_ENTRIES));
    let now = now_secs();
    let items = match listed {
        Ok(items) => items,
        Err(e) => {
            let msg = e.downcast_ref::<probe::LinkProblem>().map_or_else(|| format!("{e:#}"), |p| p.title.clone());
            failed(url, now, &msg);
            anyhow::bail!(msg);
        }
    };
    // Read the history once, before taking the list's lock.
    let downloaded = history::downloaded_urls();
    let (new, f) = edit(|all| {
        let Some(f) = all.iter_mut().find(|f| f.url == url) else { return (Vec::new(), None) };
        let new =
            settle(f, &items, |link| downloaded.contains(link), |link| queue.find_unfinished(None, link).is_some());
        f.last_check = now;
        f.last_new = new.len();
        f.error = None;
        (new, Some(f.clone()))
    });
    let Some(f) = f else { return Ok(0) };
    if new.is_empty() {
        return Ok(0);
    }
    let kind = if f.quality == Quality::Audio { folders::Kind::Audio } else { folders::Kind::Video };
    let dir = folders::for_group(settings, &[kind], &f.name);
    let reqs: Vec<DownloadRequest> = new
        .iter()
        .map(|it| {
            let title = if it.title.is_empty() { it.url.clone() } else { it.title.clone() };
            let mut req = DownloadRequest::new(&it.url, f.quality, title);
            req.preset = f.preset;
            req.dir = Some(dir.clone());
            req.thumbnail = it.thumbnail.clone();
            req.duration = it.duration;
            req
        })
        .collect();
    let n = reqs.len();
    queue.add_group(&card_title(&f.name, n), false, reqs);
    Ok(n)
}

/// "Kurzgesagt · 2 new".
fn card_title(name: &str, n: usize) -> String {
    format!("{name} · {n} new")
}

/// The check couldn't list it: say so in Settings → Following (it stays followed).
fn failed(url: &str, now: u64, msg: &str) {
    edit(|all| {
        if let Some(f) = all.iter_mut().find(|f| f.url == url) {
            f.last_check = now;
            f.error = Some(msg.to_string());
        }
    });
}

/// Keep what a check found removed until it's reviewed: no doubles, none whose file is gone.
fn merge_removed(kept: &mut Vec<music::Removed>, found: Vec<music::Removed>) {
    kept.retain(|r| r.file.is_file());
    for r in found {
        if !kept.iter().any(|k| k.file == r.file) {
            kept.push(r);
        }
    }
}

/// A Spotify playlist: songs added since it was followed are downloaded into its folder
/// (the same one "Download" uses, so a manual sync and a check agree), and songs taken off
/// it that are still in the folder wait in Settings → Following for "Review removed".
fn check_spotify(f: &Followed, settings: &Settings, queue: &Queue) -> anyhow::Result<usize> {
    let url = f.url.as_str();
    let now = now_secs();
    let Some(link) = spotify::parse_link(url) else { return Ok(0) };
    let col = match spotify::fetch(&link, settings.spotify_credentials().as_ref()) {
        Ok(col) => col,
        Err(e) => {
            let text = e.to_string();
            let msg = if text.contains("private, removed") {
                "the playlist is private or was deleted".to_string()
            } else if text.contains("couldn't reach") {
                "couldn't reach Spotify".to_string()
            } else {
                text
            };
            failed(url, now, &msg);
            anyhow::bail!(msg);
        }
    };
    let dir = music::collection_dir(settings, &col);
    let prev = music::read_manifest(&dir);
    // Before `prepare` writes the new list over the old one.
    let removed = music::removed_tracks(&dir, &col);
    let jobs = music::plan_songs(&col, settings, &dir);
    let items: Vec<Item> = col.tracks.iter().map(song_item).collect();
    let have = |id: &str| jobs.iter().any(|j| j.track.id == id && music::have_song(&dir, prev.as_ref(), j));
    let (new, f) = edit(|all| {
        let Some(f) = all.iter_mut().find(|f| f.url == url) else { return (Vec::new(), None) };
        let new = settle(f, &items, have, |id| queue.music_unfinished(id));
        merge_removed(&mut f.removed, removed.clone());
        f.name = col.name.clone();
        f.last_check = now;
        f.last_new = new.len();
        f.error = None;
        (new, Some(f.clone()))
    });
    let Some(f) = f else { return Ok(0) };
    if new.is_empty() && removed.is_empty() {
        return Ok(0);
    }
    // The playlist file and the folder's list follow Spotify's list (renames included).
    let (_dir, jobs) = music::prepare(&col, settings)?;
    let reqs: Vec<DownloadRequest> = jobs
        .into_iter()
        .filter(|j| new.iter().any(|i| i.url == j.track.id))
        .map(DownloadRequest::music)
        .collect();
    let n = reqs.len();
    if n > 0 {
        queue.add_group(&card_title(&f.name, n), false, reqs);
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(url: &str) -> Item {
        Item {
            kind: crate::post::ItemKind::Video,
            title: url.to_string(),
            duration: None,
            thumbnail: None,
            url: url.to_string(),
            playlist_index: None,
            direct_url: None,
            ext: String::new(),
        }
    }

    #[test]
    fn only_new_uploads_and_retries() {
        let mut f = Followed { channel: true, seen: vec!["a".into()], ..Default::default() };
        // Newest first: c is newer than b; b is downloaded first.
        let new = settle(&mut f, &[item("c"), item("b"), item("a")], |_| false, |_| false);
        assert_eq!(new.iter().map(|i| i.url.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert_eq!(f.pending, ["b", "c"]);
        // d is new but was downloaded by hand: seen, not fetched.
        let new = settle(&mut f, &[item("d")], |l| l == "d", |l| l == "b" || l == "c");
        assert!(new.is_empty() && f.seen.contains(&"d".to_string()));
        // b downloaded, c is still in the queue: nothing new.
        let new = settle(&mut f, &[item("c"), item("b"), item("a")], |l| l == "b", |l| l == "c");
        assert!(new.is_empty());
        assert!(f.seen.contains(&"b".to_string()));
        assert_eq!(f.pending, ["c"]);
        // c failed: queued again, three times at most, then given up on.
        for _ in 0..2 {
            let new = settle(&mut f, &[item("c")], |_| false, |_| false);
            assert_eq!(new.len(), 1);
        }
        let new = settle(&mut f, &[item("c")], |_| false, |_| false);
        assert!(new.is_empty());
        assert!(f.seen.contains(&"c".to_string()) && f.pending.is_empty());
    }

    #[test]
    fn spotify_playlists_are_kept_by_their_plain_link() {
        let id = "37i9dQZF1DXcBWIGoYBM5M";
        let want = format!("https://open.spotify.com/playlist/{id}");
        assert_eq!(list_url(&format!("https://open.spotify.com/intl-de/playlist/{id}?si=abc")), want);
        assert_eq!(list_url(&format!("spotify:playlist:{id}")), want);
        assert!(spotify_followable(&want));
        assert!(!spotify_followable("https://open.spotify.com/album/4m2880jivSbbyEGAKfITCa"));
        assert!(Followed { url: want, ..Default::default() }.is_spotify());
        assert!(!Followed { url: "https://www.youtube.com/playlist?list=PL1".into(), ..Default::default() }.is_spotify());
    }

    #[test]
    fn removed_songs_wait_once_each() {
        let dir = std::env::temp_dir().join(format!("neria-follow-removed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("A.mp3");
        let b = dir.join("B.mp3");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let r = |p: &std::path::Path| music::Removed { title: p.display().to_string(), file: p.to_path_buf() };
        let mut kept = vec![r(&a)];
        merge_removed(&mut kept, vec![r(&a), r(&b)]);
        assert_eq!(kept, [r(&a), r(&b)]);
        // A went to the Recycle Bin by hand: it's no longer asked about.
        std::fs::remove_file(&a).unwrap();
        merge_removed(&mut kept, Vec::new());
        assert_eq!(kept, [r(&b)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn channels_are_followed_by_their_videos_tab() {
        assert_eq!(list_url("https://www.youtube.com/@kurzgesagt"), "https://www.youtube.com/@kurzgesagt/videos");
        assert_eq!(list_url("https://youtube.com/channel/UC1/featured"), "https://www.youtube.com/channel/UC1/videos");
        assert_eq!(list_url("https://www.youtube.com/@x/shorts"), "https://www.youtube.com/@x/shorts");
        assert_eq!(list_url("https://www.youtube.com/playlist?list=PL1"), "https://www.youtube.com/playlist?list=PL1");
        let f = Followed { last_check: 100, every: Every::Hourly, ..Default::default() };
        assert!(!f.due(100 + 3_599) && f.due(100 + 3_600));
        assert!(!Followed { paused: true, ..f }.due(u64::MAX));
    }
}
