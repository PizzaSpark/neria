//! Spotify links on the Download page: preview, "download all", and the sync
//! review for songs removed from a playlist (never deleted without asking).

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use neria_core::download::DownloadRequest;
use neria_core::music;
use neria_core::queue::Queue;
use neria_core::settings::Settings;
use neria_core::spotify::{self, Collection, Kind, Link};
use neria_core::{fmt, probe};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppWindow, SyncItem};

/// A looked-up Spotify link waiting for the Download button.
pub struct Pending {
    pub url: String,
    pub collection: Collection,
}

pub type PendingSlot = Arc<Mutex<Option<Pending>>>;

/// Preview a Spotify link (runs the lookup in the background).
pub fn preview(ui: &AppWindow, url: String, link: Link, settings: Settings, generation: Arc<AtomicU64>, my_gen: u64, slot: PendingSlot) {
    ui.set_probe_is_spotify(true);
    ui.set_spotify_count(0);
    ui.set_sync_items(ModelRc::from(Rc::new(VecModel::<SyncItem>::default())));
    *slot.lock().unwrap() = None;

    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = spotify::fetch(&link, settings.spotify_credentials().as_ref());
        let cover = result
            .as_ref()
            .ok()
            .and_then(|c| c.cover_url.as_deref())
            .and_then(|u| probe::fetch_thumbnail(u).ok());
        let removed = result
            .as_ref()
            .map(|c| music::removed_tracks(&music::collection_dir(&settings.output_dir, c), c))
            .unwrap_or_default();

        let _ = weak.upgrade_in_event_loop(move |ui| {
            if generation.load(Ordering::SeqCst) != my_gen {
                return;
            }
            match result {
                Ok(col) => {
                    let n = col.tracks.len();
                    let mut meta = Vec::new();
                    if let Some(o) = &col.owner {
                        meta.push(o.clone());
                    }
                    meta.push(if n == 1 { "1 song".into() } else { format!("{n} songs") });
                    let total: f64 = col.tracks.iter().map(|t| t.duration_secs()).sum();
                    if col.kind != Kind::Track {
                        meta.push(fmt::duration(total));
                    }
                    meta.push("audio from YouTube Music".into());
                    let mut note = String::new();
                    if col.truncated {
                        note = match col.kind {
                            Kind::Artist => "Spotify's public page only lists an artist's top 10. Paste album links for more.".into(),
                            _ => "Spotify's public page shows the first 100 songs. Add your own Spotify key in Settings for all of them.".into(),
                        };
                    }
                    if let Some(n) = &col.notice {
                        note = n.clone();
                    }

                    ui.set_probe_state(2);
                    ui.set_probe_title(format!("{} · {}", col.kind.label(), col.name).into());
                    ui.set_probe_meta(meta.join("  ·  ").into());
                    ui.set_spotify_note(note.into());
                    ui.set_spotify_count(n as i32);
                    if let Some(img) = cover.and_then(|p| slint::Image::load_from_path(&p).ok()) {
                        ui.set_probe_thumb(img);
                    }
                    let items: Vec<SyncItem> = removed
                        .into_iter()
                        .map(|r| SyncItem {
                            title: r.title.into(),
                            file: r.file.to_string_lossy().as_ref().into(),
                            checked: false,
                        })
                        .collect();
                    ui.set_sync_items(ModelRc::from(Rc::new(VecModel::from(items))));
                    *slot.lock().unwrap() = Some(Pending { url, collection: col });
                    // Dev hooks for scripted checks.
                    if std::env::var_os("NERIA_SYNC_REVIEW").is_some() && ui.get_sync_items().row_count() > 0 {
                        ui.set_sync_open(true);
                    } else if std::env::var_os("NERIA_URL_START").is_some() {
                        ui.invoke_start_download();
                    }
                }
                Err(e) => {
                    ui.set_probe_state(3);
                    ui.set_probe_title(e.to_string().into());
                    ui.set_probe_meta("Check that the link is public.".into());
                }
            }
        });
    });
}

/// Queue every song of the pending collection. Returns false if `url` isn't the pending one.
pub fn enqueue(ui: &AppWindow, url: &str, slot: &PendingSlot, settings: &Settings, queue: &Queue) -> bool {
    let pending = slot.lock().unwrap().take();
    let Some(p) = pending.filter(|p| p.url == url) else { return false };
    match music::prepare(&p.collection, settings) {
        Ok((_dir, jobs)) => queue.add_batch(jobs.into_iter().map(DownloadRequest::music)),
        Err(e) => {
            ui.set_probe_state(3);
            ui.set_probe_title(format!("Couldn't prepare the folder: {e}").into());
            *slot.lock().unwrap() = Some(p);
            return true;
        }
    }
    true
}

pub fn install_sync_review(ui: &AppWindow) {
    ui.on_sync_toggle({
        let weak = ui.as_weak();
        move |i, checked| {
            let ui = weak.unwrap();
            let model = ui.get_sync_items();
            if let Some(mut row) = model.row_data(i as usize) {
                row.checked = checked;
                model.set_row_data(i as usize, row);
            }
        }
    });

    ui.on_sync_delete_selected({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let model = ui.get_sync_items();
            let (mut moved, mut failed) = (0, 0);
            let mut keep = Vec::new();
            for row in model.iter() {
                if !row.checked {
                    continue;
                }
                let file = PathBuf::from(row.file.as_str());
                // Recycle Bin, not permanent delete: mistakes stay recoverable.
                match trash::delete(&file) {
                    Ok(()) => {
                        moved += 1;
                        let lrc = file.with_extension("lrc");
                        if lrc.is_file() {
                            let _ = trash::delete(&lrc);
                        }
                    }
                    Err(_) => {
                        failed += 1;
                        keep.push(row.clone());
                    }
                }
            }
            ui.set_sync_items(ModelRc::from(Rc::new(VecModel::from(keep))));
            ui.set_sync_open(false);
            let mut msg = format!("Moved {moved} song{} to the Recycle Bin", if moved == 1 { "" } else { "s" });
            if failed > 0 {
                msg.push_str(&format!(" ({failed} couldn't be moved)"));
            }
            ui.set_spotify_note(msg.into());
        }
    });

    ui.on_sync_keep_all({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            ui.set_sync_items(ModelRc::from(Rc::new(VecModel::<SyncItem>::default())));
            ui.set_sync_open(false);
        }
    });
}
