//! Speech to text (optional): the one Transcribe dialog, opened from the Clip page
//! (Export ▾ → Transcribe…), a dropped file and the Download page (Extras → Transcript), and
//! Settings → Download → Speech to text. Getting it (a one-time download) happens in the
//! dialog too, the first time. Core: `neria_core::transcribe`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use neria_core::settings::Settings;
use neria_core::transcribe::{self, Output, Spec, Stage};
use neria_core::{fmt, tools};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{AppWindow, Extras, Transcribe};

/// What the dialog is for.
#[derive(Clone)]
enum Target {
    /// Settings → Get it…: nothing to transcribe yet.
    None,
    /// Dropped files (one or several), or the Clip page's file (with its selection).
    Files { paths: Vec<PathBuf>, selection: Option<(f64, f64)> },
    /// The Clip page on a link: `enqueue(spec, selection?)` downloads it, then transcribes.
    Link(Rc<dyn Fn(Spec, bool)>),
    /// The Download page's previewed video (Extras → Transcript).
    Download,
}

struct State {
    target: Target,
    /// Getting it or transcribing: Stop sets this.
    cancel: Option<Arc<AtomicBool>>,
    /// Extras → Transcript is on for the previewed video, with these choices.
    download: Option<Spec>,
    /// The last files saved, for Open it / Show in folder.
    saved: Vec<PathBuf>,
}

thread_local! {
    static STATE: RefCell<State> =
        RefCell::new(State { target: Target::None, cancel: None, download: None, saved: Vec::new() });
}

const VIEW_GET: i32 = 0;
const VIEW_OPTIONS: i32 = 1;
const VIEW_WORKING: i32 = 2;
const VIEW_DONE: i32 = 3;
const VIEW_FAILED: i32 = 4;

/// Picked in the dialog → the core's choices.
fn spec(ui: &AppWindow) -> Spec {
    let t = ui.global::<Transcribe>();
    let lang = t.get_language();
    Spec {
        language: (lang > 0)
            .then(|| transcribe::LANGUAGES.get(lang as usize - 1).map(|(c, _)| c.to_string()))
            .flatten(),
        output: Output::from_index(t.get_output()),
        times: t.get_times(),
        translate: t.get_translate(),
        burn: t.get_can_burn() && t.get_burn(),
        model: transcribe::Model::from_index(t.get_model()),
    }
}

/// The core's choices → the dialog.
fn show_spec(ui: &AppWindow, s: &Spec) {
    let t = ui.global::<Transcribe>();
    let lang = s
        .language
        .as_deref()
        .and_then(|l| transcribe::LANGUAGES.iter().position(|(c, _)| *c == l))
        .map(|i| i as i32 + 1)
        .unwrap_or(0);
    t.set_language(lang);
    t.set_output(s.output.index());
    t.set_times(s.times);
    t.set_burn(s.burn);
    t.set_model(s.model.index());
    t.set_translate(s.translate);
}

/// Settings → Download → Speech to text, and what Get would download.
pub fn refresh(ui: &AppWindow) {
    let t = ui.global::<Transcribe>();
    let st = transcribe::status();
    let nvidia = transcribe::has_nvidia();
    t.set_ready(st.ready());
    t.set_on_gpu(st.gpu);
    t.set_nvidia(nvidia);
    t.set_settings_text(
        if st.ready() {
            format!(
                "Ready{} · {} on this PC",
                if st.gpu { " · uses your NVIDIA card" } else { "" },
                fmt::bytes(transcribe::disk_bytes() as f64)
            )
        } else {
            "Not set up: a one-time download the first time you transcribe something.".to_string()
        }
        .into(),
    );
    t.set_best_ready(st.best);
    let best = transcribe::dir().join(transcribe::Model::Best.file());
    let bytes = best.metadata().map(|m| m.len()).unwrap_or(transcribe::Model::Best.mb() * 1_000_000);
    t.set_best_size(fmt::bytes(bytes as f64).into());
    update_size(ui);
}

/// "Get it · 575 MB": what's still missing, for the pack the tick picks.
fn update_size(ui: &AppWindow) {
    let t = ui.global::<Transcribe>();
    let st = transcribe::status();
    let gpu = t.get_nvidia() && t.get_use_gpu();
    let mut mb = 0;
    if !st.engine || st.gpu != gpu {
        mb += if gpu { transcribe::GPU_MB } else { transcribe::CPU_MB };
    }
    if !st.model {
        mb += transcribe::MODEL_MB;
    }
    t.set_get_size(if mb == 0 { String::new() } else { fmt::bytes(mb as f64 * 1_000_000.0) }.into());
}

