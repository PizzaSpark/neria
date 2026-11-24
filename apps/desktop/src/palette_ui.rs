//! The command palette (Ctrl+K; `ui/dialogs/palette.slint`, mock `design/palette.html`, picks
//! 1A · 2A · 3A + 3B · 4A · 5A): one box that reaches every action and everything saved.
//! It adds nothing of its own: each row goes where a click would (a page, a Settings section,
//! a dialog, the Library's actions). Matching: `neria_core::palette`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use chrono::{Local, TimeZone};
use neria_core::library::{self, Item, Kind};
use neria_core::palette::{self, Used};
use neria_core::workflow;
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, Capture, Commands, Music, Nav, PalRow, Phone};

/// Rows per group while typing (4A), and things before typing (2A).
const PER_GROUP: usize = 5;
const RECENT: usize = 3;
const COMMON: usize = 3;
/// Files offered after an action that wants one (3B).
const FILES: usize = 8;

/// What a file action takes (3B: action first, then which file).
#[derive(Clone, Copy, PartialEq)]
enum Wants {
    Nothing,
    /// A video or sound file.
    Media,
    /// A video (Make a GIF).
    Video,
}

struct Action {
    key: &'static str,
    name: &'static str,
    /// Other words that find it.
    words: &'static [&'static str],
    icon: &'static str,
    /// The page it goes to, said on the right.
    to: &'static str,
    wants: Wants,
}

const fn a(key: &'static str, name: &'static str, words: &'static [&'static str], icon: &'static str, to: &'static str) -> Action {
    Action { key, name, words, icon, to, wants: Wants::Nothing }
}

const fn f(key: &'static str, name: &'static str, words: &'static [&'static str], icon: &'static str, to: &'static str, wants: Wants) -> Action {
    Action { key, name, words, icon, to, wants }
}

/// Everything the palette can do, in the order shown before anything is learnt.
const ACTIONS: &[Action] = &[
    f("clip", "Clip a file…", &["cut", "trim", "part", "open", "video"], "\u{E8C6}", "Clip", Wants::Media),
    f("compress", "Make a file smaller…", &["compress", "shrink", "size", "discord", "email", "whatsapp", "mb"], "\u{E73F}", "Compress", Wants::Media),
    a("replay", "Turn on instant replay", &["replay", "record", "game", "screen", "clip that", "last minute"], "\u{E7C8}", "Capture"),
    a("download", "Download a link…", &["get", "save", "grab", "paste", "youtube", "spotify", "mp3", "song", "playlist", "live", "stream", "record"], "\u{E896}", "Download"),
    f("sound", "Convert to a sound file…", &["convert", "extract audio", "audio", "mp3", "m4a", "flac", "wav", "opus", "sound only"], "\u{E8D6}", "Compress", Wants::Media),
    f("gif", "Make a GIF…", &["gif", "webp", "meme", "animated"], "\u{E8B9}", "Clip", Wants::Video),
    f("transcribe", "Transcribe…", &["text", "captions", "subtitles", "srt", "speech", "words", "whisper"], "\u{E8A5}", "Speech to text", Wants::Media),
    a("clip-link", "Clip part of a link…", &["cut", "trim", "youtube", "twitch", "part"], "\u{E71B}", "Clip"),
    a("save-replay", "Save the last minute", &["save", "replay", "clip that", "keep"], "\u{E74E}", "Capture"),
    a("phone", "Mirror my phone", &["android", "phone", "scrcpy", "screen", "record"], "\u{E8EA}", "Capture"),
    a("lyrics", "Add lyrics to songs I have…", &["lyrics", "lrc", "karaoke", "music"], "\u{E8D6}", "Spotify"),
    a("follow", "Follow a channel…", &["follow", "subscribe", "new uploads", "channel"], "\u{E8FA}", "Settings"),
    a("update", "Check for updates", &["update", "version", "new", "tools"], "\u{E895}", "Settings"),
    a("page-0", "Go to Download", &["page"], "\u{E896}", ""),
    a("page-1", "Go to Clip", &["page"], "\u{E8C6}", ""),
    a("page-2", "Go to Compress", &["page"], "\u{E73F}", ""),
    a("page-3", "Go to Capture", &["page", "replay"], "\u{E7C8}", ""),
    a("page-4", "Go to the Library", &["page", "saved", "history"], "\u{E8F1}", ""),
    a("set-0", "Settings: Download", &["folder", "quality", "format", "template", "names", "container"], "\u{E713}", ""),
    a("set-1", "Settings: Following", &["follow", "channels", "subscriptions"], "\u{E713}", ""),
    a("set-7", "Settings: Workflows", &["workflow", "automatic", "after download", "watch folder", "then"], "\u{E713}", ""),
    a("set-8", "Settings: Tools", &["tool", "external", "command", "program", "handbrake", "script"], "\u{E713}", ""),
    a("set-2", "Settings: Spotify", &["lyrics", "numbering", "music"], "\u{E713}", ""),
    a("set-3", "Settings: Network", &["cookies", "sign in", "account", "speed", "proxy"], "\u{E713}", ""),
    a("set-4", "Settings: General", &["theme", "dark", "light", "tray", "notifications", "start", "appearance"], "\u{E713}", ""),
    a("set-5", "Settings: Voice", &["clip that", "phrase", "mic", "voice"], "\u{E713}", ""),
    a("set-6", "Settings: About", &["version", "update", "tools", "licence"], "\u{E713}", ""),
];

