//! "Clip that" (Settings → Voice, and the live player): a phrase you say presses Clip the last
//! while recording, Save the last while watching. Listens only while the live player is open
//! (and while Settings → Voice shows, for its meter and Test it), never otherwise; the mic is
//! read here and thrown away (`neria_core::voice`).
//!
//! One mic thread at a time, in one of three modes: listening for the phrases (the player),
//! the meter (Settings), or Test it (three sayings, then spellings that sound the same tried on
//! what was said). A timer keeps the right one running as the player opens and closes.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use neria_core::capture_sound::{self, Sound};
use neria_core::queue::Queue;
use neria_core::settings::Settings;
use neria_core::voice::{self, Pieces, Prefs, Sayings, Spotter, Tuning};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

use crate::{AppWindow, ClipLive, Voice, VoicePhrase, VoiceSpelling, Watch};

/// Keep the last: the chips in Settings, in seconds.
const KEEPS: [u32; 4] = [15, 30, 60, 120];

#[derive(Clone, PartialEq, Debug)]
enum Mode {
    /// The live player: act on what's heard.
    Listen(Vec<String>, Option<String>),
    /// Settings → Voice: only the meter.
    Meter(Option<String>),
    /// Test it on one phrase.
    Test(String, Option<String>),
}

struct MicThread {
    mode: Mode,
    stop: Arc<AtomicBool>,
}

