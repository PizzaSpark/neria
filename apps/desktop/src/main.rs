#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use neria_core::download::DownloadRequest;
use neria_core::queue::{JobSnapshot, JobState, Queue};
use neria_core::settings::{Quality, Settings};
use neria_core::{fmt, history, post, probe, report, tools};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

mod clipper;
mod compressor;
mod drop_ui;
mod extras_ui;
mod match_ui;
mod recent_ui;
mod setup_ui;
mod signin;
mod signin_ui;
mod spotify_ui;
mod updates_ui;

slint::include_modules!();

const CONTAINERS: [&str; 3] = ["mp4", "mkv", "webm"];
const AUDIO_FORMATS: [&str; 5] = ["mp3", "m4a", "opus", "flac", "wav"];
const BROWSERS: [&str; 7] = ["", "chrome", "edge", "firefox", "brave", "opera", "vivaldi"];
const THEMES: [&str; 3] = ["system", "dark", "light"];

fn index_of(list: &[&str], value: &str) -> i32 {
    list.iter().position(|v| *v == value).unwrap_or(0) as i32
}

fn pick(list: &[&str], i: i32) -> String {
    list.get(i.max(0) as usize).copied().unwrap_or(list[0]).to_string()
}

/// "Videos › Neria": the last two parts of the download folder, for the first-run steps.
fn short_dir(dir: &Path) -> String {
    let parts: Vec<String> =
        dir.components().rev().take(2).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join(" › ")
}

fn settings_to_ui(ui: &AppWindow, s: &Settings) {
    ui.set_output_dir(s.output_dir.to_string_lossy().as_ref().into());
    ui.set_output_short(short_dir(&s.output_dir).into());
    ui.set_show_tips(!s.tips_done);
    ui.set_sort_folders(s.sort_folders);
    ui.set_number_items(s.number_items);
    ui.set_save_thumbnail(s.save_thumbnail);
    ui.set_quality(s.quality.index());
    ui.set_container_index(index_of(&CONTAINERS, &s.container));
    ui.set_audio_format_index(index_of(&AUDIO_FORMATS, &s.audio_format));
    ui.set_prefer_h264(s.prefer_h264);
    ui.set_embed_thumbnail(s.embed_thumbnail);
    ui.set_x_gifs_as_gif(s.x_gifs_as_gif);
    ui.set_embed_metadata(s.embed_metadata);
    ui.set_embed_subs(s.embed_subs);
    ui.set_sponsorblock(s.sponsorblock);
    ui.set_allow_playlists(s.allow_playlists);
    ui.set_cookies_index(index_of(&BROWSERS, &s.cookies_browser));
    ui.set_rate_limit(s.rate_limit.as_str().into());
    ui.set_filename_template(s.filename_template.as_str().into());
    ui.set_extra_args(s.extra_args.as_str().into());
    ui.set_max_concurrent(s.max_concurrent as i32);
    ui.set_lyrics(s.lyrics);
    ui.set_verify_audio(s.verify_audio);
    ui.set_spotify_explain(s.spotify_explain);
    ui.set_spotify_client_id(s.spotify_client_id.as_str().into());
    ui.set_spotify_client_secret(s.spotify_client_secret.as_str().into());
    ui.set_check_updates(s.check_updates);
    ui.set_theme_index(index_of(&THEMES, &s.theme));
}

fn ui_to_settings(ui: &AppWindow, s: &mut Settings) {
    s.output_dir = ui.get_output_dir().as_str().into();
    s.sort_folders = ui.get_sort_folders();
    s.number_items = ui.get_number_items();
    s.save_thumbnail = ui.get_save_thumbnail();
    s.quality = Quality::from_index(ui.get_quality());
    s.container = pick(&CONTAINERS, ui.get_container_index());
    s.audio_format = pick(&AUDIO_FORMATS, ui.get_audio_format_index());
    s.prefer_h264 = ui.get_prefer_h264();
    s.embed_thumbnail = ui.get_embed_thumbnail();
    s.x_gifs_as_gif = ui.get_x_gifs_as_gif();
    s.embed_metadata = ui.get_embed_metadata();
    s.embed_subs = ui.get_embed_subs();
    s.sponsorblock = ui.get_sponsorblock();
    s.allow_playlists = ui.get_allow_playlists();
    s.cookies_browser = pick(&BROWSERS, ui.get_cookies_index());
    s.rate_limit = ui.get_rate_limit().to_string();
    s.filename_template = ui.get_filename_template().to_string();
    s.extra_args = ui.get_extra_args().to_string();
    s.max_concurrent = ui.get_max_concurrent().clamp(1, 8) as u32;
    s.lyrics = ui.get_lyrics();
    s.verify_audio = ui.get_verify_audio();
    s.spotify_explain = ui.get_spotify_explain();
    s.spotify_client_id = ui.get_spotify_client_id().trim().to_string();
    s.spotify_client_secret = ui.get_spotify_client_secret().trim().to_string();
    s.check_updates = ui.get_check_updates();
    s.theme = pick(&THEMES, ui.get_theme_index());
    s.tips_done = !ui.get_show_tips();
}

fn job_item(s: &JobSnapshot) -> JobItem {
    let (status, stats) = fmt::job_lines(s);
    let state = match s.state {
        JobState::Queued => 0,
        JobState::Downloading => 1,
        JobState::Processing => 2,
        JobState::Done => 3,
        JobState::Failed => 4,
        JobState::Cancelled => 5,
        JobState::Paused => 6,
    };
    JobItem {
        id: s.id as i32,
        group: s.group as i32,
        title: s.title.as_str().into(),
        state,
        progress: s.progress,
        status: status.into(),
        stats: stats.into(),
        warn: s.note.is_some(),
        flag: if s.note.is_none() {
            0
        } else if s.note_strong {
            2
        } else {
            1
        },
        music: s.music,
        still: s.file.as_ref().and_then(|f| f.extension()).is_some_and(|e| {
            ["jpg", "jpeg", "png", "webp", "gif", "heic"].contains(&e.to_string_lossy().to_lowercase().as_str())
        }),
        thumb: s.thumbnail.as_deref().and_then(cached_thumb).unwrap_or_default(),
        thumb_url: s.thumbnail.clone().unwrap_or_default().into(),
        // Spotify songs are matched and checked in steps: no pausing half-way.
        can_pause: state == 1 && !s.music,
        can_resume: state == 6,
        can_cancel: state <= 2 || state == 6,
        can_retry: state == 4 || state == 5,
        can_remove: (3..=5).contains(&state),
        ..Default::default()
    }
}

