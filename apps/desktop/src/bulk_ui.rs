//! Several links at once, "Paste & download" and the clipboard watcher (Download track 9).
//!
//! Pasting (or dropping a .txt with) two or more links lists them in the picker, each
//! looked up a few at a time: its title and length, unticked with a reason when it's
//! already in the queue, was downloaded before, is a playlist/post or a Spotify link
//! (those get their own preview), or can't be reached. Download adds one plain row per
//! ticked link, in the list's order. "Paste & download" skips all that and queues every
//! link on the clipboard with the chips as they are. The watcher (off by default) offers a
//! link copied in another app: a strip on the Download page, a notification from the tray.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use neria_core::download::DownloadRequest;
use neria_core::queue::Queue;
use neria_core::settings::{Quality, Settings};
use neria_core::{fmt, history, post, probe, spotify};
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppWindow, Bulk, PICKER, PickItem, ProbeState};

/// Lookups running at the same time (each is a yt-dlp process).
const LOOKUPS: usize = 3;

thread_local! {
    /// `Extractor:id` of each looked-up link, for the duplicate check of the queue rows.
    static KEYS: RefCell<HashMap<String, Option<String>>> = RefCell::default();
}

/// The watcher is on (read by its thread).
static WATCH: AtomicBool = AtomicBool::new(false);

/// "youtube.com/watch?v=…": no scheme or www, not too long.
fn short(url: &str) -> String {
    let s = url.split_once("://").map_or(url, |(_, rest)| rest);
    let s = s.strip_prefix("www.").unwrap_or(s);
    if s.chars().count() > 70 { format!("{}…", s.chars().take(69).collect::<String>()) } else { s.to_string() }
}

/// Text that is exactly one link (what the watcher offers; a report that mentions a link
/// isn't one).
fn single_link(text: &str) -> Option<String> {
    let t = text.trim();
    if t.contains(char::is_whitespace) || t.len() > 2000 {
        return None;
    }
    match probe::split_links(t).as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// The picker is showing pasted links.
pub(crate) fn active(ui: &AppWindow) -> bool {
    ui.global::<Bulk>().get_links()
}

pub(crate) fn clear(ui: &AppWindow) {
    ui.global::<Bulk>().set_links(false);
    KEYS.with(|k| k.borrow_mut().clear());
}

/// "5 links · 1 in the queue · looking up 2…" under the title.
fn summary(ui: &AppWindow) {
    let rows: Vec<PickItem> = ui.get_pick_items().iter().collect();
    let count = |tag: &str| rows.iter().filter(|r| r.tag == tag).count();
    let mut parts = Vec::new();
    for (tag, what) in [
        ("In the queue", "in the queue"),
        ("Downloaded before", "downloaded before"),
        ("Paste it on its own", "to paste on their own"),
        ("Can't download", "can't be downloaded"),
    ] {
        let n = count(tag);
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    let looking = rows.iter().filter(|r| r.detail == "…").count();
    if looking > 0 {
        parts.push(format!("looking up {looking}…"));
    }
    ui.set_probe_meta(if parts.is_empty() { "Untick any you don't want".into() } else { parts.join(" · ").into() });
}

/// List pasted links in the picker and look each one up.
pub(crate) fn show(ui: &AppWindow, state: &ProbeState, links: Vec<String>, my_gen: u64, settings: Settings) {
    let bulk = ui.global::<Bulk>();
    bulk.set_links(true);
    KEYS.with(|k| k.borrow_mut().clear());
    let mut items = Vec::new();
    let mut rows = Vec::new();
    let mut work = VecDeque::new();
    for url in &links {
        // Spotify links (several songs) and their look-up are a preview of their own.
        let own = spotify::parse_link(url).is_some();
        items.push(post::Item {
            kind: post::ItemKind::Video,
            title: String::new(),
            duration: None,
            thumbnail: None,
            url: url.clone(),
            playlist_index: None,
            direct_url: None,
            ext: String::new(),
        });
        rows.push(PickItem {
            kind: "Link".into(),
            title: short(url).into(),
            detail: if own { "".into() } else { "…".into() },
            checked: !own,
            tag: if own { "Paste it on its own".into() } else { "".into() },
            ..Default::default()
        });
        if !own {
            work.push_back(url.clone());
        }
    }
    *state.items.borrow_mut() = items;
    ui.set_pick_checked(rows.iter().filter(|r| r.checked).count() as i32);
    ui.set_pick_items(ModelRc::from(Rc::new(VecModel::from(rows))));
    ui.set_pick_note("".into());
    ui.set_probe_state(2);
    ui.set_probe_blocked(false);
    ui.set_probe_title(format!("{} links", links.len()).into());
    ui.set_probe_thumb(slint::Image::default());
    summary(ui);

    let work = Arc::new(Mutex::new(work));
    for _ in 0..LOOKUPS {
        let (work, weak, generation, settings) =
            (work.clone(), ui.as_weak(), state.generation.clone(), settings.clone());
        std::thread::spawn(move || {
            loop {
                if generation.load(Ordering::SeqCst) != my_gen {
                    return;
                }
                let Some(url) = work.lock().unwrap().pop_front() else { return };
                let result = probe::probe(&url, &settings);
                let thumb = result
                    .as_ref()
                    .ok()
                    .and_then(|i| i.thumbnail_url.as_deref())
                    .and_then(|u| probe::fetch_thumbnail(u).ok());
                let earlier = result
                    .as_ref()
                    .ok()
                    .filter(|i| i.items.len() <= 1)
                    .and_then(|i| history::find(i.key.as_deref(), &url));
                let generation = generation.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    if generation.load(Ordering::SeqCst) != my_gen {
                        return;
                    }
                    looked_up(&ui, &url, result, thumb, earlier.is_some());
                });
            }
        });
    }
}

