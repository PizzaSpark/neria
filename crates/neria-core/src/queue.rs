//! Download queue with a concurrency limit, cancel and retry.

use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::download::{self, DlEvent, DownloadRequest};
use crate::spotify::Track;
use crate::ytmusic::Candidate;
use crate::{fingerprint, music, proc};
use crate::settings::Settings;

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
    /// A Spotify track (the UI offers "pick another match").
    pub music: bool,
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
            music: false,
            verified: None,
        }
    }

    fn apply(&mut self, ev: DlEvent) {
        match ev {
            DlEvent::Info { title, streams } => {
                if !title.is_empty() {
                    self.title = title;
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
}

#[derive(Clone, Debug)]
pub struct MatchInfo {
    pub track: Track,
    pub candidates: Vec<Candidate>,
    pub chosen: Option<String>,
}

type Notify = Box<dyn Fn(JobSnapshot) + Send + Sync>;

struct Inner {
    entries: Mutex<Vec<Entry>>,
    settings: Arc<Mutex<Settings>>,
    notify: Notify,
    next_id: AtomicU64,
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
        (self.inner.notify)(snap.clone());
        self.inner.entries.lock().unwrap().push(Entry {
            snap,
            req,
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
            candidates: Vec::new(),
            chosen: None,
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
            let (title, group) = (e.snap.title.clone(), e.snap.group);
            e.snap = JobSnapshot::new(id, group, title);
            e.snap.music = true;
            e.cancel = Arc::new(AtomicBool::new(false));
            (self.inner.notify)(e.snap.clone());
        }
        self.pump();
        true
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
            let (title, group, music) = (e.snap.title.clone(), e.snap.group, e.snap.music);
            e.snap = JobSnapshot::new(id, group, title);
            e.snap.music = music;
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
        let mut running = entries
            .iter()
            .filter(|e| matches!(e.snap.state, JobState::Downloading | JobState::Processing))
            .count();

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

        let mut result = self.download_once(id, &req, &settings, &cancel, &child);

        // Spotify track: check it's the recording Spotify plays (30 s preview
        // fingerprint). A different recording (re-recorded, live, cover…)
        // triggers up to two other good candidates before we give up and flag it.
        if let (Some(job), Ok(true)) = (req.music.clone(), &result) {
            if job.verify && !cancel.load(Ordering::SeqCst) {
                result = self.verify_and_retry(id, &mut req, &job, &settings, &cancel, &child, result);
            }
        }

        let cancelled = cancel.load(Ordering::SeqCst);

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

    fn download_once(
        &self,
        id: u64,
        req: &DownloadRequest,
        settings: &Settings,
        cancel: &AtomicBool,
        child: &Mutex<Option<Child>>,
    ) -> anyhow::Result<bool> {
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let keep_title = req.music.is_some();
        download::run(req, settings, child, cancel, |ev| {
            // Spotify jobs keep "Artist - Title" rather than the YouTube video title.
            let ev = match ev {
                DlEvent::Info { streams, .. } if keep_title => DlEvent::Info { title: String::new(), streams },
                ev => ev,
            };
            // Progress lines arrive very fast; repaint at most ~10x per second.
            let force = !matches!(ev, DlEvent::Progress { finished: false, .. });
            let emit = force || last_emit.elapsed() >= Duration::from_millis(100);
            if emit {
                last_emit = Instant::now();
            }
            self.update(id, emit, |s| s.apply(ev));
        })
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
                let note = format!("Doesn't sound like the {} preview ({:.0}% similar). Pick another?", reference.source, sim * 100.0);
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
            result = self.download_once(id, req, settings, cancel, child);
            if !matches!(result, Ok(true)) || cancel.load(Ordering::SeqCst) {
                return result;
            }
            if going_back {
                let pct = best.as_ref().map_or(0.0, |(b, _)| b * 100.0);
                let note = format!("Doesn't sound like the {} preview ({pct:.0}% similar). Pick another?", reference.source);
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