impl Drop for MicThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct State {
    settings: Option<Arc<Mutex<Settings>>>,
    queue: Option<Queue>,
    mic: Option<MicThread>,
    /// The mic couldn't be opened: when to try again.
    retry_at: Option<Instant>,
    /// Test it is running (its mode wins over the others).
    testing: bool,
    /// The Listening mark was clicked: quiet until this stream's player closes.
    muted_here: bool,
    get_cancel: Option<Arc<AtomicBool>>,
    /// The last clip made on hearing it, and an Undo waiting for it to be saved.
    last_clip: Option<u64>,
    undo_wanted: Option<Instant>,
    /// When the last one was heard while watching: Show looks for the file saved since.
    saved_since: Option<std::time::SystemTime>,
    saved_secs: f64,
    /// Bumped by each toast and flash, so an older one's timer doesn't hide a newer one.
    toast_gen: u64,
    /// Sample screens for design checks (`NERIA_VOICE_DEMO`): nothing listens.
    demo: bool,
    _timer: Option<Timer>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

fn prefs() -> Prefs {
    ST.with(|s| s.borrow().settings.as_ref().map(|st| st.lock().unwrap().voice.clone()).unwrap_or_default())
}

fn save(f: impl FnOnce(&mut Prefs)) {
    ST.with(|s| {
        if let Some(st) = &s.borrow().settings {
            let mut st = st.lock().unwrap();
            f(&mut st.voice);
            let _ = st.save();
        }
    });
}

fn tune() -> Tuning {
    Tuning::default()
}

pub fn install(ui: &AppWindow, settings: Arc<Mutex<Settings>>, queue: Queue) {
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.settings = Some(settings);
        s.queue = Some(queue);
    });
    show_prefs(ui);
    let v = ui.global::<Voice>();

    v.on_toggled({
        let weak = ui.as_weak();
        move |on| {
            let ui = weak.unwrap();
            save(|p| p.on = on);
            ui.global::<Voice>().set_error("".into());
            if on && !voice::ready() {
                get_model(&ui);
            } else if on && prefs().tested.is_empty() {
                start_test(&ui);
            }
            show_prefs(&ui);
        }
    });
    v.on_get_cancel(|| {
        ST.with(|s| {
            if let Some(c) = &s.borrow().get_cancel {
                c.store(true, Ordering::Relaxed);
            }
        })
    });
    v.on_phrase_edited({
        let weak = ui.as_weak();
        move |i, text| {
            let ui = weak.unwrap();
            let i = i as usize;
            save(|p| {
                if let Some(slot) = p.phrases.get_mut(i) {
                    *slot = text.to_string();
                }
            });
            // A changed phrase hasn't been tested: its tag goes (the field keeps its text).
            let v = ui.global::<Voice>();
            let rows = v.get_phrases();
            if let Some(mut row) = slint::Model::row_data(&rows, i) {
                if !row.tag.is_empty() && row.text != text {
                    row.text = text.clone();
                    row.tag = "".into();
                    slint::Model::set_row_data(&rows, i, row);
                }
            }
            check_phrase(&ui, &text);
        }
    });
    v.on_add_phrase({
        let weak = ui.as_weak();
        move || {
            save(|p| p.phrases.push(String::new()));
            show_prefs(&weak.unwrap());
        }
    });
    v.on_remove_phrase({
        let weak = ui.as_weak();
        move |i| {
            save(|p| {
                if (i as usize) < p.phrases.len() && p.phrases.len() > 1 {
                    p.phrases.remove(i as usize);
                }
            });
            show_prefs(&weak.unwrap());
        }
    });
    v.on_changed({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let v = ui.global::<Voice>();
            let mic = v.get_mic();
            let name = if mic <= 0 {
                String::new()
            } else {
                slint::Model::row_data(&v.get_mics(), mic as usize).map(|s| s.to_string()).unwrap_or_default()
            };
            let keep = KEEPS[v.get_keep().clamp(0, 3) as usize];
            let sound = v.get_sound();
            save(|p| {
                p.mic = name;
                p.keep = keep;
                p.sound = sound;
            });
        }
    });
    v.on_test_start({
        let weak = ui.as_weak();
        move || start_test(&weak.unwrap())
    });
    v.on_test_stop(|| {
        // The mic thread sees the stop and reports what it has.
        ST.with(|s| {
            if let Some(m) = &s.borrow().mic {
                if matches!(m.mode, Mode::Test(..)) {
                    m.stop.store(true, Ordering::Relaxed);
                }
            }
        })
    });
    v.on_use_spelling({
        let weak = ui.as_weak();
        move |text| {
            let ui = weak.unwrap();
            let text = text.to_string();
            let heard = slint::Model::iter(&ui.global::<Voice>().get_spellings())
                .find(|s| s.text.as_str() == text)
                .and_then(|s| s.heard.split(' ').next().and_then(|n| n.parse::<u8>().ok()))
                .unwrap_or(0);
            save(|p| {
                if p.phrases.is_empty() {
                    p.phrases.push(text.clone());
                } else {
                    p.phrases[0] = text.clone();
                }
                p.tested = text.clone();
                p.tested_heard = heard;
            });
            let v = ui.global::<Voice>();
            v.set_spellings(ModelRc::default());
            v.set_test_note(format!("Using \"{text}\". Test it again any time.").into());
            show_prefs(&ui);
        }
    });
    v.on_remove_model({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            ST.with(|s| s.borrow_mut().mic = None);
            match voice::remove() {
                Ok(()) => crate::show_toast(&ui, "Voice model removed from this PC."),
                Err(e) => crate::show_toast(&ui, &format!("Couldn't remove it all: {e}")),
            }
            save(|p| p.on = false);
            show_prefs(&ui);
        }
    });
    v.on_section_open(|_| {});
    v.on_mark_clicked({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            ui.set_settings_section(5);
            ui.set_drawer_open(true);
        }
    });
    // The mark's menu: how much to keep (the same setting as Settings → Voice).
    v.on_keep_picked({
        let weak = ui.as_weak();
        move |i| {
            let i = i.clamp(0, 3);
            save(|p| p.keep = KEEPS[i as usize]);
            weak.unwrap().global::<Voice>().set_keep(i);
        }
    });
    // …and stop listening for this stream, or listen again.
    v.on_set_paused({
        let weak = ui.as_weak();
        move |paused| {
            ST.with(|s| {
                let mut s = s.borrow_mut();
                s.muted_here = paused;
                s.retry_at = None;
                if paused {
                    s.mic = None;
                }
            });
            let ui = weak.unwrap();
            ui.global::<Voice>().set_paused(paused);
            reconcile(&ui);
        }
    });
    v.on_undo({
        let weak = ui.as_weak();
        move || {
            ST.with(|s| s.borrow_mut().undo_wanted = Some(Instant::now()));
            let ui = weak.unwrap();
            hide_toast(&ui);
            finish_undo(&ui);
        }
    });
    v.on_show({
        let weak = ui.as_weak();
        move || show_saved(&weak.unwrap())
    });

    if let Ok(demo) = std::env::var("NERIA_VOICE_DEMO") {
        ST.with(|s| s.borrow_mut().demo = true);
        show_demo(ui, &demo);
        return;
    }

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(300), {
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                reconcile(&ui);
            }
        }
    });
    ST.with(|s| s.borrow_mut()._timer = Some(timer));
}

