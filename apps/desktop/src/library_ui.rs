//! The Library page (`pages/library.slint`, mock `design/library.html`): everything Neria
//! saved (`neria_core::library`), by day, with kind tabs, Site / Date / Sort and search, the
//! picked thing's details, and what to do next with it. Files are checked in the background
//! (Gone, or "On E:, not connected"); frames for the rows are made once and kept next to
//! `library.json` (`library-thumbs/`), like `recent_ui.rs` does for Recent downloads.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, TimeZone};
use neria_core::library::{self, Item, Kind, Sort, Tab, Where};
use neria_core::settings::Settings;
use neria_core::{fmt, media};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{AppWindow, ClipGif, Compress, LibAttached, LibDetail, LibRow, Library, Music, show_toast};

/// Frames made per pass before the list is shown again.
const FRAMES_AT_ONCE: usize = 10;
/// The Gone check runs at most this often (each time the page opens).
const CHECK_EVERY: Duration = Duration::from_secs(20);

#[derive(Default)]
struct State {
    loaded: bool,
    loading: bool,
    /// Something was saved while reading: read again once this read is shown.
    again: bool,
    items: Vec<Item>,
    places: HashMap<u64, Where>,
    /// file → its cached frame (None: it has none, e.g. sound).
    frames: HashMap<PathBuf, Option<PathBuf>>,
    images: HashMap<PathBuf, slint::Image>,
    /// Lengths found while making frames (records that didn't know theirs).
    lengths: HashMap<u64, f64>,
    making: bool,
    checked: Option<Instant>,
    /// After Find it…: every record found (the picked one first).
    found: Vec<(u64, PathBuf)>,
    /// Sample data (`NERIA_LIBRARY_DEMO`): nothing is read or written.
    demo: bool,
    /// Recordings getting their missed bits back now.
    repairing: std::collections::HashSet<u64>,
    settings: Option<Arc<Mutex<Settings>>>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(v)))
}

// ───────────── Loading and checking ─────────────

/// Read the Library (filled once from history on the very first open), then show it and
/// check its files.
fn load(ui: &AppWindow, settings: Arc<Mutex<Settings>>) {
    if with(|s| {
        if s.loading {
            s.again = true;
        }
        s.demo || s.loading
    }) {
        return;
    }
    with(|s| s.loading = true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let brought = {
            let s = settings.lock().unwrap().clone();
            library::fill_once(&s)
        };
        let items = library::load();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with(|s| {
                s.items = items;
                s.loaded = true;
                s.loading = false;
            });
            if let Some(n) = brought.filter(|n| *n > 0) {
                let things = if n == 1 { "1 thing".to_string() } else { format!("{} things", thousands(n as u64)) };
                ui.global::<Library>().set_brought_in(format!("Brought in {things} Neria saved before.").into());
            }
            rebuild(&ui);
            if with(|s| s.checked.is_none_or(|t| t.elapsed() > CHECK_EVERY)) {
                check(&ui);
            }
            if ui.global::<Library>().get_selected() == 0 && WIDE.with(|w| w.get()) {
                select_first(&ui);
            } else {
                // The picked thing's details may have changed (a transcript attached).
                let id = ui.global::<Library>().get_selected();
                if id != 0 {
                    select(&ui, id as u64);
                }
            }
            if with(|s| std::mem::take(&mut s.again)) {
                load(&ui, settings);
            }
        });
    });
}

thread_local! {
    static WIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Are the files still there? Off the UI thread (a sleeping drive can take seconds).
fn check(ui: &AppWindow) {
    let items = with(|s| {
        s.checked = Some(Instant::now());
        s.items.clone()
    });
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let checked = library::check(&items);
        // Attached files and sizes change (a transcript made later): keep the record current.
        let changed: Vec<(u64, Vec<PathBuf>, Option<u64>)> = checked
            .iter()
            .filter(|c| {
                items.iter().find(|i| i.id == c.id).is_some_and(|i| i.attached != c.attached || i.size != c.size)
            })
            .map(|c| (c.id, c.attached.clone(), c.size))
            .collect();
        library::refresh(&changed);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with(|s| {
                for c in checked {
                    if let Some(it) = s.items.iter_mut().find(|i| i.id == c.id) {
                        it.attached = c.attached;
                        it.size = c.size;
                    }
                    s.places.insert(c.id, c.place);
                }
            });
            rebuild(&ui);
        });
    });
}

fn place(s: &State, it: &Item) -> Where {
    s.places.get(&it.id).cloned().unwrap_or(Where::Here)
}

// ───────────── Frames ─────────────

fn frame_path(file: &Path) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    file.to_string_lossy().to_lowercase().hash(&mut h);
    if let Ok(m) = std::fs::metadata(file) {
        m.len().hash(&mut h);
    }
    library::thumbs_dir().join(format!("{:016x}.jpg", h.finish()))
}

/// Has a picture worth making (a video file that's here).
fn wants_frame(it: &Item) -> bool {
    !it.kind.is_sound() && !it.kind.is_text() && !library::is_audio_file(&it.file)
}

/// Make frames for these rows (a few at a time), then show the list again.
fn make_frames(ui: &AppWindow, ids: &[u64]) {
    let todo: Vec<(u64, PathBuf, bool)> = with(|s| {
        if s.making {
            return Vec::new();
        }
        let todo: Vec<(u64, PathBuf, bool)> = ids
            .iter()
            .filter_map(|id| s.items.iter().find(|i| i.id == *id))
            .filter(|it| place(s, it) == Where::Here && !s.frames.contains_key(&it.file))
            .filter(|it| wants_frame(it) || it.length.is_none())
            .take(FRAMES_AT_ONCE)
            .map(|it| (it.id, it.file.clone(), wants_frame(it)))
            .collect();
        s.making = !todo.is_empty();
        todo
    });
    if todo.is_empty() {
        return;
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let _ = std::fs::create_dir_all(library::thumbs_dir());
        let mut done = Vec::new();
        for (id, file, picture) in todo {
            let cached = frame_path(&file);
            let info = media::file_info(&file).ok();
            let frame = if !picture {
                None
            } else if cached.is_file() {
                Some(cached)
            } else {
                info.as_ref()
                    .filter(|i| i.has_video)
                    .and_then(|i| media::thumbnails(&file, i.duration * 0.3, 1, 108).into_iter().next())
                    .and_then(|made| std::fs::copy(&made, &cached).ok().map(|_| cached))
            };
            let length = info.map(|i| i.duration).filter(|d| *d > 0.0);
            done.push((id, file, frame, length));
        }
        let lengths: Vec<(u64, f64)> = done.iter().filter_map(|(id, _, _, l)| l.map(|l| (*id, l))).collect();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let demo = with(|s| {
                s.making = false;
                for (id, file, frame, length) in done {
                    s.frames.insert(file, frame);
                    if let Some(l) = length {
                        s.lengths.insert(id, l);
                        if let Some(it) = s.items.iter_mut().find(|i| i.id == id && i.length.is_none()) {
                            it.length = Some(l);
                        }
                    }
                }
                s.demo
            });
            if !demo {
                std::thread::spawn(move || library::set_lengths(&lengths));
            }
            rebuild(&ui);
        });
    });
}