thread_local! {
    // Row thumbnails by URL: loaded once, shared by every row (and retry) that uses them.
    static THUMBS: RefCell<HashMap<String, Option<slint::Image>>> = RefCell::default();
    // Every queue row, group cards included. The UI shows `VIEW`: the same rows minus
    // those inside closed cards.
    static ROWS: Rc<VecModel<JobItem>> = Rc::new(VecModel::default());
    // Group cards the user closed (open by default, so every item is in view).
    static COLLAPSED: Rc<RefCell<HashSet<i32>>> = Rc::default();
    static GROUP_TITLES: RefCell<HashMap<i32, String>> = RefCell::default();
    // Queue filter: 0 all, 1 active, 2 done, 3 needs a look.
    static FILTER: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
    static RESET_VIEW: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

fn rows() -> Rc<VecModel<JobItem>> {
    ROWS.with(|r| r.clone())
}

/// Which filter a (non-card) row belongs to: 1 active, 2 done, 3 needs a look.
fn row_bucket(j: &JobItem) -> i32 {
    match j.state {
        0..=2 | 6 => 1,
        4 => 3,
        3 if j.warn => 3,
        _ => 2,
    }
}

/// Does the row show under the current filter? A card shows when any of its rows does.
fn passes_filter(j: &JobItem) -> bool {
    let filter = FILTER.with(|f| f.get());
    if filter == 0 {
        return true;
    }
    if j.header {
        return rows().iter().any(|m| !m.header && m.group == j.group && row_bucket(&m) == filter);
    }
    row_bucket(j) == filter
}

/// Give the queue list its rows: those of the current filter, minus the rows of closed cards.
fn install_job_view(ui: &AppWindow) {
    let collapsed = COLLAPSED.with(|c| c.clone());
    let view = Rc::new(slint::FilterModel::new(rows(), move |j: &JobItem| {
        (!j.child || !collapsed.borrow().contains(&j.group)) && passes_filter(j)
    }));
    ui.set_jobs(ModelRc::from(view.clone()));
    RESET_VIEW.with(|r| *r.borrow_mut() = Some(Box::new(move || view.reset())));
}

fn toggle_group(group: i32) {
    COLLAPSED.with(|e| {
        let mut e = e.borrow_mut();
        if !e.remove(&group) {
            e.insert(group);
        }
    });
    refresh_group(group);
    RESET_VIEW.with(|r| {
        if let Some(reset) = r.borrow().as_ref() {
            reset();
        }
    });
}

fn cached_thumb(url: &str) -> Option<slint::Image> {
    THUMBS.with(|t| t.borrow().get(url).cloned().flatten())
}

/// Fetch a row's thumbnail in the background (once per URL), then show it on
/// every row that uses it.
fn ensure_thumb(ui: &AppWindow, url: &str) {
    let first = THUMBS.with(|t| {
        let mut t = t.borrow_mut();
        if t.contains_key(url) {
            false
        } else {
            t.insert(url.to_string(), None);
            true
        }
    });
    if !first {
        return;
    }
    let (weak, url) = (ui.as_weak(), url.to_string());
    std::thread::spawn(move || {
        let path = probe::fetch_thumbnail(&url).ok();
        let _ = weak.upgrade_in_event_loop(move |_ui| {
            let Some(img) = path.and_then(|p| slint::Image::load_from_path(&p).ok()) else { return };
            THUMBS.with(|t| t.borrow_mut().insert(url.clone(), Some(img.clone())));
            let model = rows();
            let mut groups = HashSet::new();
            for i in 0..model.row_count() {
                if let Some(mut row) = model.row_data(i)
                    && row.thumb_url == url.as_str()
                {
                    row.thumb = img.clone();
                    groups.insert(row.group);
                    model.set_row_data(i, row);
                }
            }
            for g in groups.into_iter().filter(|g| *g != 0) {
                refresh_group(g);
            }
        });
    });
}

/// A group card summing up its rows ("3 of 12 saved · 1 failed").
fn group_card(group: i32, m: &[JobItem]) -> JobItem {
    let n = m.len();
    let count = |st: i32| m.iter().filter(|j| j.state == st).count();
    let (queued, running, done, failed, paused) = (count(0), count(1) + count(2), count(3), count(4), count(6));
    let checks = m.iter().filter(|j| j.state == 3 && j.warn).count();
    let state = if running > 0 {
        1
    } else if queued > 0 {
        0
    } else if paused > 0 {
        6
    } else if failed > 0 {
        4
    } else if done == 0 {
        5
    } else {
        3
    };
    let mut parts = vec![format!("{done} of {n} saved")];
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if paused > 0 {
        parts.push(format!("{paused} paused"));
    }
    if checks > 0 {
        parts.push(format!("{checks} to check"));
    }
    if let Some(now) = m.iter().find(|j| j.state == 1 || j.state == 2) {
        parts.push(format!("now: {}", now.title));
    }
    let progress = m.iter().map(|j| if j.state == 3 { 1.0 } else { j.progress }).sum::<f32>() / n as f32;
    let title = GROUP_TITLES.with(|t| t.borrow().get(&group).cloned()).unwrap_or_else(|| m[0].title.to_string());
    JobItem {
        id: -group,
        group,
        title: title.into(),
        state,
        progress,
        status: parts.join("  ·  ").into(),
        stats: if state <= 2 || state == 6 { format!("{:.0}%", progress * 100.0).into() } else { Default::default() },
        warn: checks > 0,
        music: m.iter().all(|j| j.music),
        still: true,
        thumb: m.iter().find(|j| j.thumb.size().width > 0).map(|j| j.thumb.clone()).unwrap_or_default(),
        header: true,
        expanded: !COLLAPSED.with(|c| c.borrow().contains(&group)),
        can_pause: m.iter().any(|j| j.state == 0 || j.can_pause),
        can_resume: paused > 0,
        can_cancel: m.iter().any(|j| j.can_cancel),
        can_retry: m.iter().any(|j| j.can_retry),
        can_remove: m.iter().all(|j| j.can_remove),
        ..Default::default()
    }
}

/// Add, update or drop a group's card (only groups of two or more get one).
fn refresh_group(group: i32) {
    let vec = rows();
    let members: Vec<(usize, JobItem)> =
        vec.iter().enumerate().filter(|(_, j)| j.group == group && !j.header).collect();
    let card = vec.iter().position(|j| j.header && j.group == group);
    let grouped = members.len() >= 2;
    for (i, mut j) in members.iter().cloned() {
        if j.child != grouped {
            j.child = grouped;
            vec.set_row_data(i, j);
        }
    }
    if !grouped {
        if let Some(h) = card {
            vec.remove(h);
        }
        return;
    }
    let items: Vec<JobItem> = members.iter().map(|(_, j)| j.clone()).collect();
    let head = group_card(group, &items);
    match card {
        Some(h) => vec.set_row_data(h, head),
        None => vec.insert(members[0].0, head),
    }
}

/// Update (or insert) a job row and refresh the counters.
/// Job updates waiting for the UI thread: the newest per job, and whether it may add a row.
/// A big playlist sends dozens of updates a second; the UI applies them in one pass.
#[derive(Default)]
struct PendingJobs {
    snaps: Vec<(JobSnapshot, bool)>,
    scheduled: bool,
}

/// Called by the queue (any thread): keep the update, and wake the UI once.
fn queue_update(weak: &slint::Weak<AppWindow>, pending: &Arc<Mutex<PendingJobs>>, snap: JobSnapshot) {
    let mut p = pending.lock().unwrap();
    // New rows only come from new or restored jobs (updates for removed rows are dropped).
    let insertable = snap.state == JobState::Queued || snap.restored;
    match p.snaps.iter_mut().find(|(s, _)| s.id == snap.id) {
        Some(slot) => {
            slot.1 |= insertable;
            slot.0 = snap;
        }
        None => p.snaps.push((snap, insertable)),
    }
    if !p.scheduled {
        p.scheduled = true;
        let pending = pending.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let snaps = {
                let mut p = pending.lock().unwrap();
                p.scheduled = false;
                std::mem::take(&mut p.snaps)
            };
            let mut groups = HashSet::new();
            for (snap, insertable) in &snaps {
                upsert_job(&ui, snap, *insertable);
                if snap.group != 0 {
                    groups.insert(snap.group as i32);
                }
            }
            for g in groups {
                refresh_group(g);
            }
            refresh_counts(&ui);
        });
    }
}

