//! Edit tags (`ui/dialogs/tags.slint`, mock `design/audio-tools.html` 3A): one sound file or
//! several. Fields the files agree on show their value; fields they differ in say "Several"
//! and stay as each file has them unless something is typed. Find tags online (one file):
//! `neria_core::tags::find_online`. Writing: `neria_core::tags::write`.

use std::cell::RefCell;
use std::path::PathBuf;

use neria_core::tags::{self, Tags};
use slint::ComponentHandle;

use crate::{AppWindow, TagEdit, show_toast};

#[derive(Default)]
struct State {
    files: Vec<PathBuf>,
    /// The record's Spotify link, for Find tags online (one file).
    url: String,
    /// What each field showed when the dialog opened (to tell what was typed).
    shown: Tags,
    /// A cover found online, written with Save.
    cover: Option<PathBuf>,
    /// Track numbers set by "Number in order".
    numbered: bool,
}

thread_local! {
    static ST: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

/// The value all of them share, or None (and "several") when they differ.
fn common(all: &[Tags], get: impl Fn(&Tags) -> Option<String>) -> (Option<String>, bool) {
    let first = all.first().and_then(&get);
    let same = all.iter().all(|t| get(t) == first);
    if same { (first, false) } else { (None, true) }
}

/// Open the dialog on these files (read in the background).
pub fn open(ui: &AppWindow, files: Vec<PathBuf>, url: String) {
    if files.is_empty() {
        return;
    }
    let t = ui.global::<TagEdit>();
    let n = files.len();
    t.set_count(n as i32);
    t.set_subtitle(match n {
        1 => files[0].file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default().into(),
        n => format!("{n} songs").into(),
    });
    t.set_title("".into());
    t.set_artist("".into());
    t.set_album("".into());
    t.set_track("".into());
    t.set_year("".into());
    t.set_status_bad(false);
    t.set_status("Reading the tags…".into());
    t.set_has_cover(false);
    t.set_cover(slint::Image::default());
    t.set_finding(false);
    t.set_saving(false);
    t.set_open(true);
    with(|s| *s = State { files: files.clone(), url, ..State::default() });
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let all: Vec<Tags> = files.iter().map(|f| tags::read(f)).collect();
        let cover = (files.len() == 1).then(|| tags::cover(&files[0])).flatten();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let t = ui.global::<TagEdit>();
            let (title, ts) = common(&all, |x| x.title.clone());
            let (artist, ars) = common(&all, |x| x.artist.clone());
            let (album, als) = common(&all, |x| x.album.clone());
            let (track, trs) = common(&all, |x| x.track.clone());
            let (year, ys) = common(&all, |x| x.year.clone());
            t.set_title(title.clone().unwrap_or_default().into());
            t.set_artist(artist.clone().unwrap_or_default().into());
            t.set_album(album.clone().unwrap_or_default().into());
            t.set_track(track.clone().unwrap_or_default().into());
            t.set_year(year.clone().unwrap_or_default().into());
            t.set_title_several(ts);
            t.set_artist_several(ars);
            t.set_album_several(als);
            t.set_track_several(trs);
            t.set_year_several(ys);
            if let Some(img) = cover.and_then(|c| slint::Image::load_from_path(&c).ok()) {
                t.set_cover(img);
                t.set_has_cover(true);
            }
            t.set_status("".into());
            with(|s| s.shown = Tags { title, artist, album, track, year });
        });
    });
}

/// What to write: a field typed in (or found) is set for all; one left as it showed stays.
fn asked(t: &TagEdit, shown: &Tags) -> Tags {
    let pick = |now: slint::SharedString, was: &Option<String>, several: bool| {
        let now = now.trim().to_string();
        let was = was.clone().unwrap_or_default();
        if several && now.is_empty() || !several && now == was { None } else { Some(now) }
    };
    Tags {
        title: pick(t.get_title(), &shown.title, t.get_title_several()),
        artist: pick(t.get_artist(), &shown.artist, t.get_artist_several()),
        album: pick(t.get_album(), &shown.album, t.get_album_several()),
        track: pick(t.get_track(), &shown.track, t.get_track_several()),
        year: pick(t.get_year(), &shown.year, t.get_year_several()),
    }
}