fn image_of(s: &mut State, file: &Path) -> slint::Image {
    let Some(Some(frame)) = s.frames.get(file).cloned() else { return slint::Image::default() };
    s.images.entry(frame.clone()).or_insert_with(|| slint::Image::load_from_path(&frame).unwrap_or_default()).clone()
}

// ───────────── Words ─────────────

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "Today", "Yesterday", "Monday", "20 Nov", "20 Nov 2025".
fn day_label(at: u64) -> String {
    let Some(t) = Local.timestamp_opt(at as i64, 0).single() else { return String::new() };
    let now = Local::now();
    let days = (now.date_naive() - t.date_naive()).num_days();
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => t.format("%A").to_string(),
        _ if t.year() == now.year() => t.format("%-d %b").to_string(),
        _ => t.format("%-d %b %Y").to_string(),
    }
}

/// "Today, 13:05", "20 Nov, 22:10".
fn when(at: u64) -> String {
    let Some(t) = Local.timestamp_opt(at as i64, 0).single() else { return String::new() };
    format!("{}, {}", day_label(at), t.format("%H:%M"))
}

fn day_key(at: u64) -> i64 {
    Local.timestamp_opt(at as i64, 0).single().map_or(0, |t| t.date_naive().num_days_from_ce() as i64)
}

fn local_midnight() -> u64 {
    let now = Local::now();
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|t| Local.from_local_datetime(&t).single())
        .map_or(0, |t| t.timestamp().max(0) as u64)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn folder_of(p: &Path) -> String {
    p.parent().map(|d| format!("{}\\", d.display())).unwrap_or_default()
}

/// "youtube.com/watch?v=k3…".
fn short_link(url: &str) -> String {
    let s = url.split_once("://").map_or(url, |(_, r)| r).trim_start_matches("www.");
    if s.chars().count() > 34 { format!("{}…", s.chars().take(33).collect::<String>()) } else { s.to_string() }
}

/// Watch (0) · Play (1) · Open (2) · Update playlist (3) · Download again (4) · Find it… (5).
fn main_action(it: &Item, gone: bool) -> i32 {
    if gone {
        return if !it.url.is_empty() && it.kind != Kind::Playlist { 4 } else { 5 };
    }
    match it.kind {
        Kind::Playlist => 3,
        Kind::Transcript | Kind::Captions | Kind::Gif => 2,
        Kind::Compressed if library::is_audio_file(&it.file) => 1,
        Kind::Compressed => 2,
        k if k.is_sound() || library::is_audio_file(&it.file) => 1,
        _ => 0,
    }
}

fn is_media(it: &Item) -> bool {
    !it.kind.is_text() && it.kind != Kind::Playlist && it.kind != Kind::Gif
}

fn place_words(it: &Item) -> String {
    match it.kind {
        Kind::ReplayPc => "This PC".into(),
        Kind::ReplayPhone => "Phone".into(),
        _ if it.source.is_some() && it.url.is_empty() => {
            format!("from {}", it.source.as_deref().map(file_name).unwrap_or_default())
        }
        _ => it.site.clone(),
    }
}

fn length_of(s: &State, it: &Item) -> Option<f64> {
    it.length.or_else(|| s.lengths.get(&it.id).copied()).filter(|l| *l > 0.0)
}

fn marks(it: &Item) -> String {
    let mut words: Vec<&str> = Vec::new();
    for a in &it.attached {
        let w = library::attached_label(a);
        if !words.contains(&w) {
            words.push(w);
        }
    }
    words.join(" · ")
}

fn songs_in(s: &State, it: &Item) -> usize {
    library::songs_of(&s.items, it).len()
}

fn row_of(s: &mut State, it: &Item) -> LibRow {
    let w = place(s, it);
    let gone = w == Where::Gone;
    let offline = matches!(w, Where::Offline(_));
    let length = length_of(s, it);
    let size = it.size.filter(|b| *b > 0).map(|b| fmt::mb(b as f64));
    let (tag, tag_kind) = match &w {
        Where::Gone => ("Gone".to_string(), 3),
        Where::Offline(d) => (format!("On {d}, not connected"), 0),
        Where::Here => (it.kind.label().to_string(), 0),
    };
    let mut stats: Vec<String> = Vec::new();
    if it.kind == Kind::Playlist {
        let n = songs_in(s, it);
        stats.push(format!("{n} song{}", if n == 1 { "" } else { "s" }));
    }
    if let Some(l) = length {
        stats.push(fmt::duration(l));
    }
    stats.extend(size);
    if it.kind == Kind::Compressed && !it.note.is_empty() {
        stats.push(it.note.clone());
    }
    let place = if gone { format!("was {}", folder_of(&it.file)) } else { place_words(it) };
    LibRow {
        heading: false,
        day: SharedString::default(),
        id: it.id as i32,
        title: it.name().into(),
        tag: tag.into(),
        tag_kind,
        gone,
        offline,
        place: place.into(),
        stats: if gone { String::new() } else { stats.join(" · ") }.into(),
        size: it.size.filter(|b| *b > 0 && !gone).map(|b| fmt::mb(b as f64)).unwrap_or_default().into(),
        marks: marks(it).into(),
        length: if it.kind.is_sound() || it.kind.is_text() { String::new() } else { length.map(fmt::duration).unwrap_or_default() }
            .into(),
        thumb: image_of(s, &it.file),
        sound: it.kind.is_sound() || (library::is_audio_file(&it.file) && it.kind != Kind::Playlist),
        playlist: it.kind == Kind::Playlist,
        text: it.kind.is_text(),
        main: main_action(it, gone),
        can_clip: is_media(it) && !gone && !offline,
    }
}

