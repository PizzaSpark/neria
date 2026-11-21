//! The Capture page: instant replay of a game, a window or a monitor (`neria_core::capture`).
//!
//! Turned on by hand (or when Neria starts, if asked), it keeps the last N seconds until it's
//! turned off, also from the tray. A game that isn't open yet is waited for, and waited for
//! again when it closes. The save key (F8 by default) works inside games; "clip that" heard
//! by `voice_ui` saves too. A save goes to the Videos folder, plays the "saved" sound, and
//! says so: Neria's own toast when its window is in front, else a Windows notification, both
//! with Open in clipper.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use neria_core::capture::{self, KEEPS, Options, Prefs, Route, ScreenBuffer, Source};
use neria_core::capture_pick::{self, Monitor, Program};
use neria_core::hotkey::{self, Hotkey};
use neria_core::settings::Settings;
use neria_core::{folders, fmt, history, media};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use neria_core::capture::Role;
use neria_core::capture_sound::{self, AppSound};

use crate::{AppWindow, Capture, CaptureApp, CaptureSaved, CaptureSource, CaptureTrack, Voice};

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Off,
    Starting,
    On,
    /// The game picked isn't open: started when its window shows.
    Waiting,
    /// It stopped by itself: why, in plain words.
    Stopped(String),
}

/// What pressed save.
pub enum By {
    Key,
    Button,
    Tray,
    /// "clip that" heard; whether to play the "saved" sound (Settings → Voice).
    Voice(String, bool),
}

#[derive(Default)]
struct State {
    settings: Option<Arc<Mutex<Settings>>>,
    /// Turned on (by hand, the tray or at start), until turned off.
    wanted: bool,
    phase: Option<Phase>,
    buffer: Option<Arc<ScreenBuffer>>,
    hotkey: Option<hotkey::Listener>,
    /// Bumped by each start, so an older start that finishes late is dropped.
    started_n: u64,
    /// The route (and chip) that worked for a source and encoder ("m:0 auto"), so a restart
    /// is quick.
    route: Option<(String, Route, Option<u32>)>,
    /// The Encoder chips, in order (Automatic first).
    encoders: Vec<capture::Encoder>,
    monitors: Vec<Monitor>,
    programs: Vec<Program>,
    saving: bool,
    /// The last clip saved, for Undo / "undo that".
    last: Option<(PathBuf, Instant)>,
    toast_gen: u64,
    ticks: u64,
    /// What's being recorded now: its name ("VALORANT", "Built-in screen") and the game's
    /// file name when it's a game (Automatic moves when another game comes to the front).
    target: Option<(String, Option<String>)>,
    /// "Everything" is built app by app (two or more never recorded): new apps need a restart.
    by_app: bool,
    /// When it last started keeping, and the last save (a restart waits for a quiet moment).
    since: Option<Instant>,
    saved_at: Option<Instant>,
    /// The apps playing sound, as last asked (levels for the track list).
    apps: Vec<AppSound>,
    asking_apps: bool,
    /// Frames and lengths of saved clips, worked out once.
    probed: Vec<(PathBuf, Option<f64>, Option<PathBuf>)>,
    /// Sample screens for design checks (`NERIA_CAPTURE_DEMO`): nothing records.
    demo: bool,
    _timer: Option<Timer>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

fn prefs() -> Prefs {
    ST.with(|s| s.borrow().settings.as_ref().map(|st| st.lock().unwrap().capture.clone()).unwrap_or_default())
}

fn save_prefs(f: impl FnOnce(&mut Prefs)) {
    ST.with(|s| {
        if let Some(st) = &s.borrow().settings {
            let mut st = st.lock().unwrap();
            f(&mut st.capture);
            let _ = st.save();
        }
    });
}

fn phase() -> Phase {
    ST.with(|s| s.borrow().phase.clone().unwrap_or(Phase::Off))
}

fn set_phase(p: Phase) {
    ST.with(|s| s.borrow_mut().phase = Some(p));
}

/// Instant replay is keeping the screen now (voice listens for "clip that" then).
pub fn running() -> bool {
    phase() == Phase::On
}

/// Turned on (keeping, starting, or waiting for the game): closing the window goes to the tray.
pub fn on() -> bool {
    ST.with(|s| s.borrow().wanted)
}

/// What's recorded, for the status and the file's name: Automatic's current target, else
/// what's picked.
fn what() -> String {
    let p = prefs();
    if p.automatic() {
        return ST
            .with(|s| s.borrow().target.as_ref().map(|t| t.0.clone()))
            .unwrap_or_else(|| "the game in front".into());
    }
    p.what()
}

/// "1 min", "30 s", "5 min".
fn keep_text(secs: u32) -> String {
    if secs >= 60 && secs % 60 == 0 { format!("{} min", secs / 60) } else { format!("{secs} s") }
}

/// The plain status, also the tray's tooltip: "Keeping the last 1 min of VALORANT".
/// The save key ("F8"), for the Phone tab.
pub fn save_key() -> String {
    prefs().key
}

pub fn status_line() -> Option<String> {
    if !on() {
        return None;
    }
    let p = prefs();
    Some(match phase() {
        Phase::On => format!("Keeping the last {} of {}", keep_text(p.keep), what()),
        Phase::Starting => format!("Starting instant replay of {}", what()),
        Phase::Waiting => format!("Waiting for {} to open", what()),
        Phase::Stopped(_) => format!("Couldn't record {}", what()),
        Phase::Off => return None,
    })
}

/// The tray menu's save line: "Save the last 1 min (F8)".
pub fn save_label() -> String {
    let p = prefs();
    format!("Save the last {} ({})", keep_text(p.keep), p.key)
}

/// Something that changes the tray's menu (on / off / keeping).
pub fn tray_key() -> (bool, bool) {
    (on(), running())
}

fn source_key(s: &Source) -> String {
    match s {
        Source::Monitor(i) => format!("m:{i}"),
        Source::Window(exe) => format!("w:{}", exe.to_lowercase()),
    }
}

fn route_short(r: Route) -> &'static str {
    match r {
        Route::Nvenc => "NVIDIA",
        Route::Amf => "AMD",
        Route::Qsv => "Intel",
        Route::NvencCopy => "NVIDIA (copied)",
        Route::Cpu => "the CPU",
    }
}