/// Settings → Voice from the saved prefs.
fn show_prefs(ui: &AppWindow) {
    let p = prefs();
    let v = ui.global::<Voice>();
    v.set_on(p.on);
    v.set_ready(voice::ready());
    let mut phrases = p.phrases.clone();
    if phrases.is_empty() {
        phrases.push(String::new());
    }
    v.set_phrases(ModelRc::new(VecModel::from(
        phrases
            .iter()
            .map(|t| {
                let tested = !p.tested.is_empty() && t.trim().eq_ignore_ascii_case(p.tested.trim());
                VoicePhrase {
                    text: t.into(),
                    tag: if tested { format!("Heard {} of 3", p.tested_heard).into() } else { "".into() },
                    kind: if p.tested_heard >= 3 { 1 } else { 2 },
                }
            })
            .collect::<Vec<_>>(),
    )));
    // Listing the mics starts COM for many threads, which the window's own thread mustn't
    // (it breaks drag and drop): asked on another thread.
    let (weak, picked) = (ui.as_weak(), p.mic.clone());
    let demo = std::env::var_os("NERIA_VOICE_DEMO").is_some();
    std::thread::spawn(move || {
        if demo {
            return;
        }
        let names = capture_sound::mics();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let mut mics = vec![SharedString::from("Windows' default")];
            mics.extend(names.into_iter().map(SharedString::from));
            let mic = mics.iter().position(|m| !picked.is_empty() && m.as_str() == picked).unwrap_or(0);
            let v = ui.global::<Voice>();
            v.set_mics(ModelRc::new(VecModel::from(mics)));
            v.set_mic(mic as i32);
        });
    });
    v.set_keep(KEEPS.iter().position(|k| *k == p.keep).unwrap_or(1) as i32);
    v.set_sound(p.sound);
    let bytes: u64 = std::fs::read_dir(voice::dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();
    v.set_model_text(format!("Voice model · {}", neria_core::fmt::bytes(bytes as f64)).into());
    if v.get_test() == 0 && v.get_test_note().is_empty() {
        v.set_test_note("Say your phrase three times, the way you would mid-game.".into());
    }
}

/// A phrase the model can't take (no letters): say so under it.
fn check_phrase(ui: &AppWindow, text: &str) {
    let v = ui.global::<Voice>();
    if text.trim().is_empty() {
        v.set_error("".into());
        return;
    }
    let msg = Pieces::load().and_then(|p| p.encode(text).map(|_| ())).err().map(|e| format!("\"{text}\": {e}"));
    v.set_error(msg.unwrap_or_default().into());
}

/// Fetch the voice model (5 MB), then Test it.
fn get_model(ui: &AppWindow) {
    let cancel = Arc::new(AtomicBool::new(false));
    ST.with(|s| s.borrow_mut().get_cancel = Some(cancel.clone()));
    let v = ui.global::<Voice>();
    v.set_getting(true);
    v.set_progress(0.0);
    v.set_get_text("".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut last = Instant::now() - Duration::from_secs(1);
        let w = weak.clone();
        let res = voice::install(&cancel, |d, t| {
            if last.elapsed() < Duration::from_millis(100) {
                return;
            }
            last = Instant::now();
            let (p, text) = match t {
                Some(t) => (
                    d as f32 / t.max(1) as f32,
                    format!("{} / {}", neria_core::fmt::bytes(d as f64), neria_core::fmt::bytes(t as f64)),
                ),
                None => (0.0, neria_core::fmt::bytes(d as f64)),
            };
            let _ = w.upgrade_in_event_loop(move |ui| {
                let v = ui.global::<Voice>();
                v.set_progress(p);
                v.set_get_text(text.into());
            });
        });
        let cancelled = cancel.load(Ordering::Relaxed);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ST.with(|s| s.borrow_mut().get_cancel = None);
            let v = ui.global::<Voice>();
            v.set_getting(false);
            match res {
                Ok(()) => {
                    show_prefs(&ui);
                    start_test(&ui);
                }
                Err(e) => {
                    save(|p| p.on = false);
                    if !cancelled {
                        v.set_error(format!("Couldn't get the voice model: {e:#}").into());
                    }
                    show_prefs(&ui);
                }
            }
        });
    });
}

