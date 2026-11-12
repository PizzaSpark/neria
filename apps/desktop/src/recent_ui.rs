//! "Recent downloads" on the empty Clip and Compress pages: the newest files from
//! `history.json` that still exist, with their length, age and a frame as thumbnail, so the
//! file you most likely want is one click away. Loaded in the background when either page
//! opens; durations and frames are cached per file.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use neria_core::{fmt, history, media};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, RecentItem};

const VIDEO: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v", "flv", "wmv", "ts", "mpg", "mpeg", "ogv"];
const AUDIO: &[&str] = &["mp3", "m4a", "aac", "wav", "flac", "opus", "ogg", "wma"];
/// Rows per page.
const SHOWN: usize = 5;

fn kind(path: &Path) -> (bool, bool) {
    let e = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    (VIDEO.contains(&e.as_str()), AUDIO.contains(&e.as_str()))
}

/// "4 min ago", "3 h ago", "yesterday", "5 days ago".
fn age(at: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let s = now.saturating_sub(at);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// What we learned about a file: its length and a frame (videos), worked out once.
#[derive(Clone)]
struct Probed {
    duration: Option<f64>,
    frame: Option<PathBuf>,
}

thread_local! {
    static CACHE: RefCell<HashMap<PathBuf, Probed>> = RefCell::default();
}

/// Fill both pages' lists. Call when Clip or Compress opens.
pub fn refresh(ui: &AppWindow) {
    let weak = ui.as_weak();
    let known: HashMap<PathBuf, Probed> = CACHE.with(|c| c.borrow().clone());
    std::thread::spawn(move || {
        let records = history::recent(SHOWN * 2, |p| {
            let (v, a) = kind(p);
            v || a
        });
        let mut found = Vec::new();
        for r in records {
            let probed = known.get(&r.file).cloned().unwrap_or_else(|| {
                let info = media::file_info(&r.file).ok();
                let frame = info
                    .as_ref()
                    .filter(|i| i.has_video)
                    .and_then(|i| media::thumbnails(&r.file, i.duration * 0.3, 1, 108).into_iter().next());
                Probed { duration: info.map(|i| i.duration), frame }
            });
            found.push((r, probed));
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let mut clip = Vec::new();
            let mut compress = Vec::new();
            for (r, p) in found {
                CACHE.with(|c| c.borrow_mut().insert(r.file.clone(), p.clone()));
                let (video, _) = kind(&r.file);
                let title = if r.title.is_empty() {
                    r.file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                } else {
                    r.title.clone()
                };
                let mut detail = Vec::new();
                if let Some(d) = p.duration {
                    detail.push(fmt::duration(d));
                }
                detail.push(age(r.at));
                let item = RecentItem {
                    path: r.file.to_string_lossy().as_ref().into(),
                    title: title.into(),
                    detail: detail.join("  ·  ").into(),
                    thumb: p.frame.and_then(|f| slint::Image::load_from_path(&f).ok()).unwrap_or_default(),
                    video,
                };
                if clip.len() < SHOWN {
                    clip.push(item.clone());
                }
                if video && compress.len() < SHOWN {
                    compress.push(item);
                }
            }
            ui.set_recent_clip(ModelRc::from(Rc::new(VecModel::from(clip))));
            ui.set_recent_compress(ModelRc::from(Rc::new(VecModel::from(compress))));
        });
    });
}

pub fn install(ui: &AppWindow) {
    ui.on_refresh_recent({
        let weak = ui.as_weak();
        move || refresh(&weak.unwrap())
    });
}
