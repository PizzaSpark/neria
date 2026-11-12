//! Running in the background: closing to the tray, Windows notifications (with Neria's own
//! chime), and "Download later".
//!
//! Closing with downloads going asks (or does what Settings → General says): keep going in
//! the tray, or stop and quit. The window is then hidden, not closed: the event loop keeps
//! running (`slint::run_event_loop_until_quit`) and the tray icon brings it back.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use neria_core::queue::{JobSnapshot, JobState, Queue};
use neria_core::settings::Settings;
use slint::winit_030::WinitWindowAccessor;
use slint::{CloseRequestResponse, ComponentHandle, Model};

use crate::{AppWindow, Background, QueueActions};

thread_local! {
    static TRAY: RefCell<Option<tray_icon::TrayIcon>> = const { RefCell::new(None) };
    static TRAY_TIMER: slint::Timer = slint::Timer::default();
    static IN_TRAY: Cell<bool> = const { Cell::new(false) };
    /// Told once per session that Neria went on in the tray.
    static TOLD_TRAY: Cell<bool> = const { Cell::new(false) };
    /// Each job's last state, to spot the moment it finishes.
    static LAST: RefCell<HashMap<u64, JobState>> = RefCell::default();
    /// Groups whose "12 songs saved" was sent (sent again if they start over).
    static GROUP_TOLD: RefCell<HashSet<i32>> = RefCell::default();
    /// "Download later" picked: the next Download waits until then.
    static LATER: RefCell<Option<(u64, String)>> = const { RefCell::new(None) };
}

const APP_ID: &str = "Neria.Downloader";
const SOUNDS: [&str; 3] = ["chime", "windows", "none"];
const CLOSE: [&str; 3] = ["ask", "tray", "quit"];

fn idx(list: &[&str], v: &str) -> i32 {
    list.iter().position(|x| *x == v).unwrap_or(0) as i32
}

// ---------- Settings ----------

fn to_ui(ui: &AppWindow, s: &Settings) {
    let b = ui.global::<Background>();
    b.set_close_action(idx(&CLOSE, &s.close_action));
    b.set_notify(s.notify);
    b.set_notify_unfocused(s.notify_unfocused);
    b.set_sound_done(idx(&SOUNDS, &s.sound_done));
    b.set_sound_failed(idx(&SOUNDS, &s.sound_failed));
}

fn from_ui(ui: &AppWindow, s: &mut Settings) {
    let b = ui.global::<Background>();
    s.close_action = CLOSE[b.get_close_action().clamp(0, 2) as usize].into();
    s.notify = b.get_notify();
    s.notify_unfocused = b.get_notify_unfocused();
    s.sound_done = SOUNDS[b.get_sound_done().clamp(0, 2) as usize].into();
    s.sound_failed = SOUNDS[b.get_sound_failed().clamp(0, 2) as usize].into();
}

// ---------- Sounds ----------

/// A short WAV made on the spot: two soft sine notes with a quick fade (no sound file to
/// ship). Done goes up, failed goes down and lower.
fn chime(failed: bool) -> &'static [u8] {
    static DONE: OnceLock<Vec<u8>> = OnceLock::new();
    static FAIL: OnceLock<Vec<u8>> = OnceLock::new();
    let make = |notes: [f32; 2]| {
        let rate = 22_050u32;
        let note_len = (rate as f32 * 0.16) as usize;
        let tail = (rate as f32 * 0.32) as usize;
        let mut samples: Vec<i16> = Vec::new();
        for (n, f) in notes.iter().enumerate() {
            let len = if n == 0 { note_len } else { tail };
            for i in 0..len {
                let t = i as f32 / rate as f32;
                let attack = (i as f32 / 220.0).min(1.0);
                let env = attack * (-t * if n == 0 { 9.0 } else { 7.0 }).exp();
                let v = (2.0 * std::f32::consts::PI * f * t).sin() * 0.6
                    + (4.0 * std::f32::consts::PI * f * t).sin() * 0.12;
                samples.push((v * env * 0.32 * i16::MAX as f32) as i16);
            }
        }
        let data_len = (samples.len() * 2) as u32;
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        wav
    };
    if failed { FAIL.get_or_init(|| make([523.25, 392.0])) } else { DONE.get_or_init(|| make([880.0, 1318.5])) }
}