fn open_dialog(ui: &AppWindow, target: Target, subtitle: String) {
    let t = ui.global::<Transcribe>();
    let busy = STATE.with(|s| s.borrow().cancel.is_some());
    if busy {
        // One at a time: show the one going.
        t.set_open(true);
        crate::show_toast(ui, "Neria is already transcribing something. It'll be done first.");
        return;
    }
    refresh(ui);
    STATE.with(|s| s.borrow_mut().target = target.clone());
    t.set_for_download(matches!(target, Target::Download));
    t.set_is_link(matches!(target, Target::Link(_)));
    // Captions burned into the picture: one video (a link or a download is one too).
    t.set_can_burn(match &target {
        Target::Files { paths, .. } => {
            paths.len() == 1 && neria_core::media::file_info(&paths[0]).is_ok_and(|i| i.width > 0)
        }
        Target::Link(_) | Target::Download => true,
        _ => false,
    });
    t.set_subtitle(subtitle.into());
    t.set_error("".into());
    t.set_getting(false);
    t.set_view(if transcribe::ready() && !matches!(target, Target::None) { VIEW_OPTIONS } else { VIEW_GET });
    t.set_open(true);
}

/// Dropped files (several are joined, or one each). In the order people number them.
pub fn open_files(ui: &AppWindow, paths: Vec<PathBuf>) {
    let paths = transcribe::in_order(paths);
    let t = ui.global::<Transcribe>();
    let name = |p: &PathBuf| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let subtitle = match paths.as_slice() {
        [one] => match neria_core::media::duration(one) {
            Some(d) => format!("{} · {}", name(one), transcribe::clock(d)),
            None => name(one),
        },
        many => format!("{} files", many.len()),
    };
    t.set_files(paths.len() as i32);
    t.set_files_text(
        paths.iter().enumerate().map(|(i, p)| format!("{}. {}", i + 1, name(p))).collect::<Vec<_>>().join("\n").into(),
    );
    t.set_together(0);
    t.set_has_selection(false);
    open_dialog(ui, Target::Files { paths, selection: None }, subtitle);
}

/// Clip page on a file: the selection, or the whole file.
pub fn open_clip_file(ui: &AppWindow, path: PathBuf, selection: (f64, f64), duration: f64) {
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    clip_choices(ui, selection, duration);
    let sel = has_part(selection, duration).then_some(selection);
    open_dialog(ui, Target::Files { paths: vec![path], selection: sel }, name);
}

/// Clip page on a link: downloaded first (the selection, or all of it), then transcribed.
pub fn open_clip_link(
    ui: &AppWindow,
    title: String,
    selection: (f64, f64),
    duration: f64,
    enqueue: Rc<dyn Fn(Spec, bool)>,
) {
    clip_choices(ui, selection, duration);
    open_dialog(ui, Target::Link(enqueue), title);
}

/// A selection that's more than the whole thing (with a little slack at the ends).
fn has_part((a, b): (f64, f64), duration: f64) -> bool {
    b > a && (a > 0.5 || (duration > 0.0 && b < duration - 0.5))
}

fn clip_choices(ui: &AppWindow, selection: (f64, f64), duration: f64) {
    let t = ui.global::<Transcribe>();
    let part = has_part(selection, duration);
    t.set_files(1);
    t.set_has_selection(part);
    t.set_part(0);
    t.set_selection_text(
        format!("The selection · {}–{}", transcribe::clock(selection.0), transcribe::clock(selection.1)).into(),
    );
}

/// The Download page previewed something new (or nothing): Transcript is off again.
pub fn reset_download(ui: &AppWindow) {
    STATE.with(|s| s.borrow_mut().download = None);
    show_download(ui);
}

/// Extras → Transcript's button.
fn show_download(ui: &AppWindow) {
    let ex = ui.global::<Extras>();
    let d = STATE.with(|s| s.borrow().download.clone());
    ex.set_transcript_on(d.is_some());
    ex.set_transcript_text(
        match d.map(|d| d.output) {
            None => "Transcript",
            Some(Output::Text) => "Transcript: text",
            Some(Output::Captions) => "Transcript: captions",
            Some(Output::Both) => "Transcript: text + captions",
        }
        .into(),
    );
}

/// The choices for the previewed download, when Transcript is on.
pub fn download_spec() -> Option<Spec> {
    STATE.with(|s| s.borrow().download.clone())
}