pub fn install(ui: &AppWindow, settings: Arc<Mutex<Settings>>) {
    ST.with(|s| s.borrow_mut().settings = Some(settings));
    capture::clean_leftovers();
    let c = ui.global::<Capture>();
    show_prefs(ui);

    c.on_turn_on({
        let weak = ui.as_weak();
        move || turn_on(&weak.unwrap())
    });
    c.on_turn_off({
        let weak = ui.as_weak();
        move || turn_off(&weak.unwrap())
    });
    c.on_try_again({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            halt();
            begin(&ui);
        }
    });
    c.on_save_now({
        let weak = ui.as_weak();
        move || save(&weak.unwrap(), By::Button)
    });
    c.on_pick({
        let weak = ui.as_weak();
        move |id| pick(&weak.unwrap(), &id)
    });
    c.on_show_all_changed({
        let weak = ui.as_weak();
        move || show_sources(&weak.unwrap())
    });
    c.on_changed({
        let weak = ui.as_weak();
        move || changed(&weak.unwrap())
    });
    c.on_key_pick_start({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            // The key reaches Neria's window only while nobody else holds it.
            ST.with(|s| s.borrow_mut().hotkey = None);
            let c = ui.global::<Capture>();
            c.set_key_problem("".into());
            c.set_picking_key(true);
        }
    });
    c.on_key_pick_cancel({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            ui.global::<Capture>().set_picking_key(false);
            register_key(&ui);
        }
    });
    c.on_key_picked({
        let weak = ui.as_weak();
        move |text, ctrl, alt, shift, win| key_picked(&weak.unwrap(), &text, ctrl, alt, shift, win)
    });
    c.on_never_add({
        let weak = ui.as_weak();
        move |exe| {
            let exe = exe.to_string();
            save_prefs(|p| {
                if !p.is_never(&exe) {
                    p.never.push(exe.clone());
                }
            });
            never_changed(&weak.unwrap());
        }
    });
    c.on_never_remove({
        let weak = ui.as_weak();
        move |exe| {
            save_prefs(|p| p.never.retain(|n| !n.eq_ignore_ascii_case(&exe)));
            never_changed(&weak.unwrap());
        }
    });
    c.on_never_menu({
        let weak = ui.as_weak();
        move || never_choices(&weak.unwrap())
    });
    c.on_open_clip({
        let weak = ui.as_weak();
        move |path| open_in_clipper(&weak.unwrap(), Path::new(path.as_str()))
    });
    c.on_show_file(|path| crate::reveal(Path::new(path.as_str())));
    c.on_forget({
        let weak = ui.as_weak();
        move |path| {
            let path = PathBuf::from(path.as_str());
            save_prefs(|p| p.saved.retain(|f| f != &path));
            show_saved(&weak.unwrap());
        }
    });
    c.on_undo({
        let weak = ui.as_weak();
        move || {
            undo(&weak.unwrap(), None);
        }
    });
    c.on_toast_closed({
        let weak = ui.as_weak();
        move || hide_toast(&weak.unwrap())
    });
    c.on_page_shown({
        let weak = ui.as_weak();
        move |_| {
            let ui = weak.unwrap();
            refresh_sources(&ui);
            show_saved(&ui);
            list_mics(&ui);
        }
    });

    if let Ok(demo) = std::env::var("NERIA_CAPTURE_DEMO") {
        ST.with(|s| s.borrow_mut().demo = true);
        show_demo(ui, &demo);
        return;
    }

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_secs(1), {
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                tick(&ui);
            }
        }
    });
    ST.with(|s| s.borrow_mut()._timer = Some(timer));
}

/// Once ffmpeg is there: turn on if asked to at start.
pub fn after_tools(ui: &AppWindow) {
    if prefs().on_at_start && !ST.with(|s| s.borrow().demo) && !on() {
        turn_on(ui);
    }
}

fn turn_on(ui: &AppWindow) {
    if ST.with(|s| s.borrow().demo) {
        return;
    }
    ST.with(|s| s.borrow_mut().wanted = true);
    register_key(ui);
    begin(ui);
}

fn turn_off(ui: &AppWindow) {
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.wanted = false;
        s.hotkey = None;
        s.started_n += 1;
    });
    halt();
    set_phase(Phase::Off);
    show(ui);
    crate::phone_ui::take_key(ui);
}

/// Stop recording (off the UI thread: ffmpeg is stopped and its pieces deleted).
fn halt() {
    if let Some(b) = ST.with(|s| s.borrow_mut().buffer.take()) {
        std::thread::spawn(move || drop(b));
    }
}

/// Start keeping what's picked, or wait for the game to open.
fn begin(ui: &AppWindow) {
    let p = prefs();
    let monitors = capture_pick::monitors();
    // Automatic: the game in front, else the screen in use (the main one if unsure).
    let (source, label, game) = if p.automatic() {
        let f = capture_pick::front();
        match f.game {
            Some(g) => (Source::Window(g.exe.clone()), g.name.clone(), Some(g.exe)),
            None => {
                let m = f
                    .monitor
                    .and_then(|i| monitors.iter().find(|m| m.index == i))
                    .or_else(|| monitors.iter().find(|m| m.main))
                    .cloned();
                let i = m.as_ref().map(|m| m.index).unwrap_or(0);
                (Source::Monitor(i), m.map(|m| m.name).unwrap_or_else(|| "your screen".into()), None)
            }
        }
    } else {
        let source = p.source(&monitors);
        let game = match &source {
            Source::Window(exe) => Some(exe.clone()),
            Source::Monitor(_) => None,
        };
        (source, p.what(), game)
    };
    ST.with(|s| s.borrow_mut().target = Some((label.clone(), game.clone())));
    if let Source::Window(exe) = &source {
        if !capture_pick::window_open(exe) {
            set_phase(Phase::Waiting);
            show(ui);
            return;
        }
    }
    set_phase(Phase::Starting);
    show(ui);
    let key = format!("{} {}", source_key(&source), p.encoder);
    let (started_n, cached) = ST.with(|s| {
        let mut s = s.borrow_mut();
        s.started_n += 1;
        (s.started_n, s.route.as_ref().filter(|(k, _, _)| *k == key).map(|(_, r, c)| (*r, *c)))
    });
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let plan = p.sound_plan(game.as_deref().map(|g| (g, label.as_str())));
        let by_app = plan.by_app;
        let options = Options {
            source,
            fps: p.fps,
            keep: p.keep,
            mbps: p.mbps,
            route: cached.map(|c| c.0),
            chip: cached.and_then(|c| c.1),
            encoder: capture::Encoder::from_name(&p.encoder),
            sounds: plan.sounds,
            tracks: plan.tracks,
        };
        let started = ScreenBuffer::start(options);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let current = ST.with(|s| s.borrow().started_n == started_n && s.borrow().wanted);
            match started {
                Ok(b) if current => {
                    ST.with(|s| {
                        let mut s = s.borrow_mut();
                        s.route = Some((key, b.route, b.options.chip));
                        s.buffer = Some(Arc::new(b));
                        s.by_app = by_app;
                        s.since = Some(Instant::now());
                    });
                    set_phase(Phase::On);
                }
                Ok(b) => {
                    std::thread::spawn(move || drop(b));
                }
                Err(e) if current => set_phase(Phase::Stopped(explain(&e.to_string(), &prefs()))),
                Err(_) => {}
            }
            show(&ui);
        });
    });
}

