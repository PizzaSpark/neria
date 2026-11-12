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
use crate::{folders, history, paths, probe};

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
}

impl Followed {
    /// Due for a check at `now` (paused ones never are).
    pub fn due(&self, now: u64) -> bool {
        !self.paused && now >= self.last_check.saturating_add(self.every.secs())
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

/// The link a check lists for `url` (a channel's Videos tab), also the key it's kept by.
pub fn list_url(url: &str) -> String {
    probe::channel_videos_url(url).unwrap_or_else(|| url.trim().to_string())
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
    let listed = probe::list_entries(&f.url, settings, f.channel.then_some(CHANNEL_ENTRIES));
    let now = now_secs();
    let items = match listed {
        Ok(items) => items,
        Err(e) => {
            let msg = e.downcast_ref::<probe::LinkProblem>().map_or_else(|| format!("{e:#}"), |p| p.title.clone());
            edit(|all| {
                if let Some(f) = all.iter_mut().find(|f| f.url == url) {
                    f.last_check = now;
                    f.error = Some(msg.clone());
                }
            });
            anyhow::bail!(msg);
        }
    };
    let (new, f) = edit(|all| {
        let Some(f) = all.iter_mut().find(|f| f.url == url) else { return (Vec::new(), None) };
        let new = settle(f, &items, history::ever_downloaded, |link| queue.find_unfinished(None, link).is_some());
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
    let title = if n == 1 { format!("{} · 1 new", f.name) } else { format!("{} · {n} new", f.name) };
    queue.add_group(&title, false, reqs);
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