fn remember(ui: &AppWindow, settings: &Arc<Mutex<Settings>>) {
    let mut s = settings.lock().unwrap();
    s.transcribe = spec(ui);
    let _ = s.save();
}

pub fn install(ui: &AppWindow, settings: Arc<Mutex<Settings>>) {
    let t = ui.global::<Transcribe>();
    t.set_languages(ModelRc::new(VecModel::from(
        std::iter::once("Detect".to_string())
            .chain(transcribe::LANGUAGES.iter().map(|(_, n)| n.to_string()))
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    )));
    show_spec(ui, &settings.lock().unwrap().transcribe);
    refresh(ui);
    show_download(ui);

    t.on_changed({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move || remember(&weak.unwrap(), &settings)
    });

    t.on_open_get({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            open_dialog(&ui, Target::None, String::new());
            ui.global::<Transcribe>().set_view(VIEW_GET);
            update_size(&ui);
        }
    });

    t.on_open_for_download({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let t = ui.global::<Transcribe>();
            if let Some(d) = download_spec() {
                show_spec(&ui, &d);
            }
            t.set_download_on(download_spec().is_some());
            t.set_has_selection(false);
            t.set_files(1);
            open_dialog(&ui, Target::Download, ui.get_probe_title().to_string());
        }
    });

    t.on_set_download({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move |on| {
            let ui = weak.unwrap();
            let s = on.then(|| spec(&ui));
            STATE.with(|st| st.borrow_mut().download = s);
            if on {
                remember(&ui, &settings);
            }
            show_download(&ui);
            ui.global::<Transcribe>().set_open(false);
        }
    });

    t.on_remove({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            if STATE.with(|s| s.borrow().cancel.is_some()) {
                crate::show_toast(&ui, "Speech to text is busy. Remove it once it's done.");
                return;
            }
            match transcribe::remove() {
                Ok(()) => crate::show_toast(&ui, "Speech to text removed from this PC."),
                Err(e) => crate::show_toast(&ui, &format!("Couldn't remove it all: {e}")),
            }
            reset_download(&ui);
            refresh(&ui);
        }
    });

    t.on_remove_best({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move || {
            let ui = weak.unwrap();
            if STATE.with(|s| s.borrow().cancel.is_some()) {
                crate::show_toast(&ui, "Speech to text is busy. Remove it once it's done.");
                return;
            }
            match transcribe::remove_model(transcribe::Model::Best) {
                Ok(()) => crate::show_toast(&ui, "The Best quality model is off this PC. Fast stays."),
                Err(e) => crate::show_toast(&ui, &format!("Couldn't remove it: {e}")),
            }
            // Back to Fast, which is still there.
            let t = ui.global::<Transcribe>();
            t.set_model(0);
            t.set_translate(false);
            remember(&ui, &settings);
            refresh(&ui);
        }
    });

    t.on_stop(|| {
        STATE.with(|s| {
            if let Some(c) = &s.borrow().cancel {
                c.store(true, Ordering::SeqCst);
            }
        });
    });

    t.on_get({
        let weak = ui.as_weak();
        move || get(&weak.unwrap())
    });

    t.on_start({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move || {
            let ui = weak.unwrap();
            remember(&ui, &settings);
            start(&ui);
        }
    });

    // Dev hooks: `NERIA_TRANSCRIBE` (files, `;` between) opens them in the dialog as if
    // dropped; `NERIA_TRANSCRIBE_START` presses Transcribe too.
    if let Ok(list) = std::env::var("NERIA_TRANSCRIBE") {
        let files: Vec<PathBuf> = list.split(';').filter(|s| !s.trim().is_empty()).map(PathBuf::from).collect();
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(800), move || {
            if let Some(ui) = weak.upgrade() {
                open_files(&ui, files);
                if std::env::var_os("NERIA_TRANSCRIBE_START").is_some() && transcribe::ready() {
                    start(&ui);
                }
            }
        });
    }

    t.on_open_result(|| {
        if let Some(f) = STATE.with(|s| s.borrow().saved.first().cloned()) {
            let _ = open::that(f);
        }
    });
    t.on_show_result(|| {
        if let Some(f) = STATE.with(|s| s.borrow().saved.first().cloned()) {
            crate::reveal(&f);
        }
    });
}