/// ffmpeg's reason, in words that say what to do.
fn explain(problem: &str, _p: &Prefs) -> String {
    let what = what();
    let low = problem.to_lowercase();
    if low.contains("no window") || low.contains("window not found") {
        format!("{what}'s window isn't there. If it's in exclusive fullscreen, pick Borderless in the game, or record the monitor.")
    } else if low.contains("driver") {
        "The graphics driver is too old for Neria's ffmpeg. Update it, or Neria records on the CPU after Try again.".into()
    } else if low.contains("mic") {
        format!("{problem}. Pick another mic, or untick Mic.")
    } else if problem.trim().is_empty() {
        "It stopped by itself. Try again.".into()
    } else {
        problem.trim().to_string()
    }
}

fn tick(ui: &AppWindow) {
    let n = ST.with(|s| {
        let mut s = s.borrow_mut();
        s.ticks += 1;
        s.ticks
    });
    // The page's list of windows follows what opens and closes.
    if ui.get_page() == 3 && n % 3 == 0 && !crate::background_ui::in_tray() {
        refresh_sources(ui);
    }
    // The apps playing sound: levels for the track list, and new ones to take in.
    if (ui.get_page() == 3 && !crate::background_ui::in_tray()) || (on() && n % 3 == 0) {
        ask_apps(ui);
    }
    if !on() {
        return;
    }
    let p = prefs();
    // Automatic: another game came to the front.
    if p.automatic() && phase() == Phase::On && n % 2 == 0 {
        if let Some(g) = capture_pick::front().game {
            let now = ST.with(|s| s.borrow().target.as_ref().and_then(|t| t.1.clone()));
            if !now.is_some_and(|e| e.eq_ignore_ascii_case(&g.exe)) {
                halt();
                begin(ui);
                return;
            }
        }
    }
    match phase() {
        Phase::Waiting => {
            if p.kind == "window" && capture_pick::window_open(&p.exe) {
                begin(ui);
            }
        }
        Phase::On => {
            let stopped = ST.with(|s| s.borrow().buffer.as_ref().filter(|b| !b.running()).map(|b| b.problem.lock().unwrap().clone()));
            if let Some(problem) = stopped {
                halt();
                let game_gone = ST.with(|s| s.borrow().target.as_ref().and_then(|t| t.1.clone()))
                    .is_some_and(|exe| !capture_pick::window_open(&exe));
                if p.automatic() && game_gone {
                    // The game closed: back to the screen until the next game.
                    begin(ui);
                } else if p.kind == "window" && !capture_pick::window_open(&p.exe) {
                    // The game closed: wait for it again.
                    set_phase(Phase::Waiting);
                } else {
                    set_phase(Phase::Stopped(explain(&problem, &p)));
                }
            }
        }
        _ => {}
    }
    show(ui);
}

/// The status band from where things are.
fn show(ui: &AppWindow) {
    if ST.with(|s| s.borrow().demo) {
        return;
    }
    let c = ui.global::<Capture>();
    let p = prefs();
    let what = what();
    let keep = keep_text(p.keep);
    let ph = if on() { phase() } else { Phase::Off };
    let (state, status, detail) = match &ph {
        Phase::Off => (
            0,
            "Instant replay is off".to_string(),
            if p.automatic() {
                format!("Will keep the last {keep} of the game in front")
            } else {
                format!("Will keep the last {keep} of {what}")
            },
        ),
        Phase::Starting => (1, format!("Starting instant replay of {what}…"), "Finding the quickest way on this PC".into()),
        Phase::On => (2, format!("Keeping the last {keep} of {what}"), {
            if p.automatic() {
                let game = ST.with(|s| s.borrow().target.as_ref().is_some_and(|t| t.1.is_some()));
                let follows = if game { "Automatic: moves to the next game you open." } else { "Automatic: no game in front yet." };
                format!("{follows} {}", how_to_save(&p))
            } else {
                how_to_save(&p)
            }
        }),
        Phase::Waiting => (3, format!("Waiting for {what} to open"), "It starts by itself when the game's window shows.".into()),
        Phase::Stopped(why) => (4, format!("Couldn't record {what}"), why.clone()),
    };
    c.set_state(state);
    c.set_status(status.into());
    c.set_detail(detail.into());
    let tech = ST.with(|s| {
        let s = s.borrow();
        let b = s.buffer.as_ref().filter(|_| ph == Phase::On)?;
        let (kept, bytes) = b.kept();
        let tracks = b.options.titles().len();
        let mut parts = Vec::new();
        if (kept as u32) < b.options.keep {
            parts.push(format!("{} kept so far", fmt::duration(kept)));
        } else {
            parts.push(fmt::bytes(bytes as f64));
        }
        parts.push(format!("{} fps", b.options.fps));
        parts.push(route_short(b.route).to_string());
        parts.push(match tracks {
            0 => "no sound".into(),
            1 => "1 sound track".into(),
            n => format!("{n} sound tracks"),
        });
        Some(parts.join(" · "))
    });
    c.set_tech(tech.unwrap_or_default().into());
}

/// "Press F8 or say "clip that" to save it."
fn how_to_save(p: &Prefs) -> String {
    match voice_phrase() {
        Some(phrase) => format!("Press {} or say \"{phrase}\" to save it.", p.key),
        None => format!("Press {} to save it, even inside the game.", p.key),
    }
}

/// The phrase voice listens for while instant replay runs (voice on and its model there).
fn voice_phrase() -> Option<String> {
    let v = ST.with(|s| s.borrow().settings.as_ref().map(|st| st.lock().unwrap().voice.clone()))?;
    (v.on && neria_core::voice::ready()).then(|| v.phrases().into_iter().next()).flatten()
}