fn action(key: &str) -> Option<&'static Action> {
    ACTIONS.iter().find(|a| a.key == key)
}

/// What a row does.
#[derive(Clone)]
enum Entry {
    Heading,
    Link(String),
    Action(&'static str),
    Thing(u64),
    /// One of a thing's actions (`library_ui::act` name).
    Do(u64, &'static str),
    /// A file for the action being done (3B), from the Library…
    File(u64),
    /// …or picked from disk.
    ChooseFile,
    /// A saved workflow: then which file.
    Workflow(u64),
    /// One of the user's tools: then which file.
    Tool(u64),
}

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Top,
    /// A thing's actions (3A).
    Thing(u64),
    /// Which file for this action (3B).
    Action(&'static str),
    /// Which file for this saved workflow.
    Workflow(u64),
    /// Which file for this tool.
    Tool(u64),
}

struct State {
    step: Step,
    entries: Vec<Entry>,
    items: Vec<Item>,
    used: Used,
    copied: Option<String>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State {
        step: Step::Top,
        entries: Vec::new(),
        items: Vec::new(),
        used: Used::default(),
        copied: None,
    });
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

fn looks_like_link(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with("https://") || t.starts_with("http://")) && !t.contains(char::is_whitespace)
}

/// "youtube.com/watch?v=k3…".
fn short_link(url: &str) -> String {
    let s = url.split_once("://").map_or(url, |(_, r)| r).trim_start_matches("www.");
    if s.chars().count() > 40 { format!("{}…", s.chars().take(39).collect::<String>()) } else { s.to_string() }
}

/// "today", "yesterday", "Saturday", "2 Nov".
fn day(at: u64) -> String {
    let Some(t) = Local.timestamp_opt(at as i64, 0).single() else { return String::new() };
    let days = (Local::now().date_naive() - t.date_naive()).num_days();
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=6 => t.format("%A").to_string(),
        _ => t.format("%-d %b").to_string(),
    }
}

/// The name split around what's typed (shown bold).
fn split(name: &str, query: &str) -> (String, String, String) {
    match palette::highlight(query, name) {
        Some((a, b)) => (name[..a].to_string(), name[a..b].to_string(), name[b..].to_string()),
        None => (name.to_string(), String::new(), String::new()),
    }
}

fn heading(text: &str) -> PalRow {
    PalRow { heading: true, pre: text.to_uppercase().into(), ..PalRow::default() }
}