/// Get it: the engine (NVIDIA pack when ticked), the model, voice detection.
fn get(ui: &AppWindow) {
    if STATE.with(|s| s.borrow().cancel.is_some()) {
        return;
    }
    let t = ui.global::<Transcribe>();
    let gpu = t.get_nvidia() && t.get_use_gpu();
    let cancel = Arc::new(AtomicBool::new(false));
    STATE.with(|s| s.borrow_mut().cancel = Some(cancel.clone()));
    t.set_getting(true);
    t.set_error("".into());
    t.set_progress(0.0);
    t.set_status("Starting the download…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut last = Instant::now() - Duration::from_secs(1);
        let w = weak.clone();
        let result = transcribe::install(gpu, &cancel, |p| {
            if last.elapsed() >= Duration::from_millis(120) {
                last = Instant::now();
                let (f, text) = (p.fraction(), p.text());
                let _ = w.upgrade_in_event_loop(move |ui| {
                    let t = ui.global::<Transcribe>();
                    t.set_progress(f);
                    t.set_status(text.into());
                });
            }
        });
        let stopped = cancel.load(Ordering::SeqCst);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            STATE.with(|s| s.borrow_mut().cancel = None);
            let t = ui.global::<Transcribe>();
            t.set_getting(false);
            refresh(&ui);
            match result {
                Ok(()) => {
                    let target = STATE.with(|s| s.borrow().target.clone());
                    if matches!(target, Target::None) {
                        t.set_open(false);
                        crate::show_toast(&ui, "Speech to text is ready.");
                    } else {
                        t.set_view(VIEW_OPTIONS);
                    }
                }
                Err(_) if stopped => t.set_status("".into()),
                Err(e) => t.set_error(tools::explain(&e, "speech to text").into()),
            }
        });
    });
}

/// Transcribe: from a file (the whole of it, its selection, or several), or a link through
/// the download queue.
fn start(ui: &AppWindow) {
    let t = ui.global::<Transcribe>();
    let spec = spec(ui);
    let target = STATE.with(|s| s.borrow().target.clone());
    let jobs: Vec<transcribe::Request> = match target {
        Target::Link(enqueue) => {
            enqueue(spec, t.get_has_selection() && t.get_part() == 0);
            t.set_open(false);
            return;
        }
        Target::Files { paths, selection } => {
            let range = selection.filter(|_| t.get_part() == 0 || !t.get_has_selection());
            if paths.len() > 1 && t.get_together() == 1 {
                paths
                    .into_iter()
                    .map(|p| transcribe::Request { inputs: vec![p], range: None, spec: spec.clone(), out_dir: None })
                    .collect()
            } else {
                vec![transcribe::Request { inputs: paths, range, spec, out_dir: None }]
            }
        }
        Target::None | Target::Download => return,
    };

    let cancel = Arc::new(AtomicBool::new(false));
    STATE.with(|s| s.borrow_mut().cancel = Some(cancel.clone()));
    t.set_view(VIEW_WORKING);
    t.set_progress(0.0);
    t.set_heard("".into());
    t.set_status("Getting the sound ready…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let n = jobs.len();
        let mut saved = Vec::new();
        let mut language = String::new();
        let mut lines = 0;
        let mut error = None;
        for (i, job) in jobs.iter().enumerate() {
            let mut last = Instant::now() - Duration::from_secs(1);
            let w = weak.clone();
            let which = if n > 1 { format!(" ({} of {n})", i + 1) } else { String::new() };
            let r = transcribe::run(job, &cancel, |p| {
                // Lines heard always show; the bar at most every 100 ms.
                let line = match &p.stage {
                    Stage::Listening(l) if !l.is_empty() => Some(l.clone()),
                    _ => None,
                };
                if line.is_none() && last.elapsed() < Duration::from_millis(100) {
                    return;
                }
                last = Instant::now();
                let f = (i as f32 + p.fraction) / n as f32;
                let status = match p.stage {
                    Stage::Getting(what) => format!("Getting {what}…"),
                    Stage::Preparing => format!("Getting the sound ready{which}…"),
                    Stage::Listening(_) => format!("Listening{which}… {:.0}%", p.fraction * 100.0),
                };
                let _ = w.upgrade_in_event_loop(move |ui| {
                    let t = ui.global::<Transcribe>();
                    t.set_progress(f);
                    t.set_status(status.into());
                    if let Some(l) = line {
                        t.set_heard(format!("“{l}”").into());
                    }
                });
            });
            match r {
                Ok(done) => {
                    // Several files make one transcript, next to the first.
                    if let Some(input) = job.inputs.first() {
                        neria_core::library::transcribed(input, &done.files);
                    }
                    saved.extend(done.files);
                    language = done.language;
                    lines += done.segments;
                }
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
            }
        }
        let stopped = cancel.load(Ordering::SeqCst);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            STATE.with(|s| s.borrow_mut().cancel = None);
            let t = ui.global::<Transcribe>();
            if stopped {
                t.set_view(VIEW_OPTIONS);
                return;
            }
            if let Some(e) = error {
                t.set_error(plain_error(&e).into());
                t.set_view(VIEW_FAILED);
                if !t.get_open() {
                    crate::show_toast(&ui, "The transcript failed. Open Transcribe again to see why.");
                }
                return;
            }
            let names: Vec<String> =
                saved.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
            let what = if lines == 0 {
                "No speech heard".to_string()
            } else {
                format!("{} · {lines} lines", transcribe::language_name(&language))
            };
            t.set_result(format!("Saved next to the {} · {what}", if n > 1 { "files" } else { "file" }).into());
            t.set_result_files(names.join("\n").into());
            t.set_view(VIEW_DONE);
            STATE.with(|s| s.borrow_mut().saved = saved.clone());
            crate::background_ui::notify(
                &ui,
                crate::background_ui::Note {
                    title: "Transcript saved".into(),
                    body: names.first().cloned().unwrap_or_default(),
                    image: None,
                    file: saved.first().cloned(),
                    failed: false,
                },
            );
            if !t.get_open() {
                crate::show_toast(&ui, &format!("Transcript saved · {}", names.first().cloned().unwrap_or_default()));
            }
        });
    });
}

