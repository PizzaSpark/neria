#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use neria_core::download::DownloadRequest;
use neria_core::queue::{JobSnapshot, JobState, Queue};
use neria_core::settings::{Quality, Settings};
use neria_core::{fmt, probe, tools};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

mod clipper;
mod compressor;
mod match_ui;
mod spotify_ui;

slint::include_modules!();

const CONTAINERS: [&str; 3] = ["mp4", "mkv", "webm"];
const AUDIO_FORMATS: [&str; 5] = ["mp3", "m4a", "opus", "flac", "wav"];
const BROWSERS: [&str; 7] = ["", "chrome", "edge", "firefox", "brave", "opera", "vivaldi"];

fn index_of(list: &[&str], value: &str) -> i32 {
    list.iter().position(|v| *v == value).unwrap_or(0) as i32
}

fn pick(list: &[&str], i: i32) -> String {
    list.get(i.max(0) as usize).copied().unwrap_or(list[0]).to_string()
}

fn settings_to_ui(ui: &AppWindow, s: &Settings) {
    ui.set_output_dir(s.output_dir.to_string_lossy().as_ref().into());
    ui.set_quality(s.quality.index());
    ui.set_container_index(index_of(&CONTAINERS, &s.container));
    ui.set_audio_format_index(index_of(&AUDIO_FORMATS, &s.audio_format));
    ui.set_prefer_h264(s.prefer_h264);
    ui.set_embed_thumbnail(s.embed_thumbnail);
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
    ui.set_spotify_client_id(s.spotify_client_id.as_str().into());
    ui.set_spotify_client_secret(s.spotify_client_secret.as_str().into());
}

fn ui_to_settings(ui: &AppWindow, s: &mut Settings) {
    s.output_dir = ui.get_output_dir().as_str().into();
    s.quality = Quality::from_index(ui.get_quality());
    s.container = pick(&CONTAINERS, ui.get_container_index());
    s.audio_format = pick(&AUDIO_FORMATS, ui.get_audio_format_index());
    s.prefer_h264 = ui.get_prefer_h264();
    s.embed_thumbnail = ui.get_embed_thumbnail();
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
    s.spotify_client_id = ui.get_spotify_client_id().trim().to_string();
    s.spotify_client_secret = ui.get_spotify_client_secret().trim().to_string();
}

fn job_item(s: &JobSnapshot) -> JobItem {
    let (status, stats) = fmt::job_lines(s);
    JobItem {
        id: s.id as i32,
        group: s.group as i32,
        title: s.title.as_str().into(),
        state: match s.state {
            JobState::Queued => 0,
            JobState::Downloading => 1,
            JobState::Processing => 2,
            JobState::Done => 3,
            JobState::Failed => 4,
            JobState::Cancelled => 5,
        },
        progress: s.progress,
        status: status.into(),
        stats: stats.into(),
        warn: s.note.is_some(),
        music: s.music,
    }
}

/// Update (or insert) a job row and refresh the active counter.
fn upsert_job(ui: &AppWindow, snap: &JobSnapshot) {
    let model = ui.get_jobs();
    let Some(vec) = model.as_any().downcast_ref::<VecModel<JobItem>>() else { return };
    let item = job_item(snap);
    // Dev hook for scripted checks: open "pick another match" on the first flagged song.
    static PICKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if item.warn && item.state == 3 && std::env::var_os("NERIA_MATCH_PICK").is_some() && !PICKED.swap(true, Ordering::SeqCst) {
        let (weak, id) = (ui.as_weak(), item.id);
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_match_pick(id);
            }
        });
    }
    match vec.iter().position(|j| j.id == item.id) {
        Some(i) => vec.set_row_data(i, item),
        // New jobs go on top, except album/playlist tracks, which follow the
        // previous track of their group so they read in order.
        // Updates for rows the user removed are ignored.
        None if snap.state == JobState::Queued => {
            let after = if item.group != 0 {
                vec.iter().enumerate().filter(|(_, j)| j.group == item.group).map(|(i, _)| i).last()
            } else {
                None
            };
            vec.insert(after.map_or(0, |i| i + 1), item);
        }
        None => {}
    }
    refresh_active(ui);
}

