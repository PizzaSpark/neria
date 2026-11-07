//! Download queue with a concurrency limit, cancel and retry.

use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::download::{self, DlEvent, DownloadRequest};
use crate::{music, proc};
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
        let snap = JobSnapshot::new(id, group, title);
        (self.inner.notify)(snap.clone());
        self.inner.entries.lock().unwrap().push(Entry {
            snap,
            req,
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
        });
        self.pump();
        id
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
            let (title, group) = (e.snap.title.clone(), e.snap.group);
            e.snap = JobSnapshot::new(id, group, title);
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
        // Spotify track: resume if already on disk, else find it on YouTube Music.
        if let Some(job) = req.music.clone() {
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
            match music::find_match(&job) {
                Ok(c) if !cancel.load(Ordering::SeqCst) => {
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
                        Ok(_) => s.state = JobState::Cancelled,
                    });
                    self.pump();
                    return;
                }
            }
        }

        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let keep_title = req.music.is_some();
        let result = download::run(&req, &settings, &child, &cancel, |ev| {
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
        });

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