/// Update (or insert) a job row. The caller refreshes group cards and counters.
fn upsert_job(ui: &AppWindow, snap: &JobSnapshot, insertable: bool) {
    let vec = rows();
    let mut item = job_item(snap);
    if let Some(url) = &snap.thumbnail {
        ensure_thumb(ui, url);
    }
    if let Some(t) = &snap.group_title {
        GROUP_TITLES.with(|g| g.borrow_mut().insert(item.group, t.clone()));
    }
    // Dev hook for scripted checks: open "pick another match" on the first flagged song.
    static PICKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if item.warn
        && item.state == 3
        && std::env::var_os("NERIA_MATCH_PICK").is_some()
        && !PICKED.swap(true, Ordering::SeqCst)
    {
        let (weak, id) = (ui.as_weak(), item.id);
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_match_pick(id);
            }
        });
    }
    // Dev hook: copy the report of the first failed download.
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if item.state == 4 && std::env::var_os("NERIA_REPORT").is_some() && !REPORTED.swap(true, Ordering::SeqCst) {
        let (weak, id) = (ui.as_weak(), item.id);
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_report_job(id);
            }
        });
    }
    match vec.iter().position(|j| j.id == item.id && !j.header) {
        Some(i) => {
            item.child = vec.row_data(i).is_some_and(|r| r.child);
            vec.set_row_data(i, item)
        }
        // New jobs go on top, except playlist/album items, which follow the previous
        // item of their group so they read in order. Rows from the last session are
        // added whatever their state. Updates for rows the user removed are ignored.
        None if insertable => {
            let after = if item.group != 0 {
                vec.iter().enumerate().filter(|(_, j)| j.group == item.group).map(|(i, _)| i).last()
            } else {
                None
            };
            vec.insert(after.map_or(0, |i| i + 1), item);
        }
        None => {}
    }
}

fn refresh_counts(ui: &AppWindow) {
    let model = rows();
    let count = |f: &dyn Fn(i32) -> bool| model.iter().filter(|j| !j.header && f(j.state)).count() as i32;
    ui.set_active_count(count(&|s| s <= 2));
    ui.set_running_count(count(&|s| s == 1 || s == 2));
    ui.set_paused_count(count(&|s| s == 6));
    // "Clear finished" leaves playlists that are still going: count what it would remove.
    let open: HashSet<i32> =
        model.iter().filter(|j| !j.header && j.group != 0 && !(3..=5).contains(&j.state)).map(|j| j.group).collect();
    let finished = model.iter().filter(|j| !j.header && (3..=5).contains(&j.state) && !open.contains(&j.group)).count();
    ui.set_finished_count(finished as i32);
    let in_bucket = |b: i32| model.iter().filter(|j| !j.header && row_bucket(j) == b).count() as i32;
    ui.set_filter_counts(ModelRc::new(VecModel::from(vec![
        model.iter().filter(|j| !j.header).count() as i32,
        in_bucket(1),
        in_bucket(2),
        in_bucket(3),
    ])));
}

fn remove_rows(ui: &AppWindow, ids: &[u64]) {
    let vec = rows();
    let mut groups = HashSet::new();
    let mut i = 0;
    while i < vec.row_count() {
        let row = vec.row_data(i).unwrap();
        if !row.header && ids.contains(&(row.id as u64)) {
            groups.insert(row.group);
            vec.remove(i);
        } else {
            i += 1;
        }
    }
    for g in groups.into_iter().filter(|g| *g != 0) {
        refresh_group(g);
    }
    refresh_counts(ui);
}

