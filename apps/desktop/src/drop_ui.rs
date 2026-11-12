//! Drag things in from other apps. Files from Explorer: onto Clip or Compress opens them
//! there; anywhere else asks which of the two. Links from a browser: onto Clip opens them in
//! the clipper; anywhere else they go into the Download box. Slint doesn't deliver drops
//! from other apps on Windows yet and winit only reports files, so on Windows Neria
//! registers its own OLE drop target (`ole` below); winit's file events stay as a fallback.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use neria_core::probe;
use slint::ComponentHandle;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};

use crate::AppWindow;

const VIDEO: &[&str] =
    &["mp4", "mkv", "webm", "mov", "avi", "m4v", "flv", "wmv", "ts", "mts", "m2ts", "3gp", "mpg", "mpeg", "ogv"];
const AUDIO: &[&str] = &["mp3", "m4a", "aac", "wav", "flac", "opus", "ogg", "wma"];

/// `drop-kind` in the UI: 0 not media, 1 video, 2 audio, 3 a link.
const LINK: i32 = 3;

fn ext(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn file_kind(path: &Path) -> i32 {
    let e = ext(path);
    if VIDEO.contains(&e.as_str()) {
        1
    } else if AUDIO.contains(&e.as_str()) {
        2
    } else {
        0
    }
}

/// Show the drop overlay for a file being dragged over the window.
fn hover_file(ui: &AppWindow, path: &Path) {
    ui.set_drop_name(name(path).into());
    ui.set_drop_kind(file_kind(path));
    ui.set_drop_hover(true);
}

/// Show the drop overlay for a link being dragged over the window.
fn hover_link(ui: &AppWindow, url: &str) {
    ui.set_drop_name(short_link(url).into());
    ui.set_drop_kind(LINK);
    ui.set_drop_hover(true);
}

/// "youtube.com/watch?v=…" for the overlay: no scheme or www, and not too long.
fn short_link(url: &str) -> String {
    let s = url.split_once("://").map_or(url, |(_, rest)| rest);
    let s = s.strip_prefix("www.").unwrap_or(s);
    if s.chars().count() > 60 { format!("{}…", s.chars().take(59).collect::<String>()) } else { s.to_string() }
}

/// The first line of dragged text, when it's a web link.
fn as_link(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    probe::looks_like_url(line).then(|| line.to_string())
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
                    hover_file(&ui, path);
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

    // The window exists once the event loop runs: swap in our drop target then.
    #[cfg(windows)]
    {
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::ZERO, move || {
            if let Some(ui) = weak.upgrade() {
                ole::install(&ui);
            }
        });
    }

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

/// A link dropped on the window: the clipper on the Clip page, else the Download box
/// (which shows its preview, like pasting it).
fn link_dropped(ui: &AppWindow, url: String) {
    if !ui.get_tools_ready() {
        return;
    }
    ui.set_drawer_open(false);
    if ui.get_page() == 1 {
        ui.invoke_clip_open_link(url.into(), "".into());
    } else {
        ui.set_page(0);
        ui.set_url(url.as_str().into());
        ui.invoke_url_edited(url.into());
    }
}

/// Dev hooks for design checks: `NERIA_DROP_HOVER=<file or link>` shows the overlay as if
/// it were being dragged over the window; `NERIA_DROP=<file or link>` acts as if it was dropped.
pub fn dev_hooks(ui: &AppWindow) {
    if let Some(v) = std::env::var_os("NERIA_DROP_HOVER") {
        match as_link(&v.to_string_lossy()) {
            Some(url) => hover_link(ui, &url),
            None => hover_file(ui, Path::new(&v)),
        }
    }
    #[cfg(windows)]
    if std::env::var_os("NERIA_DROP_CLIPBOARD").is_some() {
        ole::drop_clipboard(ui);
    }
    if let Some(v) = std::env::var_os("NERIA_DROP") {
        match as_link(&v.to_string_lossy()) {
            Some(url) => link_dropped(ui, url),
            None => dropped(ui, PathBuf::from(v)),
        }
    }
}

#[cfg(windows)]
mod ole {
    use super::*;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HWND, POINTL};
    use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::{
        CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl,
        RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
    };
    use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    use windows::core::{Ref, implement, w};

    /// What's being dragged over the window.
    enum Dragged {
        File { path: PathBuf, more: bool },
        Link(String),
        Other,
    }

    #[implement(IDropTarget)]
    struct Target {
        weak: slint::Weak<AppWindow>,
        effect: Cell<DROPEFFECT>,
        // Browsers put the link of a dragged tab/link here (UTF-16).
        url_format: u16,
    }

    fn format(cf: u16) -> FORMATETC {
        FORMATETC {
            cfFormat: cf,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }

    /// The UTF-16 text the data object holds in this format, if any.
    fn text(data: &IDataObject, cf: u16) -> Option<String> {
        unsafe {
            let mut medium = data.GetData(&format(cf)).ok()?;
            let h = medium.u.hGlobal;
            let p = GlobalLock(h) as *const u16;
            let out = (!p.is_null()).then(|| {
                let units = std::slice::from_raw_parts(p, GlobalSize(h) / 2);
                let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
                String::from_utf16_lossy(&units[..end])
            });
            let _ = GlobalUnlock(h);
            ReleaseStgMedium(&mut medium);
            out
        }
    }

    /// Files from Explorer.
    fn files(data: &IDataObject) -> Vec<PathBuf> {
        unsafe {
            let Ok(mut medium) = data.GetData(&format(CF_HDROP.0)) else { return Vec::new() };
            let drop = HDROP(medium.u.hGlobal.0);
            let count = DragQueryFileW(drop, u32::MAX, None);
            let mut out = Vec::new();
            for i in 0..count {
                let len = DragQueryFileW(drop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(drop, i, Some(&mut buf));
                out.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
            }
            ReleaseStgMedium(&mut medium);
            out
        }
    }

    impl Target {
        fn read(&self, data: Option<&IDataObject>) -> Dragged {
            let Some(data) = data else { return Dragged::Other };
            let found = files(data);
            if let Some(first) = found.first() {
                return Dragged::File { path: first.clone(), more: found.len() > 1 };
            }
            [self.url_format, CF_UNICODETEXT.0]
                .into_iter()
                .find_map(|cf| text(data, cf).and_then(|t| as_link(&t)))
                .map_or(Dragged::Other, Dragged::Link)
        }
    }

    impl IDropTarget_Impl for Target_Impl {
        fn DragEnter(
            &self,
            data: Ref<IDataObject>,
            _keys: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> windows::core::Result<()> {
            let what = self.read(data.ok().ok());
            let e = if matches!(what, Dragged::Other) { DROPEFFECT_NONE } else { DROPEFFECT_COPY };
            self.effect.set(e);
            if let Some(ui) = self.weak.upgrade() {
                match &what {
                    Dragged::File { path, .. } => hover_file(&ui, path),
                    Dragged::Link(url) => hover_link(&ui, url),
                    Dragged::Other => {}
                }
            }
            unsafe { *effect = e };
            Ok(())
        }

        fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
            unsafe { *effect = self.effect.get() };
            Ok(())
        }

        fn DragLeave(&self) -> windows::core::Result<()> {
            if let Some(ui) = self.weak.upgrade() {
                ui.set_drop_hover(false);
            }
            Ok(())
        }

        fn Drop(
            &self,
            data: Ref<IDataObject>,
            _keys: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> windows::core::Result<()> {
            let what = self.read(data.ok().ok());
            unsafe { *effect = self.effect.get() };
            let Some(ui) = self.weak.upgrade() else { return Ok(()) };
            ui.set_drop_hover(false);
            match what {
                Dragged::File { path, more } => {
                    dropped(&ui, path);
                    if more {
                        crate::show_toast(&ui, "One file at a time: Neria opened the first one.");
                    }
                }
                Dragged::Link(url) => link_dropped(&ui, url),
                Dragged::Other => {}
            }
            Ok(())
        }
    }

    /// Replace winit's drop target (files only) with ours (files and links).
    pub fn install(ui: &AppWindow) {
        let hwnd = ui
            .window()
            .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
                _ => None,
            })
            .flatten();
        let Some(hwnd) = hwnd else { return };
        let target: IDropTarget = Target {
            weak: ui.as_weak(),
            effect: Cell::new(DROPEFFECT_NONE),
            url_format: unsafe { RegisterClipboardFormatW(w!("UniformResourceLocatorW")) } as u16,
        }
        .into();
        unsafe {
            let hwnd = HWND(hwnd as *mut _);
            let _ = RevokeDragDrop(hwnd);
            if let Err(e) = RegisterDragDrop(hwnd, &target) {
                eprintln!("drop target: {e}");
            }
        }
    }

    /// Dev hook (`NERIA_DROP_CLIPBOARD`): read the clipboard through the same code a drop
    /// uses (it hands out the same kind of data object) and act as if it was dropped.
    pub fn drop_clipboard(ui: &AppWindow) {
        let target = Target {
            weak: ui.as_weak(),
            effect: Cell::new(DROPEFFECT_NONE),
            url_format: unsafe { RegisterClipboardFormatW(w!("UniformResourceLocatorW")) } as u16,
        };
        let data = unsafe { windows::Win32::System::Ole::OleGetClipboard() };
        match data.as_ref().map(|d| target.read(Some(d))) {
            Ok(Dragged::File { path, .. }) => dropped(ui, path),
            Ok(Dragged::Link(url)) => link_dropped(ui, url),
            Ok(Dragged::Other) => eprintln!("drop clipboard: nothing to drop"),
            Err(e) => eprintln!("drop clipboard: {e}"),
        }
    }
}