fn detail_of(s: &mut State, it: &Item) -> LibDetail {
    let row = row_of(s, it);
    let w = place(s, it);
    let here = w == Where::Here;
    let mut keys: Vec<SharedString> = Vec::new();
    let mut values: Vec<SharedString> = Vec::new();
    let mut kv = |k: &str, v: String| {
        if !v.is_empty() {
            keys.push(k.into());
            values.push(v.into());
        }
    };
    let saved = if it.note.is_empty() || it.kind == Kind::Compressed {
        when(it.at)
    } else {
        format!("{} · {}", when(it.at), it.note)
    };
    kv(if it.kind == Kind::Playlist { "Updated" } else { "Saved" }, saved);
    let mut size: Vec<String> = Vec::new();
    if let Some(l) = length_of(s, it) {
        size.push(fmt::duration(l));
    }
    if let Some(b) = it.size.filter(|b| *b > 0) {
        size.push(fmt::mb(b as f64));
    }
    if it.kind == Kind::Compressed && !it.note.is_empty() {
        size.push(it.note.clone());
    }
    kv("Size", size.join(" · "));
    if let Some((a, b)) = it.section {
        kv("Part", format!("{} to {}", fmt::duration(a), fmt::duration(b)));
    }
    if !it.url.is_empty() {
        kv("From", short_link(&it.url));
    }
    if let Some(src) = &it.source {
        kv("Made from", src.display().to_string());
    }
    if let Some(g) = it.gaps.as_ref().filter(|g| !g.holes.is_empty()) {
        let n = g.holes.len();
        let places = if n == 1 { "in 1 place".to_string() } else { format!("in {n} places") };
        let busy = if s.repairing.contains(&it.id) { " · getting them back…" } else { "" };
        kv("Missed", format!("{} {places}{busy}", fmt::duration(g.missed())));
    }
    let file = it.file.display().to_string();
    match &w {
        Where::Gone => kv("Was", file),
        _ if it.kind == Kind::Playlist => kv("Folder", file),
        _ => kv("File", file),
    }
    let attached: Vec<LibAttached> = it
        .attached
        .iter()
        .map(|p| {
            let name = file_name(p);
            let stem = it.file.file_stem().map(|s| s.to_string_lossy().len()).unwrap_or(0);
            LibAttached {
                label: library::attached_label(p).into(),
                detail: name.get(stem.min(name.len())..).unwrap_or("").to_string().into(),
                path: p.to_string_lossy().as_ref().into(),
            }
        })
        .collect();
    let has_text = it.attached.iter().any(|a| matches!(library::attached_label(a), "Transcript" | "Captions"));
    let media = is_media(it);
    let (songs, more) = if it.kind == Kind::Playlist {
        let all = library::songs_of(&s.items, it);
        let shown: Vec<LibAttached> = all
            .iter()
            .take(40)
            .map(|song| LibAttached {
                label: song.name().into(),
                detail: song.length.map(fmt::duration).unwrap_or_default().into(),
                path: song.file.to_string_lossy().as_ref().into(),
            })
            .collect();
        let more = if all.len() > 40 { format!("and {} more", all.len() - 40) } else { String::new() };
        (shown, more)
    } else {
        (Vec::new(), String::new())
    };
    LibDetail {
        id: it.id as i32,
        title: row.title.clone(),
        tag: row.tag.clone(),
        tag_kind: row.tag_kind,
        thumb: row.thumb.clone(),
        length: row.length.clone(),
        sound: row.sound,
        playlist: row.playlist,
        text: row.text,
        gone: row.gone,
        offline: match &w {
            Where::Offline(d) => format!("On {d}, not connected").into(),
            _ => SharedString::default(),
        },
        keys: model(keys),
        values: model(values),
        attached: model(attached),
        songs: model(songs),
        more: more.into(),
        no_transcript: media && here && !has_text,
        main: row.main,
        can_clip: row.can_clip,
        can_compress: here && media && it.kind != Kind::Compressed,
        can_transcribe: here && media,
        can_gif: here
            && matches!(it.kind, Kind::Video | Kind::Clip | Kind::Recording | Kind::ReplayPc | Kind::ReplayPhone)
            && !library::is_audio_file(&it.file),
        can_download: !it.url.is_empty() && it.kind != Kind::Playlist,
        // Any file (to paste into Discord, a chat or a folder); a playlist is a folder.
        can_copy_file: here && it.kind != Kind::Playlist,
        has_link: !it.url.is_empty(),
        has_source: it.source.is_some(),
        spotify: it.url.contains("spotify.com"),
        can_delete: here && it.kind != Kind::Playlist,
        can_rematch: here && it.kind == Kind::Song,
        can_repair: here && !it.url.is_empty() && it.gaps.as_ref().is_some_and(|g| !g.holes.is_empty()),
        repairing: s.repairing.contains(&it.id),
    }
}

// ───────────── The list ─────────────