fn play_chime(failed: bool) {
    #[cfg(windows)]
    {
        use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
        let wav = chime(failed);
        // SAFETY: the buffer is static, so it outlives the asynchronous playback.
        unsafe {
            let _ = PlaySoundW(
                windows::core::PCWSTR(wav.as_ptr() as *const u16),
                None,
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }
    #[cfg(not(windows))]
    let _ = failed;
}

// ---------- Notifications ----------

/// Windows shows toasts only for an app id it knows: a per-user registry entry with
/// Neria's name and icon (it holds no settings; removed again when notifications go off).
fn register_app_id(on: bool) {
    #[cfg(windows)]
    {
        let key = format!(r"HKCU\Software\Classes\AppUserModelId\{APP_ID}");
        let quiet = |args: &[&str]| {
            let _ = neria_core::proc::command("reg")
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        };
        if !on {
            quiet(&["delete", &key, "/f"]);
            return;
        }
        let icon = neria_core::paths::cache_dir().join("neria-icon.png");
        if !icon.is_file() {
            let _ = std::fs::write(&icon, include_bytes!("../assets/neria-256.png"));
        }
        quiet(&["add", &key, "/v", "DisplayName", "/d", "Neria", "/f"]);
        quiet(&["add", &key, "/v", "IconUri", "/d", &icon.to_string_lossy(), "/f"]);
    }
    #[cfg(not(windows))]
    let _ = on;
}

/// What a notification says and offers.
pub struct Note {
    pub title: String,
    pub body: String,
    /// A thumbnail URL (fetched, or already cached) or a local picture.
    pub image: Option<String>,
    /// The saved file: "Open" / "Show in folder".
    pub file: Option<PathBuf>,
    pub failed: bool,
}

/// Neria is in the tray (its window hidden).
pub fn in_tray() -> bool {
    IN_TRAY.with(Cell::get)
}

fn window_focused(ui: &AppWindow) -> bool {
    !IN_TRAY.with(Cell::get) && ui.window().with_winit_window(|w| w.has_focus()).unwrap_or(false)
}

/// Send a Windows notification (if Settings allow it right now).
pub fn notify(ui: &AppWindow, note: Note) {
    let b = ui.global::<Background>();
    if !b.get_notify() || (b.get_notify_unfocused() && window_focused(ui)) {
        return;
    }
    let sound = if note.failed { b.get_sound_failed() } else { b.get_sound_done() };
    if sound == 0 {
        play_chime(note.failed);
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        use tauri_winrt_notification::{Sound, Toast};
        let image = note.image.as_deref().and_then(|src| {
            if src.starts_with("http") {
                neria_core::probe::fetch_thumbnail(src).ok()
            } else {
                Some(PathBuf::from(src))
            }
        });
        let mut toast = Toast::new(APP_ID).title(&note.title).text1(&note.body);
        if let Some(img) = &image {
            toast = toast.image(img, "");
        }
        toast = toast.sound(if sound == 1 {
            Some(if note.failed { Sound::Reminder } else { Sound::Default })
        } else {
            None
        });
        if note.file.is_some() {
            toast = toast.add_button("Open", "open").add_button("Show in folder", "show");
        }
        let file = note.file.clone();
        let toast = toast.on_activated(move |action| {
            let file = file.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| match (action.as_deref(), file) {
                (Some("open"), Some(f)) => {
                    let _ = open::that(f);
                }
                (Some("show"), Some(f)) => crate::reveal(&f),
                _ => show_window(&ui),
            });
            Ok(())
        });
        let _ = toast.show();
    });
}

