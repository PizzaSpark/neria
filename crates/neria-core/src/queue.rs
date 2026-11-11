//! Download queue with a concurrency limit, cancel and retry.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::download::{self, DlEvent, DownloadRequest};
use crate::settings::Quality;
use crate::settings::Settings;
use crate::spotify::Track;
use crate::ytmusic::Candidate;
use crate::{disk, fingerprint, gif, history, music, paths, probe, proc, report, tools};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Downloading,
    Processing,
    Done,
    Failed,
    Cancelled,
    /// Stopped by the user; yt-dlp's `.part` files stay so Resume picks up where it left off.
    Paused,
}

impl JobState {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Downloading | Self::Processing)
    }

    /// Nothing more will happen to it (paused jobs still will).
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug)]
pub struct JobSnapshot {
    pub id: u64,
    /// Jobs added together (an album/playlist) share a group, so the UI can keep them in order.
    pub group: u64,
    /// The playlist/post/album name of the group (shown on its card).
    pub group_title: Option<String>,
    pub title: String,
    pub state: JobState,
    /// Overall progress 0..1 across all streams.
    pub progress: f32,
    pub stage: u32,
    pub stages: u32,
    pub downloaded: Option<f64>,
    pub total: Option<f64>,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
    pub postprocessor: Option<String>,
    pub error: Option<String>,
    pub file: Option<PathBuf>,
    /// Shown on a finished row, e.g. a Spotify match worth double-checking.
    pub note: Option<String>,
    /// Thumbnail (video) or cover (Spotify song) URL for the row.
    pub thumbnail: Option<String>,
    /// A Spotify track (the UI offers "pick another match").
    pub music: bool,
    /// An X (Twitter) "GIF" (a short silent MP4), saved as a real .gif when the setting is on.
    pub gif: bool,
    /// Every X GIF this job saved (a whole post can hold several), to convert afterwards.
    pub gif_files: Vec<PathBuf>,
    /// The item yt-dlp is on right now is a GIF (its file follows).
    pending_gif: bool,
    /// Spotify track audio check: Some(source) when it matched that preview,
    /// Some("none") when no preview existed anywhere, None when not checked.
    pub verified: Option<&'static str>,
    /// `Extractor:id` of what's being downloaded, once known.
    pub key: Option<String>,
    /// Brought back from the last session (the UI adds it whatever its state).
    pub restored: bool,
}

impl JobSnapshot {
    fn new(id: u64, group: u64, title: String) -> Self {
        Self {
            id,
            group,
            group_title: None,
            title,
            state: JobState::Queued,
            progress: 0.0,
            stage: 0,
            stages: 1,
            downloaded: None,
            total: None,
            speed: None,
            eta: None,
            postprocessor: None,
            error: None,
            file: None,
            note: None,
            thumbnail: None,
            music: false,
            gif: false,
            gif_files: Vec::new(),
            pending_gif: false,
            verified: None,
            key: None,
            restored: false,
        }
    }

    /// A fresh start for the same job (retry, resume, rematch), keeping what the row shows.
    fn restart(&self) -> Self {
        let mut s = Self::new(self.id, self.group, self.title.clone());
        s.group_title = self.group_title.clone();
        s.music = self.music;
        s.gif = self.gif;
        s.thumbnail = self.thumbnail.clone();
        s.key = self.key.clone();
        s
    }

    fn apply(&mut self, ev: DlEvent) {
        match ev {
            DlEvent::Info { title, streams, thumbnail, gif, key } => {
                self.gif |= gif;
                if key.is_some() {
                    self.key = key;
                }
                self.pending_gif = gif;
                if !title.is_empty() {
                    self.title = title;
                }
                // A Spotify cover set up front beats the YouTube thumbnail.
                if self.thumbnail.is_none() {
                    self.thumbnail = thumbnail;
                }
                self.stages = streams.max(1);
            }
            DlEvent::Progress { finished, downloaded, total, speed, eta, fragment } => {
                self.state = JobState::Downloading;
                if finished {
                    self.stage = (self.stage + 1).min(self.stages);
                    self.speed = None;
                    self.eta = None;
                    self.progress = self.stage as f32 / self.stages as f32;
                    return;
                }
                self.downloaded = downloaded;
                self.total = total;
                self.speed = speed;
                self.eta = eta;
                let frac = match (downloaded, total, fragment) {
                    (Some(d), Some(t), _) if t > 0.0 => d / t,
                    (_, _, Some((i, n))) => i / n,
                    _ => 0.0,
                };
                let stage = self.stage.min(self.stages - 1) as f64;
                self.progress = ((stage + frac.clamp(0.0, 1.0)) / self.stages as f64) as f32;
            }
            DlEvent::Postprocess(name) => {
                self.state = JobState::Processing;
                self.progress = 1.0;
                self.postprocessor = Some(name);
            }
            DlEvent::File(path) => {
                if self.pending_gif && !self.gif_files.contains(&path) {
                    self.gif_files.push(path.clone());
                }
                self.file = Some(path);
            }
            DlEvent::Error(msg) => self.error = Some(msg),
            DlEvent::Log(_) => {}
        }
    }
}

struct Entry {
    snap: JobSnapshot,
    req: DownloadRequest,
    cancel: Arc<AtomicBool>,
    /// Set with `cancel` when the user pauses: the job ends as Paused, keeping its files.
    paused: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    /// Files yt-dlp said it would write, so Cancel can remove the half-done pieces.
    dests: Vec<PathBuf>,
    /// Spotify jobs: YouTube Music candidates (best first) and the URL used.
    candidates: Vec<Candidate>,
    chosen: Option<String>,
    /// Last lines of yt-dlp output, for "Report a problem".
    log: VecDeque<String>,
}