/// Windows' "app mode" (Settings → Personalisation → Colours). Dark if unknown.
fn windows_prefers_dark() -> bool {
    #[cfg(windows)]
    {
        let out = neria_core::proc::command("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();
        if let Ok(out) = out {
            // "    AppsUseLightTheme    REG_DWORD    0x0"
            return !String::from_utf8_lossy(&out.stdout).contains("0x1");
        }
    }
    true
}

fn apply_theme(ui: &AppWindow) {
    let dark = match ui.get_theme_index() {
        1 => true,
        2 => false,
        _ => windows_prefers_dark(),
    };
    ui.global::<Theme>().set_dark(dark);
}

/// Dev hook (`NERIA_DEMO`): sample queue rows in every state, for design checks.
fn demo_rows(ui: &AppWindow) {
    let vec = rows();
    let row =
        |id: i32, title: &str, state: i32, progress: f32, status: &str, stats: &str, warn: bool, music: bool| JobItem {
            id,
            title: title.into(),
            state,
            progress,
            status: status.into(),
            stats: stats.into(),
            warn,
            music,
            can_pause: state == 1 && !music,
            can_resume: state == 6,
            can_cancel: state <= 2 || state == 6,
            can_retry: state == 4 || state == 5,
            can_remove: (3..=5).contains(&state),
            ..Default::default()
        };
    vec.push(row(
        9001,
        "Lo-fi beats to study to, 3 hour mix",
        1,
        0.62,
        "Downloading 1/2",
        "241.0 MB / 388.0 MB  ·  8.4 MB/s  ·  0:17 left",
        false,
        false,
    ));
    vec.push(row(9010, "Mountain timelapse 8K (paused)", 6, 0.31, "120.0 MB of 388.0 MB so far", "", false, false));
    // A playlist card with three songs (open it to see them).
    GROUP_TITLES.with(|g| g.borrow_mut().insert(77, "A Very 1999 Christmas (Full Album)".into()));
    for (id, title, state, progress, status) in [
        (9021, "01 Santa Claus Is Coming To Town", 3, 1.0, "01 Santa Claus Is Coming To Town.mp3"),
        (9022, "02 The Christmas Song", 1, 0.4, "Downloading"),
        (9023, "03 Let It Snow! Let It Snow! Let It Snow!", 0, 0.0, "Waiting in queue…"),
    ] {
        vec.push(JobItem { group: 77, ..row(id, title, state, progress, status, "", false, false) });
    }
    refresh_group(77);
    vec.push(row(
        9002,
        "Daft Punk - Get Lucky (feat. Pharrell Williams and Nile Rodgers)",
        2,
        1.0,
        "Checking the sound against the official preview…",
        "",
        false,
        true,
    ));
    vec.push(row(
        9003,
        "YOASOBI「アイドル」Official Music Video · Кино — Группа крови · 아이유 · 🎵",
        0,
        0.0,
        "Waiting in queue…",
        "",
        false,
        false,
    ));
    vec.push(row(
        9004,
        "Mountain timelapse 8K",
        3,
        1.0,
        "Mountain timelapse 8K [a1b2c3].mp4",
        "412.0 MB",
        false,
        false,
    ));
    vec.push(row(
        9005,
        "Bonnie Tyler - Total Eclipse of the Heart",
        3,
        1.0,
        "Only 58% like the Spotify preview, probably another recording",
        "",
        true,
        true,
    ));
    if let Some(mut r) = vec.row_data(vec.row_count() - 1) {
        r.flag = 2;
        vec.set_row_data(vec.row_count() - 1, r);
    }
    vec.push(JobItem {
        flag: 1,
        ..row(
            9007,
            "a-ha - Take On Me",
            3,
            1.0,
            "Close to the Spotify preview, maybe another master or edit",
            "",
            true,
            true,
        )
    });
    vec.push(row(9006, "https://vimeo.com/1", 4, 0.0, "This video was removed or doesn't exist", "", false, false));
    refresh_counts(ui);
}

/// Set by "Got it, download" so the next Download skips the Spotify heads-up once.
static HEADS_UP_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Dev hook (`NERIA_DIALOG`): open a dialog with sample data, for design checks.
/// `match`, `sync`, `spotify`, `toast`, `update`, `update-ready`, `update-failed`.
fn demo_dialog(ui: &AppWindow, which: &str) {
    match which {
        "match" => {
            let item = |title: &str,
                        detail: &str,
                        score: i32,
                        ruled_out: bool,
                        current: bool,
                        check_state: i32,
                        check_text: &str| MatchItem {
                title: title.into(),
                detail: detail.into(),
                score,
                ruled_out,
                current,
                url: "https://music.youtube.com/watch?v=dQw4w9WgXcQ".into(),
                check_state,
                check_text: check_text.into(),
            };
            let items = vec![
                item("Total Eclipse of the Heart (Live)", "Bonnie Tyler · 6:12", 64, false, true, 4, "Different (41%)"),
                item(
                    "Total Eclipse of the Heart",
                    "Bonnie Tyler · Faster Than the Speed of Night · 6:59",
                    96,
                    false,
                    false,
                    2,
                    "Same recording",
                ),
                item(
                    "Total Eclipse of the Heart (Single Edit)",
                    "Bonnie Tyler · 4:30",
                    88,
                    false,
                    false,
                    3,
                    "Unsure (72%)",
                ),
                item(
                    "Total Eclipse of the Heart (Turn Around)",
                    "Bonnie Tyler · 7:00",
                    81,
                    false,
                    false,
                    1,
                    "Checking…",
                ),
                item("Total Eclipse of the Heart — Karaoke Version", "Sing King · 6:58", 52, false, false, 0, ""),
                item("Total Eclipse of the Heart (2015 Remaster)", "Bonnie Tyler · 3:02", 0, true, false, 0, ""),
                item(
                    "Total Eclipse of the Heart (Acoustic)",
                    "Bonnie Tyler · 5:40",
                    70,
                    false,
                    false,
                    5,
                    "Couldn't check",
                ),
            ];
            ui.set_match_heading("Bonnie Tyler – Total Eclipse of the Heart · 6:59".into());
            ui.set_match_sub(
                "Best matches on YouTube Music. The one in use doesn't sound like Spotify's preview.".into(),
            );
            ui.set_match_query("Bonnie Tyler Total Eclipse of the Heart".into());
            ui.set_match_can_check(true);
            ui.set_match_items(ModelRc::from(Rc::new(VecModel::from(items))));
            ui.set_match_open(true);
        }
        "sync" => {
            let items: Vec<SyncItem> = [
                "Daft Punk - One More Time",
                "Justice - D.A.N.C.E.",
                "Kavinsky - Nightcall (feat. Lovefoxxx) [Drive Original Motion Picture Soundtrack]",
                "Air - La femme d'argent",
            ]
            .iter()
            .enumerate()
            .map(|(i, t)| SyncItem { title: (*t).into(), file: format!("C:\\Music\\{t}.mp3").into(), checked: i == 1 })
            .collect();
            ui.set_sync_items(ModelRc::from(Rc::new(VecModel::from(items))));
            ui.set_sync_ticked(1);
            ui.set_sync_open(true);
        }
        "spotify" => ui.set_spotify_heads_up(true),
        // A post with several items, for checking the item picker's layout.
        "pick" => {
            let item = |kind: &str, title: &str, detail: &str, checked: bool, can_quality: bool| PickItem {
                kind: kind.into(),
                title: title.into(),
                detail: detail.into(),
                thumb: Default::default(),
                checked,
                quality: 0,
                can_quality,
            };
            let rows = vec![
                item("Video", "Sunset over the harbour, long take", "2:41", true, true),
                item("Photo", "", "", true, false),
                item("Video", "Behind the scenes", "0:38", true, true),
                item("GIF", "", "0:05", false, false),
                item("Photo", "", "", true, false),
            ];
            let checked = rows.iter().filter(|r| r.checked).count() as i32;
            ui.set_probe_state(2);
            ui.set_probe_title("A post with photos and videos".into());
            ui.set_probe_meta("@someone · 5 items".into());
            ui.set_pick_items(ModelRc::from(Rc::new(VecModel::from(rows))));
            ui.set_pick_checked(checked);
            ui.set_pick_note("One video in this post couldn't be reached.".into());
        }
        // A video with captions, dubbed audio and chapters; `captions` / `audio` / `chapters`
        // also open that picker.
        "extras" | "captions" | "audio" | "chapters" => {
            use neria_core::tracks::{AudioTrack, Caption, Chapter};
            let cap = |lang: &str, name: &str, auto: bool| Caption { lang: lang.into(), name: name.into(), auto };
            let mut captions =
                vec![cap("en", "English", false), cap("es", "Spanish", false), cap("de", "German", false)];
            for (l, n) in [("en-orig", "English (Original)"), ("ar", "Arabic"), ("fr", "French"), ("hi", "Hindi")] {
                captions.push(cap(l, n, true));
            }
            for (l, n) in [("it", "Italian"), ("ja", "Japanese"), ("ko", "Korean"), ("pt", "Portuguese")] {
                captions.push(cap(l, n, true));
            }
            let track =
                |lang: &str, name: &str, original: bool| AudioTrack { lang: lang.into(), name: name.into(), original };
            let titles = ["Intro", "The twilight zone", "Midnight zone", "The abyss", "Hadal zone", "Credits"];
            let info = probe::MediaInfo {
                title: "How the Deep Sea Works, Explained".into(),
                uploader: Some("Kurzgesagt".into()),
                duration: Some(1122.0),
                max_height: Some(2160),
                captions,
                audio_tracks: vec![
                    track("en", "English", true),
                    track("es", "Spanish", false),
                    track("fr", "French", false),
                ],
                chapters: titles
                    .iter()
                    .enumerate()
                    .map(|(i, t)| Chapter { start: i as f64 * 187.0, end: (i + 1) as f64 * 187.0, title: (*t).into() })
                    .collect(),
                ..Default::default()
            };
            ui.set_url("https://www.youtube.com/watch?v=deepsea".into());
            ui.set_probe_state(2);
            ui.set_probe_title(info.title.as_str().into());
            ui.set_probe_meta(info.meta_line().into());
            let settings = Settings { caption_langs: vec!["en".into()], ..Settings::load() };
            extras_ui::set_info(ui, &info, &settings);
            let kind = match which {
                "captions" => 1,
                "audio" => 2,
                "chapters" => 3,
                _ => 0,
            };
            if kind > 0 {
                ui.global::<Extras>().invoke_show(kind);
            }
        }
        // A link that needs an account: "Sign in to YouTube" on the problem.
        "signin" => {
            let url = "https://www.youtube.com/watch?v=private123";
            ui.set_url(url.into());
            let p = probe::explain(
                "[youtube] x: Sign in to confirm your age. This video may be inappropriate for some users.",
            );
            show_problem(ui, &p.title, &p.hint, p.permanent);
            signin_ui::show_for_problem(ui, url, Some(&p));
        }
        "toast" => {
            ui.set_toast("Report copied. Paste it in a message to whoever helps you with Neria (Ctrl+V).".into())
        }
        "update" | "update-ready" | "update-failed" => {
            ui.set_update_version("0.2.0".into());
            ui.set_update_banner(true);
            match which {
                "update-ready" => {
                    ui.set_update_state(3);
                    ui.set_update_status("Neria 0.2.0 is ready. Restart to start using it.".into());
                    ui.set_active_count(2);
                }
                "update-failed" => {
                    ui.set_update_state(4);
                    ui.set_update_status("Couldn't download the update. Check your connection and try again.".into());
                }
                _ => {}
            }
        }
        _ => {}
    }
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn start_setup(ui: &AppWindow, settings: Arc<Mutex<Settings>>, queue: Queue) {
    if !setup_ui::begin(ui) {
        return;
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let w = weak.clone();
        let result = tools::ensure_all(|p| {
            let _ = w.upgrade_in_event_loop(move |ui| setup_ui::progress(&ui, &p));
        });

        if let Err(e) = result {
            let _ = weak.upgrade_in_event_loop(move |ui| setup_ui::failed(&ui, &e));
            return;
        }
        let _ = weak.upgrade_in_event_loop(|ui| setup_ui::finished(&ui));

        // Sites change constantly; keep yt-dlp fresh (once a day, before any download runs).
        let due = now_secs().saturating_sub(settings.lock().unwrap().last_update_check) > 24 * 3600;
        if due {
            let _ = weak.upgrade_in_event_loop(|ui| ui.set_setup_label("Checking yt-dlp for updates…".into()));
            let _ = tools::update_ytdlp();
            let mut s = settings.lock().unwrap();
            s.last_update_check = now_secs();
            let _ = s.save();
        }

        // Tools are in place and fresh: downloads left from last time can carry on.
        queue.set_ready(true);
        let check_updates = settings.lock().unwrap().check_updates;
        let version = tools::ytdlp_version().unwrap_or_default();
        let versions: Vec<String> = report::tool_versions().iter().map(|(n, v)| format!("{n} {v}")).collect();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_ytdlp_version(version.into());
            ui.set_tool_versions(versions.join("\n").into());
            ui.set_tools_ready(true);
            if check_updates {
                updates_ui::check(&ui, false);
            }
            // Dev/automation hook: open a file straight into the clipper.
            if let Some(path) = std::env::var_os("NERIA_OPEN") {
                ui.invoke_clip_open_path(path.to_string_lossy().as_ref().into());
            }
            if let Some(path) = std::env::var_os("NERIA_COMPRESS") {
                ui.invoke_cmp_open_path(path.to_string_lossy().as_ref().into());
            }
            if let Ok(page) = std::env::var("NERIA_PAGE") {
                let names = ["download", "clip", "compress"];
                if let Some(i) = names.iter().position(|n| page.eq_ignore_ascii_case(n)) {
                    ui.set_page(i as i32);
                }
            }
            drop_ui::dev_hooks(&ui);
            if let Ok(which) = std::env::var("NERIA_DIALOG") {
                demo_dialog(&ui, &which);
            }
            if let Ok(filter) = std::env::var("NERIA_FILTER") {
                let names = ["all", "active", "done", "attention"];
                if let Some(i) = names.iter().position(|n| filter.eq_ignore_ascii_case(n)) {
                    ui.set_queue_filter(i as i32);
                    ui.invoke_queue_filter_changed(i as i32);
                }
            }
            if let Ok(section) = std::env::var("NERIA_SETTINGS") {
                let names = ["download", "spotify", "network", "appearance", "about"];
                if let Some(i) = names.iter().position(|n| section.eq_ignore_ascii_case(n)) {
                    ui.set_settings_section(i as i32);
                }
                ui.set_drawer_open(true);
            }
            if let Some(url) = std::env::var_os("NERIA_URL") {
                let url = url.to_string_lossy().into_owned();
                ui.set_url(url.as_str().into());
                ui.invoke_url_edited(url.into());
            }
        });
    });
}