/// Show the Library as the tabs, menus and search say.
fn rebuild(ui: &AppWindow) {
    let lib = ui.global::<Library>();
    let tab = Tab::from_index(lib.get_tab());
    let query = lib.get_query().to_string();
    let date = lib.get_date();
    let how = Sort::from_index(lib.get_sort());
    let site_pick = lib.get_site();
    let selected = lib.get_selected() as u64;
    let sites_before: Vec<String> = (0..lib.get_sites().row_count())
        .filter_map(|i| lib.get_sites().row_data(i).map(|s| s.to_string()))
        .collect();
    let picked_site = sites_before.get(site_pick.max(0) as usize).cloned().filter(|_| site_pick > 0);
    let since = library::since(date, library::now(), local_midnight());

    let (rows, counts, sites, total, gone, empty, detail, want_frames, no_match) = with(|s| {
        let playlists = library::playlist_dirs(&s.items);
        // A playlist's songs hang under it, unless you're searching for one.
        let listed: Vec<Item> = s
            .items
            .iter()
            .filter(|i| {
                !query.is_empty()
                    || i.kind == Kind::Playlist
                    || i.file.parent().is_none_or(|p| !playlists.contains(p))
            })
            .cloned()
            .collect();
        // Site menu: what's there, by count (before the site is picked).
        let mut by_site: HashMap<String, usize> = HashMap::new();
        for it in &listed {
            *by_site.entry(library::site_name(it)).or_default() += 1;
        }
        let mut sites: Vec<(String, usize)> = by_site.into_iter().collect();
        sites.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let base: Vec<&Item> = listed
            .iter()
            .filter(|i| i.at >= since)
            .filter(|i| picked_site.as_ref().is_none_or(|p| &library::site_name(i) == p))
            .filter(|i| query.is_empty() || library::matches(i, &query))
            .collect();
        let mut counts = [0i32; 7];
        for it in &base {
            counts[0] += 1;
            let t = it.kind.tab();
            if t != Tab::All {
                counts[t as usize] += 1;
            }
            if place(s, it) == Where::Gone {
                counts[6] += 1;
            }
        }
        let mut shown: Vec<&Item> = base
            .iter()
            .copied()
            .filter(|i| match tab {
                Tab::All => true,
                Tab::Gone => place(s, i) == Where::Gone,
                t => i.kind.tab() == t,
            })
            .collect();
        library::sort(&mut shown, how);
        let by_day = matches!(how, Sort::Newest | Sort::Oldest);
        let shown: Vec<Item> = shown.into_iter().cloned().collect();
        let mut rows = Vec::new();
        let mut want = Vec::new();
        let mut i = 0;
        while i < shown.len() {
            let key = day_key(shown[i].at);
            let n = if by_day { shown[i..].iter().take_while(|x| day_key(x.at) == key).count() } else { shown.len() };
            if by_day {
                rows.push(LibRow {
                    heading: true,
                    day: day_label(shown[i].at).to_uppercase().into(),
                    stats: n.to_string().into(),
                    ..LibRow::default()
                });
            }
            for it in &shown[i..i + n] {
                if want.len() < 60 && !s.frames.contains_key(&it.file) {
                    want.push(it.id);
                }
                rows.push(row_of(s, it));
            }
            i += n;
        }
        let all_bytes: u64 = s.items.iter().filter(|i| i.kind != Kind::Playlist).filter_map(|i| i.size).sum();
        let things = listed.len() as u64;
        let total = format!(
            "{} thing{} · {}",
            thousands(things),
            if things == 1 { "" } else { "s" },
            fmt::mb(all_bytes as f64)
        );
        let gone = listed.iter().filter(|i| place(s, i) == Where::Gone).count();
        let empty = s.items.is_empty() && s.loaded;
        let detail = s.items.iter().find(|i| i.id == selected).cloned().map(|it| detail_of(s, &it));
        // "Nothing from Twitch in the last 7 days."
        let mut words = vec!["Nothing".to_string()];
        if tab != Tab::All {
            words = vec![format!("No {}", ["", "videos", "sound", "clips", "recordings", "replays", "gone files"][tab as usize])];
        }
        if !query.is_empty() {
            words.push(format!("matches “{query}”"));
        }
        if let Some(p) = &picked_site {
            words.push(format!("from {p}"));
        }
        if date > 0 {
            words.push(["", "today", "in the last 7 days", "in the last 30 days", "in the last 12 months"][date as usize].into());
        }
        let no_match = format!("{}.", words.join(" "));
        (rows, counts, sites, total, gone, empty, detail, want, no_match)
    });
    let mut site_names: Vec<SharedString> = vec!["Any site".into()];
    let mut site_n = vec![0];
    for (name, n) in &sites {
        site_names.push(name.as_str().into());
        site_n.push(*n as i32);
    }
    // Keep the picked site picked (its place in the menu can move).
    if let Some(p) = &picked_site {
        let at = site_names.iter().position(|n| n.as_str() == p).unwrap_or(0);
        lib.set_site(at as i32);
    }
    lib.set_rows(model(rows));
    lib.set_counts(model(counts.to_vec()));
    lib.set_sites(model(site_names));
    lib.set_site_counts(model(site_n));
    lib.set_total(total.into());
    lib.set_gone(gone as i32);
    lib.set_gone_text(
        format!("{gone} file{} moved or deleted since they were saved.", if gone == 1 { " was" } else { "s were" }).into(),
    );
    lib.set_empty(empty);
    lib.set_no_match(no_match.into());
    match detail {
        Some(d) => lib.set_detail(d),
        None if selected != 0 => {
            lib.set_selected(0);
            lib.set_sheet_open(false);
        }
        None => {}
    }
    if !want_frames.is_empty() {
        make_frames(ui, &want_frames);
    }
}

fn select(ui: &AppWindow, id: u64) {
    let lib = ui.global::<Library>();
    lib.set_selected(id as i32);
    let detail = with(|s| s.items.iter().find(|i| i.id == id).cloned().map(|it| detail_of(s, &it)));
    if let Some(d) = detail {
        lib.set_detail(d);
    }
}

fn select_first(ui: &AppWindow) {
    let rows = ui.global::<Library>().get_rows();
    let first = (0..slint::Model::row_count(&rows))
        .filter_map(|i| slint::Model::row_data(&rows, i))
        .find(|r| !r.heading)
        .map(|r| r.id as u64);
    if let Some(id) = first {
        select(ui, id);
    }
}

fn item(id: u64) -> Option<Item> {
    with(|s| s.items.iter().find(|i| i.id == id).cloned())
}

/// Forget records here and in `library.json` (not in the demo).
fn forget(ui: &AppWindow, ids: &[u64]) {
    let demo = with(|s| {
        s.items.retain(|i| !ids.contains(&i.id));
        s.demo
    });
    if !demo {
        let ids = ids.to_vec();
        std::thread::spawn(move || library::remove(&ids));
    }
    if ids.contains(&(ui.global::<Library>().get_selected() as u64)) {
        ui.global::<Library>().set_selected(0);
        ui.global::<Library>().set_sheet_open(false);
    }
    rebuild(ui);
}

/// Records found somewhere else (Find it…): saved, checked again, shown.
fn relocate(ui: &AppWindow, moves: Vec<(u64, PathBuf)>) {
    let demo = with(|s| {
        for (id, to) in &moves {
            if let Some(it) = s.items.iter_mut().find(|i| i.id == *id) {
                it.file = to.clone();
                it.attached = library::attached_of(to);
            }
            s.places.insert(*id, Where::Here);
        }
        s.demo
    });
    if !demo {
        std::thread::spawn(move || library::relocate(&moves));
    }
    rebuild(ui);
}