fn start_test(ui: &AppWindow) {
    let p = prefs();
    let Some(phrase) = p.phrases().into_iter().next() else {
        ui.global::<Voice>().set_error("Type a phrase first.".into());
        return;
    };
    let v = ui.global::<Voice>();
    v.set_test(1);
    v.set_boxes(ModelRc::new(VecModel::from(vec![1, 0, 0])));
    v.set_spellings(ModelRc::default());
    v.set_test_note("Say it three times, the way you would mid-game.".into());
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.testing = true;
        s.mic = None;
    });
    let mode = Mode::Test(phrase, p.mic());
    start_mic(ui, mode);
}

/// Keep the right mic thread running: Test it, the live player, Settings' meter, or none.
fn reconcile(ui: &AppWindow) {
    let (testing, muted) = ST.with(|s| (s.borrow().testing, s.borrow().muted_here));
    if testing {
        return;
    }
    let p = prefs();
    let w = ui.global::<Watch>();
    let live = ui.global::<ClipLive>();
    let player =
        ui.get_page() == 1 && (live.get_on() || (w.get_on() && w.get_recording_id() != 0 && !w.get_rec_done()));
    if !player && muted {
        ST.with(|s| s.borrow_mut().muted_here = false);
    }
    let settings_open = ui.get_drawer_open() && ui.get_settings_section() == 5;
    let ready = p.on && voice::ready();
    let want = if ready && player && !muted && !p.phrases().is_empty() {
        Some(Mode::Listen(p.phrases(), p.mic()))
    } else if ready && settings_open {
        Some(Mode::Meter(p.mic()))
    } else {
        None
    };
    let v = ui.global::<Voice>();
    // The mark shows whenever voice could listen here, also once stopped ("Not listening"),
    // so it can be turned back on from the same place.
    let phrases = p.phrases();
    let mark = ready && player && !phrases.is_empty();
    v.set_listening(mark);
    v.set_paused(mark && muted);
    if mark {
        let more = if phrases.len() > 1 { format!(" +{}", phrases.len() - 1) } else { String::new() };
        v.set_listen_text(format!("Listening for \"{}\"{more}", phrases[0]).into());
    }
    if !matches!(want, Some(Mode::Listen(..))) {
        v.set_mic_missing(false);
    }
    let running = ST.with(|s| s.borrow().mic.as_ref().map(|m| m.mode.clone()));
    if running == want {
        return;
    }
    ST.with(|s| s.borrow_mut().mic = None);
    if want.is_none() {
        v.set_level(0.0);
    }
    if let Some(mode) = want {
        let wait = ST.with(|s| s.borrow().retry_at.is_some_and(|t| Instant::now() < t));
        if !wait {
            start_mic(ui, mode);
        }
    }
}

/// What the mic thread tells the UI.
enum Msg {
    Level(f32),
    Heard(String),
    Boxes(Vec<i32>),
    /// Test it ended: the boxes, the sound at 16 kHz and where each saying was.
    TestDone(Vec<i32>, Vec<f32>, Vec<(f64, f64)>),
    /// The mic or the model couldn't be opened.
    Failed(String, bool),
}

