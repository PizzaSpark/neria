//! Drag files in from Explorer: onto Clip or Compress opens them there; anywhere else asks
//! which of the two. Slint doesn't deliver drops from other apps on Windows yet, so this
//! listens to winit's file events directly.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};

use crate::AppWindow;

const VIDEO: &[&str] =
    &["mp4", "mkv", "webm", "mov", "avi", "m4v", "flv", "wmv", "ts", "mts", "m2ts", "3gp", "mpg", "mpeg", "ogv"];
const AUDIO: &[&str] = &["mp3", "m4a", "aac", "wav", "flac", "opus", "ogg", "wma"];

fn ext(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn install(ui: &AppWindow) {
    // Explorer sends one event per file; take the first of a batch.
    let last_drop: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let weak = ui.as_weak();
    ui.window().on_winit_window_event(move |_, event| {
        let Some(ui) = weak.upgrade() else { return EventResult::Propagate };
        match event {
            WindowEvent::HoveredFile(path) => {
                // Only the first file of a hover batch sets the label.
                if !ui.get_drop_hover() {
                    ui.set_drop_hover(true);
                    ui.set_drop_name(name(path).into());
                    let e = ext(path);
                    ui.set_drop_kind(if VIDEO.contains(&e.as_str()) {
                        1
                    } else if AUDIO.contains(&e.as_str()) {
                        2
                    } else {
                        0
                    });
                }
            }
            WindowEvent::HoveredFileCancelled => ui.set_drop_hover(false),
            WindowEvent::DroppedFile(path) => {
                ui.set_drop_hover(false);
                let now = Instant::now();
                if last_drop.get().is_some_and(|t| now.duration_since(t) < Duration::from_millis(600)) {
                    crate::show_toast(&ui, "One file at a time: Neria opened the first one.");
                    return EventResult::PreventDefault;
                }
                last_drop.set(Some(now));
                dropped(&ui, path.clone());
            }
            _ => return EventResult::Propagate,
        }
        EventResult::PreventDefault
    });

    ui.on_drop_choose({
        let weak = ui.as_weak();
        move |page| {
            let ui = weak.unwrap();
            let file = ui.get_drop_file().to_string();
            ui.set_drop_choice_open(false);
            match page {
                1 => ui.invoke_clip_open_path(file.into()),
                2 => ui.invoke_cmp_open_path(file.into()),
                _ => {}
            }
        }
    });
}

fn dropped(ui: &AppWindow, path: PathBuf) {
    if !ui.get_tools_ready() {
        return;
    }
    if path.is_dir() {
        crate::show_toast(ui, "That's a folder. Drop a video or audio file instead.");
        return;
    }
    let e = ext(&path);
    let video = VIDEO.contains(&e.as_str());
    let audio = AUDIO.contains(&e.as_str());
    if !video && !audio {
        let what = if e.is_empty() { "that file".to_string() } else { format!("a .{e} file") };
        crate::show_toast(ui, &format!("Neria opens video and audio files, not {what}."));
        return;
    }
    let file = path.to_string_lossy().into_owned();
    match ui.get_page() {
        1 => ui.invoke_clip_open_path(file.into()),
        2 if video => ui.invoke_cmp_open_path(file.into()),
        2 => crate::show_toast(ui, "Compress works on videos. Open audio files on the Clip page."),
        // Download page (or anywhere else): ask. Audio can only be clipped.
        _ if audio => ui.invoke_clip_open_path(file.into()),
        _ => {
            ui.set_drop_file(file.into());
            ui.set_drop_name(name(&path).into());
            ui.set_drop_choice_open(true);
        }
    }
}

/// Dev hooks for design checks: `NERIA_DROP_HOVER=<file>` shows the overlay as if that file
/// were being dragged over the window; `NERIA_DROP=<file>` acts as if it was dropped.
pub fn dev_hooks(ui: &AppWindow) {
    if let Some(p) = std::env::var_os("NERIA_DROP_HOVER").map(PathBuf::from) {
        let e = ext(&p);
        ui.set_drop_name(name(&p).into());
        ui.set_drop_kind(if VIDEO.contains(&e.as_str()) {
            1
        } else if AUDIO.contains(&e.as_str()) {
            2
        } else {
            0
        });
        ui.set_drop_hover(true);
    }
    if let Some(p) = std::env::var_os("NERIA_DROP").map(PathBuf::from) {
        dropped(ui, p);
    }
}