/// The core's error, said plainly.
fn plain_error(e: &str) -> String {
    if e.contains("no sound") {
        "This file has no sound to transcribe.".into()
    } else if e.contains("isn't set up") {
        "Speech to text isn't set up any more. Get it again in Settings → Download.".into()
    } else {
        format!("The transcript couldn't be made: {e}")
    }
}

/// Dev hooks (`NERIA_DIALOG`): `transcribe-get` (not set up, as on a PC with an NVIDIA
/// card), `transcribe` (options for a file; `NERIA_OPEN`'s or a sample name),
/// `transcribe-files` (three dropped parts), `transcribe-work`, `transcribe-done`,
/// `transcribe-failed`, `transcript` (Extras → Transcript for a preview).
pub fn demo(ui: &AppWindow, which: &str) {
    let t = ui.global::<Transcribe>();
    let sample = |n: &str| PathBuf::from(format!(r"C:\Users\you\Videos\{n}"));
    match which {
        "transcribe-get" => {
            open_dialog(ui, Target::Files { paths: vec![sample("lecture.m4a")], selection: None }, String::new());
            t.set_nvidia(true);
            t.set_view(VIEW_GET);
            // As on a PC where nothing is there yet.
            t.set_get_size(fmt::bytes((transcribe::GPU_MB + transcribe::MODEL_MB) as f64 * 1_000_000.0).into());
        }
        "transcribe-files" => {
            open_files(ui, ["3 (compressed).m4a", "1.m4a", "2 (compressed).m4a"].iter().map(|n| sample(n)).collect());
            t.set_subtitle("3 files".into());
            t.set_view(VIEW_OPTIONS);
        }
        "transcribe-work" | "transcribe-done" | "transcribe-failed" | "transcribe" => {
            let path = std::env::var_os("NERIA_OPEN").map(PathBuf::from).unwrap_or_else(|| sample("lecture.m4a"));
            open_dialog(
                ui,
                Target::Files { paths: vec![path], selection: Some((6.0, 80.0)) },
                "lecture.m4a · 12:43".into(),
            );
            t.set_has_selection(true);
            t.set_selection_text("The selection · 00:06–01:20".into());
            t.set_view(VIEW_OPTIONS);
            // On a video: captions burned into a copy, ticked (its line under it shows).
            if t.get_can_burn() {
                t.set_burn(true);
            }
            match which {
                "transcribe-work" => {
                    t.set_view(VIEW_WORKING);
                    t.set_progress(0.42);
                    t.set_status("Listening… 42%".into());
                    t.set_heard("“So when we say glycogen storage disease, take note, okay, that this results now from an inborn error of metabolism.”".into());
                }
                "transcribe-done" => {
                    t.set_view(VIEW_DONE);
                    t.set_result("Saved next to the file · English · 271 lines".into());
                    t.set_result_files("lecture - transcript.txt\nlecture.srt".into());
                }
                "transcribe-failed" => {
                    t.set_view(VIEW_FAILED);
                    t.set_error("This file has no sound to transcribe.".into());
                }
                _ => {}
            }
        }
        "transcript" => {
            t.set_download_on(false);
            open_dialog(ui, Target::Download, "Me at the zoo".into());
        }
        _ => {}
    }
}