struct ProbeState {
    generation: Arc<AtomicU64>,
    last_url: RefCell<String>,
    spotify: spotify_ui::PendingSlot,
    /// Everything in the previewed post/playlist (the picker shows these).
    items: RefCell<Vec<post::Item>>,
    queue: Queue,
    /// `Extractor:id` of the previewed video.
    key: RefCell<Option<String>>,
    /// The earlier download of it, for "Show file".
    dup_file: RefCell<Option<PathBuf>>,
}

/// Forget the duplicate check and playlist offer of the previous link.
fn clear_link_notes(ui: &AppWindow, state: &ProbeState) {
    *state.key.borrow_mut() = None;
    *state.dup_file.borrow_mut() = None;
    ui.set_dup_text("".into());
    ui.set_dup_file(false);
    ui.set_playlist_url("".into());
}

/// "Already downloaded 3 days ago" from the history, or "Already in the queue".
fn show_duplicate(ui: &AppWindow, state: &ProbeState, url: &str, earlier: Option<history::Record>) {
    let key = state.key.borrow().clone();
    if let Some(job) = state.queue.find_unfinished(key.as_deref(), url) {
        let what = if job.state == JobState::Paused { "It's paused in the queue" } else { "It's already in the queue" };
        ui.set_dup_text(what.into());
        return;
    }
    let Some(r) = earlier else { return };
    let days = now_secs().saturating_sub(r.at) / 86_400;
    let when = match days {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        d => format!("{d} days ago"),
    };
    // Say what it was saved as: an earlier mp3 doesn't mean you have the video.
    let kind = r.file.extension().map(|e| format!(" as {}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
    ui.set_dup_text(format!("You downloaded this {when}{kind}").into());
    ui.set_dup_file(true);
    *state.dup_file.borrow_mut() = Some(r.file);
}

/// Empty the "what to download" list.
fn clear_picker(ui: &AppWindow, state: &ProbeState) {
    state.items.borrow_mut().clear();
    ui.set_pick_items(ModelRc::from(Rc::new(VecModel::<PickItem>::default())));
    ui.set_pick_checked(0);
    ui.set_pick_note("".into());
}

/// Fill the picker (everything ticked) and fetch the item pictures in the background.
fn show_picker(ui: &AppWindow, state: &ProbeState, info: &probe::MediaInfo, my_gen: u64) {
    *state.items.borrow_mut() = info.items.clone();
    let rows: Vec<PickItem> = info
        .items
        .iter()
        .map(|it| PickItem {
            kind: it.kind.label().into(),
            title: it.title.as_str().into(),
            detail: it.duration.map(fmt::duration).unwrap_or_default().into(),
            thumb: Default::default(),
            checked: true,
            quality: 0,
            // Videos only gallery-dl saw are saved from their file address, as they are.
            can_quality: it.kind == post::ItemKind::Video && it.direct_url.is_none(),
        })
        .collect();
    ui.set_pick_checked(rows.len() as i32);
    ui.set_pick_items(ModelRc::from(Rc::new(VecModel::from(rows))));
    ui.set_pick_note(info.note.clone().unwrap_or_default().into());

    let urls: Vec<(usize, String)> =
        info.items.iter().enumerate().filter_map(|(i, it)| Some((i, it.thumbnail.clone()?))).collect();
    let (weak, generation) = (ui.as_weak(), state.generation.clone());
    std::thread::spawn(move || {
        for (i, url) in urls {
            if generation.load(Ordering::SeqCst) != my_gen {
                return;
            }
            let Ok(path) = probe::fetch_thumbnail(&url) else { continue };
            let generation = generation.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if generation.load(Ordering::SeqCst) != my_gen {
                    return;
                }
                let model = ui.get_pick_items();
                if let (Some(mut row), Ok(img)) = (model.row_data(i), slint::Image::load_from_path(&path)) {
                    row.thumb = img;
                    model.set_row_data(i, row);
                }
            });
        }
    });
}