fn action_row(a: &Action, query: &str) -> PalRow {
    let (pre, hit, post) = split(a.name, query);
    PalRow { pre: pre.into(), hit: hit.into(), post: post.into(), r#where: a.to.into(), icon: a.icon.into(), ..PalRow::default() }
}

fn thing_row(it: &Item, query: &str, with_day: bool) -> PalRow {
    let name = it.name();
    let (pre, hit, post) = split(&name, query);
    let frame = crate::library_ui::frame_of(&it.file);
    let sound = it.kind.is_sound() || library::is_audio_file(&it.file);
    let mut place = it.kind.label().to_string();
    if with_day {
        place = format!("{place} · {}", day(it.at));
    }
    PalRow {
        pre: pre.into(),
        hit: hit.into(),
        post: post.into(),
        r#where: place.into(),
        has_thumb: true,
        square: sound || frame.is_none() && it.kind.is_text(),
        thumb: frame.unwrap_or_default(),
        ..PalRow::default()
    }
}

/// Things worth a file action: here, a video or a sound (a GIF wants a video).
fn fits(it: &Item, wants: Wants) -> bool {
    let media = !it.kind.is_text() && it.kind != Kind::Playlist && it.kind != Kind::Gif;
    let sound = it.kind.is_sound() || library::is_audio_file(&it.file);
    let wanted = match wants {
        Wants::Nothing => false,
        Wants::Media => media,
        Wants::Video => media && !sound,
    };
    wanted && it.file.is_file()
}

/// The actions as they are now (instant replay on or off; Save only while it runs).
fn live_name(a: &'static Action) -> Option<&'static str> {
    match a.key {
        "replay" if crate::capture_ui::on() => Some("Turn off instant replay"),
        "save-replay" if !crate::capture_ui::running() => None,
        _ => Some(a.name),
    }
}

/// Fill the rows for the step and what's typed.
fn rebuild(ui: &AppWindow) {
    let c = ui.global::<Commands>();
    let query = c.get_query().to_string();
    let q = query.trim();
    let (step, items, used, copied) = with(|s| (s.step, s.items.clone(), s.used.clone(), s.copied.clone()));
    let mut rows: Vec<PalRow> = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut push = |row: PalRow, e: Entry| {
        rows.push(row);
        entries.push(e);
    };
    let newest = |items: &[Item]| {
        let mut v: Vec<Item> = items.to_vec();
        v.sort_by(|a, b| b.at.cmp(&a.at));
        v
    };
    match step {
        Step::Top => {
            // A link typed or pasted in the box, else the one on the clipboard (2A).
            let link = if looks_like_link(q) { Some(q.to_string()) } else if q.is_empty() { copied } else { None };
            if let Some(url) = link {
                push(heading(if q.is_empty() { "Copied link" } else { "Link" }), Entry::Heading);
                let words = if q.is_empty() { "Download the copied link" } else { "Download this link" };
                push(
                    PalRow { pre: words.into(), sub: short_link(&url).into(), icon: "\u{E896}".into(), ..PalRow::default() },
                    Entry::Link(url),
                );
            }
            if q.is_empty() {
                let recent: Vec<Item> = newest(&items).into_iter().take(RECENT).collect();
                if !recent.is_empty() {
                    push(heading("Recent"), Entry::Heading);
                    for it in &recent {
                        push(thing_row(it, "", true), Entry::Thing(it.id));
                    }
                }
                // The ones used most, else the first ones (2A).
                let mut common: Vec<(usize, &'static Action)> =
                    ACTIONS.iter().enumerate().filter(|(_, a)| live_name(a).is_some()).collect();
                common.sort_by_key(|(i, a)| (std::cmp::Reverse(used.count(a.key)), *i));
                push(heading("Do something"), Entry::Heading);
                for (_, a) in common.into_iter().take(COMMON) {
                    let mut row = action_row(a, "");
                    row.pre = live_name(a).unwrap_or(a.name).into();
                    push(row, Entry::Action(a.key));
                }
            } else if !looks_like_link(q) {
                // 4A: "Do" above "Saved", the best few of each.
                let mut found: Vec<(i32, &'static Action, &'static str)> = ACTIONS
                    .iter()
                    .filter_map(|a| {
                        let name = live_name(a)?;
                        palette::score(q, name, a.words).map(|sc| (sc + used.count(a.key).min(20) as i32 * 3, a, name))
                    })
                    .collect();
                found.sort_by(|x, y| y.0.cmp(&x.0));
                if !found.is_empty() {
                    push(heading("Do"), Entry::Heading);
                    for (_, a, name) in found.into_iter().take(PER_GROUP) {
                        let (pre, hit, post) = split(name, q);
                        push(
                            PalRow { pre: pre.into(), hit: hit.into(), post: post.into(), ..action_row(a, q) },
                            Entry::Action(a.key),
                        );
                    }
                }
                // Saved workflows by name ("discord-ready" → which file).
                let mut flows: Vec<(i32, workflow::Workflow)> = workflow::list()
                    .into_iter()
                    .filter_map(|w| palette::score(q, &w.name, &["workflow", "run"]).map(|sc| (sc, w)))
                    .collect();
                flows.sort_by(|x, y| y.0.cmp(&x.0));
                if !flows.is_empty() {
                    push(heading("Workflows"), Entry::Heading);
                    for (_, w) in flows.into_iter().take(PER_GROUP) {
                        let (pre, hit, post) = split(&w.name, q);
                        push(
                            PalRow {
                                pre: pre.into(),
                                hit: hit.into(),
                                post: post.into(),
                                sub: w.what().into(),
                                icon: "\u{E945}".into(),
                                r#where: "Workflow".into(),
                                ..PalRow::default()
                            },
                            Entry::Workflow(w.id),
                        );
                    }
                }
                // The user's own tools by name ("upscale" → which file).
                let mut tools: Vec<(i32, neria_core::tool::Tool)> = neria_core::tool::list()
                    .into_iter()
                    .filter_map(|t| palette::score(q, &t.name, &["tool", "run", &t.program_name()]).map(|sc| (sc, t)))
                    .collect();
                tools.sort_by(|x, y| y.0.cmp(&x.0));
                if !tools.is_empty() {
                    push(heading("Your tools"), Entry::Heading);
                    for (_, t) in tools.into_iter().take(PER_GROUP) {
                        let (pre, hit, post) = split(&t.name, q);
                        push(
                            PalRow {
                                pre: pre.into(),
                                hit: hit.into(),
                                post: post.into(),
                                sub: t.summary().into(),
                                icon: "\u{E90F}".into(),
                                r#where: "Tool".into(),
                                ..PalRow::default()
                            },
                            Entry::Tool(t.id),
                        );
                    }
                }
                let mut things: Vec<(i32, &Item)> = items
                    .iter()
                    .filter_map(|it| {
                        let file = it.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        palette::score(q, &it.name(), &[&it.site, it.kind.label(), &file]).map(|sc| (sc, it))
                    })
                    .collect();
                things.sort_by(|x, y| y.0.cmp(&x.0).then(y.1.at.cmp(&x.1.at)));
                if !things.is_empty() {
                    push(heading("Saved"), Entry::Heading);
                    for (_, it) in things.into_iter().take(PER_GROUP) {
                        push(thing_row(it, q, true), Entry::Thing(it.id));
                    }
                }
            }
        }
        Step::Thing(id) => {
            for (what, label, icon) in crate::library_ui::thing_actions(id) {
                if palette::score(q, label, &[]).is_some() {
                    let (pre, hit, post) = split(label, q);
                    push(
                        PalRow { pre: pre.into(), hit: hit.into(), post: post.into(), icon: icon.into(), ..PalRow::default() },
                        Entry::Do(id, what),
                    );
                }
            }
        }
        Step::Action(_) | Step::Workflow(_) | Step::Tool(_) => {
            let wants = match step {
                Step::Action(key) => action(key).map_or(Wants::Nothing, |a| a.wants),
                _ => Wants::Media,
            };
            let files: Vec<Item> = newest(&items)
                .into_iter()
                .filter(|it| {
                    let file = it.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    palette::score(q, &it.name(), &[&file]).is_some()
                })
                .filter(|it| fits(it, wants))
                .take(FILES)
                .collect();
            if !files.is_empty() {
                push(heading("From the Library"), Entry::Heading);
                for it in &files {
                    let mut row = thing_row(it, q, false);
                    if let Some(b) = it.size.filter(|b| *b > 0) {
                        row.r#where = neria_core::fmt::mb(b as f64).into();
                    }
                    push(row, Entry::File(it.id));
                }
            }
            push(PalRow { pre: "Choose a file…".into(), icon: "\u{E8B7}".into(), ..PalRow::default() }, Entry::ChooseFile);
        }
    }
    let first = entries.iter().position(|e| !matches!(e, Entry::Heading)).unwrap_or(0);
    with(|s| s.entries = entries);
    c.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
    select(ui, first as i32);
}

fn select(ui: &AppWindow, i: i32) {
    let c = ui.global::<Commands>();
    c.set_selected(i);
    let into = with(|s| match s.entries.get(i.max(0) as usize) {
        Some(Entry::Thing(_)) => true,
        Some(Entry::Action(k)) => action(k).is_some_and(|a| a.wants != Wants::Nothing),
        Some(Entry::Workflow(_) | Entry::Tool(_)) => true,
        _ => false,
    });
    c.set_can_go_into(into);
}

/// The crumb and the box's words for the step.
fn show_step(ui: &AppWindow) {
    let c = ui.global::<Commands>();
    let step = with(|s| s.step);
    c.set_crumb_has_thumb(false);
    c.set_crumb_thumb(slint::Image::default());
    match step {
        Step::Top => {
            c.set_crumb("".into());
            c.set_placeholder("What do you want to do?".into());
        }
        Step::Thing(id) => {
            let it = with(|s| s.items.iter().find(|i| i.id == id).cloned());
            if let Some(it) = it {
                c.set_crumb(it.name().into());
                if let Some(frame) = crate::library_ui::frame_of(&it.file) {
                    c.set_crumb_thumb(frame);
                    c.set_crumb_has_thumb(true);
                } else {
                    c.set_crumb_icon(if it.kind.is_sound() { "\u{EC4F}".into() } else { "\u{E8A5}".into() });
                }
            }
            c.set_placeholder("Do what?".into());
        }
        Step::Action(key) => {
            if let Some(a) = action(key) {
                c.set_crumb(a.name.trim_end_matches('…').into());
                c.set_crumb_icon(a.icon.into());
            }
            c.set_placeholder("Which file?".into());
        }
        Step::Workflow(id) => {
            if let Some(w) = workflow::get(id) {
                c.set_crumb(w.name.into());
                c.set_crumb_icon("\u{E945}".into());
            }
            c.set_placeholder("Which file?".into());
        }
        Step::Tool(id) => {
            if let Some(t) = neria_core::tool::get(id) {
                c.set_crumb(t.name.into());
                c.set_crumb_icon("\u{E90F}".into());
            }
            c.set_placeholder("Which file?".into());
        }
    }
}

fn go(ui: &AppWindow, step: Step) {
    with(|s| s.step = step);
    ui.global::<Commands>().set_query("".into());
    show_step(ui);
    rebuild(ui);
}

fn close(ui: &AppWindow) {
    ui.global::<Nav>().set_palette_open(false);
}

fn settings(ui: &AppWindow, section: i32) {
    ui.set_settings_section(section);
    ui.set_drawer_open(true);
}

/// Do an action that wants nothing more.
fn run(ui: &AppWindow, key: &str) {
    Used::picked(key);
    ui.set_drawer_open(false);
    match key {
        "download" => ui.set_page(0),
        "clip-link" => ui.set_page(1),
        "replay" => {
            ui.set_page(3);
            ui.global::<Phone>().set_tab(0);
            let c = ui.global::<Capture>();
            if crate::capture_ui::on() { c.invoke_turn_off() } else { c.invoke_turn_on() }
        }
        "save-replay" => ui.global::<Capture>().invoke_save_now(),
        "phone" => {
            ui.set_page(3);
            let p = ui.global::<Phone>();
            p.set_tab(1);
            p.invoke_shown(true);
        }
        "lyrics" => {
            let m = ui.global::<Music>();
            m.set_lyrics_dir("".into());
            m.invoke_add_lyrics();
        }
        "follow" => settings(ui, 1),
        "update" => {
            settings(ui, 6);
            ui.invoke_update_check();
        }
        k if k.starts_with("page-") => ui.set_page(k[5..].parse().unwrap_or(0)),
        k if k.starts_with("set-") => settings(ui, k[4..].parse().unwrap_or(0)),
        _ => {}
    }
}

fn pick(ui: &AppWindow, i: i32) {
    let Some(entry) = with(|s| s.entries.get(i.max(0) as usize).cloned()) else { return };
    match entry {
        Entry::Heading => {}
        Entry::Link(url) => {
            close(ui);
            ui.set_drawer_open(false);
            crate::library_ui::go_download(ui, &url);
        }
        Entry::Action(key) if action(key).is_some_and(|a| a.wants != Wants::Nothing) => go(ui, Step::Action(key)),
        Entry::Workflow(id) => go(ui, Step::Workflow(id)),
        Entry::Tool(id) => go(ui, Step::Tool(id)),
        Entry::Action(key) => {
            close(ui);
            run(ui, key);
        }
        Entry::Thing(id) => {
            close(ui);
            ui.set_drawer_open(false);
            crate::library_ui::act(ui, id, "main");
        }
        Entry::Do(id, what) => {
            close(ui);
            ui.set_drawer_open(false);
            crate::library_ui::act(ui, id, what);
        }
        Entry::File(id) => {
            let step = with(|s| s.step);
            let file = with(|s| s.items.iter().find(|i| i.id == id).map(|i| i.file.clone()));
            let Some(file) = file else { return };
            match step {
                Step::Action(key) => {
                    Used::picked(key);
                    close(ui);
                    ui.set_drawer_open(false);
                    crate::library_ui::act_on_file(ui, &file, key);
                }
                Step::Workflow(wid) => {
                    close(ui);
                    if let Some(w) = workflow::get(wid) {
                        crate::workflow_ui::run_on(ui, &w, &[file]);
                    }
                }
                Step::Tool(tid) => {
                    close(ui);
                    crate::tool_ui::run_one(ui, tid, file);
                }
                _ => {}
            }
        }
        Entry::ChooseFile => {
            let step = with(|s| s.step);
            if !matches!(step, Step::Action(_) | Step::Workflow(_) | Step::Tool(_)) {
                return;
            }
            close(ui);
            let picked: Option<PathBuf> = rfd::FileDialog::new().set_title("Which file?").pick_file();
            let Some(file) = picked else { return };
            match step {
                Step::Action(key) => {
                    Used::picked(key);
                    ui.set_drawer_open(false);
                    crate::library_ui::act_on_file(ui, &file, key);
                }
                Step::Workflow(wid) => {
                    if let Some(w) = workflow::get(wid) {
                        crate::workflow_ui::run_on(ui, &w, &[file]);
                    }
                }
                Step::Tool(tid) => crate::tool_ui::run_one(ui, tid, file),
                _ => {}
            }
        }
    }
}

pub fn install(ui: &AppWindow) {
    let c = ui.global::<Commands>();
    c.on_opened({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let copied = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()).filter(|t| looks_like_link(t));
            let items = crate::library_ui::items_for_palette();
            with(|s| {
                s.step = Step::Top;
                s.items = items;
                s.used = Used::load();
                s.copied = copied.map(|t| t.trim().to_string());
            });
            ui.global::<Commands>().set_query("".into());
            show_step(&ui);
            rebuild(&ui);
        }
    });
    c.on_changed({
        let weak = ui.as_weak();
        move || rebuild(&weak.unwrap())
    });
    c.on_move({
        let weak = ui.as_weak();
        move |d| {
            let ui = weak.unwrap();
            let now = ui.global::<Commands>().get_selected();
            let next = with(|s| {
                let n = s.entries.len() as i32;
                let mut i = now;
                for _ in 0..n {
                    i += d;
                    if i < 0 || i >= n {
                        return now;
                    }
                    if !matches!(s.entries[i as usize], Entry::Heading) {
                        return i;
                    }
                }
                now
            });
            select(&ui, next);
        }
    });
    c.on_pick({
        let weak = ui.as_weak();
        move |i| pick(&weak.unwrap(), i)
    });
    c.on_go_into({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let i = ui.global::<Commands>().get_selected();
            match with(|s| s.entries.get(i.max(0) as usize).cloned()) {
                Some(Entry::Thing(id)) => {
                    go(&ui, Step::Thing(id));
                    true
                }
                Some(Entry::Action(key)) if action(key).is_some_and(|a| a.wants != Wants::Nothing) => {
                    go(&ui, Step::Action(key));
                    true
                }
                _ => false,
            }
        }
    });
    c.on_back({
        let weak = ui.as_weak();
        move || go(&weak.unwrap(), Step::Top)
    });
    // `NERIA_PALETTE`: open it with this typed (`1`: nothing typed; `thing:<part of a title>`
    // its actions; `action:<key>` which file), for checks; `NERIA_PALETTE_ENTER`: then Enter.
    if let Ok(typed) = std::env::var("NERIA_PALETTE") {
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(600), move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.global::<Nav>().set_palette_open(true);
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(200), move || {
                let Some(ui) = weak.upgrade() else { return };
                if let Some(title) = typed.strip_prefix("thing:") {
                    let id = with(|s| s.items.iter().rev().find(|i| i.name().to_lowercase().contains(&title.to_lowercase())).map(|i| i.id));
                    if let Some(id) = id {
                        go(&ui, Step::Thing(id));
                    }
                } else if let Some(key) = typed.strip_prefix("action:") {
                    if let Some(a) = action(key) {
                        go(&ui, Step::Action(a.key));
                    }
                } else if typed != "1" {
                    ui.global::<Commands>().set_query(typed.as_str().into());
                    rebuild(&ui);
                }
                if std::env::var_os("NERIA_PALETTE_ENTER").is_some() {
                    let weak = ui.as_weak();
                    slint::Timer::single_shot(std::time::Duration::from_millis(800), move || {
                        if let Some(ui) = weak.upgrade() {
                            let i = ui.global::<Commands>().get_selected();
                            pick(&ui, i);
                        }
                    });
                }
            });
        });
    }
}