fn save(ui: &AppWindow) {
    let t = ui.global::<TagEdit>();
    let (files, shown, cover, numbered) = with(|s| (s.files.clone(), s.shown.clone(), s.cover.clone(), s.numbered));
    let mut want = asked(&t, &shown);
    if numbered {
        want.track = None;
    }
    if want == Tags::default() && cover.is_none() && !numbered {
        t.set_open(false);
        show_toast(ui, "Nothing to change: the tags already say that.");
        return;
    }
    t.set_saving(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let total = files.len();
        let mut failed = Vec::new();
        let mut titles = Vec::new();
        for (i, f) in files.iter().enumerate() {
            let mut each = want.clone();
            if numbered {
                each.track = Some(format!("{}/{total}", i + 1));
            }
            match tags::write(f, &each, cover.as_deref()) {
                Ok(()) => {
                    // The Library's row says the song as its tags now do: "Artist - Title".
                    let now = tags::read(f);
                    if let Some(title) = now.title {
                        let name = match now.artist {
                            Some(a) => format!("{a} - {title}"),
                            None => title,
                        };
                        titles.push((f.clone(), name));
                    }
                }
                Err(e) => {
                    failed.push(format!("{}: {e}", f.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()))
                }
            }
        }
        neria_core::library::retitle(&titles);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            crate::library_ui::forget_frames(&files);
            let t = ui.global::<TagEdit>();
            t.set_saving(false);
            if failed.is_empty() {
                t.set_open(false);
                show_toast(&ui, if total == 1 { "Tags saved" } else { "Tags saved on all of them" });
            } else {
                t.set_status_bad(true);
                t.set_status(
                    format!("Couldn't save {} of {total} (open in a player?): {}", failed.len(), failed[0]).into(),
                );
            }
        });
    });
}

fn find_online(ui: &AppWindow) {
    let t = ui.global::<TagEdit>();
    if t.get_finding() || t.get_saving() {
        return;
    }
    let Some((file, url)) = with(|s| s.files.first().cloned().map(|f| (f, s.url.clone()))) else { return };
    t.set_finding(true);
    t.set_status("".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let found = tags::find_online(&file, &url);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let t = ui.global::<TagEdit>();
            t.set_finding(false);
            match found {
                Ok(f) => {
                    let g = |v: &Option<String>| v.clone().map(slint::SharedString::from);
                    if let Some(v) = g(&f.tags.title) {
                        t.set_title(v);
                    }
                    if let Some(v) = g(&f.tags.artist) {
                        t.set_artist(v);
                    }
                    if let Some(v) = g(&f.tags.album) {
                        t.set_album(v);
                    }
                    if let Some(v) = g(&f.tags.track) {
                        t.set_track(v);
                    }
                    if let Some(v) = g(&f.tags.year) {
                        t.set_year(v);
                    }
                    if let Some(img) = f.cover.as_ref().and_then(|c| slint::Image::load_from_path(c).ok()) {
                        t.set_cover(img);
                        t.set_has_cover(true);
                    }
                    with(|s| s.cover = f.cover.clone());
                    t.set_status_bad(false);
                    t.set_status(format!("Found on {}: check them, then Save.", f.from).into());
                }
                Err(e) => {
                    t.set_status_bad(true);
                    t.set_status(format!("Couldn't find it online: {e}").into());
                }
            }
        });
    });
}

pub fn install(ui: &AppWindow) {
    let t = ui.global::<TagEdit>();
    t.on_save({
        let weak = ui.as_weak();
        move || save(&weak.unwrap())
    });
    t.on_find_online({
        let weak = ui.as_weak();
        move || find_online(&weak.unwrap())
    });
    t.on_number_in_order({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let t = ui.global::<TagEdit>();
            if t.get_saving() {
                return;
            }
            with(|s| s.numbered = true);
            t.set_track(format!("1–{} in order", t.get_count()).into());
            t.set_track_several(false);
        }
    });
    // `NERIA_TAGS`: files (`;` between) in the dialog, for checks; `NERIA_TAGS_FIND` presses
    // Find tags online.
    if let Ok(files) = std::env::var("NERIA_TAGS") {
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
            let Some(ui) = weak.upgrade() else { return };
            let files: Vec<PathBuf> = files.split(';').filter(|s| !s.trim().is_empty()).map(PathBuf::from).collect();
            open(&ui, files, String::new());
            if std::env::var_os("NERIA_TAGS_FIND").is_some() {
                let weak = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
                    if let Some(ui) = weak.upgrade() {
                        find_online(&ui);
                    }
                    // `NERIA_TAGS_SAVE`: then Save, once it's found.
                    if std::env::var_os("NERIA_TAGS_SAVE").is_some() {
                        let weak = weak.clone();
                        slint::Timer::single_shot(std::time::Duration::from_secs(6), move || {
                            if let Some(ui) = weak.upgrade() {
                                save(&ui);
                            }
                        });
                    }
                });
            }
        });
    }
}