/// Settings into the page.
fn show_prefs(ui: &AppWindow) {
    let p = prefs();
    let c = ui.global::<Capture>();
    c.set_keep(KEEPS.iter().position(|k| *k == p.keep).unwrap_or(1) as i32);
    c.set_per_app(p.per_app);
    // Encoder: Automatic (saying which it is here), then this PC's chips, the CPU.
    let list = capture::encoders();
    let auto = list.first().copied().unwrap_or(capture::Encoder::Cpu);
    let mut all = vec![capture::Encoder::Auto];
    all.extend(list);
    let names: Vec<SharedString> = all
        .iter()
        .map(|e| if *e == capture::Encoder::Auto { format!("Automatic ({})", auto.label()).into() } else { e.label().into() })
        .collect();
    c.set_encoders(ModelRc::new(VecModel::from(names)));
    let picked = capture::Encoder::from_name(&p.encoder);
    c.set_encoder(all.iter().position(|e| *e == picked).unwrap_or(0) as i32);
    c.set_encoder_hint(
        match picked {
            capture::Encoder::Auto | capture::Encoder::Nvidia if auto == capture::Encoder::Nvidia => {
                "NVIDIA's encoder is the sharpest here, and it leaves the processor's power to the game."
            }
            capture::Encoder::Intel => {
                "Leaves the NVIDIA chip alone, but shares the processor's power and heat (games that lean on the processor may lose a little), and looks a little softer in fast motion."
            }
            capture::Encoder::Cpu => "Works everywhere, and costs the game the most. For when no other one works.",
            _ => "The fastest encoder this PC has.",
        }
        .into(),
    );
    ST.with(|s| s.borrow_mut().encoders = all);
    c.set_mic(p.mic);
    show_never(ui);
    c.set_key(p.key.clone().into());
    c.set_at_start(p.on_at_start);
    c.set_voice_hint(match voice_phrase() {
        Some(phrase) => format!("Saying \"{phrase}\" saves too (Settings → Voice).").into(),
        None => "".into(),
    });
    show(ui);
}

fn list_mics(ui: &AppWindow) {
    if ST.with(|s| s.borrow().demo) {
        return;
    }
    // Listing mics starts COM for many threads, which the window's own thread mustn't.
    let (weak, picked) = (ui.as_weak(), prefs().mic_name);
    std::thread::spawn(move || {
        let names = capture::mics();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let mut mics = vec![SharedString::from("Windows' default")];
            mics.extend(names.into_iter().map(SharedString::from));
            let i = mics.iter().position(|m| !picked.is_empty() && m.as_str() == picked).unwrap_or(0);
            let c = ui.global::<Capture>();
            c.set_mics(ModelRc::new(VecModel::from(mics)));
            c.set_mic_index(i as i32);
        });
    });
}

fn changed(ui: &AppWindow) {
    let c = ui.global::<Capture>();
    let mic_name = if c.get_mic_index() <= 0 {
        String::new()
    } else {
        slint::Model::row_data(&c.get_mics(), c.get_mic_index() as usize).map(|s| s.to_string()).unwrap_or_default()
    };
    let before = prefs();
    save_prefs(|p| {
        p.keep = KEEPS[c.get_keep().clamp(0, 3) as usize];
        p.per_app = c.get_per_app();
        if let Some(e) = ST.with(|s| s.borrow().encoders.get(c.get_encoder().max(0) as usize).copied()) {
            p.encoder = e.name().into();
        }
        p.mic = c.get_mic();
        p.mic_name = mic_name;
        p.on_at_start = c.get_at_start();
    });
    let after = prefs();
    // How much and which tracks are fixed while it runs: start again with them.
    let restart = after.keep != before.keep
        || after.per_app != before.per_app
        || after.encoder != before.encoder
        || after.mic != before.mic
        || after.mic_name != before.mic_name;
    if restart && on() && phase() != Phase::Waiting {
        halt();
        begin(ui);
    }
    if after.encoder != before.encoder {
        show_prefs(ui);
    }
    show(ui);
}

/// The id of what's picked: "w:<exe>" or "m:<monitor name>".
fn picked_id(p: &Prefs, monitors: &[Monitor]) -> String {
    if p.automatic() {
        return "auto".into();
    }
    if p.kind == "window" && !p.exe.is_empty() {
        return format!("w:{}", p.exe);
    }
    match p.source(monitors) {
        Source::Monitor(i) => monitors.iter().find(|m| m.index == i).map(|m| format!("m:{}", m.name)).unwrap_or_default(),
        Source::Window(exe) => format!("w:{exe}"),
    }
}

fn refresh_sources(ui: &AppWindow) {
    if ST.with(|s| s.borrow().demo) {
        return;
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let monitors = capture_pick::monitors();
        let programs = capture_pick::programs();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ST.with(|s| {
                let mut s = s.borrow_mut();
                s.monitors = monitors;
                s.programs = programs;
            });
            // Nothing picked yet: the main screen, by name.
            let p = prefs();
            if p.kind == "monitor" && p.monitor.is_empty() {
                if let Some(m) = ST.with(|s| s.borrow().monitors.iter().find(|m| m.main).cloned()) {
                    save_prefs(|p| {
                        p.monitor = m.name.clone();
                        p.monitor_index = m.index;
                        p.label = m.name.clone();
                    });
                    show(&ui);
                }
            }
            show_sources(&ui);
        });
    });
}

fn show_sources(ui: &AppWindow) {
    let c = ui.global::<Capture>();
    let all = c.get_show_all();
    let p = prefs();
    let (monitors, programs) = ST.with(|s| (s.borrow().monitors.clone(), s.borrow().programs.clone()));
    let mut rows = Vec::new();
    let now = ST.with(|s| s.borrow().target.as_ref().map(|t| t.0.clone()));
    rows.push(CaptureSource {
        id: "auto".into(),
        kind: 3,
        name: "Automatic".into(),
        hint: match (p.automatic() && on(), now) {
            (true, Some(t)) => format!("The game in front · now: {t}").into(),
            _ => "The game in front, else the screen you're using".into(),
        },
    });
    let picked_exe = (p.kind == "window").then(|| p.exe.clone()).unwrap_or_default();
    for g in &programs {
        if g.game || all || g.exe.eq_ignore_ascii_case(&picked_exe) {
            rows.push(CaptureSource {
                id: format!("w:{}", g.exe).into(),
                kind: if g.game { 0 } else { 1 },
                name: g.name.clone().into(),
                hint: g.exe.clone().into(),
            });
        }
    }
    // The game picked but not open now: still listed, it's waited for.
    if !picked_exe.is_empty() && !programs.iter().any(|g| g.exe.eq_ignore_ascii_case(&picked_exe)) {
        rows.insert(
            1,
            CaptureSource {
                id: format!("w:{picked_exe}").into(),
                kind: 0,
                name: p.what().into(),
                hint: "Not open now · starts when it opens".into(),
            },
        );
    }
    for m in &monitors {
        rows.push(CaptureSource { id: format!("m:{}", m.name).into(), kind: 2, name: m.name.clone().into(), hint: m.detail().into() });
    }
    c.set_sources(ModelRc::new(VecModel::from(rows)));
    c.set_picked(picked_id(&p, &monitors).into());
}