fn count_picked(ui: &AppWindow) {
    let n = ui.get_pick_items().iter().filter(|r| r.checked).count();
    ui.set_pick_checked(n as i32);
}

/// The link to use for `text`: Spotify URIs as they are, web links with
/// `https://` added if it was left out. `None` if it isn't a link.
fn link_of(text: &str) -> Option<String> {
    let text = text.trim();
    if text.starts_with("spotify:") {
        return Some(text.to_string());
    }
    probe::normalize_url(text)
}

/// Text someone is probably still typing as a link ("h", "http", "https:/").
fn partial_link(text: &str) -> bool {
    let t = text.trim().to_ascii_lowercase();
    t.is_empty() || "https://".starts_with(&t) || "http://".starts_with(&t) || "spotify:".starts_with(&t)
}

fn show_problem(ui: &AppWindow, title: &str, hint: &str, permanent: bool) {
    ui.set_probe_state(3);
    ui.set_probe_blocked(permanent);
    ui.set_probe_title(title.into());
    ui.set_probe_meta(hint.into());
}

fn trigger_probe(ui: &AppWindow, state: &ProbeState, settings: &Arc<Mutex<Settings>>, text: &str) {
    let Some(url) = link_of(text) else {
        state.generation.fetch_add(1, Ordering::SeqCst);
        clear_picker(ui, state);
        extras_ui::clear(ui);
        clear_link_notes(ui, state);
        state.last_url.borrow_mut().clear();
        ui.set_probe_blocked(false);
        ui.set_probe_state(if partial_link(text) { 0 } else { 4 });
        ui.set_probe_is_spotify(false);
        ui.set_probe_is_gif(false);
        return;
    };
    // Same link again: keep the preview, but look it up again after an error
    // (pasting the link once more is how you retry).
    if !ui.get_tools_ready() || (*state.last_url.borrow() == url && ui.get_probe_state() != 3) {
        return;
    }
    *state.last_url.borrow_mut() = url.clone();
    signin_ui::show_for_problem(ui, &url, None);
    extras_ui::clear(ui);

    let my_gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    clear_picker(ui, state);
    clear_link_notes(ui, state);
    ui.set_probe_state(1);
    ui.set_probe_blocked(false);
    ui.set_probe_title("".into());
    ui.set_probe_meta(url.as_str().into());
    ui.set_probe_thumb(slint::Image::default());
    ui.set_spotify_note("".into());

    let generation = state.generation.clone();
    let settings = settings.lock().unwrap().clone();
    if let Some(link) = neria_core::spotify::parse_link(&url) {
        spotify_ui::preview(ui, url, link, settings, generation, my_gen, state.spotify.clone());
        return;
    }
    ui.set_probe_is_spotify(false);
    ui.set_probe_is_gif(false);
    // Spotify links we can't handle (podcasts, short links…): say so without asking yt-dlp,
    // which would only report "DRM" or "unsupported".
    if let Some((title, hint)) = neria_core::spotify::unsupported_reason(&url) {
        show_problem(ui, title, hint, true);
        return;
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        // Debounce typing: only the latest edit actually runs yt-dlp.
        std::thread::sleep(Duration::from_millis(350));
        if generation.load(Ordering::SeqCst) != my_gen {
            return;
        }
        let result = probe::probe(&url, &settings);
        let thumb =
            result.as_ref().ok().and_then(|i| i.thumbnail_url.as_deref()).and_then(|u| probe::fetch_thumbnail(u).ok());
        // Downloaded before? (Single videos; playlist items already in their folder are skipped.)
        let earlier =
            result.as_ref().ok().filter(|i| i.items.is_empty()).and_then(|i| history::find(i.key.as_deref(), &url));

        let _ = weak.upgrade_in_event_loop(move |ui| {
            if generation.load(Ordering::SeqCst) != my_gen {
                return;
            }
            match result {
                Ok(info) => {
                    ui.set_probe_state(2);
                    ui.set_probe_title(info.title.as_str().into());
                    ui.set_probe_meta(info.meta_line().into());
                    ui.set_probe_is_gif(info.gif || is_single_gif(&info));
                    extras_ui::set_info(&ui, &info, &settings);
                    if info.items.len() > 1
                        && let Some(state) = PICKER.with(|p| p.borrow().as_ref().and_then(|w| w.upgrade()))
                    {
                        show_picker(&ui, &state, &info, my_gen);
                    }
                    if let Some(img) = thumb.and_then(|p| slint::Image::load_from_path(&p).ok()) {
                        ui.set_probe_thumb(img);
                    }
                    if info.items.is_empty()
                        && let Some(state) = PICKER.with(|p| p.borrow().as_ref().and_then(|w| w.upgrade()))
                    {
                        *state.key.borrow_mut() = info.key.clone();
                        show_duplicate(&ui, &state, &url, earlier);
                        ui.set_playlist_url(probe::playlist_link(&url).unwrap_or_default().into());
                    }
                }
                Err(e) => {
                    let p = e.downcast_ref::<probe::LinkProblem>().cloned().unwrap_or_else(|| {
                        probe::LinkProblem::new(format!("{e:#}"), "You can still try downloading it.", false)
                    });
                    show_problem(&ui, &p.title, &p.hint, p.permanent);
                    signin_ui::show_for_problem(&ui, &url, Some(&p));
                }
            }
            // Dev hook: press Download once the preview is in (if it's allowed).
            if std::env::var_os("NERIA_URL_START").is_some() && !ui.get_probe_blocked() {
                ui.invoke_start_download();
            }
        });
    });
}

thread_local! {
    // The probe state, for filling the picker from a finished lookup (UI thread only).
    static PICKER: RefCell<Option<std::rc::Weak<ProbeState>>> = const { RefCell::new(None) };
}

/// A post that is just one GIF (and photos gallery-dl saw are absent).
fn is_single_gif(info: &probe::MediaInfo) -> bool {
    info.items.len() == 1 && info.items[0].kind == post::ItemKind::Gif
}

/// Show a short message at the bottom of the window for a few seconds.
pub(crate) fn show_toast(ui: &AppWindow, msg: &str) {
    static SHOWN: AtomicU64 = AtomicU64::new(0);
    let me = SHOWN.fetch_add(1, Ordering::SeqCst) + 1;
    ui.set_toast(msg.into());
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_secs(4), move || {
        // A newer toast keeps its own full time.
        if SHOWN.load(Ordering::SeqCst) == me
            && let Some(ui) = weak.upgrade()
        {
            ui.set_toast("".into());
        }
    });
}