// ───────────── Doing things ─────────────

fn go_download(ui: &AppWindow, url: &str) {
    ui.set_page(0);
    ui.set_url(url.into());
    ui.invoke_url_edited(url.into());
}

/// Find it…: where did this file go? Then the others gone from the same folder, there too.
fn find_one(ui: &AppWindow, it: &Item) {
    let mut start = it.file.parent().map(Path::to_path_buf);
    while let Some(d) = start.clone() {
        if d.is_dir() {
            break;
        }
        start = d.parent().map(Path::to_path_buf);
    }
    let mut dialog = rfd::FileDialog::new().set_title(format!("Where is {}?", file_name(&it.file)));
    if let Some(d) = start {
        dialog = dialog.set_directory(d);
    }
    if let Some(ext) = it.file.extension() {
        let ext = ext.to_string_lossy().into_owned();
        dialog = dialog.add_filter(ext.to_uppercase(), &[ext.as_str()]);
    }
    let Some(found) = dialog.pick_file() else { return };
    let items = with(|s| s.items.clone());
    let all = library::find_with(&items, it.id, &found);
    let dir = found.parent().map(|d| d.display().to_string()).unwrap_or_default();
    let lib = ui.global::<Library>();
    if all.len() <= 1 {
        relocate(ui, all);
        lib.set_found_text(format!("Found {} in {dir}.", it.name()).into());
        lib.set_found_count(1);
        with(|s| s.found.clear());
    } else {
        let more = all.len() - 1;
        lib.set_found_text(
            format!(
                "Found {} in {dir}, and {more} more gone file{} from the same folder there too.",
                it.name(),
                if more == 1 { "" } else { "s" }
            )
            .into(),
        );
        lib.set_found_count(all.len() as i32);
        with(|s| s.found = all);
    }
    lib.set_found_open(true);
}

/// Find them…: one folder, every gone file whose name is in it.
fn find_gone(ui: &AppWindow) {
    let Some(dir) = rfd::FileDialog::new().set_title("Where did the gone files go?").pick_folder() else { return };
    let moves: Vec<(u64, PathBuf)> = with(|s| {
        s.items
            .iter()
            .filter(|i| place(s, i) == Where::Gone)
            .filter_map(|i| {
                let there = dir.join(i.file.file_name()?);
                there.exists().then_some((i.id, there))
            })
            .collect()
    });
    let n = moves.len();
    relocate(ui, moves);
    let lib = ui.global::<Library>();
    lib.set_found_text(
        match n {
            0 => format!("None of the gone files are in {}.", dir.display()),
            1 => format!("Found 1 gone file in {}.", dir.display()),
            _ => format!("Found {n} gone files in {}.", dir.display()),
        }
        .into(),
    );
    lib.set_found_count(1);
    lib.set_found_open(true);
}

/// Put the file itself on the clipboard (paste into Discord, a chat or a folder).
#[cfg(windows)]
fn copy_file(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::DROPFILES;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let head = std::mem::size_of::<DROPFILES>();
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, head + wide.len() * 2) else { return false };
        let p = GlobalLock(mem) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        let files = p as *mut DROPFILES;
        (*files).pFiles = head as u32;
        (*files).fWide = true.into();
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, p.add(head), wide.len() * 2);
        let _ = GlobalUnlock(mem);
        if OpenClipboard(None).is_err() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        let _ = EmptyClipboard();
        let ok = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(mem.0))).is_ok();
        let _ = CloseClipboard();
        if !ok {
            let _ = GlobalFree(Some(HGLOBAL(mem.0)));
        }
        ok
    }
}

#[cfg(not(windows))]
fn copy_file(_: &Path) -> bool {
    false
}

fn act(ui: &AppWindow, id: u64, what: &str) {
    let Some(it) = item(id) else { return };
    let here = with(|s| place(s, &it)) == Where::Here;
    let gone = with(|s| place(s, &it)) == Where::Gone;
    let path: SharedString = it.file.to_string_lossy().as_ref().into();
    let what = if what == "main" {
        match main_action(&it, gone) {
            0 | 1 => "watch",
            2 => "open-with",
            3 => "update",
            4 => "download",
            _ => "find",
        }
    } else {
        what
    };
    if !here && !matches!(what, "download" | "update" | "find" | "remove" | "copy-link" | "spotify" | "original") {
        show_toast(ui, "That file isn't there. Find it first, or download it again.");
        return;
    }
    ui.global::<Library>().set_sheet_open(false);
    match what {
        "watch" => ui.invoke_watch_file(path),
        "clip" => {
            // A clip of a file: back to the original at its In / Out, to cut it again.
            let again = (it.kind == Kind::Clip)
                .then(|| it.source.clone().zip(it.section))
                .flatten()
                .filter(|(src, _)| src.is_file());
            match again {
                Some((src, (a, b))) => {
                    ui.invoke_clip_open_path(src.to_string_lossy().as_ref().into());
                    let weak = ui.as_weak();
                    slint::Timer::single_shot(Duration::from_millis(1200), move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_clip_in(a as f32);
                            ui.set_clip_out(b as f32);
                        }
                    });
                }
                None => ui.invoke_clip_open_path(path),
            }
            ui.set_page(1);
        }
        "compress" => {
            ui.set_page(2);
            ui.global::<Compress>().invoke_open_path(path);
        }
        "transcribe" => crate::transcribe_ui::open_files(ui, vec![it.file.clone()]),
        "gif" => {
            ui.invoke_clip_open_path(path);
            ui.set_page(1);
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(1500), move || {
                if let Some(ui) = weak.upgrade() {
                    let g = ui.global::<ClipGif>();
                    g.set_open(true);
                    g.invoke_changed();
                }
            });
        }
        "show" if it.kind == Kind::Playlist => {
            let _ = open::that(&it.file);
        }
        "show" => crate::reveal(&it.file),
        "open-with" => {
            let _ = open::that(&it.file);
        }
        "download" | "update" => go_download(ui, &it.url),
        "copy-link" => match arboard::Clipboard::new().and_then(|mut c| c.set_text(it.url.clone())) {
            Ok(()) => show_toast(ui, "Link copied"),
            Err(_) => show_toast(ui, "Couldn't copy the link. Try again."),
        },
        "copy-file" => {
            if copy_file(&it.file) {
                show_toast(ui, "File copied. Paste it into Discord, a chat or a folder (Ctrl+V).");
            } else {
                show_toast(ui, "Couldn't copy the file. Try again.");
            }
        }
        "original" => {
            let Some(src) = it.source.clone() else { return };
            let original = with(|s| s.items.iter().find(|i| i.file == src).map(|i| i.id));
            match original {
                Some(oid) => {
                    let lib = ui.global::<Library>();
                    lib.set_tab(0);
                    lib.set_query("".into());
                    lib.set_site(0);
                    lib.set_date(0);
                    rebuild(ui);
                    select(ui, oid);
                }
                None if src.exists() => crate::reveal(&src),
                None => show_toast(ui, "The original isn't there any more."),
            }
        }
        "spotify" => {
            let _ = open::that(&it.url);
        }
        "lyrics" => {
            let music = ui.global::<Music>();
            music.set_lyrics_dir(path);
            music.invoke_add_lyrics();
        }
        "find" => find_one(ui, &it),
        "rematch" => crate::match_ui::open_song(it.file.clone(), it.url.clone()),
        "repair" => repair(ui, &it),
        "remove" => {
            forget(ui, &[id]);
            show_toast(ui, if here { "Removed from the Library. The file is still there." } else { "Removed from the Library" });
        }
        _ => {}
    }
}