fn pick(ui: &AppWindow, id: &str) {
    let (monitors, programs) = ST.with(|s| (s.borrow().monitors.clone(), s.borrow().programs.clone()));
    let before = prefs();
    if id == "auto" {
        save_prefs(|p| p.kind = "auto".into());
    } else if let Some(exe) = id.strip_prefix("w:") {
        let name = programs.iter().find(|g| g.exe.eq_ignore_ascii_case(exe)).map(|g| g.name.clone());
        save_prefs(|p| {
            if !(p.kind == "window" && p.exe.eq_ignore_ascii_case(exe)) {
                p.label = name.unwrap_or_else(|| exe.trim_end_matches(".exe").to_string());
            }
            p.kind = "window".into();
            p.exe = exe.to_string();
        });
    } else if let Some(name) = id.strip_prefix("m:") {
        if let Some(m) = monitors.iter().find(|m| m.name == name) {
            save_prefs(|p| {
                p.kind = "monitor".into();
                p.exe.clear();
                p.monitor = m.name.clone();
                p.monitor_index = m.index;
                p.label = m.name.clone();
            });
        }
    }
    show_sources(ui);
    if prefs() != before && on() {
        halt();
        begin(ui);
    }
    show(ui);
}

/// Hold the save key while instant replay is on; say so when another program has it.
fn register_key(ui: &AppWindow) {
    if !on() {
        return;
    }
    // The phone holds the key while instant replay is off; one holder at a time.
    crate::phone_ui::release_key();
    let p = prefs();
    let c = ui.global::<Capture>();
    let key = match Hotkey::parse(&p.key) {
        Ok(k) => k,
        Err(e) => {
            c.set_key_problem(format!("{e}. Pick another key.").into());
            return;
        }
    };
    let weak = ui.as_weak();
    match hotkey::listen(key, move || {
        let _ = weak.upgrade_in_event_loop(|ui| {
            save(&ui, By::Key);
            // The phone mirrored at the same time: its last seconds too.
            if crate::phone_ui::mirroring() {
                crate::phone_ui::clip(&ui);
            }
        });
    }) {
        Ok(l) => {
            ST.with(|s| s.borrow_mut().hotkey = Some(l));
            c.set_key_problem("".into());
        }
        Err(_) => c.set_key_problem(
            format!("{} is already used by another program (Medal, ShadowPlay, Discord?). Pick another key.", p.key).into(),
        ),
    }
}

/// Slint's text for a key → its name for `Hotkey::parse` ("F8", "PageUp", "S").
fn key_name(text: &str) -> Option<String> {
    let c = text.chars().next()?;
    let n = c as u32;
    Some(match c {
        '\u{F704}'..='\u{F71B}' => format!("F{}", n - 0xF704 + 1),
        '\u{F727}' => "Insert".into(),
        '\u{7f}' => "Delete".into(),
        '\u{F729}' => "Home".into(),
        '\u{F72B}' => "End".into(),
        '\u{F72C}' => "PageUp".into(),
        '\u{F72D}' => "PageDown".into(),
        '\u{F72F}' => "ScrollLock".into(),
        '\u{F730}' => "Pause".into(),
        '\u{F731}' => "PrintScreen".into(),
        c if c.is_ascii_alphanumeric() => c.to_ascii_uppercase().to_string(),
        _ => return None,
    })
}

/// "Ctrl+Alt+F10".
fn key_text(name: &str, ctrl: bool, alt: bool, shift: bool, win: bool) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if ctrl {
        parts.push("Ctrl");
    }
    if alt {
        parts.push("Alt");
    }
    if shift {
        parts.push("Shift");
    }
    if win {
        parts.push("Win");
    }
    parts.push(name);
    parts.join("+")
}

fn key_picked(ui: &AppWindow, text: &str, ctrl: bool, alt: bool, shift: bool, win: bool) {
    let c = ui.global::<Capture>();
    c.set_picking_key(false);
    let Some(name) = key_name(text) else {
        c.set_key_problem("That key can't be the save key. Try F1–F12, or a letter with Ctrl or Alt.".into());
        register_key(ui);
        return;
    };
    // A bare letter or number would stop it being typed anywhere else.
    if name.len() == 1 && !(ctrl || alt || win) {
        c.set_key_problem(format!("{name} alone couldn't be typed anywhere else then. Add Ctrl or Alt, or pick an F key.").into());
        register_key(ui);
        return;
    }
    let text = key_text(&name, ctrl, alt, shift, win);
    let Ok(key) = Hotkey::parse(&text) else {
        c.set_key_problem("That key can't be the save key. Try F1–F12.".into());
        register_key(ui);
        return;
    };
    // Free? Held for a moment to find out.
    match hotkey::listen(key, || {}) {
        Ok(l) => drop(l),
        Err(_) => {
            c.set_key_problem(
                format!("{text} is already used by another program (Medal, ShadowPlay, Discord?). Pick another key.").into(),
            );
            register_key(ui);
            return;
        }
    }
    save_prefs(|p| p.key = text.clone());
    c.set_key(text.into());
    c.set_key_problem("".into());
    register_key(ui);
    show(ui);
}

/// Save what's kept now: the key, the button, the tray or "clip that".
pub fn save(ui: &AppWindow, by: By) {
    let p = prefs();
    let heard = match &by {
        By::Voice(phrase, _) => Some(format!("Heard \"{}\"", phrase.to_lowercase())),
        _ => None,
    };
    let buffer = ST.with(|s| s.borrow().buffer.clone()).filter(|_| phase() == Phase::On);
    let Some(buffer) = buffer else {
        toast(ui, heard.as_deref().unwrap_or("Not yet"), "Nothing kept to save yet", false, None);
        return;
    };
    if ST.with(|s| std::mem::replace(&mut s.borrow_mut().saving, true)) {
        return;
    }
    match &by {
        By::Key => hotkey::confirm_sound(),
        By::Voice(_, true) => hotkey::confirm_sound(),
        _ => {}
    }
    let settings = ST.with(|s| s.borrow().settings.clone());
    let out_dir = settings
        .map(|s| folders::for_kind(&s.lock().unwrap(), folders::Kind::Video))
        .unwrap_or_else(|| folders::for_kind(&Settings::load(), folders::Kind::Video));
    let what = what();
    let name = format!("{what} {}", chrono::Local::now().format("%Y-%m-%d %H-%M"));
    ST.with(|s| s.borrow_mut().saved_at = Some(Instant::now()));
    let secs = p.keep;
    let by_voice = heard.is_some();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let titles = buffer.options.titles();
        let saved = capture::save_from(&buffer.dir, secs, &out_dir, &titles, &name);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ST.with(|s| s.borrow_mut().saving = false);
            match saved {
                Ok(file) => {
                    let text = format!("Saved the last {} of {what}", keep_text(secs));
                    saved_file(&ui, &file, &name, heard.as_deref().unwrap_or("Saved"), &text, by_voice);
                }
                Err(e) => toast(&ui, heard.as_deref().unwrap_or("Not saved"), &e.to_string(), false, None),
            }
        });
    });
}