/// Put a problem report on the clipboard and say so.
fn copy_report(ui: &AppWindow, text: &str) {
    match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())) {
        Ok(()) => show_toast(ui, "Report copied. Paste it in a message to whoever helps you with Neria (Ctrl+V)."),
        Err(_) => show_toast(ui, "Couldn't copy the report to the clipboard. Try again."),
    }
}

pub(crate) fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = open::that(path.parent().unwrap_or(path));
    }
}

fn main() -> anyhow::Result<()> {
    // `neria --sign-in <site> [dark|light]`: just the sign-in window (see signin.rs).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--sign-in") {
        let saved =
            signin::run(args.get(2).map_or("", String::as_str), args.get(3).map(String::as_str) != Some("light"));
        std::process::exit(if saved { 0 } else { 2 });
    }
    // Put bin/ first on our own PATH so tools that spawn tools (mpv's yt-dlp
    // hook -> yt-dlp -> deno/ffmpeg) find the bundled copies.
    {
        let mut dirs = vec![neria_core::paths::bin_dir()];
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        if let Ok(joined) = std::env::join_paths(dirs) {
            // SAFETY: first thing in main, before any other thread exists.
            unsafe { std::env::set_var("PATH", joined) };
        }
    }

    // The video preview renders through OpenGL, so insist on a GL renderer.
    slint::BackendSelector::new().require_opengl().select()?;

    // Leftovers from an update (the replaced exe, old tool files).
    neria_core::update::cleanup();

    let settings = Arc::new(Mutex::new(Settings::load()));
    let ui = AppWindow::new()?;
    settings_to_ui(&ui, &settings.lock().unwrap());
    ui.set_app_version(report::VERSION.into());
    apply_theme(&ui);
    install_job_view(&ui);
    if std::env::var_os("NERIA_DEMO").is_some() {
        demo_rows(&ui);
    }
    // Dev hook for design checks: window size in logical pixels, e.g. "780x580".
    if let Some((w, h)) = std::env::var("NERIA_WINDOW").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?))
    }) {
        ui.window().set_size(slint::LogicalSize::new(w, h));
    }

    let weak = ui.as_weak();
    let pending = Arc::new(Mutex::new(PendingJobs::default()));
    let queue = Queue::new(settings.clone(), move |snap| queue_update(&weak, &pending, snap));
    // Bring back last session's queue; it carries on once the tools are checked.
    queue.set_ready(false);
    queue.enable_persistence();

    let save = {
        let weak = ui.as_weak();
        let settings = settings.clone();
        move || {
            let ui = weak.unwrap();
            let mut s = settings.lock().unwrap();
            ui_to_settings(&ui, &mut s);
            let _ = s.save();
        }
    };

    ui.on_theme_changed({
        let weak = ui.as_weak();
        move || apply_theme(&weak.unwrap())
    });

    ui.on_settings_changed({
        let save = save.clone();
        move || save()
    });

    let probe_state = Rc::new(ProbeState {
        generation: Arc::new(AtomicU64::new(0)),
        last_url: RefCell::new(String::new()),
        spotify: Arc::new(Mutex::new(None)),
        items: RefCell::new(Vec::new()),
        queue: queue.clone(),
        key: RefCell::new(None),
        dup_file: RefCell::new(None),
    });
    PICKER.with(|p| *p.borrow_mut() = Some(Rc::downgrade(&probe_state)));

    ui.on_pick_toggle({
        let weak = ui.as_weak();
        move |i, on| {
            let ui = weak.unwrap();
            let model = ui.get_pick_items();
            if let Some(mut row) = model.row_data(i as usize) {
                row.checked = on;
                model.set_row_data(i as usize, row);
            }
            count_picked(&ui);
        }
    });
    ui.on_pick_quality({
        let weak = ui.as_weak();
        move |i, q| {
            let model = weak.unwrap().get_pick_items();
            if let Some(mut row) = model.row_data(i as usize) {
                row.quality = q;
                model.set_row_data(i as usize, row);
            }
        }
    });
    ui.on_pick_all({
        let weak = ui.as_weak();
        move |on| {
            let ui = weak.unwrap();
            let model = ui.get_pick_items();
            for i in 0..model.row_count() {
                if let Some(mut row) = model.row_data(i) {
                    row.checked = on;
                    model.set_row_data(i, row);
                }
            }
            count_picked(&ui);
        }
    });
    spotify_ui::install_sync_review(&ui);
    match_ui::install(&ui, queue.clone());
    updates_ui::install(&ui);

    ui.on_url_edited({
        let weak = ui.as_weak();
        let (probe_state, settings) = (probe_state.clone(), settings.clone());
        move |text| trigger_probe(&weak.unwrap(), &probe_state, &settings, &text)
    });

    ui.on_paste({
        let weak = ui.as_weak();
        let (probe_state, settings) = (probe_state.clone(), settings.clone());
        move || {
            let ui = weak.unwrap();
            let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            ui.set_url(text.into());
            trigger_probe(&ui, &probe_state, &settings, text);
        }
    });

    ui.on_start_download({
        let weak = ui.as_weak();
        let (queue, probe_state, save, settings) = (queue.clone(), probe_state.clone(), save.clone(), settings.clone());
        move || {
            let ui = weak.unwrap();
            // Enter in the link box gets here too, so re-check what the button checks.
            let Some(url) = link_of(&ui.get_url()) else { return };
            if !ui.get_tools_ready() || ui.get_probe_blocked() {
                return;
            }
            // The first download: the "How it works" steps have done their job.
            ui.set_show_tips(false);
            save();
            if neria_core::spotify::parse_link(&url).is_some() {
                // First time: explain where the audio comes from (once the lookup is done).
                let looked_up = probe_state.spotify.lock().unwrap().as_ref().is_some_and(|p| p.url == url);
                if looked_up && ui.get_spotify_explain() && !HEADS_UP_OK.swap(false, Ordering::SeqCst) {
                    ui.set_spotify_heads_up_skip(true);
                    ui.set_spotify_heads_up(true);
                    return;
                }
                // Spotify: download every song once the lookup has finished.
                let settings = settings.lock().unwrap().clone();
                if !spotify_ui::enqueue(&ui, &url, &probe_state.spotify, &settings, &queue) {
                    return; // still looking it up (or the lookup failed)
                }
            } else if !probe_state.items.borrow().is_empty() {
                // A post or playlist: one row per picked item, in one group.
                let picked: Vec<(usize, Option<Quality>)> = ui
                    .get_pick_items()
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.checked)
                    .map(|(i, r)| (i, (r.quality > 0).then(|| Quality::from_index(r.quality - 1))))
                    .collect();
                let items = probe_state.items.borrow().clone();
                let settings = settings.lock().unwrap().clone();
                let title = ui.get_probe_title().to_string();
                let reqs = post::requests(&title, &items, &picked, Quality::from_index(ui.get_quality()), &settings);
                if reqs.is_empty() {
                    return;
                }
                // Playlists keep a playlist file in their folder; posts don't need one.
                queue.add_group(&title, post::is_playlist(&items), reqs);
            } else {
                let title_hint = if ui.get_probe_state() == 2 { ui.get_probe_title().to_string() } else { url.clone() };
                let quality = if ui.get_probe_is_gif() { Quality::Best } else { Quality::from_index(ui.get_quality()) };
                let mut req = DownloadRequest::new(url, quality, title_hint);
                req.key = probe_state.key.borrow().clone();
                // "Download again": replace a file of the same name.
                req.force = ui.get_dup_text() != "";
                // Captions, audio tracks and chapters picked on the preview.
                let reqs = extras_ui::requests(&ui, req, &settings.lock().unwrap().clone());
                extras_ui::remember(&ui, &settings);
                if reqs.len() == 1 {
                    queue.add(reqs.into_iter().next().unwrap());
                } else {
                    queue.add_group(&ui.get_probe_title(), false, reqs);
                }
            }

            probe_state.generation.fetch_add(1, Ordering::SeqCst);
            probe_state.last_url.borrow_mut().clear();
            ui.set_url("".into());
            ui.set_probe_state(0);
            ui.set_probe_blocked(false);
            ui.set_probe_is_spotify(false);
            ui.set_probe_is_gif(false);
            clear_picker(&ui, &probe_state);
            clear_link_notes(&ui, &probe_state);
            extras_ui::clear(&ui);
        }
    });

    ui.on_dup_show({
        let probe_state = probe_state.clone();
        move || {
            if let Some(f) = probe_state.dup_file.borrow().as_ref() {
                reveal(f);
            }
        }
    });
    ui.on_use_playlist({
        let weak = ui.as_weak();
        let (probe_state, settings) = (probe_state.clone(), settings.clone());
        move || {
            let ui = weak.unwrap();
            let link = ui.get_playlist_url().to_string();
            if link.is_empty() {
                return;
            }
            ui.set_url(link.as_str().into());
            trigger_probe(&ui, &probe_state, &settings, &link);
        }
    });

    ui.on_spotify_heads_up_ok({
        let (weak, save) = (ui.as_weak(), save.clone());
        move |skip| {
            let ui = weak.unwrap();
            ui.set_spotify_heads_up(false);
            if skip {
                ui.set_spotify_explain(false);
                save();
            }
            HEADS_UP_OK.store(true, Ordering::SeqCst);
            ui.invoke_start_download();
        }
    });

    // Rows have their job id; group cards the group id, negated.
    ui.on_cancel_job({
        let queue = queue.clone();
        move |id| if id < 0 { queue.cancel_group(-id as u64) } else { queue.cancel(id as u64) }
    });
    ui.on_retry_job({
        let queue = queue.clone();
        move |id| if id < 0 { queue.retry_group(-id as u64) } else { queue.retry(id as u64) }
    });
    ui.on_pause_job({
        let queue = queue.clone();
        move |id| if id < 0 { queue.pause_all(Some(-id as u64)) } else { queue.pause(id as u64) }
    });
    ui.on_resume_job({
        let queue = queue.clone();
        move |id| if id < 0 { queue.resume_all(Some(-id as u64)) } else { queue.resume(id as u64) }
    });
    ui.on_pause_all({
        let queue = queue.clone();
        move || queue.pause_all(None)
    });
    ui.on_resume_all({
        let queue = queue.clone();
        move || queue.resume_all(None)
    });
    ui.on_toggle_group(toggle_group);
    ui.on_remove_job({
        let (queue, weak) = (queue.clone(), ui.as_weak());
        move |id| {
            if id < 0 {
                let ids = queue.clear_finished(Some(-id as u64));
                remove_rows(&weak.unwrap(), &ids);
            } else if queue.remove(id as u64) {
                remove_rows(&weak.unwrap(), &[id as u64]);
            }
        }
    });
    ui.on_clear_finished({
        let (queue, weak) = (queue.clone(), ui.as_weak());
        move || {
            let ids = queue.clear_finished(None);
            remove_rows(&weak.unwrap(), &ids);
        }
    });
    ui.on_open_job({
        let (queue, settings) = (queue.clone(), settings.clone());
        move |id| {
            if id < 0 {
                let dir = queue.group_dir(-id as u64).unwrap_or_else(|| settings.lock().unwrap().output_dir.clone());
                let _ = std::fs::create_dir_all(&dir);
                let _ = open::that(dir);
                return;
            }
            match queue.file_of(id as u64).filter(|p| p.exists()) {
                Some(path) => reveal(&path),
                None => {
                    let _ = open::that(&settings.lock().unwrap().output_dir);
                }
            }
        }
    });
    ui.on_open_output({
        let settings = settings.clone();
        move || {
            let dir = settings.lock().unwrap().output_dir.clone();
            let _ = std::fs::create_dir_all(&dir);
            let _ = open::that(dir);
        }
    });
    ui.on_choose_output({
        let (weak, save) = (ui.as_weak(), save.clone());
        move || {
            let ui = weak.unwrap();
            let current = ui.get_output_dir().to_string();
            if let Some(dir) = rfd::FileDialog::new().set_directory(&current).pick_folder() {
                ui.set_output_dir(dir.to_string_lossy().as_ref().into());
                ui.set_output_short(short_dir(&dir).into());
                save();
            }
        }
    });
    ui.on_hide_tips({
        let (weak, save) = (ui.as_weak(), save.clone());
        move || {
            weak.unwrap().set_show_tips(false);
            save();
        }
    });
    recent_ui::install(&ui);
    // Flagged songs: listen to the file, or keep it and drop the doubt ("It's right").
    ui.on_play_job({
        let queue = queue.clone();
        move |id| {
            if let Some(path) = queue.file_of(id as u64).filter(|p| p.exists()) {
                let _ = open::that(path);
            }
        }
    });
    ui.on_accept_job({
        let queue = queue.clone();
        move |id| queue.accept(id as u64)
    });
    ui.on_queue_filter_changed(|filter| {
        FILTER.with(|f| f.set(filter));
        RESET_VIEW.with(|r| {
            if let Some(reset) = r.borrow().as_ref() {
                reset();
            }
        });
    });
    ui.on_report_job({
        let (queue, weak) = (queue.clone(), ui.as_weak());
        move |id| {
            let ui = weak.unwrap();
            match queue.report(id as u64) {
                Some(text) => copy_report(&ui, &text),
                None => show_toast(&ui, "That download is gone from the list."),
            }
        }
    });
    ui.on_copy_report({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move || {
            let text = report::general_report(&settings.lock().unwrap());
            copy_report(&weak.unwrap(), &text);
        }
    });
    setup_ui::install(&ui);
    drop_ui::install(&ui);
    signin_ui::install(&ui);
    extras_ui::install(&ui);
    ui.on_retry_setup({
        let (weak, settings, queue) = (ui.as_weak(), settings.clone(), queue.clone());
        move || start_setup(&weak.unwrap(), settings.clone(), queue.clone())
    });

    let (_clipper, _clip_timer) = {
        let (q1, q2) = (queue.clone(), queue.clone());
        clipper::install(
            &ui,
            move |id| q1.file_of(id),
            move |req| {
                q2.add(req);
            },
        )?
    };

    let _compressor = {
        let queue = queue.clone();
        compressor::install(&ui, move |id| queue.file_of(id))
    };

    start_setup(&ui, settings.clone(), queue.clone());
    ui.run()?;

    // Save the queue for next time, then stop every yt-dlp/ffmpeg so none survives the window.
    queue.shutdown();
    Ok(())
}