fn start_mic(ui: &AppWindow, mode: Mode) {
    let stop = Arc::new(AtomicBool::new(false));
    ST.with(|s| s.borrow_mut().mic = Some(MicThread { mode: mode.clone(), stop: stop.clone() }));
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let send = |m: Msg| {
            let _ = weak.upgrade_in_event_loop(move |ui| on_msg(&ui, m));
        };
        if let Err((e, mic)) = run_mic(&mode, &stop, &send) {
            send(Msg::Failed(e, mic));
        }
    });
}

fn run_mic(mode: &Mode, stop: &AtomicBool, send: &dyn Fn(Msg)) -> Result<(), (String, bool)> {
    let (phrases, mic) = match mode {
        Mode::Listen(p, m) => (p.clone(), m.clone()),
        Mode::Meter(m) => (Vec::new(), m.clone()),
        Mode::Test(p, m) => (vec![p.clone()], m.clone()),
    };
    let mut spotter = if phrases.is_empty() {
        None
    } else {
        Some(Spotter::new(&phrases, tune()).map_err(|e| (format!("{e:#}"), false))?)
    };
    let mut reader = capture_sound::Reader::open(&Sound::Mic(mic)).map_err(|e| (format!("{e:#}"), true))?;
    let test = matches!(mode, Mode::Test(..));
    let mut sayings = Sayings::new(capture_sound::RATE);
    let mut heard_in: Vec<usize> = Vec::new();
    let mut kept: Vec<f32> = Vec::new();
    let mut boxes = vec![1, 0, 0];
    let started = Instant::now();
    let mut level_at = Instant::now() - Duration::from_secs(1);
    let mut peak = 0f32;
    let mut buf = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        buf.clear();
        reader.read(&mut buf, Duration::from_millis(100)).map_err(|e| (format!("{e:#}"), true))?;
        if buf.is_empty() {
            continue;
        }
        let samples = voice::mono(&buf);
        peak = samples.iter().fold(peak, |m, s| m.max(s.abs()));
        if level_at.elapsed() >= Duration::from_millis(90) {
            send(Msg::Level(peak.min(1.0)));
            peak = 0.0;
            level_at = Instant::now();
        }
        let heard = spotter.as_mut().map(|s| s.feed(capture_sound::RATE, &samples)).unwrap_or_default();
        if !test {
            for h in heard {
                send(Msg::Heard(h.phrase));
            }
            continue;
        }
        kept.extend(voice::to_16k(&samples));
        sayings.feed(&samples);
        for h in &heard {
            if let Some(i) = sayings.which(h.at) {
                if !heard_in.contains(&i) {
                    heard_in.push(i);
                }
            }
        }
        // The boxes: heard, missed (ended a moment ago and nothing heard), listening, waiting.
        let now = kept.len() as f64 / 16_000.0;
        let done = sayings.done.len();
        let new: Vec<i32> = (0..3)
            .map(|i| {
                if heard_in.contains(&i) {
                    2
                } else if i < done && now > sayings.done[i].1 + 1.3 {
                    3
                } else if i <= done {
                    1
                } else {
                    0
                }
            })
            .collect();
        if new != boxes {
            boxes = new;
            send(Msg::Boxes(boxes.clone()));
        }
        let all_in = boxes.iter().all(|b| *b >= 2);
        if all_in || started.elapsed() > Duration::from_secs(30) {
            break;
        }
    }
    if test {
        // Stopped early: what wasn't said stays empty; one being said counts as missed.
        let boxes: Vec<i32> = boxes.iter().map(|b| if *b == 1 && !sayings.done.is_empty() { 0 } else { *b }).collect();
        send(Msg::TestDone(boxes, kept, sayings.done.clone()));
    }
    Ok(())
}

fn on_msg(ui: &AppWindow, m: Msg) {
    let v = ui.global::<Voice>();
    match m {
        Msg::Level(l) => v.set_level(l),
        Msg::Heard(phrase) => heard(ui, &phrase),
        Msg::Boxes(b) => v.set_boxes(ModelRc::new(VecModel::from(b))),
        Msg::Failed(e, mic) => {
            let testing = ST.with(|s| {
                let mut s = s.borrow_mut();
                s.mic = None;
                s.retry_at = Some(Instant::now() + Duration::from_secs(4));
                std::mem::take(&mut s.testing)
            });
            if mic {
                v.set_mic_missing(true);
                v.set_error(format!("Couldn't open the mic: {e}").into());
            } else {
                v.set_error(e.into());
            }
            if testing {
                v.set_test(0);
            }
        }
        Msg::TestDone(boxes, audio, sayings) => test_done(ui, boxes, audio, sayings),
    }
}