/// Something saved from the Capture page (instant replay, the phone): in Saved, in the
/// history, the toast with Open in clipper (and Undo), a notification when Neria is behind.
pub fn saved_file(ui: &AppWindow, file: &Path, name: &str, tag: &str, text: &str, undo: bool) {
    save_prefs(|p| p.add_saved(file.to_path_buf()));
    ST.with(|s| s.borrow_mut().last = Some((file.to_path_buf(), Instant::now())));
    history::record(history::Record {
        key: String::new(),
        url: String::new(),
        file: file.to_path_buf(),
        title: name.to_string(),
        at: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
    });
    toast(ui, tag, text, undo, Some(file));
    crate::background_ui::notify_saved(ui, text, file);
    show_saved(ui);
}

/// Undo the clip just saved (its toast, or "undo that" within 3 minutes): the file goes.
/// False when there's nothing recent to undo.
pub fn undo(ui: &AppWindow, heard: Option<&str>) -> bool {
    let last = ST.with(|s| s.borrow_mut().last.take());
    let Some((file, at)) = last.filter(|(_, at)| at.elapsed() < Duration::from_secs(180)) else {
        return false;
    };
    let _ = at;
    let _ = std::fs::remove_file(&file);
    save_prefs(|p| p.saved.retain(|f| f != &file));
    show_saved(ui);
    toast(ui, heard.unwrap_or("Undone"), "That clip is deleted", false, None);
    true
}

/// Whether there's a clip saved in the last 3 minutes ("undo that" goes to it).
pub fn can_undo() -> bool {
    ST.with(|s| s.borrow().last.as_ref().is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(180)))
}

pub fn toast(ui: &AppWindow, tag: &str, text: &str, undo: bool, file: Option<&Path>) {
    let c = ui.global::<Capture>();
    c.set_toast_tag(tag.into());
    c.set_toast_text(text.into());
    c.set_toast_undo(undo);
    c.set_toast_file(file.map(|f| f.to_string_lossy().into_owned()).unwrap_or_default().into());
    let n = ST.with(|s| {
        let mut s = s.borrow_mut();
        s.toast_gen += 1;
        s.toast_gen
    });
    let weak = ui.as_weak();
    Timer::single_shot(Duration::from_secs(8), move || {
        if ST.with(|s| s.borrow().toast_gen) == n {
            if let Some(ui) = weak.upgrade() {
                hide_toast(&ui);
            }
        }
    });
}

fn hide_toast(ui: &AppWindow) {
    let c = ui.global::<Capture>();
    c.set_toast_text("".into());
    c.set_toast_tag("".into());
    c.set_toast_undo(false);
    c.set_toast_file("".into());
}

/// Open a saved clip in the clipper (bringing the window back from the tray).
pub fn open_in_clipper(ui: &AppWindow, file: &Path) {
    if crate::background_ui::in_tray() {
        crate::background_ui::show_window(ui);
    }
    hide_toast(ui);
    ui.set_page(1);
    ui.invoke_clip_open_path(file.to_string_lossy().as_ref().into());
}

/// "4 min ago".
fn age(t: SystemTime) -> String {
    let s = t.elapsed().map_or(0, |d| d.as_secs());
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// The Saved list: each clip still there, with its length, size, age and a frame.
pub fn show_saved(ui: &AppWindow) {
    if ST.with(|s| s.borrow().demo) {
        return;
    }
    let files: Vec<PathBuf> = prefs().saved.into_iter().filter(|f| f.is_file()).collect();
    let known = ST.with(|s| s.borrow().probed.clone());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut rows = Vec::new();
        for f in files {
            let (dur, frame) = match known.iter().find(|(k, _, _)| *k == f) {
                Some((_, d, fr)) => (*d, fr.clone()),
                None => {
                    let info = media::file_info(&f).ok();
                    let frame = info
                        .as_ref()
                        .filter(|i| i.has_video)
                        .and_then(|i| media::thumbnails(&f, i.duration * 0.5, 1, 108).into_iter().next());
                    (info.map(|i| i.duration), frame)
                }
            };
            let meta = std::fs::metadata(&f).ok();
            rows.push((f, dur, frame, meta.as_ref().map(|m| m.len()), meta.and_then(|m| m.modified().ok())));
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let mut items = Vec::new();
            for (f, dur, frame, size, when) in rows {
                ST.with(|s| {
                    let mut s = s.borrow_mut();
                    if !s.probed.iter().any(|(k, _, _)| *k == f) {
                        s.probed.push((f.clone(), dur, frame.clone()));
                    }
                });
                let mut detail = Vec::new();
                if let Some(d) = dur {
                    detail.push(fmt::duration(d));
                }
                if let Some(b) = size {
                    detail.push(fmt::bytes(b as f64));
                }
                if let Some(t) = when {
                    detail.push(age(t));
                }
                items.push(CaptureSaved {
                    path: f.to_string_lossy().as_ref().into(),
                    title: f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default().into(),
                    detail: detail.join(" · ").into(),
                    thumb: frame.and_then(|p| slint::Image::load_from_path(&p).ok()).unwrap_or_default(),
                });
            }
            ui.global::<Capture>().set_saved(ModelRc::new(VecModel::from(items)));
        });
    });
}

/// A name for an app's file name: its window's title when it's a game, else "Spotify".
fn app_label(exe: &str) -> String {
    let progs = ST.with(|s| s.borrow().programs.clone());
    progs
        .iter()
        .find(|g| g.game && g.exe.eq_ignore_ascii_case(exe))
        .map(|g| g.name.clone())
        .unwrap_or_else(|| capture_sound::app_name(exe))
}

fn show_never(ui: &AppWindow) {
    let rows: Vec<CaptureApp> =
        prefs().never.iter().map(|e| CaptureApp { exe: e.into(), name: app_label(e).into() }).collect();
    ui.global::<Capture>().set_never(ModelRc::new(VecModel::from(rows)));
}

