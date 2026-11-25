//! Drag things in from other apps. Files from Explorer: onto Clip or Compress opens them
//! there; anywhere else asks: clip, compress or transcribe it (several: compress or
//! transcribe them). Links from a browser: onto Clip opens them in
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

thread_local! {
    /// Several files dropped on the Download page, waiting for "Compress them" / "Transcribe them".
    static DROPPED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

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
    if VIDEO.contains(&e.as_str()) || AUDIO.contains(&e.as_str()) {
        1
    } else if e == "gif" {
        // Clip only: a .gif opens in the clipper (Compress takes videos and sound).
        2
    } else {
        0
    }
}

/// Over the Phone tab with a phone there: 4 an app to install, 5 files to send.
const PHONE_APK: i32 = 4;
const PHONE_FILES: i32 = 5;

/// Show the drop overlay for a file being dragged over the window.
fn hover_file(ui: &AppWindow, path: &Path) {
    if let Some((_, phone)) = crate::phone_ui::drop_target(ui) {
        let apk = ext(path) == "apk";
        ui.set_drop_name(if apk { name(path) } else { format!("{} · into its Download folder", name(path)) }.into());
        ui.set_drop_target(phone.into());
        ui.set_drop_kind(if apk { PHONE_APK } else { PHONE_FILES });
        ui.set_drop_hover(true);
        return;
    }
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
    // Our drop target goes in on the window's first event: the native window is created
    // lazily, and an event from it means it exists.
    #[cfg(windows)]
    let ole_ready = Cell::new(false);
    let weak = ui.as_weak();
    ui.window().on_winit_window_event(move |_, event| {
        let Some(ui) = weak.upgrade() else { return EventResult::Propagate };
        #[cfg(windows)]
        if !ole_ready.get() {
            ole_ready.set(ole::install(&ui));
        }
        match event {
            // How the last thing was done (a dialog or menu opened from the keyboard starts on
            // its first control; from the mouse it doesn't), and whether the window is in front
            // (a toast's time doesn't run out while Neria is behind something).
            WindowEvent::KeyboardInput { .. } => {
                ui.global::<crate::Nav>().set_keyboard(true);
                return EventResult::Propagate;
            }
            WindowEvent::MouseInput { .. } => {
                ui.global::<crate::Nav>().set_keyboard(false);
                return EventResult::Propagate;
            }
            WindowEvent::Focused(front) => {
                ui.global::<crate::Nav>().set_window_active(*front);
                return EventResult::Propagate;
            }
            WindowEvent::HoveredFile(path) => {
                // Only the first file of a hover batch sets the label.
                if !ui.get_drop_hover() {
                    hover_file(&ui, path);
                }
            }
            WindowEvent::HoveredFileCancelled => ui.set_drop_hover(false),
            WindowEvent::DroppedFile(path) => {
                ui.set_drop_hover(false);
                if let Some((serial, phone)) = crate::phone_ui::drop_target(&ui) {
                    crate::phone_ui::dropped(&ui, serial, phone, vec![path.clone()]);
                    return EventResult::PreventDefault;
                }
                // Compress takes several files (and folders of them) at once.
                if ui.get_page() == 2 && ui.get_tools_ready() && (path.is_dir() || compressible(path)) {
                    ui.global::<crate::Compress>().invoke_open_path(path.to_string_lossy().as_ref().into());
                    return EventResult::PreventDefault;
                }
                let now = Instant::now();
                if last_drop.get().is_some_and(|t| now.duration_since(t) < Duration::from_millis(600)) {
                    crate::toast_info(&ui, "One file at a time: Neria opened the first one.");
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
            // Several dropped at once: all of them (compress them, or transcribe them).
            let several = DROPPED.with(|d| d.take());
            let files = if ui.get_drop_several() && !several.is_empty() { several } else { vec![PathBuf::from(&file)] };
            match page {
                1 => ui.invoke_clip_open_path(file.into()),
                2 => {
                    for f in files {
                        ui.global::<crate::Compress>().invoke_open_path(f.to_string_lossy().as_ref().into());
                    }
                }
                3 => crate::transcribe_ui::open_files(&ui, files),
                _ => {}
            }
        }
    });
}

/// A video or a sound file: what the Compress page takes.
fn compressible(path: &Path) -> bool {
    let e = ext(path);
    VIDEO.contains(&e.as_str()) || AUDIO.contains(&e.as_str())
}

/// Several files (or folders) dropped at once: the Compress page takes every video, sound
/// file and folder; the Download page asks to compress or transcribe them all; anywhere
/// else only the first one opens.
fn dropped_files(ui: &AppWindow, first: PathBuf, all: Vec<PathBuf>) {
    // The Phone tab: to the phone (an app installed, the rest into its Download folder).
    if let Some((serial, phone)) = crate::phone_ui::drop_target(ui) {
        crate::phone_ui::dropped(ui, serial, phone, all);
        return;
    }
    let takes = |p: &PathBuf| p.is_dir() || compressible(p);
    if ui.get_page() == 2 && ui.get_tools_ready() && all.iter().any(takes) {
        for p in all.into_iter().filter(takes) {
            ui.global::<crate::Compress>().invoke_open_path(p.to_string_lossy().as_ref().into());
        }
        return;
    }
    // Several video or sound files on the Download page: compress or transcribe them all.
    let media: Vec<PathBuf> = all.iter().filter(|p| p.is_file() && compressible(p)).cloned().collect();
    if ui.get_page() == 0 && ui.get_tools_ready() && media.len() > 1 {
        ui.set_drop_file(media[0].to_string_lossy().as_ref().into());
        ui.set_drop_name(format!("{} files", media.len()).into());
        ui.set_drop_several(true);
        DROPPED.with(|d| d.replace(media));
        ui.set_drop_choice_open(true);
        return;
    }
    let more = all.len() > 1;
    dropped(ui, first);
    if more {
        crate::toast_info(ui, "One file at a time: Neria opened the first one. Compress takes several.");
    }
}

fn dropped(ui: &AppWindow, path: PathBuf) {
    if !ui.get_tools_ready() {
        return;
    }
    if path.is_dir() && ui.get_page() == 2 {
        ui.global::<crate::Compress>().invoke_open_path(path.to_string_lossy().as_ref().into());
        return;
    }
    if path.is_dir() {
        crate::toast_info(ui, "That's a folder. Drop a video or audio file instead.");
        return;
    }
    let e = ext(&path);
    // A list of links (one per line): into the Download box, which lists them.
    if e == "txt" {
        let text = std::fs::metadata(&path)
            .ok()
            .filter(|m| m.len() < 1_000_000)
            .and_then(|_| std::fs::read_to_string(&path).ok())
            .unwrap_or_default();
        let links = probe::split_links(&text);
        if links.is_empty() {
            crate::toast_info(ui, "There are no links in that file.");
            return;
        }
        let joined = links.join(" ");
        ui.set_drawer_open(false);
        ui.set_page(0);
        ui.set_url(joined.as_str().into());
        ui.invoke_url_edited(joined.into());
        return;
    }
    let video = VIDEO.contains(&e.as_str());
    let audio = AUDIO.contains(&e.as_str());
    if !video && !audio {
        let what = if e.is_empty() { "that file".to_string() } else { format!("a .{e} file") };
        crate::toast_info(ui, &format!("Neria opens video and audio files, not {what}."));
        return;
    }
    let file = path.to_string_lossy().into_owned();
    match ui.get_page() {
        1 => ui.invoke_clip_open_path(file.into()),
        2 => ui.global::<crate::Compress>().invoke_open_path(file.into()),
        // Download page (or anywhere else): ask.
        _ => {
            ui.set_drop_several(false);
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

    /// Dev aid (`NERIA_DROP_LOG=<file>`): append what each drag offered and what Neria
    /// made of it, to see why a drop from some app doesn't land.
    fn log(line: &str) {
        if let Some(path) = std::env::var_os("NERIA_DROP_LOG") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    /// The formats a data object offers, by name ("UniformResourceLocatorW", "CF 13"…).
    fn formats(data: &IDataObject) -> String {
        use windows::Win32::System::Com::DATADIR_GET;
        use windows::Win32::System::DataExchange::GetClipboardFormatNameW;
        let mut names = Vec::new();
        unsafe {
            let Ok(list) = data.EnumFormatEtc(DATADIR_GET.0 as u32) else { return "?".into() };
            loop {
                let mut f = [FORMATETC::default()];
                let mut got = 0u32;
                if list.Next(&mut f, Some(&mut got)).is_err() || got == 0 {
                    break;
                }
                let mut buf = [0u16; 128];
                let n = GetClipboardFormatNameW(f[0].cfFormat as u32, &mut buf);
                names.push(if n > 0 {
                    String::from_utf16_lossy(&buf[..n as usize])
                } else {
                    format!("CF {}", f[0].cfFormat)
                });
            }
        }
        names.join(", ")
    }

    /// What's being dragged over the window.
    #[derive(Debug)]
    enum Dragged {
        File { path: PathBuf, all: Vec<PathBuf> },
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
                return Dragged::File { path: first.clone(), all: found.clone() };
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
            if std::env::var_os("NERIA_DROP_LOG").is_some() {
                log(&format!("enter: [{}] -> {what:?}", data.ok().map(formats).unwrap_or_default()));
            }
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

        fn DragOver(
            &self,
            _keys: MODIFIERKEYS_FLAGS,
            _pt: &POINTL,
            effect: *mut DROPEFFECT,
        ) -> windows::core::Result<()> {
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
            log(&format!("drop: {what:?}"));
            unsafe { *effect = self.effect.get() };
            let Some(ui) = self.weak.upgrade() else { return Ok(()) };
            ui.set_drop_hover(false);
            match what {
                Dragged::File { path, all } => super::dropped_files(&ui, path, all),
                Dragged::Link(url) => link_dropped(&ui, url),
                Dragged::Other => {}
            }
            Ok(())
        }
    }

    /// Replace winit's drop target (files only) with ours (files and links). False when
    /// there's no native window to put it on.
    pub fn install(ui: &AppWindow) -> bool {
        let hwnd = ui
            .window()
            .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
                _ => None,
            })
            .flatten();
        let Some(hwnd) = hwnd else { return false };
        let target: IDropTarget = Target {
            weak: ui.as_weak(),
            effect: Cell::new(DROPEFFECT_NONE),
            url_format: unsafe { RegisterClipboardFormatW(w!("UniformResourceLocatorW")) } as u16,
        }
        .into();
        unsafe {
            let hwnd = HWND(hwnd as *mut _);
            let _ = RevokeDragDrop(hwnd);
            match RegisterDragDrop(hwnd, &target) {
                Ok(()) => log("drop target registered"),
                Err(e) => {
                    log(&format!("drop target: {e}"));
                    eprintln!("drop target: {e}");
                }
            }
        }
        true
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
            Ok(Dragged::File { path, all }) => super::dropped_files(ui, path, all),
            Ok(Dragged::Link(url)) => link_dropped(ui, url),
            Ok(Dragged::Other) => eprintln!("drop clipboard: nothing to drop"),
            Err(e) => eprintln!("drop clipboard: {e}"),
        }
    }
}