fn test_done(ui: &AppWindow, boxes: Vec<i32>, audio: Vec<f32>, sayings: Vec<(f64, f64)>) {
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.testing = false;
        s.mic = None;
        s.retry_at = None;
    });
    let v = ui.global::<Voice>();
    v.set_boxes(ModelRc::new(VecModel::from(boxes.clone())));
    let heard = boxes.iter().filter(|b| **b == 2).count();
    let said = boxes.iter().filter(|b| **b >= 2).count();
    let Some(phrase) = prefs().phrases().into_iter().next() else {
        v.set_test(0);
        return;
    };
    if said == 0 {
        v.set_test(0);
        v.set_test_note("Didn't hear you say anything. Check the mic and its meter, then try again.".into());
        return;
    }
    save(|p| {
        p.tested = phrase.clone();
        p.tested_heard = heard as u8;
    });
    show_prefs(ui);
    if heard == said && said == 3 {
        v.set_test(0);
        v.set_test_note("Heard you every time. You're set.".into());
        return;
    }
    let alternatives = Pieces::load().map(|p| voice::spellings(&p, &phrase)).unwrap_or_default();
    if alternatives.is_empty() || heard == said {
        v.set_test(0);
        v.set_test_note(
            if heard == said {
                "Heard every one you said. Say it three times for a full test."
            } else {
                "It missed some. Say it a little louder or closer, or pick other words."
            }
            .into(),
        );
        return;
    }
    v.set_test(2);
    v.set_test_note("It missed some. Trying spellings that sound the same…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let found = voice::try_spellings(&audio, &sayings, &alternatives, tune());
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let v = ui.global::<Voice>();
            v.set_test(0);
            let better: Vec<_> = found.into_iter().filter(|(_, n)| *n > heard).take(3).collect();
            if better.is_empty() {
                v.set_test_note(
                    "It missed some, and no other spelling did better. Say it louder or closer, or pick other words."
                        .into(),
                );
                return;
            }
            v.set_test_note(
                format!("It didn't catch \"{phrase}\" every time. These sound the same, tried on what you just said:")
                    .into(),
            );
            v.set_spellings(ModelRc::new(VecModel::from(
                better
                    .iter()
                    .enumerate()
                    .map(|(i, (t, n))| VoiceSpelling {
                        text: t.into(),
                        heard: format!("{n} of {said}").into(),
                        best: i == 0,
                    })
                    .collect::<Vec<_>>(),
            )));
        });
    });
}

/// The phrase was heard in the live player: do what its button does.
fn heard(ui: &AppWindow, phrase: &str) {
    let p = prefs();
    let secs = p.keep as f64;
    let w = ui.global::<Watch>();
    let live = ui.global::<ClipLive>();
    let said = format!("Heard \"{}\"", phrase.to_lowercase());
    let len = neria_core::fmt::duration(secs);
    if w.get_on() && w.get_recording_id() != 0 && !w.get_rec_done() {
        if !w.get_rec_can_clip() {
            toast(ui, &said, "It can clip once the recording has caught up", 2, false);
            return;
        }
        let Some(queue) = ST.with(|s| s.borrow().queue.clone()) else { return };
        let cid = queue.clip_back(w.get_recording_id() as u64, secs, crate::clipper::behind_live(ui));
        ST.with(|s| {
            let mut s = s.borrow_mut();
            s.last_clip = Some(cid);
            s.undo_wanted = None;
            s.saved_since = None;
        });
        toast(ui, &said, &format!("Clipping the last {len}"), 1, true);
        ui.global::<Voice>().set_heard_show(true);
    } else if live.get_on() && (live.get_can_rewind() || live.get_buffering()) {
        ST.with(|s| {
            let mut s = s.borrow_mut();
            s.last_clip = None;
            s.undo_wanted = None;
            // A moment back: the file can be stamped as it opens, a hair before now.
            s.saved_since = Some(std::time::SystemTime::now() - Duration::from_secs(2));
            s.saved_secs = secs;
        });
        live.invoke_last(secs as f32);
        toast(ui, &said, &format!("Saving the last {len} to your Videos"), 1, true);
        ui.global::<Voice>().set_heard_show(true);
    } else {
        toast(ui, &said, "Nothing kept to clip from yet", 2, false);
        return;
    }
    if p.sound {
        neria_core::hotkey::confirm_sound();
    }
    flash(ui);
}