/// After the queue's updates are in the rows: a notification for each download that just
/// finished, or one for a whole group once all of it has.
pub fn after_updates(ui: &AppWindow, snaps: &[JobSnapshot]) {
    let mut groups: HashSet<i32> = HashSet::new();
    for snap in snaps {
        let before = LAST.with(|l| l.borrow_mut().insert(snap.id, snap.state));
        let finished = matches!(snap.state, JobState::Done | JobState::Failed);
        let was_going = before.is_some_and(|b| b.is_active());
        if snap.state.is_active() && snap.group != 0 {
            GROUP_TOLD.with(|g| g.borrow_mut().remove(&(snap.group as i32)));
        }
        if snap.restored || !finished || !was_going {
            continue;
        }
        if snap.group != 0 && group_size(snap.group as i32) > 1 {
            groups.insert(snap.group as i32);
            continue;
        }
        let note = if snap.state == JobState::Done {
            Note {
                title: "Saved".into(),
                body: snap.title.clone(),
                image: snap.thumbnail.clone(),
                file: snap.file.clone(),
                failed: false,
            }
        } else {
            Note {
                title: "Couldn't download it".into(),
                body: format!("{}\n{}", snap.title, snap.error.clone().unwrap_or_default()),
                image: snap.thumbnail.clone(),
                file: None,
                failed: true,
            }
        };
        notify(ui, note);
    }
    for g in groups {
        group_finished(ui, g);
    }
}

fn group_size(group: i32) -> usize {
    crate::rows().iter().filter(|j| !j.header && j.group == group).count()
}

/// The whole group is through: "12 songs saved" / "11 of 12 saved, 1 failed".
fn group_finished(ui: &AppWindow, group: i32) {
    let rows: Vec<crate::JobItem> = crate::rows().iter().filter(|j| !j.header && j.group == group).collect();
    if rows.iter().any(|j| j.state <= 2 || j.state == 6) || GROUP_TOLD.with(|g| !g.borrow_mut().insert(group)) {
        return;
    }
    let n = rows.len();
    let saved = rows.iter().filter(|j| j.state == 3).count();
    let failed = rows.iter().filter(|j| j.state == 4).count();
    let what = if rows.iter().all(|j| j.music) { "songs" } else { "items" };
    let title = match failed {
        0 => format!("{n} {what} saved"),
        f => format!("{saved} of {n} saved, {f} failed"),
    };
    let card = crate::rows().iter().find(|j| j.header && j.group == group);
    let body = card.as_ref().map(|c| c.title.to_string()).unwrap_or_default();
    let image = rows.iter().find(|j| !j.thumb_url.is_empty()).map(|j| j.thumb_url.to_string());
    let file = rows.iter().find(|j| j.state == 3).map(|j| j.id as u64).and_then(crate::file_of_job);
    notify(ui, Note { title, body, image, file, failed: failed > 0 && saved == 0 });
}

// ---------- Tray ----------

fn tray_menu(paused: bool) -> tray_icon::menu::Menu {
    use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id("show", "Show Neria", true, None));
    let _ = menu.append(&PredefinedMenuItem::separator());
    if paused {
        let _ = menu.append(&MenuItem::with_id("resume", "Resume all", true, None));
    } else {
        let _ = menu.append(&MenuItem::with_id("pause", "Pause all", true, None));
    }
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id("quit", "Quit Neria", true, None));
    menu
}

fn tooltip(ui: &AppWindow) -> String {
    match ui.get_active_count() {
        0 => "Neria".into(),
        1 => "Neria · 1 download going".into(),
        n => format!("Neria · {n} downloads going"),
    }
}

/// Hide the window and keep going from the tray.
pub fn go_to_tray(ui: &AppWindow) {
    let built = TRAY.with(|t| {
        let mut t = t.borrow_mut();
        if t.is_none() {
            let icon = tray_icon::Icon::from_resource(1, None).ok();
            let mut b = tray_icon::TrayIconBuilder::new()
                .with_tooltip(tooltip(ui))
                .with_menu(Box::new(tray_menu(ui.get_paused_count() > 0 && ui.get_running_count() == 0)))
                .with_menu_on_left_click(false);
            if let Some(icon) = icon {
                b = b.with_icon(icon);
            }
            *t = b.build().ok();
        }
        t.is_some()
    });
    if !built {
        // No tray (very unusual): don't hide a window you couldn't get back.
        ui.window().set_minimized(true);
        return;
    }
    IN_TRAY.with(|t| t.set(true));
    let _ = ui.hide();
    let weak = ui.as_weak();
    TRAY_TIMER.with(|timer| {
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(150), move || {
            if let Some(ui) = weak.upgrade() {
                poll_tray(&ui);
            }
        })
    });
    if !TOLD_TRAY.with(|t| t.replace(true)) {
        let going = ui.get_active_count();
        notify(
            ui,
            Note {
                title: "Neria keeps going in the tray".into(),
                body: format!(
                    "{} Click the Neria icon next to the clock to bring it back.",
                    if going == 1 {
                        "1 download is going.".to_string()
                    } else {
                        format!("{going} downloads are going.")
                    }
                ),
                image: None,
                file: None,
                failed: false,
            },
        );
    }
}

