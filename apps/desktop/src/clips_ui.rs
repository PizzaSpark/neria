//! Clip page → Clips (`ClipList` global, `dialogs/clips.slint`): several selections of one
//! video, saved in one go. The list lives here; the page shows it as marks on the timeline,
//! and Export uses it (see `clipper.rs`).

use std::cell::RefCell;
use std::rc::Rc;

use neria_core::fmt;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppWindow, ClipItem, ClipList};

#[derive(Clone, Debug)]
pub struct Clip {
    pub name: String,
    pub start: f64,
    pub end: f64,
}

thread_local! {
    static LIST: RefCell<Vec<Clip>> = const { RefCell::new(Vec::new()) };
}

pub fn clips() -> Vec<Clip> {
    LIST.with(|l| l.borrow().clone())
}

fn range(start: f64, end: f64) -> String {
    format!("{} – {} · {}", fmt::duration(start), fmt::duration(end), fmt::duration(end - start))
}

/// The list into the dialog and the timeline, and the line saying what Export will do.
pub fn show(ui: &AppWindow) {
    let list = clips();
    let items: Vec<ClipItem> = list
        .iter()
        .map(|c| ClipItem {
            name: c.name.clone().into(),
            start: c.start as f32,
            end: c.end as f32,
            range: range(c.start, c.end).into(),
        })
        .collect();
    let g = ui.global::<ClipList>();
    g.set_items(ModelRc::from(Rc::new(VecModel::from(items))));
    g.set_note(note(ui, &list).into());
}

fn note(ui: &AppWindow, list: &[Clip]) -> String {
    if list.is_empty() {
        return String::new();
    }
    // How they'll be saved depends on choices made on the page (Fast / Exact…): the page
    // adds that part itself, so it's never out of date.
    let _ = ui;
    let n = list.len();
    let total: f64 = list.iter().map(|c| c.end - c.start).sum();
    format!("{n} {}, {} in all", if n == 1 { "clip" } else { "clips" }, fmt::duration(total))
}

pub fn add(ui: &AppWindow) {
    let (start, end) = (ui.get_clip_in() as f64, ui.get_clip_out() as f64);
    if end <= start {
        return;
    }
    LIST.with(|l| {
        let mut l = l.borrow_mut();
        // The same selection twice is once.
        if l.iter().any(|c| (c.start - start).abs() < 0.05 && (c.end - end).abs() < 0.05) {
            return;
        }
        let name = format!("Clip {}", l.len() + 1);
        l.push(Clip { name, start, end });
    });
    // The band on the timeline and the count on Clips say it was added.
    show(ui);
}

/// Every chapter of the open video, by its name.
pub fn add_chapters(ui: &AppWindow) {
    let chapters = ui.get_clip_chapters();
    let dur = ui.get_clip_duration() as f64;
    LIST.with(|l| {
        let mut l = l.borrow_mut();
        for c in chapters.iter() {
            let (start, end) = (c.start as f64, (c.end as f64).min(if dur > 0.0 { dur } else { f64::MAX }));
            if end > start && !l.iter().any(|x| (x.start - start).abs() < 0.05 && (x.end - end).abs() < 0.05) {
                l.push(Clip { name: c.title.to_string(), start, end });
            }
        }
    });
    show(ui);
}

/// Paste times…: the clipboard's timestamps ("0:00 Intro, 3:12 …") as named clips.
pub fn paste_times(ui: &AppWindow) {
    let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
    let dur = ui.get_clip_duration() as f64;
    let parts = neria_core::tracks::parts_from_text(&text, dur);
    if parts.is_empty() {
        crate::show_toast(ui, "Copy a list of times first (like \"0:00 Intro\" on each line), then Paste times.");
        return;
    }
    let n = parts.len();
    LIST.with(|l| {
        let mut l = l.borrow_mut();
        for (name, start, end) in parts {
            if !l.iter().any(|x| (x.start - start).abs() < 0.05 && (x.end - end).abs() < 0.05) {
                l.push(Clip { name, start, end });
            }
        }
    });
    show(ui);
    crate::show_toast(ui, &format!("{n} parts added from the copied times"));
}

pub fn remove(ui: &AppWindow, i: usize) {
    LIST.with(|l| {
        let mut l = l.borrow_mut();
        if i < l.len() {
            l.remove(i);
        }
    });
    show(ui);
}

/// While typing: the row keeps its text (no rebuild), only the name behind it changes.
pub fn rename(ui: &AppWindow, i: usize, name: &str) {
    LIST.with(|l| {
        if let Some(c) = l.borrow_mut().get_mut(i) {
            c.name = name.to_string();
        }
    });
    let g = ui.global::<ClipList>();
    if let Some(mut item) = g.get_items().row_data(i) {
        item.name = name.into();
        g.get_items().set_row_data(i, item);
    }
}

pub fn move_to(ui: &AppWindow, from: usize, to: usize) {
    LIST.with(|l| {
        let mut l = l.borrow_mut();
        if from < l.len() && to < l.len() && from != to {
            let c = l.remove(from);
            l.insert(to, c);
        }
    });
    show(ui);
}

/// In and Out to that clip (the caller seeks to it).
pub fn pick(ui: &AppWindow, i: usize) -> Option<f64> {
    let c = clips().get(i).cloned()?;
    ui.set_clip_in(c.start as f32);
    ui.set_clip_out(c.end as f32);
    Some(c.start)
}

pub fn clear(ui: &AppWindow) {
    LIST.with(|l| l.borrow_mut().clear());
    show(ui);
}

pub fn install(ui: &AppWindow) {
    let g = ui.global::<ClipList>();
    let w = ui.as_weak();
    g.on_add({
        let w = w.clone();
        move || add(&w.unwrap())
    });
    g.on_add_chapters({
        let w = w.clone();
        move || add_chapters(&w.unwrap())
    });
    g.on_paste_times({
        let w = w.clone();
        move || paste_times(&w.unwrap())
    });
    // `NERIA_CLIP_PASTE_TIMES`: these times (`|` between lines) on the clipboard, then Paste
    // times… with the clip list open, once a file is open (with `NERIA_OPEN`).
    if let Ok(times) = std::env::var("NERIA_CLIP_PASTE_TIMES") {
        let w = w.clone();
        slint::Timer::single_shot(std::time::Duration::from_secs(4), move || {
            let Some(ui) = w.upgrade() else { return };
            let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(times.replace('|', "\n")));
            ui.global::<ClipList>().set_open(true);
            paste_times(&ui);
        });
    }
    g.on_remove({
        let w = w.clone();
        move |i| remove(&w.unwrap(), i.max(0) as usize)
    });
    g.on_rename({
        let w = w.clone();
        move |i, name| rename(&w.unwrap(), i.max(0) as usize, &name)
    });
    g.on_move({
        let w = w.clone();
        move |from, to| move_to(&w.unwrap(), from.max(0) as usize, to.max(0) as usize)
    });
    g.on_pick({
        let w = w.clone();
        move |i| {
            let ui = w.unwrap();
            if let Some(t) = pick(&ui, i.max(0) as usize) {
                ui.invoke_clip_seek(t as f32);
            }
        }
    });
    g.on_clear({
        let w = w.clone();
        move || clear(&w.unwrap())
    });
    g.on_join_changed(move || show(&w.unwrap()));
}