/// One link's lookup is back: fill in its row.
fn looked_up(
    ui: &AppWindow,
    url: &str,
    result: anyhow::Result<probe::MediaInfo>,
    thumb: Option<std::path::PathBuf>,
    downloaded_before: bool,
) {
    let Some(state) = PICKER.with(|p| p.borrow().as_ref().and_then(|w| w.upgrade())) else { return };
    // Rows may have moved since: find the link's row now.
    let Some(i) = state.items.borrow().iter().position(|it| it.url == url) else { return };
    let model = ui.get_pick_items();
    let Some(mut row) = model.row_data(i) else { return };
    row.detail = "".into();
    match result {
        Ok(info) => {
            if !info.title.is_empty() {
                row.title = info.title.as_str().into();
            }
            if info.items.len() > 1 {
                // A playlist or a post: its own picker.
                row.tag = "Paste it on its own".into();
                row.checked = false;
            } else {
                row.detail = info.duration.map(fmt::duration).unwrap_or_default().into();
                row.can_quality = !info.gif;
                if state.queue.find_unfinished(info.key.as_deref(), url).is_some() {
                    row.tag = "In the queue".into();
                    row.checked = false;
                } else if downloaded_before {
                    row.tag = "Downloaded before".into();
                    row.checked = false;
                }
            }
            KEYS.with(|k| k.borrow_mut().insert(url.to_string(), info.key.clone()));
            if let Some(it) = state.items.borrow_mut().get_mut(i) {
                it.title = info.title.clone();
                it.duration = info.duration;
            }
        }
        Err(_) => {
            row.tag = "Can't download".into();
            row.checked = false;
        }
    }
    if let Some(img) = thumb.and_then(|p| slint::Image::load_from_path(&p).ok()) {
        row.thumb = img;
    }
    model.set_row_data(i, row);
    crate::count_picked(ui);
    summary(ui);
}

/// Download: one plain row per ticked link, in the list's order.
pub(crate) fn enqueue(ui: &AppWindow, state: &ProbeState, queue: &Queue) {
    let (quality, preset) = crate::chosen_quality(ui);
    let items = state.items.borrow().clone();
    for (i, row) in ui.get_pick_items().iter().enumerate() {
        let Some(item) = items.get(i).filter(|_| row.checked) else { continue };
        let (q, format) = crate::pick_choice(row.quality);
        let title = if item.title.is_empty() { item.url.clone() } else { item.title.clone() };
        let mut req = DownloadRequest::new(item.url.clone(), q.unwrap_or(quality), title);
        req.format = format;
        req.preset = preset;
        req.key = KEYS.with(|k| k.borrow().get(&item.url).cloned().flatten());
        queue.add(req);
    }
}

/// Queue these links with the chips as they are, without a preview. Spotify links need
/// their look-up first, so they're left for the link box. Returns what to say.
fn queue_links(ui: &AppWindow, queue: &Queue, links: &[String]) -> String {
    let (quality, preset) = crate::chosen_quality(ui);
    let quality = if ui.get_probe_is_gif() { Quality::Best } else { quality };
    let (mut added, mut queued) = (0, 0);
    let mut spotify_links = Vec::new();
    for url in links {
        if spotify::parse_link(url).is_some() {
            spotify_links.push(url.clone());
            continue;
        }
        if queue.find_unfinished(None, url).is_some() {
            queued += 1;
            continue;
        }
        let mut req = DownloadRequest::new(url.clone(), quality, url.clone());
        req.preset = preset;
        queue.add(req);
        added += 1;
    }
    let mut said = match added {
        0 => String::new(),
        1 => "Downloading it with your settings.".to_string(),
        n => format!("Downloading {n} links with your settings."),
    };
    if queued > 0 {
        let what = if queued == 1 { "One was".to_string() } else { format!("{queued} were") };
        said = format!("{said} {what} already in the queue.").trim().to_string();
    }
    if let Some(first) = spotify_links.first() {
        // Spotify needs the look-up (which songs, how many): show its preview instead.
        if added == 0 {
            ui.set_page(0);
            ui.set_url(first.as_str().into());
            ui.invoke_url_edited(first.as_str().into());
            said = format!("{said} Spotify links need a look-up first: check it, then Download.").trim().to_string();
        } else {
            said = format!("{said} Spotify links were left out: paste them on their own.");
        }
    }
    if added > 0 {
        ui.set_show_tips(false);
    }
    said
}