fn poll_tray(ui: &AppWindow) {
    use tray_icon::menu::MenuEvent;
    use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
    while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
        match ev {
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
            | TrayIconEvent::DoubleClick { .. } => show_window(ui),
            _ => {}
        }
    }
    while let Ok(ev) = MenuEvent::receiver().try_recv() {
        match ev.id.0.as_str() {
            "show" => show_window(ui),
            "pause" => ui.invoke_pause_all(),
            "resume" => ui.invoke_resume_all(),
            "quit" => quit(ui),
            _ => {}
        }
    }
    // Pause all / Resume all follows the queue; the menu is only rebuilt when that flips.
    thread_local! {
        static MENU_PAUSED: Cell<Option<bool>> = const { Cell::new(None) };
    }
    let paused = ui.get_paused_count() > 0 && ui.get_running_count() == 0;
    let flipped = MENU_PAUSED.with(|m| m.replace(Some(paused))) != Some(paused);
    TRAY.with(|t| {
        if let Some(t) = t.borrow().as_ref() {
            let _ = t.set_tooltip(Some(tooltip(ui)));
            if flipped {
                t.set_menu(Some(Box::new(tray_menu(paused))));
            }
        }
    });
}

/// Bring the window back from the tray.
pub fn show_window(ui: &AppWindow) {
    let _ = ui.show();
    ui.window().set_minimized(false);
    let _ = ui.window().with_winit_window(|w| w.focus_window());
    IN_TRAY.with(|t| t.set(false));
    TRAY_TIMER.with(|t| t.stop());
    TRAY.with(|t| *t.borrow_mut() = None);
}

/// Stop everything and close (the queue is saved in main after the loop ends).
pub fn quit(ui: &AppWindow) {
    TRAY_TIMER.with(|t| t.stop());
    TRAY.with(|t| *t.borrow_mut() = None);
    let _ = ui.hide();
    let _ = slint::quit_event_loop();
}

fn close_requested(ui: &AppWindow) -> CloseRequestResponse {
    let going = ui.get_active_count();
    if going == 0 {
        quit(ui);
        return CloseRequestResponse::HideWindow;
    }
    match ui.global::<Background>().get_close_action() {
        1 => {
            go_to_tray(ui);
            CloseRequestResponse::KeepWindowShown
        }
        2 => {
            quit(ui);
            CloseRequestResponse::HideWindow
        }
        _ => {
            let b = ui.global::<Background>();
            b.set_going(going);
            b.set_remember(false);
            b.set_ask_open(true);
            CloseRequestResponse::KeepWindowShown
        }
    }
}

// ---------- Download later ----------

/// When "Download later" starts, and how to say it: (Unix seconds, "02:00", "Starts tonight
/// at 02:00"). `None` for a time that isn't one.
pub fn later_time(choice: i32, typed: &str, now: chrono::DateTime<chrono::Local>) -> Option<(u64, String, String)> {
    use chrono::{Duration as D, NaiveTime, TimeZone};
    let at_today = |t: NaiveTime| {
        let today = now.date_naive().and_time(t);
        let when = chrono::Local.from_local_datetime(&today).earliest()?;
        Some(if when <= now { when + D::days(1) } else { when })
    };
    let when = match choice {
        0 => now + D::hours(1),
        1 => at_today(NaiveTime::from_hms_opt(2, 0, 0)?)?,
        2 => {
            let t = now.date_naive().succ_opt()?.and_time(NaiveTime::from_hms_opt(8, 0, 0)?);
            chrono::Local.from_local_datetime(&t).earliest()?
        }
        _ => {
            let typed = typed.trim().replace('.', ":");
            let (h, m) = typed.split_once(':').unwrap_or((&typed, "0"));
            let t = NaiveTime::from_hms_opt(h.trim().parse().ok()?, m.trim().parse().ok()?, 0)?;
            at_today(t)?
        }
    };
    let label = when.format("%H:%M").to_string();
    let day = if when.date_naive() == now.date_naive() {
        if when.format("%H").to_string().parse::<u32>().unwrap_or(0) >= 18 { "tonight" } else { "today" }
    } else if when.format("%H").to_string().parse::<u32>().unwrap_or(0) < 6 {
        "tonight"
    } else {
        "tomorrow"
    };
    Some((when.timestamp().max(0) as u64, label.clone(), format!("Starts {day} at {label}")))
}