fn toast(ui: &AppWindow, tag: &str, text: &str, kind: i32, undo: bool) {
    let v = ui.global::<Voice>();
    v.set_heard_tag(tag.into());
    v.set_heard_text(text.into());
    v.set_heard_kind(kind);
    v.set_heard_undo(undo);
    v.set_heard_show(false);
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
    let v = ui.global::<Voice>();
    v.set_heard_text("".into());
    v.set_heard_tag("".into());
    v.set_heard_undo(false);
}

fn flash(ui: &AppWindow) {
    ui.global::<Voice>().set_flash(true);
    let weak = ui.as_weak();
    Timer::single_shot(Duration::from_millis(650), move || {
        if let Some(ui) = weak.upgrade() {
            ui.global::<Voice>().set_flash(false);
        }
    });
}

/// Show: the clip just made, in Explorer (from its queue row while recording; while watching,
/// the newest video saved since it was heard). Still being saved: says where it goes.
fn show_saved(ui: &AppWindow) {
    let (queue, cid, since) = ST.with(|s| {
        let s = s.borrow();
        (s.queue.clone(), s.last_clip, s.saved_since)
    });
    let dir = neria_core::folders::for_kind(&Settings::load(), neria_core::folders::Kind::Video);
    let file = match (queue, cid, since) {
        (Some(q), Some(cid), _) => q.file_of(cid),
        (_, _, Some(since)) => std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .filter(|(t, p)| *t >= since && !p.extension().is_some_and(|x| x == "part" || x == "ytdl"))
            .max_by_key(|(t, _)| *t)
            .map(|(_, p)| p),
        _ => None,
    };
    match file {
        Some(f) if f.is_file() => crate::reveal(&f),
        _ => crate::show_toast(ui, &format!("Still saving. It goes in {}.", dir.display())),
    }
}

/// Undo while watching: "Save the last" made no row of its own we know the id of, so the
/// video saved in the Videos folder since it was heard, named with its length ("0m30s"), is
/// deleted once it's there (and its row, if it has one).
fn undo_watched(ui: &AppWindow, queue: &Queue, wanted: Instant) {
    let (since, secs) = ST.with(|s| (s.borrow().saved_since, s.borrow().saved_secs));
    let Some(since) = since else { return };
    let dir = neria_core::folders::for_kind(&Settings::load(), neria_core::folders::Kind::Video);
    let stamp = neria_core::clip::stamp(secs);
    let found = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(t, p)| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            *t >= since && name.contains(&stamp) && !name.ends_with(".part") && !name.ends_with(".ytdl")
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p);
    if let Some(file) = found {
        let _ = std::fs::remove_file(&file);
        if let Some(id) = queue.id_of_file(&file) {
            ui.invoke_remove_job(id as i32);
        }
        ST.with(|s| {
            let mut s = s.borrow_mut();
            s.saved_since = None;
            s.undo_wanted = None;
        });
        crate::show_toast(ui, "Undone: that clip is deleted.");
        return;
    }
    if wanted.elapsed() > Duration::from_secs(120) {
        ST.with(|s| s.borrow_mut().undo_wanted = None);
        return;
    }
    let weak: Weak<AppWindow> = ui.as_weak();
    Timer::single_shot(Duration::from_millis(700), move || {
        if let Some(ui) = weak.upgrade() {
            finish_undo(&ui);
        }
    });
}