fn refresh_active(ui: &AppWindow) {
    let model = ui.get_jobs();
    let active = model.iter().filter(|j| j.state <= 2).count();
    ui.set_active_count(active as i32);
}

fn remove_rows(ui: &AppWindow, ids: &[u64]) {
    let model = ui.get_jobs();
    let Some(vec) = model.as_any().downcast_ref::<VecModel<JobItem>>() else { return };
    let mut i = 0;
    while i < vec.row_count() {
        if ids.contains(&(vec.row_data(i).unwrap().id as u64)) {
            vec.remove(i);
        } else {
            i += 1;
        }
    }
    refresh_active(ui);
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn start_setup(ui: &AppWindow, settings: Arc<Mutex<Settings>>) {
    ui.set_setup_error("".into());
    ui.set_setup_progress(0.0);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let w = weak.clone();
        let result = tools::ensure_all(|p| {
            let (label, frac) = (p.text(), p.fraction());
            let _ = w.upgrade_in_event_loop(move |ui| {
                ui.set_setup_label(label.into());
                ui.set_setup_progress(frac);
            });
        });

        if let Err(e) = result {
            let msg = format!("{e:#}");
            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_setup_error(msg.into()));
            return;
        }

        // Sites change constantly; keep yt-dlp fresh (once a day, before any download runs).
        let due = now_secs().saturating_sub(settings.lock().unwrap().last_update_check) > 24 * 3600;
        if due {
            let _ = weak.upgrade_in_event_loop(|ui| {
                ui.set_setup_label("Checking for yt-dlp updates…".into());
                ui.set_setup_progress(1.0);
            });
            let _ = tools::update_ytdlp();
            let mut s = settings.lock().unwrap();
            s.last_update_check = now_secs();
            let _ = s.save();
        }

        let version = tools::ytdlp_version().unwrap_or_default();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_ytdlp_version(version.into());
            ui.set_tools_ready(true);
            // Dev/automation hook: open a file straight into the clipper.
            if let Some(path) = std::env::var_os("NERIA_OPEN") {
                ui.invoke_clip_open_path(path.to_string_lossy().as_ref().into());
            }
            if let Some(path) = std::env::var_os("NERIA_COMPRESS") {
                ui.invoke_cmp_open_path(path.to_string_lossy().as_ref().into());
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
}

fn trigger_probe(ui: &AppWindow, state: &ProbeState, settings: &Arc<Mutex<Settings>>, text: &str) {
    let url = text.trim().to_string();
    let spotify_link = neria_core::spotify::parse_link(&url);
    if !probe::looks_like_url(&url) && spotify_link.is_none() {
        state.generation.fetch_add(1, Ordering::SeqCst);
        state.last_url.borrow_mut().clear();
        ui.set_probe_state(0);
        ui.set_probe_is_spotify(false);
        return;
    }
    if !ui.get_tools_ready() || *state.last_url.borrow() == url {
        return;
    }
    *state.last_url.borrow_mut() = url.clone();

    let my_gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    ui.set_probe_state(1);
    ui.set_probe_title("".into());
    ui.set_probe_meta(url.as_str().into());
    ui.set_probe_thumb(slint::Image::default());
    ui.set_spotify_note("".into());

    let generation = state.generation.clone();
    let settings = settings.lock().unwrap().clone();
    if let Some(link) = spotify_link {
        spotify_ui::preview(ui, url, link, settings, generation, my_gen, state.spotify.clone());
        return;
    }
    ui.set_probe_is_spotify(false);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        // Debounce typing: only the latest edit actually runs yt-dlp.
        std::thread::sleep(Duration::from_millis(350));
        if generation.load(Ordering::SeqCst) != my_gen {
            return;
        }
        let result = probe::probe(&url, &settings);
        let thumb = result
            .as_ref()
            .ok()
            .and_then(|i| i.thumbnail_url.as_deref())
            .and_then(|u| probe::fetch_thumbnail(u).ok());

        let _ = weak.upgrade_in_event_loop(move |ui| {
            if generation.load(Ordering::SeqCst) != my_gen {
                return;
            }
            match result {
                Ok(info) => {
                    ui.set_probe_state(2);
                    ui.set_probe_title(info.title.as_str().into());
                    ui.set_probe_meta(info.meta_line().into());
                    if let Some(img) = thumb.and_then(|p| slint::Image::load_from_path(&p).ok()) {
                        ui.set_probe_thumb(img);
                    }
                }
                Err(e) => {
                    ui.set_probe_state(3);
                    ui.set_probe_title(e.to_string().into());
                    ui.set_probe_meta("You can still try downloading it.".into());
                }
            }
        });
    });
}

