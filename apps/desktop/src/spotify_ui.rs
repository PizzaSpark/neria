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

use crate::{AppWindow, Music, SyncItem, show_toast};

/// A looked-up Spotify link waiting for the Download button.
pub struct Pending {
    pub url: String,
    pub collection: Collection,
}

pub type PendingSlot = Arc<Mutex<Option<Pending>>>;

/// Preview a Spotify link (runs the lookup in the background).
pub fn preview(
    ui: &AppWindow,
    url: String,
    link: Link,
    settings: Settings,
    generation: Arc<AtomicU64>,
    my_gen: u64,
    slot: PendingSlot,
) {
    ui.set_probe_is_spotify(true);
    ui.set_spotify_count(0);
    ui.set_sync_items(ModelRc::from(Rc::new(VecModel::<SyncItem>::default())));
    *slot.lock().unwrap() = None;

    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = spotify::fetch(&link, settings.spotify_credentials().as_ref());
        let cover =
            result.as_ref().ok().and_then(|c| c.cover_url.as_deref()).and_then(|u| probe::fetch_thumbnail(u).ok());
        let removed =
            result.as_ref().map(|c| music::removed_tracks(&music::collection_dir(&settings, c), c)).unwrap_or_default();

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
                            _ => "Spotify's public page shows the first 100 songs. Add your own Spotify key in Settings → Spotify for all of them.".into(),
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
                    ui.set_sync_ticked(0);
                    *slot.lock().unwrap() = Some(Pending { url, collection: col });
                    // Dev hooks for scripted checks.
                    if std::env::var_os("NERIA_SYNC_REVIEW").is_some() && ui.get_sync_items().row_count() > 0 {
                        ui.set_sync_open(true);
                    } else if std::env::var_os("NERIA_LYRICS_ONLY").is_some() {
                        ui.global::<Music>().invoke_lyrics_only();
                    } else if std::env::var_os("NERIA_URL_START").is_some() {
                        ui.invoke_start_download();
                    }
                }
                Err(e) => {
                    let text = e.to_string();
                    let (title, hint, permanent) = if text.contains("private, removed") {
                        ("This Spotify link is private, removed or not available", "Check that it opens on Spotify and that the playlist is public.", true)
                    } else if text.contains("couldn't reach") {
                        ("Couldn't reach Spotify", "Check your internet connection, then paste the link again.", false)
                    } else if text.contains("format changed") {
                        ("Spotify changed its page and Neria can't read it yet", "A Neria update will fix this. Try again later.", false)
                    } else {
                        (text.as_str(), "Paste the link again to retry.", false)
                    };
                    ui.set_probe_state(3);
                    ui.set_probe_blocked(permanent);
                    ui.set_probe_title(title.into());
                    ui.set_probe_meta(hint.into());
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
        Ok((_dir, jobs)) => queue.add_group(&p.collection.name, false, jobs.into_iter().map(DownloadRequest::music)),
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
            ui.set_sync_ticked(model.iter().filter(|r| r.checked).count() as i32);
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
            ui.set_sync_ticked(0);
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

/// "Lyrics only" on a Spotify preview, and Settings → Spotify → "Add lyrics to songs I
/// already have".
pub fn install_lyrics(ui: &AppWindow, slot: PendingSlot, settings: Arc<Mutex<Settings>>, queue: Queue) {
    let music = ui.global::<Music>();
    music.on_lyrics_only({
        let weak = ui.as_weak();
        let (settings, queue) = (settings.clone(), queue.clone());
        move || {
            let ui = weak.unwrap();
            // The preview stays: "Download N songs" still works afterwards.
            let Some(col) = slot.lock().unwrap().as_ref().map(|p| p.collection.clone()) else { return };
            let settings = settings.lock().unwrap().clone();
            match music::prepare_lyrics_only(&col, &settings) {
                Ok((_dir, jobs)) => {
                    let n = jobs.len();
                    queue.add_group(
                        &format!("Lyrics · {}", col.name),
                        false,
                        jobs.into_iter().map(DownloadRequest::music),
                    );
                    let what = if n == 1 { "this song".to_string() } else { format!("{n} songs") };
                    show_toast(&ui, &format!("Saving the lyrics of {what}: .lrc when synced, .txt with the words."));
                }
                Err(e) => show_toast(&ui, &format!("Couldn't prepare the folder: {e}")),
            }
        }
    });
    music.on_add_lyrics({
        let weak = ui.as_weak();
        move || {
            let current = settings.lock().unwrap().clone();
            let start = neria_core::folders::for_kind(&current, neria_core::folders::Kind::Audio);
            // Dev hook: that folder, without the picker.
            let picked = match std::env::var_os("NERIA_ADD_LYRICS") {
                Some(d) => Some(std::path::PathBuf::from(d)),
                None => rfd::FileDialog::new().set_directory(&start).pick_folder(),
            };
            let Some(dir) = picked else { return };
            show_toast(&weak.unwrap(), "Reading the songs in that folder…");
            let (weak, queue) = (weak.clone(), queue.clone());
            std::thread::spawn(move || {
                let (jobs, have, untagged) = music::add_lyrics_jobs(&dir, &current);
                let n = jobs.len();
                if n > 0 {
                    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    queue.add_group(&format!("Lyrics · {name}"), false, jobs.into_iter().map(DownloadRequest::music));
                }
                let songs = |k: usize| if k == 1 { "1 song".to_string() } else { format!("{k} songs") };
                let mut msg = match n {
                    0 => "No songs there need lyrics".to_string(),
                    _ => format!("Adding lyrics to {}", songs(n)),
                };
                let mut extra = Vec::new();
                if have > 0 {
                    extra.push(format!("{} already have lyrics", songs(have)));
                }
                if untagged > 0 {
                    extra.push(format!("{} have no artist/title tags", songs(untagged)));
                }
                if !extra.is_empty() {
                    msg.push_str(&format!(" ({})", extra.join(", ")));
                }
                msg.push('.');
                let _ = weak.upgrade_in_event_loop(move |ui| show_toast(&ui, &msg));
            });
        }
    });
}