/// Get the missed bits back: fetch a recording's holes from the site again and splice them
/// in (the file changes in place; its record keeps what's still missing).
fn repair(ui: &AppWindow, it: &Item) {
    let Some(gaps) = it.gaps.clone() else { return };
    let (busy, demo, settings) = with(|s| (s.repairing.contains(&it.id), s.demo, s.settings.clone()));
    if busy {
        show_toast(ui, "Still getting them back. It can take a few minutes.");
        return;
    }
    if demo {
        show_toast(ui, "Getting the missed bits back…");
        return;
    }
    let Some(settings) = settings else { return };
    with(|s| s.repairing.insert(it.id));
    select(ui, it.id);
    show_toast(ui, "Getting the missed bits back from the site. It can take a few minutes.");
    let (weak, id, file, url) = (ui.as_weak(), it.id, it.file.clone(), it.url.clone());
    std::thread::spawn(move || {
        let s = settings.lock().unwrap().clone();
        let never = std::sync::atomic::AtomicBool::new(false);
        let result = neria_core::record::get_holes_back(&file, &url, &gaps, &s, &never);
        let left = match &result {
            Ok(left) => Some(neria_core::record::Gaps { holes: left.clone(), ..gaps.clone() }),
            Err(_) => Some(gaps.clone()),
        };
        let now = library::set_gaps(id, left.clone());
        let _ = weak.upgrade_in_event_loop(move |ui| {
            with(|s| {
                s.repairing.remove(&id);
                if let (Some(it), Some(now)) = (s.items.iter_mut().find(|i| i.id == id), now) {
                    *it = now;
                }
            });
            let got = gaps.missed() - left.as_ref().map_or(0.0, |g| g.missed());
            let msg = match result {
                Ok(left) if left.is_empty() => format!("Got back all {} missed", fmt::duration(gaps.missed())),
                Ok(_) if got > 0.0 => format!("Got back {} of {} missed", fmt::duration(got), fmt::duration(gaps.missed())),
                Ok(_) => "Fetched the missed bits, but they didn't line up with the recording".to_string(),
                Err(e) => format!("Couldn't get them back: {e}"),
            };
            show_toast(&ui, &msg);
            rebuild(&ui);
            if ui.global::<Library>().get_selected() as u64 == id {
                select(&ui, id);
            }
        });
    });
}

/// Delete file… said yes: to the Recycle Bin, and out of the Library.
fn delete(ui: &AppWindow) {
    let lib = ui.global::<Library>();
    lib.set_delete_open(false);
    let id = lib.get_detail().id as u64;
    let Some(it) = item(id) else { return };
    if with(|s| s.demo) {
        forget(ui, &[id]);
        return;
    }
    match trash::delete(&it.file) {
        Ok(()) => {
            forget(ui, &[id]);
            show_toast(ui, "Moved to the Recycle Bin");
        }
        Err(e) => show_toast(ui, &format!("Couldn't delete it: {e}")),
    }
}

pub fn install(ui: &AppWindow, settings: Arc<Mutex<Settings>>) {
    with(|s| s.settings = Some(settings.clone()));
    let lib = ui.global::<Library>();
    lib.on_shown({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move |wide| {
            let ui = weak.unwrap();
            WIDE.with(|w| w.set(wide));
            if with(|s| s.demo) {
                rebuild(&ui);
                if wide && ui.global::<Library>().get_selected() == 0 {
                    select_first(&ui);
                }
                return;
            }
            // Read again each time: things were saved meanwhile.
            load(&ui, settings.clone());
        }
    });
    lib.on_changed({
        let weak = ui.as_weak();
        move || rebuild(&weak.unwrap())
    });
    lib.on_clear_filters({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let lib = ui.global::<Library>();
            lib.set_query("".into());
            lib.set_site(0);
            lib.set_date(0);
            lib.set_tab(0);
            rebuild(&ui);
        }
    });
    lib.on_select({
        let weak = ui.as_weak();
        move |id| select(&weak.unwrap(), id as u64)
    });
    lib.on_act({
        let weak = ui.as_weak();
        move |id, what| act(&weak.unwrap(), id as u64, what.as_str())
    });
    lib.on_open_file(|path| {
        let _ = open::that(path.as_str());
    });
    lib.on_delete_confirm({
        let weak = ui.as_weak();
        move || delete(&weak.unwrap())
    });
    lib.on_find_gone({
        let weak = ui.as_weak();
        move || find_gone(&weak.unwrap())
    });
    lib.on_remove_gone({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let ids: Vec<u64> =
                with(|s| s.items.iter().filter(|i| place(s, i) == Where::Gone).map(|i| i.id).collect());
            let n = ids.len();
            forget(&ui, &ids);
            show_toast(&ui, &format!("Removed {n} gone file{} from the Library", if n == 1 { "" } else { "s" }));
        }
    });
    lib.on_found_all({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let all = with(|s| std::mem::take(&mut s.found));
            ui.global::<Library>().set_found_open(false);
            relocate(&ui, all);
        }
    });
    lib.on_found_one({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let first: Vec<(u64, PathBuf)> = with(|s| std::mem::take(&mut s.found).into_iter().take(1).collect());
            ui.global::<Library>().set_found_open(false);
            relocate(&ui, first);
        }
    });
    lib.on_found_close({
        let weak = ui.as_weak();
        move || weak.unwrap().global::<Library>().set_found_open(false)
    });
    // Saved elsewhere while the page shows (an instant replay, a finished download, a
    // transcript): read again at once. A closed page reads again when it opens.
    {
        let (weak, settings) = (ui.as_weak(), settings.clone());
        library::on_saved(move || {
            let (weak, settings) = (weak.clone(), settings.clone());
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade()
                    && ui.get_page() == 4
                    && ui.window().is_visible()
                {
                    load(&ui, settings);
                }
            });
        });
    }
    demo(ui);
    press_for_checks(ui);
}