impl Entry {
    fn new(snap: JobSnapshot, req: DownloadRequest) -> Self {
        Self {
            snap,
            req,
            cancel: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
            dests: Vec::new(),
            candidates: Vec::new(),
            chosen: None,
            log: VecDeque::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MatchInfo {
    pub track: Track,
    pub candidates: Vec<Candidate>,
    pub chosen: Option<String>,
}

type Notify = Box<dyn Fn(JobSnapshot) + Send + Sync>;

const SPACE_ADVICE: &str = "Free some up or change the folder in Settings.";

/// Remove what an interrupted download of `final_path` left behind: `.part`/`.ytdl`
/// files, per-format pieces (`name.f401.mp4`) and the thumbnail. Never the finished file.
fn remove_leftovers(final_path: &std::path::Path) {
    let (Some(dir), Some(stem)) = (final_path.parent(), final_path.file_stem()) else { return };
    let prefix = format!("{}.", stem.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        let piece = rest.starts_with('f')
            && rest[1..].split('.').next().is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let leftover =
            rest.ends_with(".part") || rest.ends_with(".ytdl") || piece || ["webp", "jpg", "png"].contains(&rest);
        if leftover && entry.path() != final_path {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Jobs added together: a playlist, the items picked from a post, a Spotify album.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Group {
    title: String,
    /// Keep `<title>.m3u8` in the group's folder up to date (playlists).
    m3u: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
enum SavedState {
    Waiting,
    Paused,
    Done,
    Failed,
    Cancelled,
}

/// One row of `queue.json`.
#[derive(Serialize, Deserialize)]
struct SavedJob {
    group: u64,
    req: DownloadRequest,
    title: String,
    state: SavedState,
    #[serde(default)]
    downloaded: Option<f64>,
    #[serde(default)]
    total: Option<f64>,
    #[serde(default)]
    progress: f32,
    #[serde(default)]
    file: Option<PathBuf>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    dests: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Default)]
struct SavedQueue {
    groups: HashMap<u64, Group>,
    jobs: Vec<SavedJob>,
}

fn queue_file() -> PathBuf {
    paths::app_dir().join("queue.json")
}

/// Video/audio files with this name already in `dir` (a playlist item saved before).
fn existing_media(dir: &std::path::Path, stem: &str, quality: Quality, gif: bool) -> Option<PathBuf> {
    const VIDEO: &[&str] = &["mp4", "mkv", "webm", "mov", "m4v"];
    const AUDIO: &[&str] = &["mp3", "m4a", "opus", "ogg", "flac", "wav", "aac"];
    let exts: &[&str] = if gif {
        &["gif", "mp4"]
    } else if quality == Quality::Audio {
        AUDIO
    } else {
        VIDEO
    };
    exts.iter().map(|e| dir.join(format!("{stem}.{e}"))).find(|p| p.is_file())
}

/// A playlist's file list: this download's items, plus songs listed before that are still
/// in the folder (an earlier download of other items). Numbered names (`01 …`) sort by
/// number, which is the playlist order.
fn merge_m3u(
    mut files: Vec<(String, f64, String)>,
    earlier: Vec<(String, f64, String)>,
    exists: impl Fn(&str) -> bool,
) -> Vec<(String, f64, String)> {
    for old in earlier {
        if !files.iter().any(|f| f.2 == old.2) && exists(&old.2) {
            files.push(old);
        }
    }
    if files.iter().all(|f| f.2.starts_with(|c: char| c.is_ascii_digit())) {
        files.sort_by(|a, b| a.2.cmp(&b.2));
    }
    files
}

fn is_picture(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|e| {
        ["jpg", "jpeg", "png", "webp", "gif", "heic"].contains(&e.to_string_lossy().to_lowercase().as_str())
    })
}

struct Inner {
    entries: Mutex<Vec<Entry>>,
    groups: Mutex<HashMap<u64, Group>>,
    /// Jobs only start once this is set (the app waits for its tools first).
    ready: AtomicBool,
    /// Keep `queue.json` up to date (the app; not the command-line examples).
    persist: AtomicBool,
    /// Shutting down: jobs being stopped now are saved as they were, not as cancelled.
    closing: AtomicBool,
    settings: Arc<Mutex<Settings>>,
    notify: Notify,
    next_id: AtomicU64,
    /// When yt-dlp was last updated because a site broke it (one update at a time).
    ytdlp_updated: Mutex<Option<Instant>>,
}

/// Cheap to clone; all clones share the same queue.
#[derive(Clone)]
pub struct Queue {
    inner: Arc<Inner>,
}

impl Queue {
    /// `notify` is called (from any thread) whenever a job changes.
    pub fn new(settings: Arc<Mutex<Settings>>, notify: impl Fn(JobSnapshot) + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                entries: Mutex::new(Vec::new()),
                groups: Mutex::new(HashMap::new()),
                ready: AtomicBool::new(true),
                persist: AtomicBool::new(false),
                closing: AtomicBool::new(false),
                settings,
                notify: Box::new(notify),
                next_id: AtomicU64::new(1),
                ytdlp_updated: Mutex::new(None),
            }),
        }
    }

    pub fn add(&self, req: DownloadRequest) -> u64 {
        self.add_to_group(req, 0)
    }

    /// Add several jobs that belong together (kept in order in the UI).
    pub fn add_batch(&self, reqs: impl IntoIterator<Item = DownloadRequest>) {
        self.add_group("", false, reqs);
    }

    /// Add a playlist/post/album: its jobs share a card in the UI titled `title`. With `m3u`,
    /// `<title>.m3u8` in their folder lists the saved files in order.
    pub fn add_group(&self, title: &str, m3u: bool, reqs: impl IntoIterator<Item = DownloadRequest>) {
        let group = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.groups.lock().unwrap().insert(group, Group { title: title.to_string(), m3u });
        for r in reqs {
            self.add_to_group(r, group);
        }
        self.save();
    }

    fn add_to_group(&self, req: DownloadRequest, group: u64) -> u64 {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let title = if req.title_hint.is_empty() { req.url.clone() } else { req.title_hint.clone() };
        let mut snap = JobSnapshot::new(id, group, title);
        snap.group_title = self.group_title(group);
        snap.music = req.music.is_some();
        snap.gif = req.gif;
        snap.key = req.key.clone();
        snap.thumbnail = req.music.as_ref().and_then(|m| m.track.cover_url.clone()).or_else(|| req.thumbnail.clone());
        (self.inner.notify)(snap.clone());
        self.inner.entries.lock().unwrap().push(Entry::new(snap, req));
        if group == 0 {
            self.save();
        }
        self.pump();
        id
    }

    fn group_title(&self, group: u64) -> Option<String> {
        let groups = self.inner.groups.lock().unwrap();
        groups.get(&group).map(|g| g.title.clone()).filter(|t| !t.is_empty())
    }

    /// Where a group's files go (for "Show folder" on its card).
    pub fn group_dir(&self, group: u64) -> Option<PathBuf> {
        let settings = self.inner.settings.lock().unwrap().clone();
        let entries = self.inner.entries.lock().unwrap();
        let e = entries.iter().find(|e| e.snap.group == group)?;
        Some(download::out_dir(&e.req, &settings))
    }

    /// Hold back (false) or start (true) running jobs. Rows can be added either way.
    pub fn set_ready(&self, ready: bool) {
        self.inner.ready.store(ready, Ordering::SeqCst);
        if ready {
            self.pump();
        }
    }

    /// Keep the queue in `queue.json` from now on, and bring back what was left in it
    /// last time: waiting and interrupted downloads wait again, paused ones stay paused.
    pub fn enable_persistence(&self) {
        self.inner.persist.store(true, Ordering::SeqCst);
        let saved: SavedQueue =
            std::fs::read(queue_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut new_group: HashMap<u64, u64> = HashMap::new();
        for job in saved.jobs {
            let group = match job.group {
                0 => 0,
                old => *new_group.entry(old).or_insert_with(|| {
                    let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
                    let g = saved.groups.get(&old).cloned().unwrap_or(Group { title: String::new(), m3u: false });
                    self.inner.groups.lock().unwrap().insert(id, g);
                    id
                }),
            };
            let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            let mut snap = JobSnapshot::new(id, group, job.title);
            snap.group_title = self.group_title(group);
            snap.music = job.req.music.is_some();
            snap.gif = job.req.gif;
            snap.thumbnail = job.thumbnail;
            snap.key = job.key;
            snap.file = job.file;
            snap.note = job.note;
            snap.error = job.error;
            snap.downloaded = job.downloaded;
            snap.total = job.total;
            snap.progress = job.progress;
            snap.restored = true;
            snap.state = match job.state {
                SavedState::Waiting => JobState::Queued,
                SavedState::Paused => JobState::Paused,
                SavedState::Done => JobState::Done,
                SavedState::Failed => JobState::Failed,
                SavedState::Cancelled => JobState::Cancelled,
            };
            if snap.state == JobState::Done {
                snap.progress = 1.0;
            }
            (self.inner.notify)(snap.clone());
            snap.restored = false;
            let mut entry = Entry::new(snap, job.req);
            entry.dests = job.dests;
            self.inner.entries.lock().unwrap().push(entry);
        }
        self.save();
        self.pump();
    }

    /// Write `queue.json`: every unfinished job, plus the finished ones of groups that
    /// still have unfinished jobs (so the playlist comes back whole).
    fn save(&self) {
        let entries = self.inner.entries.lock().unwrap();
        self.save_locked(&entries);
    }

    fn save_locked(&self, entries: &[Entry]) {
        if !self.inner.persist.load(Ordering::SeqCst) || self.inner.closing.load(Ordering::SeqCst) {
            return;
        }
        let open: HashSet<u64> =
            entries.iter().filter(|e| e.snap.group != 0 && !e.snap.state.is_finished()).map(|e| e.snap.group).collect();
        let jobs: Vec<SavedJob> = entries
            .iter()
            .filter(|e| !e.snap.state.is_finished() || open.contains(&e.snap.group))
            .map(|e| SavedJob {
                group: e.snap.group,
                req: e.req.clone(),
                title: e.snap.title.clone(),
                state: match e.snap.state {
                    JobState::Paused => SavedState::Paused,
                    JobState::Done => SavedState::Done,
                    JobState::Failed => SavedState::Failed,
                    JobState::Cancelled => SavedState::Cancelled,
                    _ => SavedState::Waiting,
                },
                downloaded: e.snap.downloaded,
                total: e.snap.total,
                progress: e.snap.progress,
                file: e.snap.file.clone(),
                note: e.snap.note.clone(),
                error: e.snap.error.clone(),
                thumbnail: e.snap.thumbnail.clone(),
                key: e.snap.key.clone(),
                dests: e.dests.clone(),
            })
            .collect();
        if jobs.is_empty() {
            let _ = std::fs::remove_file(queue_file());
            return;
        }
        let used: HashSet<u64> = jobs.iter().map(|j| j.group).collect();
        let groups = self.inner.groups.lock().unwrap();
        let saved = SavedQueue {
            groups: groups.iter().filter(|(id, _)| used.contains(id)).map(|(id, g)| (*id, g.clone())).collect(),
            jobs,
        };
        if let Ok(json) = serde_json::to_vec(&saved) {
            let tmp = queue_file().with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, queue_file());
            }
        }
    }

    /// A download of this video (by key, else the same link) that's waiting, running or
    /// paused, for the duplicate check.
    pub fn find_unfinished(&self, key: Option<&str>, url: &str) -> Option<JobSnapshot> {
        let entries = self.inner.entries.lock().unwrap();
        entries
            .iter()
            .filter(|e| !e.snap.state.is_finished() && e.req.section.is_none() && e.req.music.is_none())
            .find(|e| match key {
                Some(k) => e.snap.key.as_deref() == Some(k) || e.req.url == url,
                None => e.req.url == url,
            })
            .map(|e| e.snap.clone())
    }

    /// For the "pick another match" dialog: the Spotify track, the candidates
    /// found so far (may be empty, e.g. for "Already downloaded" rows) and the
    /// URL currently used.
    pub fn match_info(&self, id: u64) -> Option<MatchInfo> {
        let entries = self.inner.entries.lock().unwrap();
        let e = entries.iter().find(|e| e.snap.id == id)?;
        let job = e.req.music.as_ref()?;
        Some(MatchInfo { track: job.track.clone(), candidates: e.candidates.clone(), chosen: e.chosen.clone() })
    }

    /// Re-download a Spotify track from a URL the user picked, replacing the file.
    pub fn rematch(&self, id: u64, url: String) -> bool {
        {
            let mut entries = self.inner.entries.lock().unwrap();
            let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) else { return false };
            if e.snap.state.is_active() {
                return false;
            }
            let Some(job) = e.req.music.as_mut() else { return false };
            job.forced = Some(url.clone());
            e.chosen = Some(url);
            e.snap = e.snap.restart();
            e.cancel = Arc::new(AtomicBool::new(false));
            e.paused = Arc::new(AtomicBool::new(false));
            (self.inner.notify)(e.snap.clone());
        }
        self.pump();
        true
    }