fn paste_download(ui: &AppWindow, queue: &Queue) {
    if !ui.get_tools_ready() {
        return;
    }
    let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
    let links = probe::split_links(&text);
    if links.is_empty() {
        crate::show_toast(ui, "There's no link on the clipboard. Copy one, then try again.");
        return;
    }
    let said = queue_links(ui, queue, &links);
    if !said.is_empty() {
        crate::show_toast(ui, &said);
    }
}

/// Windows bumps this number whenever anything is copied: read the text only then.
fn clipboard_number() -> u32 {
    #[cfg(windows)]
    {
        // SAFETY: no arguments, only reads a counter.
        unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
    }
    #[cfg(not(windows))]
    0
}

/// The watcher's thread: notices copies, the UI thread decides whether to offer them.
fn start_watcher(ui: &AppWindow) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut last = clipboard_number();
        loop {
            std::thread::sleep(Duration::from_millis(600));
            let now = clipboard_number();
            if now == last {
                continue;
            }
            last = now;
            if !WATCH.load(Ordering::SeqCst) {
                continue;
            }
            if weak.upgrade_in_event_loop(copied).is_err() {
                return;
            }
        }
    });
}

thread_local! {
    /// The last link offered, so copying it again doesn't offer it twice.
    static OFFERED: RefCell<String> = RefCell::default();
}

/// Something was copied while the watcher is on.
fn copied(ui: AppWindow) {
    // Copied inside Neria (the link box, a report): nothing to offer.
    let focused = ui.window().with_winit_window(|w| w.has_focus()).unwrap_or(false);
    if focused && !crate::background_ui::in_tray() {
        return;
    }
    let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
    let Some(link) = single_link(&text) else { return };
    if OFFERED.with(|o| *o.borrow() == link) || ui.get_url().trim() == link {
        return;
    }
    OFFERED.with(|o| *o.borrow_mut() = link.clone());
    ui.global::<Bulk>().set_copied(short(&link).into());
    COPIED.with(|c| *c.borrow_mut() = link.clone());
    if crate::background_ui::in_tray() {
        crate::background_ui::notify(
            &ui,
            crate::background_ui::Note {
                title: "Link copied".into(),
                body: format!("{} · open Neria to download it", short(&link)),
                image: None,
                file: None,
                failed: false,
            },
        );
    }
}

thread_local! {
    /// The full link the strip offers (it shows a shortened one).
    static COPIED: RefCell<String> = RefCell::default();
}

fn take_copied(ui: &AppWindow) -> Option<String> {
    ui.global::<Bulk>().set_copied("".into());
    let link = COPIED.with(|c| std::mem::take(&mut *c.borrow_mut()));
    (!link.is_empty()).then_some(link)
}

pub(crate) fn install(ui: &AppWindow, queue: Queue, settings: Arc<Mutex<Settings>>) {
    let bulk = ui.global::<Bulk>();
    let watch = settings.lock().unwrap().watch_clipboard;
    bulk.set_watch(watch);
    WATCH.store(watch, Ordering::SeqCst);
    start_watcher(ui);

    // Dev hook for design checks: show the strip as if this link had just been copied.
    if let Ok(link) = std::env::var("NERIA_COPIED") {
        bulk.set_copied(short(&link).into());
        COPIED.with(|c| *c.borrow_mut() = link);
    }
    bulk.on_paste_download({
        let (weak, queue) = (ui.as_weak(), queue.clone());
        move || {
            if let Some(ui) = weak.upgrade() {
                paste_download(&ui, &queue);
            }
        }
    });
    bulk.on_watch_changed({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let on = ui.global::<Bulk>().get_watch();
            WATCH.store(on, Ordering::SeqCst);
            let mut s = settings.lock().unwrap();
            s.watch_clipboard = on;
            let _ = s.save();
            if !on {
                take_copied(&ui);
            }
        }
    });
    bulk.on_copied_download({
        let (weak, queue) = (ui.as_weak(), queue.clone());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(link) = take_copied(&ui) else { return };
            let said = queue_links(&ui, &queue, &[link]);
            if !said.is_empty() {
                crate::show_toast(&ui, &said);
            }
        }
    });
    bulk.on_copied_preview({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(link) = take_copied(&ui) else { return };
            ui.set_page(0);
            ui.set_url(link.as_str().into());
            ui.invoke_url_edited(link.into());
        }
    });
    bulk.on_copied_dismiss({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                take_copied(&ui);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_only_a_lone_link() {
        assert_eq!(single_link("  https://youtu.be/a \n").as_deref(), Some("https://youtu.be/a"));
        assert!(single_link("Neria report\nLink: https://youtu.be/a").is_none());
        assert!(single_link("https://youtu.be/a https://youtu.be/b").is_none());
        assert!(single_link("hello").is_none());
        assert_eq!(short("https://www.youtube.com/watch?v=x"), "youtube.com/watch?v=x");
    }
}