/// "+ Add an app": apps playing sound, then programs open now, not already never recorded.
fn never_choices(ui: &AppWindow) {
    let p = prefs();
    let (apps, progs) = ST.with(|s| (s.borrow().apps.clone(), s.borrow().programs.clone()));
    let mut rows: Vec<CaptureApp> = Vec::new();
    let mut add = |exe: &str, name: String| {
        if !p.is_never(exe) && !rows.iter().any(|r| r.exe.as_str().eq_ignore_ascii_case(exe)) {
            rows.push(CaptureApp { exe: exe.into(), name: name.into() });
        }
    };
    for a in &apps {
        add(&a.exe, app_label(&a.exe));
    }
    for g in &progs {
        add(&g.exe, if g.game { g.name.clone() } else { capture_sound::app_name(&g.exe) });
    }
    ui.global::<Capture>().set_never_choices(ModelRc::new(VecModel::from(rows)));
}

/// Never record changed: the list, the tracks, and a restart if it's on (tracks are fixed
/// while recording).
fn never_changed(ui: &AppWindow) {
    show_never(ui);
    show_tracks(ui);
    if on() && phase() != Phase::Waiting {
        halt();
        begin(ui);
    }
}

/// Ask Windows which apps play sound (off the window's thread: COM), then show the tracks,
/// and take in a new app when "Everything" is built app by app.
fn ask_apps(ui: &AppWindow) {
    if ST.with(|s| s.borrow().demo || std::mem::replace(&mut s.borrow_mut().asking_apps, true)) {
        return;
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let apps = capture_sound::playing_apps();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ST.with(|s| {
                let mut s = s.borrow_mut();
                s.asking_apps = false;
                s.apps = apps;
            });
            show_tracks(&ui);
            take_in_new_apps(&ui);
        });
    });
}

/// Apps playing sound that the running recording has no track for (not never recorded).
fn new_apps() -> Vec<AppSound> {
    let p = prefs();
    ST.with(|s| {
        let s = s.borrow();
        let Some(b) = s.buffer.as_ref() else { return Vec::new() };
        s.apps
            .iter()
            .filter(|a| !p.is_never(&a.exe))
            .filter(|a| !b.options.sounds.iter().any(|x| matches!(x, capture::Sound::App(e) if e.eq_ignore_ascii_case(&a.exe))))
            .cloned()
            .collect()
    })
}

/// Two or more never recorded: an app that started later isn't heard at all, so restart at a
/// quiet moment (kept at least 20 s, nothing saved in the last 10 s).
fn take_in_new_apps(ui: &AppWindow) {
    let (by_app, since, saved_at) = ST.with(|s| (s.borrow().by_app, s.borrow().since, s.borrow().saved_at));
    if !by_app || phase() != Phase::On || new_apps().is_empty() {
        return;
    }
    let quiet = since.is_some_and(|t| t.elapsed() > Duration::from_secs(20))
        && saved_at.is_none_or(|t| t.elapsed() > Duration::from_secs(10))
        && !ST.with(|s| s.borrow().saving);
    if quiet {
        halt();
        begin(ui);
    }
}

/// The tracks the next save holds (while on), or would hold, with each app's level.
fn show_tracks(ui: &AppWindow) {
    if ST.with(|s| s.borrow().demo) {
        return;
    }
    let p = prefs();
    let (apps, buffer, target) =
        ST.with(|s| (s.borrow().apps.clone(), s.borrow().buffer.clone(), s.borrow().target.clone()));
    let level = |exe: &str| apps.iter().find(|a| a.exe.eq_ignore_ascii_case(exe)).map(|a| a.peak).unwrap_or(0.0);
    let (sounds, tracks) = match buffer.as_ref().filter(|_| phase() == Phase::On) {
        Some(b) => (b.options.sounds.clone(), b.options.tracks.clone()),
        None => {
            let game = target.as_ref().and_then(|t| t.1.clone().map(|e| (e, t.0.clone())));
            let plan = p.plan_from(game.as_ref().map(|(e, n)| (e.as_str(), n.as_str())), &apps);
            (plan.sounds, plan.tracks)
        }
    };
    let game_exe = target.as_ref().and_then(|t| t.1.clone()).unwrap_or_default();
    let mut rows = Vec::new();
    let everything = apps.iter().filter(|a| !p.is_never(&a.exe)).map(|a| a.peak).fold(0f32, f32::max);
    let titles = Options { sounds: sounds.clone(), tracks: tracks.clone(), ..Options::default() }.titles();
    if titles.first().is_some_and(|t| t == "Everything") {
        rows.push(CaptureTrack {
            n: 1,
            name: "Everything".into(),
            kind: 0,
            note: "mixed, what players play".into(),
            level: everything,
            exe: "".into(),
        });
    }
    for (i, snd) in sounds.iter().enumerate() {
        let role = tracks.get(i).map(|t| t.role).unwrap_or(Role::Both);
        if role == Role::MixOnly {
            continue;
        }
        let name = tracks.get(i).map(|t| t.name.clone()).unwrap_or_else(|| snd.title());
        let (kind, exe, lvl) = match snd {
            capture::Sound::App(e) => (if e.eq_ignore_ascii_case(&game_exe) { 1 } else { 2 }, e.clone(), level(e)),
            capture::Sound::Mic(_) => (3, String::new(), 0.0),
            _ => (2, String::new(), 0.0),
        };
        let name = if kind == 2 { app_label(&exe) } else { name };
        rows.push(CaptureTrack { n: rows.len() as i32 + 1, name: name.into(), kind, note: "".into(), level: lvl, exe: exe.into() });
    }
    let c = ui.global::<Capture>();
    c.set_tracks(ModelRc::new(VecModel::from(rows)));
    let later: Vec<String> = if phase() == Phase::On { new_apps().iter().map(|a| app_label(&a.exe)).collect() } else { Vec::new() };
    let by_app = ST.with(|s| s.borrow().by_app);
    c.set_tracks_note(match (later.is_empty(), by_app) {
        (true, _) => "".into(),
        (false, false) => format!(
            "{} started later: it's in Everything, and gets a track of its own the next time instant replay starts.",
            later.join(", ")
        )
        .into(),
        (false, true) => format!("{} started later: Neria takes it in at the next quiet moment.", later.join(", ")).into(),
    });
}

/// The tray menu's Capture lines: (id, text, enabled).
pub fn tray_items() -> Vec<(&'static str, String, bool)> {
    if !on() {
        return vec![("cap-on", "Turn on instant replay".into(), true)];
    }
    let mut v = vec![("cap-status", status_line().unwrap_or_default(), false)];
    if running() {
        v.push(("cap-save", save_label(), true));
    }
    v.push(("cap-off", "Turn off instant replay".into(), true));
    v
}