/// The "Download later" time just confirmed, for the Download about to happen.
pub fn take_later() -> Option<(u64, String)> {
    LATER.with(|l| l.borrow_mut().take())
}

fn refresh_later(ui: &AppWindow) {
    let b = ui.global::<Background>();
    let note = later_time(b.get_later_choice(), &b.get_later_time(), chrono::Local::now())
        .map(|w| w.2)
        .unwrap_or_else(|| "Type a time like 22:30".into());
    b.set_later_note(note.into());
}

// ---------- Wiring ----------

pub fn install(ui: &AppWindow, queue: Queue, settings: Arc<Mutex<Settings>>) {
    {
        let s = settings.lock().unwrap();
        to_ui(ui, &s);
        let on = s.notify;
        std::thread::spawn(move || register_app_id(on));
    }
    let b = ui.global::<Background>();
    b.on_settings_changed({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move || {
            let ui = weak.unwrap();
            let mut s = settings.lock().unwrap();
            let was = s.notify;
            from_ui(&ui, &mut s);
            let _ = s.save();
            if was != s.notify {
                let on = s.notify;
                std::thread::spawn(move || register_app_id(on));
            }
        }
    });
    b.on_test_sound({
        let weak = ui.as_weak();
        move |failed| {
            let ui = weak.unwrap();
            let b = ui.global::<Background>();
            if (if failed { b.get_sound_failed() } else { b.get_sound_done() }) == 0 {
                play_chime(failed);
            }
        }
    });
    b.on_keep_going({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let b = ui.global::<Background>();
            b.set_ask_open(false);
            if b.get_remember() {
                b.set_close_action(1);
                b.invoke_settings_changed();
            }
            go_to_tray(&ui);
        }
    });
    b.on_quit({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let b = ui.global::<Background>();
            b.set_ask_open(false);
            if b.get_remember() {
                b.set_close_action(2);
                b.invoke_settings_changed();
            }
            quit(&ui);
        }
    });
    b.on_later_changed({
        let weak = ui.as_weak();
        move || refresh_later(&weak.unwrap())
    });
    b.on_later_confirm({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let b = ui.global::<Background>();
            let Some((at, label, _)) = later_time(b.get_later_choice(), &b.get_later_time(), chrono::Local::now())
            else {
                refresh_later(&ui);
                return;
            };
            b.set_later_open(false);
            LATER.with(|l| *l.borrow_mut() = Some((at, label)));
            ui.invoke_start_download();
            // Not used (nothing to download yet): don't let it stick to a later Download.
            LATER.with(|l| *l.borrow_mut() = None);
        }
    });
    ui.global::<QueueActions>().on_start_now({
        let queue = queue.clone();
        move |id| if id < 0 { queue.start_now(-id as u64, true) } else { queue.start_now(id as u64, false) }
    });
    ui.window().on_close_requested({
        let weak = ui.as_weak();
        move || weak.upgrade().map_or(CloseRequestResponse::HideWindow, |ui| close_requested(&ui))
    });
    let _ = queue;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn later_times() {
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 12, 21, 15, 0).unwrap();
        let (_, label, note) = later_time(1, "", now).unwrap();
        assert_eq!((label.as_str(), note.as_str()), ("02:00", "Starts tonight at 02:00"));
        let (_, label, note) = later_time(2, "", now).unwrap();
        assert_eq!((label.as_str(), note.as_str()), ("08:00", "Starts tomorrow at 08:00"));
        let (_, label, note) = later_time(3, "22.30", now).unwrap();
        assert_eq!((label.as_str(), note.as_str()), ("22:30", "Starts tonight at 22:30"));
        assert!(later_time(3, "25:00", now).is_none());
        let (at, _, _) = later_time(0, "", now).unwrap();
        assert_eq!(at, (now + chrono::Duration::hours(1)).timestamp() as u64);
        let wav = chime(false);
        assert_eq!(&wav[..4], b"RIFF");
    }
}