/// `NERIA_LIBRARY_ACT`: press Library actions as if clicked, for checks in the real app:
/// `<part of a title>=<action>` steps, `;` between, one every `NERIA_LIBRARY_ACT_EVERY`
/// seconds (default 4). Actions are `act`'s (`watch`, `clip`, `compress`, `transcribe`, `gif`,
/// `show`, `copy-file`, `copy-link`, `download`, `update`, `lyrics`, `original`, `rematch`,
/// `repair`, `remove`, `find`…) plus `select`, `delete` (Delete file… said yes); and, with any title, `back` (the
/// player's Back), `library` (the page, as from the rail), `match-use` (Pick another match:
/// Use this on its first row).
fn press_for_checks(ui: &AppWindow) {
    let Ok(steps) = std::env::var("NERIA_LIBRARY_ACT") else { return };
    let every = std::env::var("NERIA_LIBRARY_ACT_EVERY").ok().and_then(|s| s.parse().ok()).unwrap_or(4.0);
    let steps: Vec<(String, String)> = steps
        .split(';')
        .filter_map(|s| s.split_once('=').map(|(a, b)| (a.trim().to_lowercase(), b.trim().to_string())))
        .collect();
    for (n, (title, what)) in steps.into_iter().enumerate() {
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_secs_f64(every * (n as f64 + 1.0) + 2.0), move || {
            let Some(ui) = weak.upgrade() else { return };
            match what.as_str() {
                // Watching: the player's Back, as clicked.
                "back" => return ui.global::<crate::Watch>().invoke_back(),
                // The Library page, as from the rail.
                "library" => return ui.set_page(4),
                // Pick another match: "Use this" on its first row.
                "match-use" => return ui.invoke_match_use(0),
                _ => {}
            }
            let id = with(|s| s.items.iter().rev().find(|i| i.name().to_lowercase().contains(&title)).map(|i| i.id));
            let Some(id) = id else {
                eprintln!("NERIA_LIBRARY_ACT: nothing called {title:?}");
                return;
            };
            eprintln!("NERIA_LIBRARY_ACT: {what} on {title:?} ({id})");
            select(&ui, id);
            match what.as_str() {
                "select" => {}
                "delete" => delete(&ui),
                _ => act(&ui, id, &what),
            }
        });
    }
}

// ───────────── Sample data (NERIA_LIBRARY_DEMO) ─────────────