pub fn tray_action(ui: &AppWindow, id: &str) {
    match id {
        "cap-on" => turn_on(ui),
        "cap-off" => turn_off(ui),
        "cap-save" => save(ui, By::Tray),
        _ => {}
    }
}

/// `NERIA_CAPTURE_DEMO`: sample screens, nothing recording. `off`, `on`, `waiting`, `stopped`,
/// `toast` (on, a clip just saved by voice), `key` (picking the save key, one in use).
fn show_demo(ui: &AppWindow, what: &str) {
    let c = ui.global::<Capture>();
    let src = |id: &str, kind: i32, name: &str, hint: &str| CaptureSource {
        id: id.into(),
        kind,
        name: name.into(),
        hint: hint.into(),
    };
    c.set_sources(ModelRc::new(VecModel::from(vec![
        src("auto", 3, "Automatic", "The game in front · now: VALORANT"),
        src("w:VALORANT-Win64-Shipping.exe", 0, "VALORANT", "VALORANT-Win64-Shipping.exe"),
        src("w:r5apex.exe", 0, "Apex Legends", "r5apex.exe"),
        src("m:DELL U2720Q", 2, "DELL U2720Q", "Main screen · 2560 × 1440"),
        src("m:LG 24MP59G", 2, "LG 24MP59G", "Another screen · 1920 × 1080"),
    ])));
    c.set_picked("auto".into());
    c.set_mics(ModelRc::new(VecModel::from(vec![
        SharedString::from("Windows' default"),
        "Microphone (2- fifine Microphone)".into(),
    ])));
    c.set_mic_index(1);
    c.set_key("F8".into());
    c.set_encoders(ModelRc::new(VecModel::from(vec![
        SharedString::from("Automatic (NVIDIA)"),
        "NVIDIA".into(),
        "Intel".into(),
        "CPU".into(),
    ])));
    c.set_encoder_hint("NVIDIA's encoder is the sharpest here, and it leaves the processor's power to the game.".into());
    let tr = |n: i32, name: &str, kind: i32, note: &str, level: f32, exe: &str| CaptureTrack {
        n,
        name: name.into(),
        kind,
        note: note.into(),
        level,
        exe: exe.into(),
    };
    c.set_tracks(ModelRc::new(VecModel::from(vec![
        tr(1, "Everything", 0, "mixed, what players play", 0.4, ""),
        tr(2, "VALORANT", 1, "", 0.35, "VALORANT-Win64-Shipping.exe"),
        tr(3, "Discord", 2, "", 0.2, "Discord.exe"),
        tr(4, "Brave", 2, "", 0.0, "brave.exe"),
        tr(5, "Mic", 3, "", 0.0, ""),
    ])));
    c.set_never(ModelRc::new(VecModel::from(vec![CaptureApp { exe: "Spotify.exe".into(), name: "Spotify".into() }])));
    let saved = vec![
        CaptureSaved {
            path: "C:\\Videos\\VALORANT 2026-11-21 21-04 (last 1m00s).mp4".into(),
            title: "VALORANT 2026-11-21 21-04 (last 1m00s)".into(),
            detail: "1:00 · 118 MB · 2 min ago".into(),
            thumb: Default::default(),
        },
        CaptureSaved {
            path: "C:\\Videos\\VALORANT 2026-11-21 20-51 (last 1m00s).mp4".into(),
            title: "VALORANT 2026-11-21 20-51 (last 1m00s)".into(),
            detail: "1:00 · 121 MB · 15 min ago".into(),
            thumb: Default::default(),
        },
    ];
    let on = |c: &Capture| {
        c.set_state(2);
        c.set_status("Keeping the last 1 min of VALORANT".into());
        c.set_detail("Press F8 or say \"clip that\" to save it.".into());
        c.set_tech("1.2 GB · 60 fps · NVIDIA · 3 sound tracks".into());
        c.set_voice_hint("Saying \"clip that\" saves too (Settings → Voice).".into());
    };
    let v = ui.global::<Voice>();
    match what {
        "on" | "toast" => {
            on(&c);
            c.set_saved(ModelRc::new(VecModel::from(saved)));
            v.set_listening(true);
            v.set_level(0.4);
            v.set_listen_text("Listening for \"clip that\"".into());
            if what == "toast" {
                c.set_toast_tag("Heard \"clip that\"".into());
                c.set_toast_text("Saved the last 1 min of VALORANT".into());
                c.set_toast_undo(true);
                c.set_toast_file("C:\\Videos\\VALORANT.mp4".into());
            }
        }
        "waiting" => {
            c.set_state(3);
            c.set_status("Waiting for VALORANT to open".into());
            c.set_detail("It starts by itself when the game's window shows.".into());
            c.set_sources(ModelRc::new(VecModel::from(vec![
                src("w:VALORANT-Win64-Shipping.exe", 0, "VALORANT", "Not open now · starts when it opens"),
                src("m:DELL U2720Q", 2, "DELL U2720Q", "Main screen · 2560 × 1440"),
            ])));
        }
        "stopped" => {
            c.set_state(4);
            c.set_status("Couldn't record VALORANT".into());
            c.set_detail("VALORANT's window isn't there. If it's in exclusive fullscreen, pick Borderless in the game, or record the monitor.".into());
        }
        "key" => {
            on(&c);
            c.set_picking_key(true);
            c.set_key_problem("Alt+F10 is already used by another program (Medal, ShadowPlay, Discord?). Pick another key.".into());
        }
        _ => {
            c.set_state(0);
            c.set_status("Instant replay is off".into());
            c.set_detail("Will keep the last 1 min of VALORANT".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_from_slint() {
        assert_eq!(key_name("\u{F70B}").as_deref(), Some("F8"));
        assert_eq!(key_name("\u{F71B}").as_deref(), Some("F24"));
        assert_eq!(key_name("s").as_deref(), Some("S"));
        assert_eq!(key_name("\u{F72C}").as_deref(), Some("PageUp"));
        assert_eq!(key_name("\u{1b}"), None);
        assert_eq!(key_text("F10", false, true, false, false), "Alt+F10");
        for k in ["F8", "Alt+F10", "Ctrl+Shift+S", "PageUp", "PrintScreen", "ScrollLock", "Pause", "Insert"] {
            assert!(Hotkey::parse(k).is_ok(), "{k}");
        }
    }

    #[test]
    fn keep_words() {
        assert_eq!(keep_text(30), "30 s");
        assert_eq!(keep_text(60), "1 min");
        assert_eq!(keep_text(300), "5 min");
    }
}