/// Undo: the clip's file is deleted and its row goes, now if it's saved or as soon as it is.
fn finish_undo(ui: &AppWindow) {
    let (queue, cid, wanted) = ST.with(|s| {
        let s = s.borrow();
        (s.queue.clone(), s.last_clip, s.undo_wanted)
    });
    let (Some(queue), Some(wanted)) = (queue, wanted) else { return };
    let Some(cid) = cid else {
        undo_watched(ui, &queue, wanted);
        return;
    };
    if let Some(file) = queue.file_of(cid) {
        let _ = std::fs::remove_file(&file);
        ui.invoke_remove_job(cid as i32);
        ST.with(|s| {
            let mut s = s.borrow_mut();
            s.last_clip = None;
            s.undo_wanted = None;
        });
        crate::show_toast(ui, "Undone: that clip is deleted.");
        return;
    }
    if wanted.elapsed() > Duration::from_secs(120) {
        ST.with(|s| s.borrow_mut().undo_wanted = None);
        return;
    }
    // Still being saved: look again shortly.
    let weak: Weak<AppWindow> = ui.as_weak();
    Timer::single_shot(Duration::from_millis(500), move || {
        if let Some(ui) = weak.upgrade() {
            finish_undo(&ui);
        }
    });
}

/// `NERIA_VOICE_DEMO`: sample screens, nothing listening. `on` (Settings, tested), `getting`,
/// `test` (one heard, listening for the second), `test-miss` (missed, spellings offered),
/// `listening`, `heard`, `missing` (the player's mark and toast; open a live stream too).
fn show_demo(ui: &AppWindow, what: &str) {
    let v = ui.global::<Voice>();
    let row = |text: &str, tag: &str, kind: i32| VoicePhrase { text: text.into(), tag: tag.into(), kind };
    v.set_on(true);
    v.set_ready(true);
    v.set_level(0.38);
    v.set_mics(ModelRc::new(VecModel::from(vec![
        SharedString::from("Windows' default"),
        "Microphone (2- fifine Microphone)".into(),
    ])));
    v.set_mic(1);
    v.set_model_text("Voice model · 5.3 MB".into());
    v.set_phrases(ModelRc::new(VecModel::from(vec![row("clip that", "Heard 3 of 3", 1)])));
    v.set_boxes(ModelRc::new(VecModel::from(vec![2, 2, 2])));
    v.set_test_note("Heard you every time. You're set.".into());
    v.set_listen_text("Listening for \"clip that\"".into());
    match what {
        "getting" => {
            v.set_ready(false);
            v.set_getting(true);
            v.set_progress(0.4);
            v.set_get_text("2.1 MB / 5.3 MB".into());
        }
        "test" => {
            v.set_phrases(ModelRc::new(VecModel::from(vec![row("clip that", "", 1)])));
            v.set_test(1);
            v.set_boxes(ModelRc::new(VecModel::from(vec![2, 1, 0])));
            v.set_level(0.62);
            v.set_test_note("Say it three times, the way you would mid-game.".into());
        }
        "test-miss" => {
            v.set_phrases(ModelRc::new(VecModel::from(vec![row("neria clip that", "Heard 0 of 3", 2)])));
            v.set_boxes(ModelRc::new(VecModel::from(vec![3, 3, 3])));
            v.set_test_note(
                "It didn't catch \"neria clip that\" every time. These sound the same, tried on what you just said:"
                    .into(),
            );
            v.set_spellings(ModelRc::new(VecModel::from(vec![
                VoiceSpelling { text: "nearia clip that".into(), heard: "3 of 3".into(), best: true },
                VoiceSpelling { text: "near ya clip that".into(), heard: "1 of 3".into(), best: false },
            ])));
        }
        "listening" => v.set_listening(true),
        "heard" => {
            v.set_listening(true);
            v.set_flash(true);
            v.set_heard_tag("Heard \"clip that\"".into());
            v.set_heard_text("Clipping the last 30 s".into());
            v.set_heard_undo(true);
        }
        "missing" => {
            v.set_listening(true);
            v.set_mic_missing(true);
        }
        _ => {}
    }
}
