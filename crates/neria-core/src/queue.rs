//! Download queue with a concurrency limit, cancel and retry.

use std::collections::VecDeque;
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
use crate::{disk, fingerprint, gif, music, probe, proc, report, tools};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Downloading,
    Processing,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Downloading | Self::Processing)
    }
}

#[derive(Clone, Debug)]
pub struct JobSnapshot {
    pub id: u64,
    /// Jobs added together (an album/playlist) share a group, so the UI can keep them in order.
    pub group: u64,
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
    /// Spotify track audio check: Some(source) when it matched that preview,
    /// Some("none") when no preview existed anywhere, None when not checked.
    pub verified: Option<&'static str>,
}

impl JobSnapshot {
    fn new(id: u64, group: u64, title: String) -> Self {
        Self {
            id,
            group,
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
            verified: None,
        }
    }

    fn apply(&mut self, ev: DlEvent) {
        match ev {
            DlEvent::Info { title, streams, thumbnail, gif } => {
                self.gif = gif;
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
            DlEvent::File(path) => self.file = Some(path),
            DlEvent::Error(msg) => self.error = Some(msg),
            DlEvent::Log(_) => {}
        }
    }
}

struct Entry {
    snap: JobSnapshot,
    req: DownloadRequest,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    /// Spotify jobs: YouTube Music candidates (best first) and the URL used.
    candidates: Vec<Candidate>,
    chosen: Option<String>,
    /// Last lines of yt-dlp output, for "Report a problem".
    log: VecDeque<String>,
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

struct Inner {
    entries: Mutex<Vec<Entry>>,
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
        let group = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        for r in reqs {
            self.add_to_group(r, group);
        }
    }

    fn add_to_group(&self, req: DownloadRequest, group: u64) -> u64 {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let title = if req.title_hint.is_empty() { req.url.clone() } else { req.title_hint.clone() };
        let mut snap = JobSnapshot::new(id, group, title);
        snap.music = req.music.is_some();
        snap.thumbnail = req.music.as_ref().and_then(|m| m.track.cover_url.clone());
        (self.inner.notify)(snap.clone());
        self.inner.entries.lock().unwrap().push(Entry {
            snap,
            req,
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
            candidates: Vec::new(),
            chosen: None,
            log: VecDeque::new(),
        });
        self.pump();
        id
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
            let (title, group, thumbnail) = (e.snap.title.clone(), e.snap.group, e.snap.thumbnail.clone());
            e.snap = JobSnapshot::new(id, group, title);
            e.snap.music = true;
            e.snap.thumbnail = thumbnail;
            e.cancel = Arc::new(AtomicBool::new(false));
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
            JobState::Queued => {
                e.snap.state = JobState::Cancelled;
                (self.inner.notify)(e.snap.clone());
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

    pub fn retry(&self, id: u64) {
        {
            let mut entries = self.inner.entries.lock().unwrap();
            let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) else { return };
            if e.snap.state.is_active() || e.snap.state == JobState::Done {
                return;
            }
            let (title, group, music, thumbnail) =
                (e.snap.title.clone(), e.snap.group, e.snap.music, e.snap.thumbnail.clone());
            e.snap = JobSnapshot::new(id, group, title);
            e.snap.music = music;
            e.snap.thumbnail = thumbnail;
            e.cancel = Arc::new(AtomicBool::new(false));
            (self.inner.notify)(e.snap.clone());
        }
        self.pump();
    }

    /// Remove a finished job from the list. Returns false for active jobs.
    pub fn remove(&self, id: u64) -> bool {
        let mut entries = self.inner.entries.lock().unwrap();
        match entries.iter().position(|e| e.snap.id == id) {
            Some(i) if !entries[i].snap.state.is_active() => {
                entries.remove(i);
                true
            }
            _ => false,
        }
    }

    /// Remove every finished, failed or cancelled job; returns their ids.
    pub fn clear_finished(&self) -> Vec<u64> {
        let mut entries = self.inner.entries.lock().unwrap();
        let removed = entries.iter().filter(|e| !e.snap.state.is_active()).map(|e| e.snap.id).collect();
        entries.retain(|e| e.snap.state.is_active());
        removed
    }

    pub fn file_of(&self, id: u64) -> Option<PathBuf> {
        let entries = self.inner.entries.lock().unwrap();
        entries.iter().find(|e| e.snap.id == id).and_then(|e| e.snap.file.clone())
    }

    /// Cancel everything (used on shutdown so no yt-dlp/ffmpeg is left running).
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
            let settings = settings.clone();
            thread::spawn(move || queue.run_job(id, req, settings, cancel, child));
        }
    }

    fn run_job(
        &self,
        id: u64,
        mut req: DownloadRequest,
        settings: Settings,
        cancel: Arc<AtomicBool>,
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
                    self.update(id, true, |s| match result {
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

        let dir = req.music.as_ref().map(|m| m.dir.clone()).unwrap_or_else(|| settings.output_dir.clone());
        let mut result = match disk::check(&dir, 0) {
            Ok(()) => self.download_once(id, &req, &settings, &dir, &cancel, &child),
            Err(msg) => Err(anyhow::anyhow!("{msg} {SPACE_ADVICE}")),
        };

        // The site changed under yt-dlp: get a newer one right away and try once more,
        // instead of failing until tomorrow's daily update.
        if !matches!(result, Ok(true))
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

        // X "GIF" (really a silent MP4): save it as the .gif people expect.
        let mut gif_error = None;
        if matches!(result, Ok(true))
            && !cancelled
            && req.music.is_none()
            && settings.x_gifs_as_gif
            && req.quality != Quality::Audio
        {
            let file = {
                let entries = self.inner.entries.lock().unwrap();
                entries.iter().find(|e| e.snap.id == id).filter(|e| e.snap.gif).and_then(|e| e.snap.file.clone())
            };
            if let Some(video) = file {
                self.update(id, true, |s| {
                    s.state = JobState::Processing;
                    s.postprocessor = Some("MakingGif".into());
                });
                match gif::from_video(&video) {
                    Ok(out) => {
                        let _ = std::fs::remove_file(&video);
                        self.update(id, false, |s| s.file = Some(out));
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
        self.pump();
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
        let keep_title = req.music.is_some();
        // Once yt-dlp knows the size, make sure it fits; keep checking for livestreams.
        let mut last_space_check: Option<Instant> = None;
        let mut no_space: Option<String> = None;
        let (mut stage, mut stages) = (0u32, 1u32);
        // Files yt-dlp started writing, removed again if the drive fills up.
        let mut written: Vec<PathBuf> = Vec::new();
        let result = download::run(req, settings, child, cancel, |ev| {
            if let DlEvent::Log(line) = ev {
                if let Some(path) = line.strip_prefix("NERIA_DEST ") {
                    written.push(PathBuf::from(path.trim()));
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
                DlEvent::Info { streams, thumbnail, gif, .. } if keep_title => {
                    DlEvent::Info { title: String::new(), streams, thumbnail, gif }
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

    fn set_chosen(&self, id: u64, url: String) {
        let mut entries = self.inner.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) {
            e.chosen = Some(url);
        }
    }

    fn update(&self, id: u64, emit: bool, f: impl FnOnce(&mut JobSnapshot)) {
        let mut entries = self.inner.entries.lock().unwrap();
        if let Some(e) = entries.iter_mut().find(|e| e.snap.id == id) {
            f(&mut e.snap);
            if emit {
                (self.inner.notify)(e.snap.clone());
            }
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
}