pub(crate) fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = open::that(path.parent().unwrap_or(path));
    }
}

fn main() -> anyhow::Result<()> {
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

    let settings = Arc::new(Mutex::new(Settings::load()));
    let ui = AppWindow::new()?;
    settings_to_ui(&ui, &settings.lock().unwrap());
    ui.set_jobs(ModelRc::from(Rc::new(VecModel::<JobItem>::default())));

    let weak = ui.as_weak();
    let queue = Queue::new(settings.clone(), move |snap| {
        let _ = weak.upgrade_in_event_loop(move |ui| upsert_job(&ui, &snap));
    });

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

    ui.on_settings_changed({
        let save = save.clone();
        move || save()
    });

    let probe_state = Rc::new(ProbeState {
        generation: Arc::new(AtomicU64::new(0)),
        last_url: RefCell::new(String::new()),
        spotify: Arc::new(Mutex::new(None)),
    });
    spotify_ui::install_sync_review(&ui);
    match_ui::install(&ui, queue.clone());

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
            let url = ui.get_url().trim().to_string();
            if url.is_empty() || !ui.get_tools_ready() {
                return;
            }
            save();
            if neria_core::spotify::parse_link(&url).is_some() {
                // Spotify: download every song once the lookup has finished.
                let settings = settings.lock().unwrap().clone();
                if !spotify_ui::enqueue(&ui, &url, &probe_state.spotify, &settings, &queue) {
                    return; // still looking it up (or the lookup failed)
                }
            } else {
                let title_hint = if ui.get_probe_state() == 2 { ui.get_probe_title().to_string() } else { url.clone() };
                queue.add(DownloadRequest::new(url, Quality::from_index(ui.get_quality()), title_hint));
            }

            probe_state.generation.fetch_add(1, Ordering::SeqCst);
            probe_state.last_url.borrow_mut().clear();
            ui.set_url("".into());
            ui.set_probe_state(0);
            ui.set_probe_is_spotify(false);
        }
    });

    ui.on_cancel_job({
        let queue = queue.clone();
        move |id| queue.cancel(id as u64)
    });
    ui.on_retry_job({
        let queue = queue.clone();
        move |id| queue.retry(id as u64)
    });
    ui.on_remove_job({
        let (queue, weak) = (queue.clone(), ui.as_weak());
        move |id| {
            if queue.remove(id as u64) {
                remove_rows(&weak.unwrap(), &[id as u64]);
            }
        }
    });
    ui.on_clear_finished({
        let (queue, weak) = (queue.clone(), ui.as_weak());
        move || {
            let ids = queue.clear_finished();
            remove_rows(&weak.unwrap(), &ids);
        }
    });
    ui.on_open_job({
        let (queue, settings) = (queue.clone(), settings.clone());
        move |id| match queue.file_of(id as u64).filter(|p| p.exists()) {
            Some(path) => reveal(&path),
            None => {
                let _ = open::that(&settings.lock().unwrap().output_dir);
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
                save();
            }
        }
    });
    ui.on_retry_setup({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move || start_setup(&weak.unwrap(), settings.clone())
    });

    let (_clipper, _clip_timer) = {
        let (q1, q2) = (queue.clone(), queue.clone());
        clipper::install(&ui, move |id| q1.file_of(id), move |req| {
            q2.add(req);
        })?
    };

    let _compressor = {
        let queue = queue.clone();
        compressor::install(&ui, move |id| queue.file_of(id))
    };

    start_setup(&ui, settings.clone());
    ui.run()?;

    // Stop any running downloads so no orphaned yt-dlp/ffmpeg survives the window.
    queue.cancel_all();
    Ok(())
}