    /// A plain-text problem report for one row (see `report`), ready to paste.
    pub fn report(&self, id: u64) -> Option<String> {
        let settings = self.inner.settings.lock().unwrap().clone();
        let entries = self.inner.entries.lock().unwrap();
        let e = entries.iter().find(|e| e.snap.id == id)?;
        let url = e.chosen.as_deref().unwrap_or(&e.req.url);
        let spotify = e.req.music.as_ref().map(|m| {
            let t = &m.track;
            format!("{} - {} (https://open.spotify.com/track/{})", t.artist_line(), t.title, t.id)
        });
        let log: Vec<String> = e.log.iter().cloned().collect();
        let error = e.snap.error.clone().or_else(|| match e.snap.state {
            JobState::Failed => Some("Failed (no error message)".into()),
            JobState::Cancelled => Some("Cancelled".into()),
            _ => None,
        });
        Some(report::job_report(
            &settings,
            &report::JobDetails {
                url,
                title: &e.snap.title,
                error: error.as_deref(),
                note: e.snap.note.as_deref(),
                spotify,
                log: &log,
            },
        ))
    }

    fn push_log(&self, id: u64, line: String) {
        let mut entries = self.inner.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) {
            if e.log.len() >= report::LOG_LINES {
                e.log.pop_front();
            }
            e.log.push_back(line);
        }
    }

    fn set_match(&self, id: u64, candidates: Vec<Candidate>, chosen: Option<String>) {
        let mut entries = self.inner.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) {
            e.candidates = candidates;
            e.chosen = chosen;
        }
    }

    pub fn cancel(&self, id: u64) {
        let mut entries = self.inner.entries.lock().unwrap();
        let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) else { return };
        match e.snap.state {
            JobState::Queued | JobState::Paused => {
                if e.snap.state == JobState::Paused {
                    // Its half-done files were kept for Resume; nobody needs them now.
                    for path in &e.dests {
                        remove_leftovers(path);
                    }
                }
                e.snap.state = JobState::Cancelled;
                e.snap.speed = None;
                e.snap.eta = None;
                (self.inner.notify)(e.snap.clone());
                self.save_locked(&entries);
            }
            JobState::Downloading | JobState::Processing => {
                e.cancel.store(true, Ordering::SeqCst);
                if let Some(child) = e.child.lock().unwrap().as_mut() {
                    proc::kill_tree(child);
                }
            }
            _ => {}
        }
    }

    /// Stop a download but keep what it has so far. Waiting jobs just don't start.
    /// Spotify songs can't be paused mid-way (small, and matched/checked in steps).
    pub fn pause(&self, id: u64) {
        let mut entries = self.inner.entries.lock().unwrap();
        let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) else { return };
        match e.snap.state {
            JobState::Queued => {
                e.snap.state = JobState::Paused;
                (self.inner.notify)(e.snap.clone());
                self.save_locked(&entries);
            }
            // The job's thread sees the flags, ends, and marks it Paused.
            JobState::Downloading if e.req.music.is_none() => {
                e.paused.store(true, Ordering::SeqCst);
                e.cancel.store(true, Ordering::SeqCst);
                if let Some(child) = e.child.lock().unwrap().as_mut() {
                    proc::kill_tree(child);
                }
            }
            _ => {}
        }
    }

    /// Carry on with a paused job (it waits for a free slot like any other).
    pub fn resume(&self, id: u64) {
        {
            let mut entries = self.inner.entries.lock().unwrap();
            let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) else { return };
            if e.snap.state != JobState::Paused {
                return;
            }
            e.snap.state = JobState::Queued;
            e.snap.stage = 0;
            e.snap.speed = None;
            e.snap.eta = None;
            e.cancel = Arc::new(AtomicBool::new(false));
            e.paused = Arc::new(AtomicBool::new(false));
            (self.inner.notify)(e.snap.clone());
            self.save_locked(&entries);
        }
        self.pump();
    }

    /// Ids of jobs in `group` (all jobs for `None`) matching `pred`.
    fn ids_where(&self, group: Option<u64>, pred: impl Fn(&JobSnapshot) -> bool) -> Vec<u64> {
        let entries = self.inner.entries.lock().unwrap();
        entries.iter().filter(|e| group.is_none_or(|g| e.snap.group == g) && pred(&e.snap)).map(|e| e.snap.id).collect()
    }

    /// Pause everything (or one group): waiting jobs first, so no new one starts
    /// when a running one stops.
    pub fn pause_all(&self, group: Option<u64>) {
        for id in self.ids_where(group, |s| s.state == JobState::Queued) {
            self.pause(id);
        }
        for id in self.ids_where(group, |s| s.state == JobState::Downloading) {
            self.pause(id);
        }
    }

    pub fn resume_all(&self, group: Option<u64>) {
        for id in self.ids_where(group, |s| s.state == JobState::Paused) {
            self.resume(id);
        }
    }

    /// Cancel every unfinished job of a group (waiting ones first).
    pub fn cancel_group(&self, group: u64) {
        for id in self.ids_where(Some(group), |s| matches!(s.state, JobState::Queued | JobState::Paused)) {
            self.cancel(id);
        }
        for id in self.ids_where(Some(group), |s| s.state.is_active()) {
            self.cancel(id);
        }
    }

    /// Retry a group's failed and cancelled jobs.
    pub fn retry_group(&self, group: u64) {
        for id in self.ids_where(Some(group), |s| matches!(s.state, JobState::Failed | JobState::Cancelled)) {
            self.retry(id);
        }
    }

    pub fn retry(&self, id: u64) {
        {
            let mut entries = self.inner.entries.lock().unwrap();
            let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) else { return };
            if !matches!(e.snap.state, JobState::Failed | JobState::Cancelled) {
                return;
            }
            e.snap = e.snap.restart();
            e.snap.gif = e.req.gif;
            e.cancel = Arc::new(AtomicBool::new(false));
            e.paused = Arc::new(AtomicBool::new(false));
            (self.inner.notify)(e.snap.clone());
            self.save_locked(&entries);
        }
        self.pump();
    }

    /// Remove a finished job from the list. Returns false for unfinished ones.
    pub fn remove(&self, id: u64) -> bool {
        let mut entries = self.inner.entries.lock().unwrap();
        match entries.iter().position(|e| e.snap.id == id) {
            Some(i) if entries[i].snap.state.is_finished() => {
                entries.remove(i);
                self.save_locked(&entries);
                true
            }
            _ => false,
        }
    }

    /// Remove every finished, failed or cancelled job (of one group, or all); returns their ids.
    /// A playlist still downloading keeps its finished rows, so its card stays whole.
    pub fn clear_finished(&self, group: Option<u64>) -> Vec<u64> {
        let mut entries = self.inner.entries.lock().unwrap();
        let open: HashSet<u64> =
            entries.iter().filter(|e| e.snap.group != 0 && !e.snap.state.is_finished()).map(|e| e.snap.group).collect();
        let gone = |e: &Entry| {
            e.snap.state.is_finished()
                && match group {
                    Some(g) => e.snap.group == g,
                    None => !open.contains(&e.snap.group),
                }
        };
        let removed = entries.iter().filter(|e| gone(e)).map(|e| e.snap.id).collect();
        entries.retain(|e| !gone(e));
        self.save_locked(&entries);
        removed
    }

    pub fn file_of(&self, id: u64) -> Option<PathBuf> {
        let entries = self.inner.entries.lock().unwrap();
        entries.iter().find(|e| e.snap.id == id).and_then(|e| e.snap.file.clone())
    }

    /// Closing Neria: save the queue as it is (running jobs come back next time), then
    /// stop every yt-dlp/ffmpeg so none is left running.
    pub fn shutdown(&self) {
        self.save();
        self.inner.closing.store(true, Ordering::SeqCst);
        self.cancel_all();
    }

    /// Cancel everything that's running or waiting.
    pub fn cancel_all(&self) {
        let ids: Vec<u64> = {
            let entries = self.inner.entries.lock().unwrap();
            entries.iter().filter(|e| e.snap.state.is_active()).map(|e| e.snap.id).collect()
        };
        for id in ids {
            self.cancel(id);
        }
    }

    /// Start queued jobs while under the concurrency limit.
    fn pump(&self) {
        if !self.inner.ready.load(Ordering::SeqCst) || self.inner.closing.load(Ordering::SeqCst) {
            return;
        }
        let settings = self.inner.settings.lock().unwrap().clone();
        let max = settings.max_concurrent.clamp(1, 8) as usize;
        let mut entries = self.inner.entries.lock().unwrap();
        let mut running =
            entries.iter().filter(|e| matches!(e.snap.state, JobState::Downloading | JobState::Processing)).count();

        for e in entries.iter_mut() {
            if running >= max {
                break;
            }
            if e.snap.state != JobState::Queued {
                continue;
            }
            e.snap.state = JobState::Downloading;
            (self.inner.notify)(e.snap.clone());
            running += 1;

            let (queue, id, req, cancel, child) =
                (self.clone(), e.snap.id, e.req.clone(), e.cancel.clone(), e.child.clone());
            let paused = e.paused.clone();
            let settings = settings.clone();
            thread::spawn(move || queue.run_job(id, req, settings, cancel, paused, child));
        }
    }

    fn run_job(
        &self,
        id: u64,
        mut req: DownloadRequest,
        settings: Settings,
        cancel: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        child: Arc<Mutex<Option<Child>>>,
    ) {
        // A retry starts a fresh log.
        if let Some(e) = self.inner.entries.lock().unwrap().iter_mut().find(|e| e.snap.id == id) {
            e.log.clear();
        }
        // Spotify track picked by hand: download that URL, overwriting the old file.
        if let Some(url) = req.music.as_ref().and_then(|m| m.forced.clone()) {
            req.url = url;
        } else if let Some(job) = req.music.clone() {
            // Spotify track: resume if already on disk, else find it on YouTube Music.
            if let Some(existing) = music::existing_file(&job.dir, &job.stem) {
                self.update(id, true, |s| {
                    s.state = JobState::Done;
                    s.progress = 1.0;
                    s.file = Some(existing);
                    s.postprocessor = Some("AlreadyHave".into());
                });
                self.pump();
                return;
            }
            self.update(id, true, |s| {
                s.state = JobState::Processing;
                s.postprocessor = Some("Matching".into());
            });
            let found = music::find_match(&job);
            if let Ok((all, best)) = &found {
                self.set_match(id, all.clone(), best.as_ref().map(|c| c.url()));
            }
            match found {
                Ok((_, Some(c))) if !cancel.load(Ordering::SeqCst) => {
                    req.url = c.url();
                    if c.score < music::CONFIDENT {
                        let note = format!("Check match: \"{}\" by {}", c.title, c.artists);
                        self.update(id, false, |s| s.note = Some(note));
                    }
                }
                result => {
                    let cancelled = cancel.load(Ordering::SeqCst);
                    let paused = paused.load(Ordering::SeqCst);
                    self.update(id, true, |s| match result {
                        _ if paused => s.state = JobState::Paused,
                        _ if cancelled => s.state = JobState::Cancelled,
                        Err(e) => {
                            s.state = JobState::Failed;
                            s.error = Some(e.to_string());
                        }
                        Ok(_) => {
                            s.state = JobState::Failed;
                            s.error = Some("No confident match on YouTube Music. Pick one yourself.".into());
                        }
                    });
                    self.pump();
                    return;
                }
            }
        }

        let dir = download::out_dir(&req, &settings);

        // A playlist/post item that's already in its folder from last time: keep it.
        if req.music.is_none()
            && req.direct.is_none()
            && req.section.is_none()
            && !req.force
            && let Some(stem) = &req.stem
            && let Some(existing) = existing_media(&dir, stem, req.quality, req.gif)
        {
            self.update(id, true, |s| {
                s.state = JobState::Done;
                s.progress = 1.0;
                s.file = Some(existing);
                s.postprocessor = Some("AlreadyHave".into());
            });
            self.write_group_m3u(id);
            self.pump();
            return;
        }

        let mut result = match disk::check(&dir, 0) {
            Ok(()) if req.direct.is_some() => self.download_direct(id, &req, &dir, &cancel),
            Ok(()) => self.download_once(id, &req, &settings, &dir, &cancel, &child),
            Err(msg) => Err(anyhow::anyhow!("{msg} {SPACE_ADVICE}")),
        };

        // The site changed under yt-dlp: get a newer one right away and try once more,
        // instead of failing until tomorrow's daily update.
        if !matches!(result, Ok(true))
            && req.direct.is_none()
            && !cancel.load(Ordering::SeqCst)
            && self.broke_ytdlp(id, &result)
            && self.update_ytdlp(id)
        {
            self.update(id, true, |s| {
                s.error = None;
                s.state = JobState::Downloading;
            });
            result = self.download_once(id, &req, &settings, &dir, &cancel, &child);
        }

        // Spotify track: check it's the recording Spotify plays (30 s preview
        // fingerprint). A different recording (re-recorded, live, cover…)
        // triggers up to two other good candidates before we give up and flag it.
        if let (Some(job), Ok(true)) = (req.music.clone(), &result)
            && job.verify
            && !cancel.load(Ordering::SeqCst)
        {
            result = self.verify_and_retry(id, &mut req, &job, &settings, &cancel, &child, result);
        }

        let cancelled = cancel.load(Ordering::SeqCst);
        let paused = paused.load(Ordering::SeqCst);

        // X "GIF" (really a silent MP4): save it as the .gif people expect.
        let mut gif_error = None;
        if matches!(result, Ok(true))
            && !cancelled
            && req.music.is_none()
            && settings.x_gifs_as_gif
            && req.quality != Quality::Audio
        {
            // Each GIF the job saved; a GIF picked by itself counts even if yt-dlp didn't say.
            let videos: Vec<PathBuf> = {
                let entries = self.inner.entries.lock().unwrap();
                entries
                    .iter()
                    .find(|e| e.snap.id == id)
                    .map(|e| {
                        let mut v = e.snap.gif_files.clone();
                        if e.req.gif
                            && let Some(f) = &e.snap.file
                            && !v.contains(f)
                        {
                            v.push(f.clone());
                        }
                        v.retain(|f| f.extension().is_some_and(|x| !x.eq_ignore_ascii_case("gif")));
                        v
                    })
                    .unwrap_or_default()
            };
            for video in videos {
                self.update(id, true, |s| {
                    s.state = JobState::Processing;
                    s.postprocessor = Some("MakingGif".into());
                });
                match gif::from_video(&video) {
                    Ok(out) => {
                        let _ = std::fs::remove_file(&video);
                        self.update(id, false, |s| {
                            if s.file.as_ref() == Some(&video) {
                                s.file = Some(out);
                            }
                        });
                    }
                    Err(e) => gif_error = Some(format!("Saved as MP4: couldn't make the GIF ({e})")),
                }
            }
        }

        // Spotify track downloaded: add Spotify tags, cover art and lyrics.
        let mut tag_error = None;
        if let (Some(job), Ok(true), false) = (&req.music, &result, cancelled) {
            let file = {
                let entries = self.inner.entries.lock().unwrap();
                entries.iter().find(|e| e.snap.id == id).and_then(|e| e.snap.file.clone())
            }
            .or_else(|| music::existing_file(&job.dir, &job.stem));
            if let Some(file) = file {
                self.update(id, true, |s| {
                    s.state = JobState::Processing;
                    s.postprocessor = Some("Tagging".into());
                });
                tag_error = music::finish(&file, job).err().map(|e| e.to_string());
            }
        }

        self.update(id, true, |s| {
            if let Some(e) = &tag_error {
                s.error = Some(format!("Saved, but tagging failed: {e}"));
            }
            if let Some(e) = &gif_error {
                s.note = Some(e.clone());
            }
            s.speed = None;
            s.eta = None;
            match result {
                _ if paused => s.state = JobState::Paused,
                _ if cancelled => s.state = JobState::Cancelled,
                Ok(true) => {
                    s.state = JobState::Done;
                    s.progress = 1.0;
                }
                Ok(false) => s.state = JobState::Failed,
                Err(e) => {
                    s.state = JobState::Failed;
                    s.error = Some(e.to_string());
                }
            }
        });
        if cancelled && !paused && !self.inner.closing.load(Ordering::SeqCst) {
            // Cancelled for good: remove the half-done pieces (never a finished file).
            let dests = self.dests_of(id);
            thread::sleep(Duration::from_millis(300));
            for path in &dests {
                remove_leftovers(path);
            }
        }
        if !cancelled {
            self.remember(id, &req);
            self.write_group_m3u(id);
        }
        self.pump();
    }

    fn dests_of(&self, id: u64) -> Vec<PathBuf> {
        let entries = self.inner.entries.lock().unwrap();
        entries.iter().find(|e| e.snap.id == id).map(|e| e.dests.clone()).unwrap_or_default()
    }

    /// A finished download goes in the history, for the duplicate check.
    fn remember(&self, id: u64, req: &DownloadRequest) {
        if req.section.is_some() || req.music.is_some() {
            return;
        }
        let snap = {
            let entries = self.inner.entries.lock().unwrap();
            entries.iter().find(|e| e.snap.id == id).map(|e| e.snap.clone())
        };
        let Some(s) = snap.filter(|s| s.state == JobState::Done) else { return };
        let (Some(key), Some(file)) = (s.key.or_else(|| req.key.clone()), s.file) else { return };
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        history::record(history::Record { key, url: req.url.clone(), file, title: s.title, at });
    }

    /// Rewrite the playlist file of this job's group: the saved files, in playlist order.
    fn write_group_m3u(&self, id: u64) {
        let (dir, title, files) = {
            let entries = self.inner.entries.lock().unwrap();
            let Some(group) = entries.iter().find(|e| e.snap.id == id).map(|e| e.snap.group).filter(|g| *g != 0) else {
                return;
            };
            let Some(title) = self.inner.groups.lock().unwrap().get(&group).filter(|g| g.m3u).map(|g| g.title.clone())
            else {
                return;
            };
            let Some(dir) = entries.iter().find(|e| e.snap.group == group).and_then(|e| e.req.dir.clone()) else {
                return;
            };
            // Every item of the group that's in the folder: saved now, or kept from before.
            let files: Vec<(String, f64, String)> = entries
                .iter()
                .filter(|e| e.snap.group == group)
                .filter_map(|e| {
                    let file = match (&e.snap.file, &e.req.stem) {
                        (Some(f), _) if e.snap.state == JobState::Done => Some(f.clone()),
                        (_, Some(stem)) => existing_media(&dir, stem, e.req.quality, e.req.gif),
                        _ => None,
                    }
                    .filter(|f| f.parent() == Some(dir.as_path()) && !is_picture(f) && f.is_file())?;
                    let name = file.file_name()?.to_string_lossy().into_owned();
                    Some((e.snap.title.clone(), e.req.duration.unwrap_or(-1.0), name))
                })
                .collect();
            (dir, title, files)
        };
        if files.is_empty() {
            return;
        }
        let path = dir.join(format!("{}.m3u8", music::sanitize(&title)));
        let earlier = music::read_m3u(&path);
        let files = merge_m3u(files, earlier, |name| dir.join(name).is_file());
        let _ = music::write_m3u(&dir, &title, &files);
    }

    /// Did this attempt fail the way an outdated yt-dlp fails?
    fn broke_ytdlp(&self, id: u64, result: &anyhow::Result<bool>) -> bool {
        let mut text = match result {
            Err(e) => e.to_string(),
            _ => String::new(),
        };
        if let Some(e) = self.inner.entries.lock().unwrap().iter().find(|e| e.snap.id == id) {
            text.push('\n');
            text.push_str(e.snap.error.as_deref().unwrap_or(""));
            for line in e.log.iter().rev().take(15) {
                text.push('\n');
                text.push_str(line);
            }
        }
        probe::needs_newer_ytdlp(&text)
    }

    /// Update yt-dlp (once per half hour, one job at a time). True if the next try may go
    /// differently: a new version arrived, or another job just got one.
    fn update_ytdlp(&self, id: u64) -> bool {
        let mut last = self.inner.ytdlp_updated.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(30 * 60)) {
            return true;
        }
        self.update(id, true, |s| {
            s.state = JobState::Processing;
            s.postprocessor = Some("UpdatingYtdlp".into());
        });
        let before = tools::ytdlp_version();
        let ok = tools::update_ytdlp().is_ok();
        let after = tools::ytdlp_version();
        *last = Some(Instant::now());
        if ok {
            let mut s = self.inner.settings.lock().unwrap();
            s.last_update_check =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let _ = s.save();
        }
        ok && after.is_some() && after != before
    }

    fn download_once(
        &self,
        id: u64,
        req: &DownloadRequest,
        settings: &Settings,
        dir: &std::path::Path,
        cancel: &AtomicBool,
        child: &Mutex<Option<Child>>,
    ) -> anyhow::Result<bool> {
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        // Post items ("Post title #2" from yt-dlp) keep the picker's "Post · Video 2".
        let keep_title = req.music.is_some() || req.playlist_index.is_some();
        // Once yt-dlp knows the size, make sure it fits; keep checking for livestreams.
        let mut last_space_check: Option<Instant> = None;
        let mut no_space: Option<String> = None;
        let (mut stage, mut stages) = (0u32, 1u32);
        // Files yt-dlp started writing, removed again if the drive fills up.
        let mut written: Vec<PathBuf> = Vec::new();
        let result = download::run(req, settings, child, cancel, |ev| {
            if let DlEvent::Log(line) = ev {
                if let Some(path) = line.strip_prefix("NERIA_DEST ") {
                    let path = PathBuf::from(path.trim());
                    self.add_dest(id, path.clone());
                    written.push(path);
                }
                self.push_log(id, line);
                return;
            }
            match &ev {
                DlEvent::Info { streams, .. } => stages = (*streams).max(1),
                DlEvent::Progress { finished: true, .. } => stage += 1,
                DlEvent::Progress { finished: false, downloaded, total, .. }
                    if no_space.is_none() && last_space_check.is_none_or(|t| t.elapsed() >= Duration::from_secs(3)) =>
                {
                    last_space_check = Some(Instant::now());
                    let left = match (total, downloaded) {
                        (Some(t), d) => (t - d.unwrap_or(0.0)).max(0.0) as u64,
                        _ => 0,
                    };
                    // Video + audio: the merge writes the whole file once more.
                    let merge = if stages > 1 && stage + 1 < stages { total.unwrap_or(0.0) as u64 * 5 / 4 } else { 0 };
                    if let Err(msg) = disk::check(dir, left + merge) {
                        no_space = Some(msg);
                        if let Some(c) = child.lock().unwrap().as_mut() {
                            proc::kill_tree(c);
                        }
                    }
                }
                _ => {}
            }
            // Spotify jobs keep "Artist - Title" rather than the YouTube video title.
            let ev = match ev {
                DlEvent::Info { streams, thumbnail, gif, key, .. } if keep_title => {
                    DlEvent::Info { title: String::new(), streams, thumbnail, gif, key }
                }
                ev => ev,
            };
            // Progress lines arrive very fast; repaint at most ~10x per second.
            let force = !matches!(ev, DlEvent::Progress { finished: false, .. });
            let emit = force || last_emit.elapsed() >= Duration::from_millis(100);
            if emit {
                last_emit = Instant::now();
            }
            self.update(id, emit, |s| s.apply(ev));
        });
        match no_space {
            Some(msg) => {
                // Don't leave half a download on an almost full drive.
                thread::sleep(Duration::from_millis(300));
                for path in &written {
                    remove_leftovers(path);
                }
                Err(anyhow::anyhow!("{msg} {SPACE_ADVICE}"))
            }
            None => result,
        }
    }

    /// Save a file address as is (photos, and videos only gallery-dl saw).
    fn download_direct(
        &self,
        id: u64,
        req: &DownloadRequest,
        dir: &std::path::Path,
        cancel: &AtomicBool,
    ) -> anyhow::Result<bool> {
        let Some((url, ext)) = &req.direct else { return Ok(false) };
        std::fs::create_dir_all(dir)?;
        let stem = req.stem.clone().unwrap_or_else(|| music::sanitize(&req.title_hint));
        let dest = dir.join(format!("{stem}.{ext}"));
        // Same post again: keep what's there. (A GIF's MP4 became a .gif.)
        let done = if req.gif { dest.with_extension("gif") } else { dest.clone() };
        if done.is_file() {
            self.update(id, true, |s| {
                s.file = Some(done.clone());
                s.postprocessor = Some("AlreadyHave".into());
            });
            return Ok(true);
        }
        self.push_log(id, format!("> download {url}"));
        let part = dir.join(format!("{stem}.{ext}.part"));
        let mut last = Instant::now() - Duration::from_secs(1);
        let started = Instant::now();
        let result = tools::download_to_cancellable(url, &part, cancel, |done, total| {
            let emit = last.elapsed() >= Duration::from_millis(100);
            if emit {
                last = Instant::now();
            }
            let secs = started.elapsed().as_secs_f64().max(0.1);
            self.update(id, emit, |s| {
                s.state = JobState::Downloading;
                s.downloaded = Some(done as f64);
                s.total = total.map(|t| t as f64);
                s.speed = Some(done as f64 / secs);
                s.progress = total.filter(|t| *t > 0).map_or(0.0, |t| done as f32 / t as f32);
            });
        });
        if let Err(e) = result {
            let _ = std::fs::remove_file(&part);
            if cancel.load(Ordering::SeqCst) {
                return Ok(false);
            }
            self.push_log(id, format!("{e:#}"));
            return Err(anyhow::anyhow!("Couldn't save this file. Check your connection, then try again."));
        }
        std::fs::rename(&part, &dest)?;
        self.update(id, true, |s| s.file = Some(dest));
        Ok(true)
    }

    fn current_file(&self, id: u64, job: &music::MusicJob) -> Option<PathBuf> {
        let entries = self.inner.entries.lock().unwrap();
        entries
            .iter()
            .find(|e| e.snap.id == id)
            .and_then(|e| e.snap.file.clone())
            .filter(|f| f.is_file())
            .or_else(|| music::existing_file(&job.dir, &job.stem))
    }

    #[allow(clippy::too_many_arguments)]
    fn verify_and_retry(
        &self,
        id: u64,
        req: &mut DownloadRequest,
        job: &music::MusicJob,
        settings: &Settings,
        cancel: &AtomicBool,
        child: &Mutex<Option<Child>>,
        mut result: anyhow::Result<bool>,
    ) -> anyhow::Result<bool> {
        self.update(id, true, |s| {
            s.state = JobState::Processing;
            s.postprocessor = Some("Verifying".into());
        });
        let Some(reference) = fingerprint::reference_for(&job.track) else {
            self.update(id, false, |s| s.verified = Some("none"));
            return result; // no preview anywhere to compare with
        };
        let Ok(clip) = fingerprint::reference_fingerprint(&reference) else { return result };
        let picked_by_user = job.forced.is_some();
        let mut tried = vec![req.url.clone()];
        let mut best: Option<(f64, String)> = None;
        // (normalized title, duration) of versions that failed the check: the same
        // re-recording often sits on several compilations, no point trying each.
        let mut rejected: Vec<(String, Option<f64>)> = Vec::new();
        let describe = |url: &str| -> Option<(String, Option<f64>)> {
            let entries = self.inner.entries.lock().unwrap();
            let e = entries.iter().find(|e| e.snap.id == id)?;
            let c = e.candidates.iter().find(|c| c.url() == url)?;
            Some((crate::ytmusic::normalize(&c.title), c.duration))
        };
        let same_version = |a: &(String, Option<f64>), b: &(String, Option<f64>)| {
            a.0 == b.0
                && match (a.1, b.1) {
                    (Some(x), Some(y)) => (x - y).abs() <= 2.0,
                    _ => true,
                }
        };

        loop {
            self.update(id, true, |s| {
                s.state = JobState::Processing;
                s.postprocessor = Some("Verifying".into());
            });
            let sim = self
                .current_file(id, job)
                .and_then(|f| fingerprint::compare_file(&clip, &f).ok())
                .map(|a| a.similarity);
            let Some(sim) = sim else { return result }; // couldn't check: leave as is
            if best.as_ref().is_none_or(|(b, _)| sim > *b) {
                best = Some((sim, req.url.clone()));
            }
            match fingerprint::verdict(sim) {
                fingerprint::Verdict::Same => {
                    self.update(id, false, |s| {
                        s.verified = Some(reference.source);
                        s.note = None; // the audio check beats any title-based doubt
                    });
                    return result;
                }
                fingerprint::Verdict::Unsure => {
                    let note = format!("Close to the {} preview, maybe another master or edit", reference.source);
                    self.update(id, false, |s| s.note = Some(note));
                    return result;
                }
                fingerprint::Verdict::Different => {}
            }

            // Different recording: try the next good candidate (unless the user chose this one).
            if let Some(d) = describe(&req.url) {
                rejected.push(d);
            }
            let next = if picked_by_user || tried.len() >= 3 {
                None
            } else {
                let entries = self.inner.entries.lock().unwrap();
                entries.iter().find(|e| e.snap.id == id).and_then(|e| {
                    e.candidates
                        .iter()
                        .filter(|c| c.score >= crate::ytmusic::MIN_SCORE && !tried.contains(&c.url()))
                        .find(|c| {
                            let me = (crate::ytmusic::normalize(&c.title), c.duration);
                            !rejected.iter().any(|r| same_version(r, &me))
                        })
                        .cloned()
                })
            };
            let target = match next {
                Some(c) => Some(c.url()),
                // Out of options: go back to the closest one if we've moved off it.
                None => best.as_ref().map(|(_, u)| u.clone()).filter(|u| *u != req.url),
            };
            let Some(url) = target else {
                let note = format!(
                    "Doesn't sound like the {} preview ({:.0}% similar). Pick another?",
                    reference.source,
                    sim * 100.0
                );
                self.update(id, false, |s| s.note = Some(note));
                return result;
            };
            let going_back = !tried.iter().all(|t| *t != url);
            tried.push(url.clone());
            req.url = url.clone();
            if let Some(m) = req.music.as_mut() {
                m.forced = Some(url.clone()); // overwrite the file we just checked
            }
            self.set_chosen(id, url);
            self.update(id, true, |s| {
                s.state = JobState::Processing;
                s.postprocessor = Some("Retrying".into());
                s.stage = 0;
                s.progress = 0.0;
            });
            result = self.download_once(id, req, settings, &job.dir, cancel, child);
            if !matches!(result, Ok(true)) || cancel.load(Ordering::SeqCst) {
                return result;
            }
            if going_back {
                let pct = best.as_ref().map_or(0.0, |(b, _)| b * 100.0);
                let note =
                    format!("Doesn't sound like the {} preview ({pct:.0}% similar). Pick another?", reference.source);
                self.update(id, false, |s| s.note = Some(note));
                return result;
            }
        }
    }

    fn add_dest(&self, id: u64, path: PathBuf) {
        let mut entries = self.inner.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.snap.id == id)
            && !e.dests.contains(&path)
        {
            e.dests.push(path);
        }
    }

    fn set_chosen(&self, id: u64, url: String) {
        let mut entries = self.inner.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) {
            e.chosen = Some(url);
        }
    }

    fn update(&self, id: u64, emit: bool, f: impl FnOnce(&mut JobSnapshot)) {
        let mut entries = self.inner.entries.lock().unwrap();
        let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) else { return };
        let before = e.snap.state;
        f(&mut e.snap);
        if emit {
            (self.inner.notify)(e.snap.clone());
        }
        // Save on state changes only (not on every progress tick).
        if e.snap.state != before {
            self.save_locked(&entries);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_this_downloads_leftovers() {
        let dir = std::env::temp_dir().join(format!("neria-leftovers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = [
            "Clip [abc].f401.mp4.part",
            "Clip [abc].f251.webm",
            "Clip [abc].webp",
            "Clip [abc].mp4.ytdl",
            "Clip [abc] (compressed).mp4",
            "Clip [abcd].f401.mp4.part",
            "Other.mp4",
        ];
        for n in names {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        remove_leftovers(&dir.join("Clip [abc].mp4"));
        let mut left: Vec<String> =
            std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["Clip [abc] (compressed).mp4", "Clip [abcd].f401.mp4.part", "Other.mp4"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_items_saved_before() {
        let dir = std::env::temp_dir().join(format!("neria-existing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("01 Song.mp3"), b"x").unwrap();
        std::fs::write(dir.join("02 Clip.mp4.part"), b"x").unwrap();
        std::fs::write(dir.join("03 Clip.mkv"), b"x").unwrap();
        assert!(existing_media(&dir, "01 Song", Quality::Audio, false).is_some());
        // The same name as video is a different download.
        assert!(existing_media(&dir, "01 Song", Quality::Best, false).is_none());
        // Half-downloaded doesn't count.
        assert!(existing_media(&dir, "02 Clip", Quality::Best, false).is_none());
        assert!(existing_media(&dir, "03 Clip", Quality::P720, false).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn playlist_file_keeps_earlier_songs_in_order() {
        let e = |n: &str| (n.to_string(), 1.0, format!("{n}.mp3"));
        // This time only 02 and 04; 01 and 03 were saved before, 05 was deleted since.
        let merged = merge_m3u(vec![e("04 D"), e("02 B")], vec![e("01 A"), e("03 C"), e("05 E")], |f| f != "05 E.mp3");
        let names: Vec<&str> = merged.iter().map(|f| f.2.as_str()).collect();
        assert_eq!(names, ["01 A.mp3", "02 B.mp3", "03 C.mp3", "04 D.mp3"]);
        // Unnumbered: playlist order as given, earlier ones after.
        let merged = merge_m3u(vec![e("Song B")], vec![e("Song A")], |_| true);
        assert_eq!(merged.iter().map(|f| f.2.as_str()).collect::<Vec<_>>(), ["Song B.mp3", "Song A.mp3"]);
    }
}