/// `NERIA_LIBRARY_DEMO`: the page with sample things, nothing read or written.
/// `1` rows · `brought` with the one-time line · `gone` gone and not-connected rows with the
/// banner · `found` after Find it… · `empty` the first visit · `sheet` a thing picked (the
/// sheet on compact, opened in place on medium) · `playlist` the playlist picked ·
/// `filter` searching with a site set · `nomatch` · `delete` the Delete file… question.
fn demo(ui: &AppWindow) {
    let Ok(which) = std::env::var("NERIA_LIBRARY_DEMO") else { return };
    // Real files where there are some (the ui-check samples in history.json), so the rows get
    // frames: the first ones' folder.
    let history = neria_core::history::recent(10, |_| true);
    let dir = history.first().and_then(|r| r.file.parent().map(Path::to_path_buf)).unwrap_or_else(std::env::temp_dir);
    let pick = |names: &[&str]| -> PathBuf {
        names.iter().map(|n| dir.join(n)).find(|p| p.is_file()).unwrap_or_else(|| dir.join(names[0]))
    };
    let video = pick(&["sample.mp4", "chapters.mp4"]);
    let other = pick(&["timelapse.mp4", "sample.mp4"]);
    let replay = pick(&["replay.mp4", "sample.mp4"]);
    let song = pick(&["song.mp3"]);
    let now = library::now();
    let h = 3600;
    let d = 86_400;
    let mut n = 0;
    let mut mk = |kind: Kind, file: &Path, title: &str, ago: u64, f: &dyn Fn(&mut Item)| {
        n += 1;
        let mut it = Item { id: n, at: now - ago, size: std::fs::metadata(file).ok().map(|m| m.len()), ..Item::new(kind, file, title) };
        f(&mut it);
        it
    };
    let att = |names: &[&str]| -> Vec<PathBuf> { names.iter().map(|n| dir.join(n)).collect() };
    let mut items = vec![
        mk(Kind::Recording, &video, "IShowSpeed · Day 3 in Tokyo (full stream)", h, &|i| {
            i.site = "YouTube".into();
            i.url = "https://www.youtube.com/watch?v=k3Tokyo".into();
            i.length = Some(8048.0);
            i.size = Some(3_100_000_000);
            i.note = "recorded from the start".into();
            i.attached = att(&["sample.chat.json"]);
            i.gaps = Some(neria_core::record::Gaps {
                holes: vec![
                    neria_core::record::Hole { at: 1865.0, len: 42.0, about: false },
                    neria_core::record::Hole { at: 5210.0, len: 31.0, about: true },
                ],
                started_at: 0.0,
                starts: 0.0,
            });
        }),
        mk(Kind::ReplayPc, &replay, "VALORANT · 14:32", 2 * h, &|i| {
            i.length = Some(60.0);
            i.size = Some(118_000_000);
        }),
        mk(Kind::Clip, &other, "ohnepixel · the knife reveal", 3 * h, &|i| {
            i.site = "Twitch".into();
            i.url = "https://www.twitch.tv/videos/2201".into();
            i.length = Some(31.0);
            i.section = Some((3720.0, 3751.0));
            i.attached = att(&["timelapse.chat.json"]);
        }),
        mk(Kind::Video, &video, "How the James Webb telescope unfolds", d + h, &|i| {
            i.site = "YouTube".into();
            i.url = "https://www.youtube.com/watch?v=jwst".into();
            i.length = Some(761.0);
            i.size = Some(240_000_000);
            i.attached = att(&["sample.en.srt", "sample.txt"]);
        }),
        mk(Kind::Playlist, &dir.join("Chill Mix"), "Chill Mix", d + 2 * h, &|i| {
            i.site = "Spotify".into();
            i.url = "https://open.spotify.com/playlist/37i9dQZF1DX4WYpdgoIcn6".into();
            i.length = Some(10270.0);
            i.size = Some(410_000_000);
        }),
        mk(Kind::Compressed, &other, "match-final (Discord).mp4", d + 3 * h, &|i| {
            i.source = Some(dir.join("match-final.mp4"));
            i.length = Some(42.0);
            i.size = Some(9_600_000);
            i.note = "was 212 MB".into();
        }),
        mk(Kind::Song, &song, "a-ha - Take On Me", 2 * d, &|i| {
            i.site = "Spotify".into();
            i.url = "https://open.spotify.com/track/2WfaOiMkCvy7F5fcp2zZ8L".into();
            i.length = Some(225.0);
            i.attached = att(&["song.lrc"]);
        }),
        mk(Kind::ReplayPhone, &replay, "POCO X7 Pro · Brawl Stars", 3 * d, &|i| {
            i.length = Some(30.0);
        }),
        mk(Kind::Gif, &other, "cat-jump.gif", 3 * d + h, &|i| {
            i.source = Some(dir.join("cat.mp4"));
            i.length = Some(4.0);
        }),
        mk(Kind::Transcript, &dir.join("interview.txt"), "interview", 5 * d, &|i| {
            i.source = Some(dir.join("interview.m4a"));
        }),
    ];
    // The playlist's songs sit in its folder.
    let pl = dir.join("Chill Mix");
    for (k, t) in ["Khruangbin - Maria También", "Men I Trust - Show Me How", "Mild High Club - Homage"].iter().enumerate() {
        let f = pl.join(format!("{t}.mp3"));
        items.push(mk(Kind::Song, &f, t, d + 2 * h, &|i| {
            i.site = "Spotify".into();
            i.length = Some(200.0 + k as f64 * 31.0);
        }));
    }
    let gone_too = matches!(which.as_str(), "gone" | "found");
    if gone_too {
        items.push(mk(Kind::Video, &dir.join("gone").join("Lecture 7 · Fourier series.mp4"), "Lecture 7 · Fourier series", 4 * d, &|i| {
            i.site = "YouTube".into();
            i.url = "https://www.youtube.com/watch?v=fourier7".into();
            i.length = Some(2900.0);
        }));
        items.push(mk(Kind::ReplayPc, &dir.join("gone").join("VALORANT 2 Nov 22.10.mp4"), "VALORANT · 2 Nov, 22:10", 6 * d, &|i| {
            i.length = Some(60.0);
        }));
        items.push(mk(Kind::ReplayPc, &dir.join("gone").join("VALORANT 2 Nov 22.40.mp4"), "VALORANT · 2 Nov, 22:40", 6 * d + h, &|i| {
            i.length = Some(60.0);
        }));
        let free = ('E'..='Z').rev().find(|l| !Path::new(&format!("{l}:\\")).exists()).unwrap_or('Q');
        items.push(mk(Kind::ReplayPhone, Path::new(&format!("{free}:\\Phone\\Brawl Stars 0.30.mp4")), "POCO X7 Pro · Brawl Stars", 7 * d, &|i| {
            i.length = Some(30.0);
        }));
    }
    if which == "empty" {
        items.clear();
    }
    let places: HashMap<u64, Where> = items
        .iter()
        .map(|i| {
            let w = if i.kind == Kind::Playlist || i.file.starts_with(&pl) { Where::Here } else { library::where_is(&i.file) };
            // Sample files that aren't in the scratch home still show as here (except the
            // gone ones on purpose).
            let w = if w == Where::Gone && !i.file.starts_with(dir.join("gone")) { Where::Here } else { w };
            (i.id, w)
        })
        .collect();
    with(|s| {
        s.items = items;
        s.places = places;
        s.loaded = true;
        s.demo = true;
        s.checked = Some(Instant::now());
    });
    let lib = ui.global::<Library>();
    match which.as_str() {
        "brought" => lib.set_brought_in("Brought in 214 things Neria saved before.".into()),
        "filter" => {
            lib.set_query("tokyo".into());
            lib.set_sites(model(vec!["Any site".into(), "YouTube".into()]));
            lib.set_site(1);
        }
        "nomatch" => {
            lib.set_query("fourier".into());
            lib.set_date(2);
        }
        _ => {}
    }
    rebuild(ui);
    let first_id = |ui: &AppWindow| {
        let rows = ui.global::<Library>().get_rows();
        (0..slint::Model::row_count(&rows))
            .filter_map(|i| slint::Model::row_data(&rows, i))
            .find(|r| !r.heading)
            .map(|r| r.id as u64)
    };
    match which.as_str() {
        "sheet" | "delete" => {
            if let Some(id) = first_id(ui) {
                select(ui, id);
                lib.set_sheet_open(true);
            }
            if which == "delete" {
                lib.set_delete_open(true);
            }
        }
        "playlist" => {
            select(ui, 5);
            lib.set_sheet_open(true);
        }
        "menu" => {
            // The recording with holes, its ⋯ menu open (once the page is up).
            let id = with(|s| s.items.iter().find(|i| i.kind == Kind::Recording).map(|i| i.id));
            if let Some(id) = id {
                select(ui, id);
                lib.set_sheet_open(true);
            }
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(800), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<Library>().set_menu_demo(true);
                }
            });
        }
        "gone" => {
            select(ui, 14);
            lib.set_sheet_open(true);
        }
        "found" => {
            lib.set_found_text(
                "Found VALORANT · 2 Nov, 22:10 in E:\\Old clips, and 1 more gone file from the same folder there too."
                    .into(),
            );
            lib.set_found_count(2);
            lib.set_found_open(true);
        }
        _ => {}
    }
}
