//! Capture page → Phone (`ui/pages/phone.slint`, mock `design/phone.html`): connect an
//! Android phone by QR (or a pairing code, or USB), mirror it in the page with its own replay
//! buffer ("Clip the last 30 s"), record it, take a screenshot, hear it alone (Sound only),
//! present it in a bare window (scrcpy's own) named "Phone screen · Pixel 7".
//!
//! The core is `neria_core::phone`. The picture is drawn by its own mpv into a GL texture from
//! the window's rendering hook, which `clipper.rs` owns (it calls `render` / `teardown`
//! here), reading the session's raw H.264 relay with no buffering. Saves go through
//! `capture_ui::saved_file`: the same Saved list, toast and notification as instant replay.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glow::HasContext;
use neria_core::phone::{
    self, Control, Device, Info, Options, Pairing, PhoneBuffer, Place, Recording, Session, Speaker, State, Touch,
};
use neria_core::settings::Settings;
use neria_core::{fmt, folders, hotkey, paths, proc};
use neria_player::{GlRenderer, Mpv};
use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, SharedString, Timer, TimerMode, VecModel};

use crate::clipper::{Target, make_target};
use crate::{AppWindow, Phone, PhoneRow, QualityRow};
use neria_core::capture::Quality;

/// The Phone tab's states (`Phone.state`).
mod st {
    pub const NO_TOOLS: i32 = 0;
    pub const GETTING: i32 = 1;
    pub const CONNECT: i32 = 2;
    pub const PHONES: i32 = 3;
    pub const STARTING: i32 = 4;
    pub const MIRROR: i32 = 5;
    pub const SOUND: i32 = 6;
}

#[derive(Default)]
struct Ui {
    settings: Option<Arc<Mutex<Settings>>>,
    demo: bool,
    /// The tab shows (devices are looked at while it does).
    shown: bool,
    server: bool,
    looking: bool,
    pairing: Option<Pairing>,
    /// What adb sees, and what each phone said about itself (by serial).
    devices: Vec<Device>,
    infos: HashMap<String, Info>,
    session: Option<Arc<Session>>,
    buffer: Option<Arc<PhoneBuffer>>,
    speaker: Option<Speaker>,
    recording: Option<Arc<Recording>>,
    /// Open Present again once mirroring starts again (the sound's place changed meanwhile).
    represent: bool,
    /// Mirroring over Wi-Fi and the phone was plugged in: its cable name (offered, not used).
    usb_offer: Option<String>,
    /// "Not now" to the cable, for this time mirroring.
    usb_declined: bool,
    /// Mirroring started on Best (watched for the phone's picture stalling), and Best was too
    /// much for this phone this time (mirroring on Standard until the next start by hand).
    on_best: bool,
    stepped_back: bool,
    /// The clipboard shared while mirroring: the phone's count last taken, and the text both
    /// sides last had (so neither side's copy bounces back).
    clip_seen: u64,
    clip_text: Option<String>,
    /// Looking at what adb sees while mirroring (off the UI thread).
    watching: bool,
    /// Said once mirroring starts again ("Cable unplugged: mirroring over Wi-Fi now").
    note: Option<String>,
    serial: String,
    /// A finger down on the picture (moves are sent only then).
    down: bool,
    hotkey: Option<hotkey::Listener>,
    saving: bool,
    /// Phones let go of with Disconnect (by id): not connected again until Connect.
    held: Vec<String>,
    /// Remembered phones on this Wi-Fi with Wireless debugging on (by id).
    visible: Vec<String>,
    /// When pairing last failed (its note stays a while).
    pair_failed: Option<Instant>,
    /// Pair with the next phone found asking for a code.
    timer: Option<Timer>,
    // The picture.
    mpv: Option<Mpv>,
    renderer: Option<GlRenderer>,
    target: Option<Target>,
    pending: Option<String>,
    /// Where the picture's player was at the last check, and the phone's frame count then
    /// (the picture stalled when the player stays put while frames keep coming).
    last_pos: Option<f64>,
    last_frames: u64,
    /// The texture is in the page (`Phone.frame`); cleared when mirroring stops.
    shown_frame: bool,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

fn prefs() -> phone::Prefs {
    UI.with(|u| u.borrow().settings.as_ref().map(|s| s.lock().unwrap().phone.clone()).unwrap_or_default())
}

fn save_prefs(f: impl FnOnce(&mut phone::Prefs)) {
    UI.with(|u| {
        if let Some(s) = &u.borrow().settings {
            let mut s = s.lock().unwrap();
            f(&mut s.phone);
            let _ = s.save();
        }
    });
}

fn state(ui: &AppWindow) -> i32 {
    ui.global::<Phone>().get_state()
}

/// Mirroring now (the save key and "clip that" clip it).
pub fn mirroring() -> bool {
    UI.with(|u| u.borrow().buffer.is_some())
}

/// For the tray: "Mirroring Pixel 7" / "Playing from Pixel 7".
pub fn status_line() -> Option<String> {
    UI.with(|u| {
        let u = u.borrow();
        let s = u.session.as_ref()?;
        Some(if s.video.is_some() { format!("Mirroring {}", s.name) } else { format!("Playing from {}", s.name) })
    })
}

pub fn install(ui: &AppWindow, settings: Arc<Mutex<Settings>>) {
    UI.with(|u| u.borrow_mut().settings = Some(settings));
    phone::clean_leftovers();
    let p = ui.global::<Phone>();
    show_prefs(ui);

    p.on_get_tools({
        let weak = ui.as_weak();
        move || get_tools(&weak.unwrap())
    });
    p.on_shown({
        let weak = ui.as_weak();
        move |on| shown(&weak.unwrap(), on)
    });
    p.on_add_phone({
        let weak = ui.as_weak();
        move || connect_screen(&weak.unwrap())
    });
    p.on_back({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            UI.with(|u| u.borrow_mut().pairing = None);
            set_state(&ui, st::PHONES);
            show_phones(&ui);
        }
    });
    p.on_pair_code({
        let weak = ui.as_weak();
        move |code| pair_code(&weak.unwrap(), code.to_string())
    });
    p.on_mirror({
        let weak = ui.as_weak();
        move |serial| start(&weak.unwrap(), serial.to_string(), false)
    });
    p.on_sound_only({
        let weak = ui.as_weak();
        move |serial| {
            let ui = weak.unwrap();
            let serial = if serial.is_empty() { UI.with(|u| u.borrow().serial.clone()) } else { serial.to_string() };
            start(&ui, serial, true)
        }
    });
    p.on_show_picture({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let serial = UI.with(|u| u.borrow().serial.clone());
            start(&ui, serial, false)
        }
    });
    p.on_forget({
        let weak = ui.as_weak();
        move |id| {
            let ui = weak.unwrap();
            let_go(&ui, &id);
            phone::forget(&id);
            show_phones(&ui);
        }
    });
    p.on_disconnect({
        let weak = ui.as_weak();
        move |id| let_go(&weak.unwrap(), &id)
    });
    p.on_connect({
        let weak = ui.as_weak();
        move |id| {
            let ui = weak.unwrap();
            let id = id.to_string();
            UI.with(|u| u.borrow_mut().held.retain(|h| *h != id));
            let w = ui.as_weak();
            std::thread::spawn(move || {
                let res = phone::connect_id(&id);
                let _ = w.upgrade_in_event_loop(move |ui| {
                    if let Err(e) = res {
                        crate::capture_ui::toast(&ui, "Not connected", &e.to_string(), false, None);
                    }
                    look(&ui);
                });
            });
        }
    });
    p.on_stop({
        let weak = ui.as_weak();
        move || stop(&weak.unwrap(), None)
    });
    p.on_clip({
        let weak = ui.as_weak();
        move || clip(&weak.unwrap())
    });
    p.on_record({
        let weak = ui.as_weak();
        move || record(&weak.unwrap())
    });
    p.on_screenshot({
        let weak = ui.as_weak();
        move || screenshot(&weak.unwrap())
    });
    p.on_present({
        let weak = ui.as_weak();
        move || present(&weak.unwrap())
    });
    p.on_front(crate::present_ui::bring_to_front);
    p.on_switch_usb({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let Some(usb) = UI.with(|u| u.borrow_mut().usb_offer.take()) else { return };
            ui.global::<Phone>().set_usb_offer(false);
            restart_on(&ui, usb, "Switched to the cable");
        }
    });
    p.on_keep_wifi({
        let weak = ui.as_weak();
        move || {
            UI.with(|u| {
                let mut u = u.borrow_mut();
                u.usb_offer = None;
                u.usb_declined = true;
            });
            weak.unwrap().global::<Phone>().set_usb_offer(false);
        }
    });
    p.on_changed({
        let weak = ui.as_weak();
        move || changed(&weak.unwrap())
    });
    p.on_show_changed({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            if UI.with(|u| u.borrow().demo) || state(&ui) != st::MIRROR {
                return;
            }
            let serial = UI.with(|u| u.borrow().serial.clone());
            start(&ui, serial, false);
        }
    });
    p.on_quality_pick({
        let weak = ui.as_weak();
        move |i| {
            let q = Quality::ALL.get(i.max(0) as usize).copied().unwrap_or_default();
            save_prefs(|p| p.quality = q.name().into());
            quality_changed(&weak.unwrap());
        }
    });
    p.on_quality_size({
        let weak = ui.as_weak();
        move |i| {
            let h = SIZES.get(i.max(0) as usize).copied().unwrap_or(0);
            set_custom(&weak.unwrap(), |l| l.max_size = h);
        }
    });
    p.on_quality_rate({
        let weak = ui.as_weak();
        move |i| {
            let fps = RATES.get(i.max(0) as usize).copied().unwrap_or(60);
            set_custom(&weak.unwrap(), |l| l.fps = fps);
        }
    });
    p.on_quality_mbps({
        let weak = ui.as_weak();
        move |d| set_custom(&weak.unwrap(), |l| l.mbps = crate::capture_ui::step_mbps(l.mbps, d))
    });
    p.on_quality_back({
        let weak = ui.as_weak();
        move || {
            save_prefs(|p| p.quality = Quality::Standard.name().into());
            quality_changed(&weak.unwrap());
        }
    });
    p.on_volume_changed({
        let weak = ui.as_weak();
        move || {
            let v = weak.unwrap().global::<Phone>().get_volume();
            UI.with(|u| {
                if let Some(s) = &u.borrow().speaker {
                    s.set_volume(v);
                }
            });
            save_prefs(|p| p.volume = v);
        }
    });
    p.on_touch(touch);
    p.on_scroll(scroll);
    p.on_press(|code| with_session(|s| s.press(code)));
    p.on_rotate(|| with_session(|s| s.send(Control::Rotate)));
    p.on_typed(|text| typed(&text));

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(1500), {
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                tick(&ui);
            }
        }
    });
    UI.with(|u| u.borrow_mut().timer = Some(timer));

    if std::env::var_os("NERIA_PHONE_FAKE").is_some() {
        fake(ui);
        return;
    }
    if let Ok(demo) = std::env::var("NERIA_PHONE_DEMO") {
        UI.with(|u| u.borrow_mut().demo = true);
        show_demo(ui, &demo);
        return;
    }
    set_state(ui, if phone::ready() { st::PHONES } else { st::NO_TOOLS });
    show_phones(ui);
    // `NERIA_PHONE_TAB`: open on the Phone tab (with `NERIA_PAGE=capture`).
    if std::env::var_os("NERIA_PHONE_TAB").is_some() {
        p.set_tab(1);
        shown(ui, true);
    }
}

fn set_state(ui: &AppWindow, s: i32) {
    let p = ui.global::<Phone>();
    p.set_state(s);
    if s < st::STARTING {
        p.set_problem(false);
        if s != st::PHONES {
            p.set_detail("".into());
        }
    }
}

fn show_prefs(ui: &AppWindow) {
    let pr = prefs();
    let p = ui.global::<Phone>();
    p.set_sound_place(pr.place.index());
    p.set_screen_off(pr.screen_off);
    p.set_awake(pr.awake);
    p.set_clipboard(pr.clipboard);
    p.set_volume(pr.volume);
    p.set_on_top(pr.on_top);
    p.set_show_taps(pr.show_taps);
    p.set_save_key(crate::capture_ui::save_key().into());
    show_quality(ui);
}

/// Advanced's Size chips (the picture's longer side, 0: as the phone is) and frame rates.
const SIZES: [u32; 3] = [0, 1920, 1280];
const RATES: [u32; 3] = [30, 60, 120];

/// "1280 tall · 30 fps · for slow Wi-Fi".
fn quality_hint(q: Quality, l: &phone::Look) -> String {
    let mut parts = vec![if l.max_size == 0 { "As the phone".to_string() } else { format!("{} tall", l.max_size) }];
    parts.push(format!("{} fps", l.fps));
    match q {
        Quality::Smaller => parts.push("for slow Wi-Fi".into()),
        Quality::Best => parts.push("for games".into()),
        Quality::Custom => parts.push(format!("{} Mbit/s", l.mbps)),
        Quality::Standard => {}
    }
    parts.join(" · ")
}

fn show_quality(ui: &AppWindow) {
    let pr = prefs();
    let p = ui.global::<Phone>();
    let picked = pr.quality();
    let mut rows = Vec::new();
    for q in Quality::ALL {
        if q == Quality::Custom && picked != Quality::Custom {
            continue;
        }
        let l = if q == Quality::Custom { pr.look() } else { phone::quality_look(q) };
        rows.push(QualityRow {
            name: q.label().into(),
            hint: quality_hint(q, &l).into(),
            value: format!("≈ {} MB/min", l.mb_per_min()).into(),
        });
    }
    p.set_quality_rows(ModelRc::new(VecModel::from(rows)));
    p.set_quality(Quality::ALL.iter().position(|q| *q == picked).unwrap_or(1) as i32);
    p.set_quality_custom(picked == Quality::Custom);
    let stepped = UI.with(|u| u.borrow().stepped_back);
    p.set_quality_warn(stepped);
    p.set_quality_note(
        if stepped {
            "The phone's picture stalled on Best, so it's mirroring on Standard. Best is tried again next time."
        } else {
            "For the mirror, its replay and Record; changing it starts the mirror again. Present keeps its own lighter picture."
        }
        .into(),
    );
    let l = pr.look();
    p.set_q_sizes(ModelRc::new(VecModel::from(vec![SharedString::from("As the phone"), "1920".into(), "1280".into()])));
    p.set_q_size(SIZES.iter().position(|x| *x == l.max_size).unwrap_or(0) as i32);
    p.set_q_rates(ModelRc::new(VecModel::from(
        RATES.iter().map(|r| SharedString::from(r.to_string())).collect::<Vec<_>>(),
    )));
    p.set_q_rate(RATES.iter().position(|r| *r >= l.fps).unwrap_or(RATES.len() - 1) as i32);
    p.set_q_mbps(l.mbps as i32);
}

/// A number in Advanced changed: Custom, starting from what was picked.
fn set_custom(ui: &AppWindow, f: impl FnOnce(&mut phone::Look)) {
    let mut l = prefs().look();
    f(&mut l);
    save_prefs(|p| {
        p.quality = Quality::Custom.name().into();
        p.max_size = l.max_size;
        p.fps = l.fps;
        p.mbps = l.mbps;
    });
    quality_changed(ui);
}

/// The Quality changed: shown, and the mirror starts again with it (Present comes back).
fn quality_changed(ui: &AppWindow) {
    UI.with(|u| u.borrow_mut().stepped_back = false);
    show_quality(ui);
    if UI.with(|u| u.borrow().demo) {
        return;
    }
    if state(ui) == st::MIRROR {
        let serial = UI.with(|u| u.borrow().serial.clone());
        let presenting = crate::present_ui::is_open();
        start(ui, serial, false);
        UI.with(|u| u.borrow_mut().represent = presenting);
    }
}

fn changed(ui: &AppWindow) {
    let p = ui.global::<Phone>();
    let (place, off) = (Place::from_index(p.get_sound_place()), p.get_screen_off());
    let old = prefs();
    save_prefs(|x| {
        x.place = place;
        x.screen_off = off;
        x.awake = p.get_awake();
        x.clipboard = p.get_clipboard();
        x.on_top = p.get_on_top();
        x.show_taps = p.get_show_taps();
    });
    if UI.with(|u| u.borrow().demo) {
        return;
    }
    // Show taps: on the phone now (it stays as set until changed; scrcpy's own restore only
    // runs when the window it started closes).
    let taps = p.get_show_taps();
    if taps != old.show_taps {
        let serial = UI.with(|u| u.borrow().serial.clone());
        if !serial.is_empty() {
            std::thread::spawn(move || {
                let _ = phone::set_show_touches(&serial, taps);
            });
        }
    }
    // Now, while mirroring: the screen off / on, the sound on / off.
    if off != old.screen_off {
        with_session(|s| s.send(Control::ScreenPower(!off)));
    }
    // Where the sound plays: now, not the next time. The phone only takes it at the start, so
    // what runs starts again (a second or so); Present comes back with it.
    let effective = |p: Place, sound_only: bool| if sound_only && p == Place::Phone { Place::Pc } else { p };
    let now = state(ui);
    if (now == st::MIRROR || now == st::SOUND)
        && effective(place, now == st::SOUND) != effective(old.place, now == st::SOUND)
    {
        let serial = UI.with(|u| u.borrow().serial.clone());
        let presenting = crate::present_ui::is_open();
        start(ui, serial, now == st::SOUND);
        UI.with(|u| u.borrow_mut().represent = presenting);
    }
}

fn with_session(f: impl FnOnce(&Session)) {
    if let Some(s) = UI.with(|u| u.borrow().session.clone()) {
        f(&s);
    }
}

// ---------------------------------------------------------------------------------------------
// The tools, adb, the phones

fn get_tools(ui: &AppWindow) {
    if UI.with(|u| u.borrow().demo) {
        return;
    }
    set_state(ui, st::GETTING);
    let p = ui.global::<Phone>();
    p.set_tools_progress(0.0);
    p.set_tools_text("Getting scrcpy's phone tools…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let cancel = AtomicBool::new(false);
        let w2 = weak.clone();
        let mut last = Instant::now() - Duration::from_secs(1);
        let res = phone::install(&cancel, |d, t| {
            if last.elapsed() < Duration::from_millis(150) {
                return;
            }
            last = Instant::now();
            let text = match t {
                Some(t) => format!(
                    "Downloading scrcpy {}  ·  {} / {}",
                    phone::VERSION,
                    fmt::bytes(d as f64),
                    fmt::bytes(t as f64)
                ),
                None => format!("Downloading scrcpy {}  ·  {}", phone::VERSION, fmt::bytes(d as f64)),
            };
            let f = t.map(|t| d as f32 / t.max(1) as f32).unwrap_or(0.0);
            let _ = w2.upgrade_in_event_loop(move |ui| {
                let p = ui.global::<Phone>();
                p.set_tools_progress(f);
                p.set_tools_text(text.into());
            });
        });
        let _ = weak.upgrade_in_event_loop(move |ui| match res {
            Ok(()) => {
                set_state(&ui, st::PHONES);
                shown(&ui, ui.global::<Phone>().get_tab() == 1);
            }
            Err(e) => {
                set_state(&ui, st::NO_TOOLS);
                let p = ui.global::<Phone>();
                p.set_detail(neria_core::tools::explain(&e, "scrcpy").into());
                p.set_problem(true);
            }
        });
    });
}

/// The tab shows or hides: adb starts the first time, phones are looked at while it shows.
fn shown(ui: &AppWindow, on: bool) {
    UI.with(|u| u.borrow_mut().shown = on);
    if !on || UI.with(|u| u.borrow().demo) || !phone::ready() {
        return;
    }
    if !UI.with(|u| std::mem::replace(&mut u.borrow_mut().server, true)) {
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let res = phone::start_server();
            // Paired phones on this Wi-Fi reconnect a moment after the server starts.
            std::thread::sleep(Duration::from_millis(1500));
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if let Err(e) = res {
                    UI.with(|u| u.borrow_mut().server = false);
                    let p = ui.global::<Phone>();
                    p.set_detail(format!("Neria's adb didn't start: {e}").into());
                    p.set_problem(true);
                    return;
                }
                look(&ui);
            });
        });
    } else {
        look(ui);
    }
}

fn tick(ui: &AppWindow) {
    if UI.with(|u| u.borrow().demo) {
        return;
    }
    let s = state(ui);
    if s == st::MIRROR || s == st::SOUND || s == st::STARTING {
        live(ui);
    }
    if s == st::MIRROR || s == st::SOUND {
        watch_cable(ui);
    }
    // Dev hook NERIA_PHONE_MIRROR: mirror the first phone that's ready, once, with no click.
    static AUTO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if s == st::PHONES
        && std::env::var_os("NERIA_PHONE_MIRROR").is_some()
        && !AUTO.load(std::sync::atomic::Ordering::Relaxed)
    {
        let first = UI.with(|u| u.borrow().devices.iter().find(|d| d.state == State::Ready).map(|d| d.serial.clone()));
        if let Some(serial) = first {
            AUTO.store(true, std::sync::atomic::Ordering::Relaxed);
            start(ui, serial, false);
        }
    }
    if UI.with(|u| u.borrow().shown && u.borrow().server) && (s == st::CONNECT || s == st::PHONES) {
        look(ui);
    }
}

/// Look at what adb sees (and, while the QR shows, the network); off the UI thread.
fn look(ui: &AppWindow) {
    if UI.with(|u| std::mem::replace(&mut u.borrow_mut().looking, true)) {
        return;
    }
    let pairing = UI.with(|u| u.borrow().pairing.clone()).filter(|_| state(ui) == st::CONNECT);
    let known: Vec<String> = UI.with(|u| u.borrow().infos.keys().cloned().collect());
    let held = UI.with(|u| u.borrow().held.clone());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut devices = phone::devices().unwrap_or_default();
        // Remembered phones with Wireless debugging on: connected once each, by us.
        if !phone::reconnect(&devices, &held).is_empty() {
            devices = phone::devices().unwrap_or_default();
        }
        let visible = phone::visible_ids();
        let mut infos = Vec::new();
        for d in devices.iter().filter(|d| d.state == State::Ready && !known.contains(&d.serial)) {
            if let Ok(i) = phone::info(&d.serial) {
                phone::remember(&i, d.wifi);
                infos.push((d.serial.clone(), i));
            }
        }
        let seen = pairing.as_ref().map(phone::look);
        // Scanned: pair and connect here, then mirror.
        let mut paired = None;
        if let (Some(p), Some(Some(svc))) = (&pairing, seen.as_ref().map(|s| s.scanned.clone())) {
            paired = Some(
                phone::pair(&svc.addr, &p.password)
                    .and_then(|_| phone::wait_connected(svc.ip(), Duration::from_secs(20))),
            );
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            UI.with(|u| {
                let mut u = u.borrow_mut();
                u.looking = false;
                u.devices = devices;
                u.visible = visible;
                u.infos.extend(infos);
            });
            // A failed pairing's note stays until the next scan.
            let failed = UI.with(|u| u.borrow().pair_failed.is_some_and(|t| t.elapsed() < Duration::from_secs(20)));
            if let Some(seen) = seen.filter(|s| !failed || s.scanned.is_some()) {
                let p = ui.global::<Phone>();
                p.set_step(if seen.scanned.is_some() {
                    4
                } else if seen.wireless_on {
                    3
                } else {
                    1
                });
                p.set_qr_note(
                    if seen.scanned.is_some() {
                        "Scanned: pairing…"
                    } else if seen.wireless_on {
                        "A phone with Wireless debugging is on this Wi-Fi. Scan the code with it."
                    } else {
                        "Looking for your phone on this Wi-Fi…"
                    }
                    .into(),
                );
            }
            match paired {
                Some(Ok(d)) => {
                    UI.with(|u| u.borrow_mut().pairing = None);
                    set_state(&ui, st::PHONES);
                    show_phones(&ui);
                    start(&ui, d.serial, false);
                    return;
                }
                Some(Err(e)) => {
                    // A new code for another try (a password works once).
                    connect_screen(&ui);
                    UI.with(|u| u.borrow_mut().pair_failed = Some(Instant::now()));
                    ui.global::<Phone>().set_qr_note(format!("{e} Scan the new code.").into());
                    return;
                }
                None => {}
            }
            // Waiting on the QR and a phone turned up (USB, or it reconnected): show it.
            if state(&ui) == st::CONNECT && UI.with(|u| u.borrow().devices.iter().any(|d| d.state != State::Offline)) {
                UI.with(|u| u.borrow_mut().pairing = None);
                set_state(&ui, st::PHONES);
            }
            if state(&ui) == st::PHONES {
                show_phones(&ui);
            }
            // The pairing code fallback: the phone asking for a code.
            if ui.global::<Phone>().get_code_open() {
                find_code_target(&ui);
            }
        });
    });
}

/// The phones list: those connected first, then the remembered ones not here.
fn show_phones(ui: &AppWindow) {
    if UI.with(|u| u.borrow().demo) {
        return;
    }
    let (devices, infos, visible) =
        UI.with(|u| (u.borrow().devices.clone(), u.borrow().infos.clone(), u.borrow().visible.clone()));
    let mut rows = Vec::new();
    let mut ids = Vec::new();
    for d in &devices {
        let info = infos.get(&d.serial);
        if let Some(i) = info {
            // One phone connected twice (it announced itself under two names, adb took
            // both): listed once.
            if !i.id.is_empty() && ids.contains(&i.id) {
                continue;
            }
            ids.push(i.id.clone());
        }
        // The same phone by cable and over Wi-Fi at once.
        let same =
            |x: &Device| info.is_some_and(|i| !i.id.is_empty() && infos.get(&x.serial).is_some_and(|j| j.id == i.id));
        let wifi = d.wifi || devices.iter().any(|x| x.wifi && same(x));
        let usb = !d.wifi || devices.iter().any(|x| !x.wifi && same(x));
        rows.push(PhoneRow {
            serial: d.serial.as_str().into(),
            id: info.map(|i| i.id.clone()).unwrap_or_default().into(),
            name: info
                .map(|i| i.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| if d.model.is_empty() { "Android phone".into() } else { d.model.clone() })
                .into(),
            hint: format!(
                "{}{}{}",
                match (usb, wifi) {
                    (true, true) => "USB and Wi-Fi",
                    (false, _) => "Wi-Fi",
                    _ => "USB",
                },
                info.map(|i| format!(" · Android {}", i.android)).unwrap_or_default(),
                if wifi { "" } else { " · unplug the cable to disconnect" }
            )
            .into(),
            here: true,
            wifi,
            visible: false,
            problem: phone::problem(d).unwrap_or("").into(),
        });
    }
    for k in phone::known() {
        if ids.contains(&k.id) {
            continue;
        }
        let seen = visible.contains(&k.id);
        rows.push(PhoneRow {
            serial: "".into(),
            id: k.id.as_str().into(),
            name: k.name.as_str().into(),
            hint: format!(
                "{} · Android {}{}",
                if k.wifi { "Wi-Fi" } else { "USB" },
                k.android,
                if seen {
                    " · disconnected"
                } else if k.wifi {
                    " · turn on Wireless debugging and it connects"
                } else {
                    " · plug it in"
                }
            )
            .into(),
            here: false,
            wifi: false,
            visible: seen,
            problem: "".into(),
        });
    }
    let p = ui.global::<Phone>();
    let none = rows.is_empty();
    p.set_phones(ModelRc::new(VecModel::from(rows)));
    // Nothing connected, nothing known: straight to connecting.
    if none && state(ui) == st::PHONES {
        connect_screen(ui);
        return;
    }
    if state(ui) == st::PHONES
        && p.get_detail().is_empty()
        && phone::other_adb_running()
        && !devices.iter().any(|d| !d.wifi)
    {
        p.set_status("Android Studio's adb is running".into());
        p.set_detail("A phone on a USB cable may be taken by it. Close Android Studio, or use Wi-Fi.".into());
    }
}

/// Disconnect: stop mirroring it, let go of every Wi-Fi connection to it, and don't connect it
/// again by ourselves until Connect (or Neria starts again).
fn let_go(ui: &AppWindow, id: &str) {
    let serials: Vec<String> = UI.with(|u| {
        let u = u.borrow();
        u.infos
            .iter()
            .filter(|(_, i)| i.id == id)
            .map(|(s, _)| s.clone())
            .filter(|s| u.devices.iter().any(|d| d.serial == *s && d.wifi))
            .collect()
    });
    if UI.with(|u| serials.contains(&u.borrow().serial)) && state(ui) >= st::STARTING {
        stop(ui, None);
    }
    UI.with(|u| {
        let mut u = u.borrow_mut();
        if !u.held.iter().any(|h| h == id) {
            u.held.push(id.to_string());
        }
    });
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        phone::disconnect(&serials);
        let _ = weak.upgrade_in_event_loop(|ui| look(&ui));
    });
}

fn connect_screen(ui: &AppWindow) {
    let pairing = Pairing::new();
    let p = ui.global::<Phone>();
    if let Some(img) = qr_image(&pairing.qr_text()) {
        p.set_qr(img);
    }
    p.set_step(1);
    p.set_qr_note("Looking for your phone on this Wi-Fi…".into());
    p.set_code_problem("".into());
    UI.with(|u| u.borrow_mut().pairing = Some(pairing));
    set_state(ui, st::CONNECT);
}

/// The QR as a crisp image (one pixel per module, drawn pixelated).
fn qr_image(text: &str) -> Option<slint::Image> {
    let (w, m) = phone::qr(text).ok()?;
    let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(w as u32, w as u32);
    for (px, dark) in buf.make_mut_slice().iter_mut().zip(m) {
        let v = if dark { 0 } else { 255 };
        *px = slint::Rgba8Pixel { r: v, g: v, b: v, a: 255 };
    }
    Some(slint::Image::from_rgba8(buf))
}

fn find_code_target(ui: &AppWindow) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let svc = phone::services()
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.kind == phone::Kind::Pairing && !s.instance.starts_with("neria-"));
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.global::<Phone>()
                .set_code_target(svc.map(|s| format!("a phone at {}", s.ip())).unwrap_or_default().into());
        });
    });
}

fn pair_code(ui: &AppWindow, code: String) {
    let p = ui.global::<Phone>();
    if code.chars().filter(|c| c.is_ascii_digit()).count() != 6 {
        p.set_code_problem("The code is 6 digits, shown on the phone under \"Pair device with pairing code\".".into());
        return;
    }
    p.set_code_problem("Pairing…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let svc = phone::services()
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.kind == phone::Kind::Pairing && !s.instance.starts_with("neria-"));
        let res = match svc {
            None => Err(anyhow::anyhow!(
                "Neria doesn't see the phone asking for a code. Open \"Pair device with pairing code\" on it (same Wi-Fi)."
            )),
            Some(s) => phone::pair(&s.addr, &code).and_then(|_| phone::wait_connected(s.ip(), Duration::from_secs(20))),
        };
        let _ = weak.upgrade_in_event_loop(move |ui| match res {
            Ok(d) => {
                let p = ui.global::<Phone>();
                p.set_code_problem("".into());
                p.set_code_open(false);
                UI.with(|u| u.borrow_mut().pairing = None);
                set_state(&ui, st::PHONES);
                start(&ui, d.serial, false);
            }
            Err(e) => ui.global::<Phone>().set_code_problem(e.to_string().into()),
        });
    });
}

// ---------------------------------------------------------------------------------------------
// Mirroring and Sound only

fn name_of(serial: &str) -> String {
    UI.with(|u| u.borrow().infos.get(serial).map(|i| i.name.clone())).unwrap_or_else(|| "the phone".into())
}

/// Where a file dropped on the window goes, if it goes to a phone: the Capture page's Phone
/// tab with a phone there (the one mirrored, else the first ready one). Its serial and name.
pub fn drop_target(ui: &AppWindow) -> Option<(String, String)> {
    if ui.get_page() != 3 || ui.global::<Phone>().get_tab() != 1 || UI.with(|u| u.borrow().demo) {
        return None;
    }
    let serial = UI.with(|u| {
        let u = u.borrow();
        match &u.session {
            Some(s) => Some(s.serial.clone()),
            None => u.devices.iter().find(|d| d.state == State::Ready).map(|d| d.serial.clone()),
        }
    })?;
    let name = name_of(&serial);
    Some((serial, name))
}

/// Files dropped on the Phone tab: an app installed, anything else sent to its Download
/// folder. Off the UI thread; a toast says how it went.
pub fn dropped(ui: &AppWindow, serial: String, name: String, files: Vec<PathBuf>) {
    let is_apk = |p: &PathBuf| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("apk"));
    let (apks, others): (Vec<PathBuf>, Vec<PathBuf>) = files.into_iter().filter(|p| p.is_file()).partition(is_apk);
    if apks.is_empty() && others.is_empty() {
        crate::toast_info(ui, "Drop files, not folders, to send them to the phone.");
        return;
    }
    let file_name = |p: &PathBuf| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let what = match (apks.len(), others.len()) {
        (1, 0) => format!("Installing {} on {name}…", file_name(&apks[0])),
        (0, 1) => format!("Sending {} to {name}…", file_name(&others[0])),
        (a, o) => format!("Sending {} to {name}…", plural(a + o, "file")),
    };
    crate::toast_info(ui, &what);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut said = Vec::new();
        for apk in &apks {
            match phone::install_apk(&serial, apk) {
                Ok(()) => said.push(format!("Installed {} on {name}", file_name(apk))),
                Err(e) => said.push(format!("Couldn't install {}: {e}", file_name(apk))),
            }
        }
        if !others.is_empty() {
            let one = if others.len() == 1 { file_name(&others[0]) } else { plural(others.len(), "file") };
            match phone::send_files(&serial, &others) {
                Ok(()) => said.push(format!("Sent {one} to {name}'s Download folder")),
                Err(e) => said.push(format!("Couldn't send {one}: {e}")),
            }
        }
        let failed = said.iter().any(|s| s.starts_with("Couldn't"));
        let text = said.join(". ");
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if failed { crate::toast_problem(&ui, &text) } else { crate::show_toast(&ui, &text) }
        });
    });
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

/// Mirror the phone (or play its sound alone), replacing what runs.
fn start(ui: &AppWindow, serial: String, sound_only: bool) {
    if UI.with(|u| u.borrow().demo) || serial.is_empty() {
        return;
    }
    stop(ui, Some(st::STARTING));
    let pr = prefs();
    // Best stalled on this phone earlier while mirroring: Standard until it's picked again.
    let look = if UI.with(|u| u.borrow().stepped_back) { phone::quality_look(Quality::Standard) } else { pr.look() };
    let name = name_of(&serial);
    let p = ui.global::<Phone>();
    p.set_name(name.as_str().into());
    p.set_status(format!("Connecting to {name}…").into());
    p.set_detail("".into());
    p.set_problem(false);
    p.set_tech("".into());
    p.set_frame(slint::Image::default());
    UI.with(|u| u.borrow_mut().serial = serial.clone());
    let opts = if sound_only {
        Options { keep_on_phone: pr.place == Place::Both, ..Options::sound_only() }
    } else {
        // NERIA_PHONE_PLAIN: what Present asks the phone for (no sound, the encoder left alone).
        let plain = std::env::var_os("NERIA_PHONE_PLAIN").is_some();
        Options {
            screen_off: pr.screen_off,
            stay_awake: pr.awake,
            show_touches: pr.show_taps,
            plain,
            audio: !plain && pr.place.here(),
            keep_on_phone: pr.place == Place::Both,
            camera: match p.get_show() {
                1 => Some(phone::Facing::Back),
                2 => Some(phone::Facing::Front),
                _ => None,
            },
            ..Options::default()
        }
        .with_look(look)
    };
    // Best is watched for the phone not keeping up; Standard to fall back on.
    let best = !sound_only && look != phone::quality_look(Quality::Standard) && pr.quality() == Quality::Best;
    let standard = opts.clone().with_look(phone::quality_look(Quality::Standard));
    let keep = pr.keep;
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        // Its name before anything else, for the tab and the status line.
        if let Ok(i) = phone::info(&serial) {
            let (w, sr) = (weak.clone(), serial.clone());
            let _ = w.upgrade_in_event_loop(move |ui| {
                ui.global::<Phone>().set_name(i.name.as_str().into());
                // The camera needs Android 12.
                let why = if i.sdk > 0 && i.sdk < phone::CAMERA_SDK {
                    format!("{} runs Android {}: showing its camera needs Android 12 or newer.", i.name, i.android)
                } else {
                    String::new()
                };
                ui.global::<Phone>().set_camera_why(why.into());
                ui.global::<Phone>().set_status(format!("Connecting to {}…", i.name).into());
                UI.with(|u| u.borrow_mut().infos.insert(sr, i));
            });
        }
        // Best the phone can't start: Standard instead.
        let mut stepped = false;
        let session = match Session::start(&serial, opts) {
            Err(_) if best => {
                stepped = true;
                Session::start(&serial, standard)
            }
            r => r,
        };
        let res = session.and_then(|s| {
            let s = Arc::new(s);
            let buffer = if s.video.is_some() { Some(Arc::new(PhoneBuffer::start(&s, keep)?)) } else { None };
            Ok((s, buffer))
        });
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if state(&ui) != st::STARTING {
                return;
            }
            if stepped {
                UI.with(|u| u.borrow_mut().stepped_back = true);
                show_quality(&ui);
            }
            UI.with(|u| u.borrow_mut().on_best = best && !stepped);
            match res {
                Ok((s, buffer)) => running(&ui, s, buffer, sound_only),
                Err(e) => {
                    set_state(&ui, st::PHONES);
                    let p = ui.global::<Phone>();
                    p.set_status(
                        format!("Couldn't mirror {}", name_of(&UI.with(|u| u.borrow().serial.clone()))).into(),
                    );
                    p.set_detail(e.to_string().into());
                    p.set_problem(true);
                    show_phones(&ui);
                }
            }
        });
    });
    set_state(ui, st::STARTING);
}

fn running(ui: &AppWindow, s: Arc<Session>, buffer: Option<Arc<PhoneBuffer>>, sound_only: bool) {
    let pr = prefs();
    let p = ui.global::<Phone>();
    p.set_name(s.name.as_str().into());
    let speaker = if (sound_only || pr.place.here()) && s.audio.is_some() {
        match Speaker::start(&s, pr.volume) {
            Ok(sp) => Some(sp),
            Err(e) => {
                p.set_detail(e.to_string().into());
                None
            }
        }
    } else {
        None
    };
    if !s.no_sound.is_empty() && (sound_only || pr.place.here()) {
        p.set_detail(s.no_sound.as_str().into());
    }
    let url = s.video.as_ref().map(|v| v.url());
    UI.with(|u| {
        let mut u = u.borrow_mut();
        u.session = Some(s.clone());
        u.buffer = buffer;
        u.speaker = speaker;
        u.usb_offer = None;
        u.usb_declined = false;
        u.clip_seen = 0;
        u.clip_text = None;
    });
    p.set_usb_offer(false);
    if let Some(n) = UI.with(|u| u.borrow_mut().note.take()) {
        p.set_detail(n.into());
    }
    if let Some(url) = url {
        load_picture(ui, &url);
    }
    set_state(ui, if sound_only { st::SOUND } else { st::MIRROR });
    live(ui);
    take_key(ui);
    // Present was open when the sound's place changed: open it again. Dev hook
    // NERIA_PHONE_PRESENT: present as soon as mirroring starts.
    let hook = std::env::var_os("NERIA_PHONE_PRESENT").is_some();
    if !sound_only && (UI.with(|u| std::mem::take(&mut u.borrow_mut().represent)) || hook) {
        present(ui);
    }
}

/// Stop what runs (mirror, sound, recording, presenting); `then` the state to show (the
/// phones list when `None`).
fn stop(ui: &AppWindow, then: Option<i32>) {
    let (session, buffer, speaker, rec) = UI.with(|u| {
        let mut u = u.borrow_mut();
        u.hotkey = None;
        (u.session.take(), u.buffer.take(), u.speaker.take(), u.recording.take())
    });
    if crate::present_ui::is_open() {
        crate::present_ui::close();
        taps_after_present(session.as_ref().map(|s| s.serial.clone()));
    }
    stop_picture(ui);
    let p = ui.global::<Phone>();
    p.set_recording(false);
    p.set_presenting(false);
    // A recording going is saved, not lost.
    if let Some(r) = rec {
        finish_recording(ui, r);
    }
    drop(speaker);
    std::thread::spawn(move || {
        drop(buffer);
        if let Some(s) = session {
            s.stop();
        }
    });
    if let Some(s) = then {
        set_state(ui, s);
    } else if state(ui) >= st::STARTING {
        set_state(ui, st::PHONES);
        p.set_status("".into());
        p.set_detail("".into());
        show_phones(ui);
    }
}

/// The clipboard both ways while mirroring (Share the clipboard): the phone's newest copy
/// onto this PC's clipboard, this PC's newest onto the phone's.
fn share_clipboard(s: &Session) {
    if !prefs().clipboard || s.video.is_none() {
        return;
    }
    let (n, phone_text) = s.clipboard.lock().unwrap().clone();
    let mut pc = arboard::Clipboard::new().ok();
    let pc_text = pc.as_mut().and_then(|c| c.get_text().ok());
    UI.with(|u| {
        let mut u = u.borrow_mut();
        if n > u.clip_seen {
            u.clip_seen = n;
            if !phone_text.is_empty() && Some(&phone_text) != pc_text.as_ref() {
                if let Some(c) = pc.as_mut() {
                    let _ = c.set_text(phone_text.clone());
                }
                u.clip_text = Some(phone_text);
                return;
            }
        }
        if let Some(t) = pc_text.filter(|t| !t.is_empty())
            && u.clip_text.as_ref() != Some(&t)
        {
            s.send(Control::SetClipboard { text: t.clone(), paste: false });
            u.clip_text = Some(t);
        }
    });
}

/// The status lines while it runs, and noticing it ended by itself.
fn live(ui: &AppWindow) {
    let (session, buffer, rec) =
        UI.with(|u| (u.borrow().session.clone(), u.borrow().buffer.clone(), u.borrow().recording.clone()));
    let p = ui.global::<Phone>();
    let Some(s) = session else { return };
    if s.running() {
        share_clipboard(&s);
    }
    if !s.running() {
        let why = s.problem.lock().unwrap().clone();
        // The cable came out: carry on over Wi-Fi when the phone is there too.
        if !s.serial.contains(':') && !s.serial.contains("._adb-tls-connect.") && fall_back_to_wifi(ui, &s, &why) {
            return;
        }
        stop(ui, None);
        p.set_status(format!("{} stopped", s.name).into());
        p.set_detail(why.into());
        p.set_problem(true);
        return;
    }
    // Best and the phone stopped sending its picture (some encoders can't keep up): mirror
    // on Standard instead, and say so.
    if s.video.is_some() && UI.with(|u| u.borrow().on_best) && s.started.elapsed() > Duration::from_secs(4) {
        let l = s.live.lock().unwrap().clone();
        let quiet = l.last_packet.map(|t| t.elapsed() > Duration::from_secs(3)).unwrap_or(true);
        if quiet {
            UI.with(|u| {
                let mut u = u.borrow_mut();
                u.on_best = false;
                u.stepped_back = true;
            });
            show_quality(ui);
            let serial = s.serial.clone();
            let presenting = crate::present_ui::is_open();
            start(ui, serial, false);
            UI.with(|u| u.borrow_mut().represent = presenting);
            return;
        }
    }
    // The picture stopped (its player let go of the stream, or stalls while the phone goes
    // on): load it again.
    if let Some(v) = &s.video {
        let (idle, pos) = UI.with(|u| {
            let u = u.borrow();
            let m = u.mpv.as_ref();
            (m.and_then(|m| m.get_flag("idle-active")).unwrap_or(false), m.and_then(|m| m.get_f64("time-pos")))
        });
        let frames = s.live.lock().unwrap().frames;
        let stalled = UI.with(|u| {
            let mut u = u.borrow_mut();
            let same = pos.is_some() && pos == u.last_pos && frames > u.last_frames + 5;
            if !same || u.last_pos.is_none() {
                u.last_pos = pos;
                u.last_frames = frames;
            }
            same
        });
        if let Some(log) = std::env::var_os("NERIA_PHONE_DEBUG") {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
                let l = s.live.lock().unwrap().clone();
                let _ = writeln!(
                    f,
                    "{} frames {frames} pos {pos:?} idle {idle} stalled {stalled} readers {} | hooks {} updates {} draws {} | has-frame {} image {:?} target {:?} | last video {:?} ago, sound packets {}, controls {} (slowest {} ms)",
                    chrono::Local::now().format("%H:%M:%S"),
                    v.readers(),
                    HOOKS.swap(0, std::sync::atomic::Ordering::Relaxed),
                    UPDATES.swap(0, std::sync::atomic::Ordering::Relaxed),
                    DRAWS.swap(0, std::sync::atomic::Ordering::Relaxed),
                    p.get_has_frame(),
                    p.get_frame().size(),
                    UI.with(|u| u.borrow().target.as_ref().map(|t| (t.w, t.h))),
                    l.last_packet.map(|t| t.elapsed().as_millis()),
                    l.audio_packets,
                    l.controls,
                    l.slowest_control_ms
                );
            }
        }
        if (idle || stalled || v.readers() == 0) && UI.with(|u| u.borrow().renderer.is_some()) {
            UI.with(|u| {
                let mut u = u.borrow_mut();
                u.last_pos = None;
            });
            let url = v.url();
            UI.with(|u| {
                if let Some(m) = &u.borrow().mpv {
                    let _ = m.command(&["loadfile", &url, "replace"]);
                }
            });
        }
    }
    let l = s.live.lock().unwrap().clone();
    if l.width > 0 {
        p.set_frame_w(l.width as i32);
        p.set_frame_h(l.height as i32);
    }
    let how = if s.serial.contains(':') || s.serial.contains("._adb-tls-connect.") { "Wi-Fi" } else { "USB" };
    if s.video.is_some() {
        let keep = buffer.as_ref().map(|b| b.keep).unwrap_or(60);
        let presenting = crate::present_ui::is_open();
        p.set_status(format!("{} {}", if presenting { "Presenting" } else { "Mirroring" }, s.name).into());
        if !p.get_problem() && p.get_detail().is_empty() {
            p.set_detail(format!("Keeping the last {}", keep_text(keep)).into());
        }
        let secs = s.started.elapsed().as_secs_f64().max(1.0);
        p.set_tech(format!("{} × {} · {:.0} fps · {}", l.width, l.height, l.frames as f64 / secs, how).into());
        if let Some(b) = &buffer {
            let why = b.problem();
            if !why.is_empty() {
                p.set_detail(format!("The replay stopped: {why}").into());
                p.set_problem(true);
            }
        }
    } else {
        p.set_status(format!("Playing from {}", s.name).into());
        p.set_tech(format!("Sound only · {how}").into());
    }
    match &rec {
        Some(r) => {
            p.set_recording(true);
            p.set_rec_time(fmt::duration(r.started.elapsed().as_secs_f64()).into());
        }
        None => p.set_recording(false),
    }
}

fn keep_text(secs: u32) -> String {
    if secs.is_multiple_of(60) { format!("{} min", secs / 60) } else { format!("{secs} s") }
}

/// Where saves go: the Videos folder (Audio for sound alone, Photos for screenshots).
fn out_dir(kind: folders::Kind) -> PathBuf {
    let settings = UI.with(|u| u.borrow().settings.clone());
    settings
        .map(|s| folders::for_kind(&s.lock().unwrap(), kind))
        .unwrap_or_else(|| folders::for_kind(&Settings::load(), kind))
}

fn stamp_name(name: &str) -> String {
    format!("{name} {}", chrono::Local::now().format("%Y-%m-%d %H-%M"))
}

/// Clip the last 30 s (the button, the save key, the tray).
pub fn clip(ui: &AppWindow) {
    let buffer = UI.with(|u| u.borrow().buffer.clone());
    let Some(b) = buffer else { return };
    if UI.with(|u| std::mem::replace(&mut u.borrow_mut().saving, true)) {
        return;
    }
    let secs = prefs().clip;
    let name = stamp_name(&ui.global::<Phone>().get_name());
    let out = out_dir(folders::Kind::Video);
    let dir = b.dir.clone();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let res = phone::save_from(&dir, secs, &out, &name);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            UI.with(|u| u.borrow_mut().saving = false);
            match res {
                Ok(f) => {
                    let text = format!("Saved the last {} of {}", keep_text(secs), ui.global::<Phone>().get_name());
                    crate::capture_ui::saved_file(&ui, &f, &name, "Saved", &text, true, true);
                }
                Err(e) => crate::capture_ui::toast(&ui, "Not saved", &e.to_string(), false, None),
            }
        });
    });
}

fn record(ui: &AppWindow) {
    let rec = UI.with(|u| u.borrow_mut().recording.take());
    if let Some(r) = rec {
        finish_recording(ui, r);
        ui.global::<Phone>().set_recording(false);
        return;
    }
    let Some(s) = UI.with(|u| u.borrow().session.clone()) else { return };
    let kind = if s.video.is_some() { folders::Kind::Video } else { folders::Kind::Audio };
    match Recording::start(&s, &out_dir(kind), &stamp_name(&s.name)) {
        Ok(r) => {
            UI.with(|u| u.borrow_mut().recording = Some(Arc::new(r)));
            live(ui);
        }
        Err(e) => crate::capture_ui::toast(ui, "Not recording", &e.to_string(), false, None),
    }
}

fn finish_recording(ui: &AppWindow, r: Arc<Recording>) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let res = r.stop();
        let len = r.started.elapsed().as_secs_f64();
        let _ = weak.upgrade_in_event_loop(move |ui| match res {
            Ok(f) => {
                let name = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let text = format!("Recorded {} of {}", fmt::duration(len), ui.global::<Phone>().get_name());
                crate::capture_ui::saved_file(&ui, &f, &name, "Saved", &text, false, true);
            }
            Err(e) => crate::capture_ui::toast(&ui, "Not saved", &e.to_string(), false, None),
        });
    });
}

fn screenshot(ui: &AppWindow) {
    let Some(s) = UI.with(|u| u.borrow().session.clone()) else { return };
    let dir = out_dir(folders::Kind::Photo);
    let _ = std::fs::create_dir_all(&dir);
    let file = neria_core::clip::unique_path(&dir, &neria_core::music::sanitize(&stamp_name(&s.name)), "png");
    // What the page shows, at the phone's full size, at once (mpv has the frame).
    let shot = UI.with(|u| {
        u.borrow().mpv.as_ref().map(|m| m.command(&["screenshot-to-file", &file.to_string_lossy(), "video"]))
    });
    if let Some(Ok(())) = shot
        && file.is_file()
    {
        show_screenshot(ui, &file);
        return;
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let res = s.screenshot(&file);
        let _ = weak.upgrade_in_event_loop(move |ui| match res {
            Ok(()) => show_screenshot(&ui, &file),
            Err(e) => crate::capture_ui::toast(&ui, "No screenshot", &e.to_string(), false, None),
        });
    });
}

fn show_screenshot(ui: &AppWindow, file: &Path) {
    crate::capture_ui::toast(
        ui,
        "Saved",
        &format!("Screenshot: {}", file.file_name().unwrap_or_default().to_string_lossy()),
        false,
        None,
    );
    crate::background_ui::notify_saved(ui, "Saved a screenshot of the phone", file);
}

fn present(ui: &AppWindow) {
    let p = ui.global::<Phone>();
    if crate::present_ui::is_open() {
        crate::present_ui::close();
        p.set_presenting(false);
        taps_after_present(UI.with(|u| u.borrow().session.as_ref().map(|s| s.serial.clone())));
        presenting_ended(ui);
        return;
    }
    let Some(s) = UI.with(|u| u.borrow().session.clone()) else { return };
    let Some(url) = s.video.as_ref().map(|v| v.url()) else { return };
    let pr = prefs();
    let (w, h) = s.size();
    let weak = ui.as_weak();
    let hooks = crate::present_ui::Hooks {
        touch: Box::new(touch),
        scroll: Box::new(scroll),
        typed: Box::new(typed),
        press: Box::new(|c| with_session(|s| s.press(c))),
        // Closed by hand: the page's picture comes back.
        closed: Box::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<Phone>().set_presenting(false);
                taps_after_present(UI.with(|u| u.borrow().session.as_ref().map(|s| s.serial.clone())));
                presenting_ended(&ui);
            }
        }),
    };
    let o = crate::present_ui::Open {
        title: phone::present_title(&s.name),
        url: &url,
        size: (w, h),
        on_top: pr.on_top,
        beside: ui.window(),
    };
    match crate::present_ui::open(o, hooks) {
        Ok(()) => {
            // One picture at a time: the page's steps aside. The sound keeps playing from
            // Neria, which is what a window share in Discord or OBS carries.
            stop_picture(ui);
            p.set_presenting(true);
            if pr.show_taps {
                let serial = s.serial.clone();
                std::thread::spawn(move || {
                    let _ = phone::set_show_touches(&serial, true);
                });
            }
        }
        Err(e) => crate::capture_ui::toast(ui, "Can't present", &e.to_string(), false, None),
    }
}

/// Show taps was for the people watching: off again once presenting ends (unless the
/// session itself was started with it).
fn taps_after_present(serial: Option<String>) {
    let Some(serial) = serial else { return };
    if prefs().show_taps && !UI.with(|u| u.borrow().session.as_ref().is_some_and(|s| s.options.show_touches)) {
        std::thread::spawn(move || {
            let _ = phone::set_show_touches(&serial, false);
        });
    }
}

/// The id (serial number) of the phone mirrored now.
fn current_id(s: &Session) -> String {
    UI.with(|u| u.borrow().infos.get(&s.serial).map(|i| i.id.clone())).unwrap_or_default()
}

/// Mirror (or play) the same phone again on `serial`, Present with it if it was open, and say
/// `note` once it runs.
fn restart_on(ui: &AppWindow, serial: String, note: &str) {
    let sound_only = state(ui) == st::SOUND;
    let presenting = crate::present_ui::is_open();
    UI.with(|u| u.borrow_mut().note = Some(note.to_string()));
    start(ui, serial, sound_only);
    UI.with(|u| u.borrow_mut().represent = presenting);
}

/// Mirroring over Wi-Fi: has the same phone been plugged in? Then offer the cable.
fn watch_cable(ui: &AppWindow) {
    let Some(s) = UI.with(|u| u.borrow().session.clone()) else { return };
    let busy = UI.with(|u| {
        let u = u.borrow();
        u.watching || u.usb_declined || u.usb_offer.is_some()
    });
    let wifi = s.serial.contains(':') || s.serial.contains("._adb-tls-connect.");
    if busy || !wifi {
        return;
    }
    let id = current_id(&s);
    UI.with(|u| u.borrow_mut().watching = true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let devices = phone::devices().unwrap_or_default();
        let usb = phone::other_way(&devices, &id, false);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            UI.with(|u| u.borrow_mut().watching = false);
            if let Some(usb) = usb
                && UI.with(|u| u.borrow().session.as_ref().is_some_and(|x| Arc::ptr_eq(x, &s)))
            {
                UI.with(|u| u.borrow_mut().usb_offer = Some(usb));
                ui.global::<Phone>().set_usb_offer(true);
            }
        });
    });
}

/// Mirroring by cable stopped by itself: is the phone there over Wi-Fi (connected, or with
/// Wireless debugging on)? Then mirror on over Wi-Fi. False when it isn't (stop as before).
fn fall_back_to_wifi(ui: &AppWindow, s: &Arc<Session>, why: &str) -> bool {
    let id = current_id(s);
    if id.is_empty() {
        return false;
    }
    let sound_only = state(ui) == st::SOUND;
    let presenting = crate::present_ui::is_open();
    let name = s.name.clone();
    let why = why.to_string();
    // What ran stops now; the page says what's happening meanwhile.
    stop(ui, Some(st::STARTING));
    let p = ui.global::<Phone>();
    p.set_status(format!("Cable unplugged: finding {name} on the Wi-Fi…").into());
    p.set_detail("".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let mut devices = phone::devices().unwrap_or_default();
        let mut wifi = phone::other_way(&devices, &id, true);
        if wifi.is_none() && phone::connect_id(&id).is_ok() {
            devices = phone::devices().unwrap_or_default();
            wifi = phone::other_way(&devices, &id, true);
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if state(&ui) != st::STARTING {
                return;
            }
            match wifi {
                Some(serial) => {
                    UI.with(|u| u.borrow_mut().note = Some("Cable unplugged: mirroring over Wi-Fi now".into()));
                    start(&ui, serial, sound_only);
                    UI.with(|u| u.borrow_mut().represent = presenting);
                }
                None => {
                    set_state(&ui, st::PHONES);
                    let p = ui.global::<Phone>();
                    p.set_status(format!("{name} stopped").into());
                    p.set_detail(
                        format!("{why} Turn on Wireless debugging and Neria can carry on over Wi-Fi next time.").into(),
                    );
                    p.set_problem(true);
                    show_phones(&ui);
                }
            }
        });
    });
    true
}

/// A wheel turn on a picture (the page's or Present's), at `x`, `y` across it.
fn scroll(x: f32, y: f32, dy: f32) {
    with_session(|s| {
        let (w, h) = s.size();
        // A wheel notch is about 40 px here; the phone wants about one step.
        s.scroll((x * w as f32) as i32, (y * h as f32) as i32, 0.0, (dy / 40.0).clamp(-4.0, 4.0));
    })
}

/// Present closed (Stop presenting, or the window closed by hand): the page's picture and
/// sound come back.
fn presenting_ended(ui: &AppWindow) {
    let Some(s) = UI.with(|u| u.borrow().session.clone()) else { return };
    let pr = prefs();
    if pr.place.here()
        && s.audio.is_some()
        && UI.with(|u| u.borrow().speaker.is_none())
        && let Ok(sp) = Speaker::start(&s, pr.volume)
    {
        UI.with(|u| u.borrow_mut().speaker = Some(sp));
    }
    if let Some(v) = &s.video {
        load_picture(ui, &v.url());
    }
}

fn touch(kind: i32, x: f32, y: f32) {
    let Some(s) = UI.with(|u| u.borrow().session.clone()) else { return };
    let (w, h) = s.size();
    let (px, py) = ((x.clamp(0.0, 1.0) * w as f32) as i32, (y.clamp(0.0, 1.0) * h as f32) as i32);
    match kind {
        0 => {
            UI.with(|u| u.borrow_mut().down = true);
            s.touch(Touch::Down, px, py);
        }
        1 => {
            if UI.with(|u| std::mem::replace(&mut u.borrow_mut().down, false)) {
                s.touch(Touch::Up, px, py);
            }
        }
        _ => {
            if UI.with(|u| u.borrow().down) {
                s.touch(Touch::Move, px, py);
            }
        }
    }
}

/// Typing with the picture focused: letters as text, Backspace / Enter / arrows as keys.
fn typed(text: &str) {
    use phone::key;
    let code = match text {
        "\u{8}" => Some(key::DEL),
        "\n" | "\r" => Some(key::ENTER),
        "\t" => Some(61),
        "\u{f702}" => Some(21),
        "\u{f703}" => Some(22),
        "\u{f700}" => Some(19),
        "\u{f701}" => Some(20),
        "\u{7f}" => Some(112),
        _ => None,
    };
    with_session(|s| match code {
        Some(c) => s.press(c),
        None if !text.is_empty()
            && text.chars().all(|c| !c.is_control() && !('\u{f700}'..='\u{f8ff}').contains(&c)) =>
        {
            s.send(Control::Text(text.to_string()))
        }
        None => {}
    });
}

// ---------------------------------------------------------------------------------------------
// The save key

/// Hold the save key while mirroring and instant replay is off (instant replay's key clips
/// the phone too while it's on).
pub fn take_key(ui: &AppWindow) {
    if !mirroring() || crate::capture_ui::on() || UI.with(|u| u.borrow().hotkey.is_some()) {
        return;
    }
    let Ok(key) = hotkey::Hotkey::parse(&crate::capture_ui::save_key()) else { return };
    let weak = ui.as_weak();
    if let Ok(l) = hotkey::listen(key, move || {
        let _ = weak.upgrade_in_event_loop(|ui| {
            hotkey::confirm_sound();
            clip(&ui);
        });
    }) {
        UI.with(|u| u.borrow_mut().hotkey = Some(l));
    }
}

/// Instant replay turns on: it takes the key.
pub fn release_key() {
    UI.with(|u| u.borrow_mut().hotkey = None);
}

// ---------------------------------------------------------------------------------------------
// The picture: its own mpv, drawn from the rendering hook

fn load_picture(ui: &AppWindow, url: &str) {
    let ok = UI.with(|u| {
        let mut u = u.borrow_mut();
        if u.mpv.is_none() {
            let extra = [
                ("profile", "low-latency"),
                ("untimed", "yes"),
                ("cache", "no"),
                ("demuxer-lavf-format", "h264"),
                ("demuxer-lavf-o", "fflags=+nobuffer"),
                ("demuxer-lavf-probesize", "32"),
                ("demuxer-lavf-analyzeduration", "0.1"),
                ("video-latency-hacks", "yes"),
                ("aid", "no"),
                ("keep-open", "no"),
                ("pause", "no"),
            ];
            // Dev hook NERIA_PHONE_HWDEC: the decoder mpv uses for the phone ("no", "d3d11va"…).
            let hwdec = std::env::var("NERIA_PHONE_HWDEC").ok();
            let mut extra = extra.to_vec();
            if let Some(h) = hwdec.as_deref() {
                extra.push(("hwdec", h));
            }
            match Mpv::new(&paths::bin_dir().join("libmpv-2.dll"), &extra) {
                Ok(m) => u.mpv = Some(m),
                Err(e) => return Err(e.to_string()),
            }
        }
        if u.renderer.is_none() {
            u.pending = Some(url.to_string());
        } else if let Some(m) = &u.mpv {
            let _ = m.command(&["loadfile", url, "replace"]);
        }
        Ok(())
    });
    if let Err(e) = ok {
        crate::toast_problem(ui, &format!("Player unavailable: {e}"));
    }
    ui.window().request_redraw();
}

fn stop_picture(ui: &AppWindow) {
    UI.with(|u| {
        let u = u.borrow();
        if let Some(m) = &u.mpv {
            let _ = m.command(&["stop"]);
        }
    });
    UI.with(|u| {
        let mut u = u.borrow_mut();
        u.pending = None;
        u.shown_frame = false;
    });
    ui.global::<Phone>().set_frame(slint::Image::default());
    ui.global::<Phone>().set_has_frame(false);
}

/// Debug counts between two picture checks: rendering hooks, mpv's "new frame" calls, draws.
static HOOKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UPDATES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DRAWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// From the window's rendering hook: draw the phone's newest frame.
pub fn render(ui: &AppWindow, gl: &glow::Context) {
    HOOKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    UI.with(|u| {
        let mut u = u.borrow_mut();
        if u.mpv.is_none() || u.session.as_ref().is_none_or(|s| s.video.is_none()) {
            return;
        }
        if u.renderer.is_none() {
            let weak = ui.as_weak();
            let redraw = move || {
                UPDATES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        // A new frame in the same texture: the page's cached clip layer must
                        // be drawn again, or it keeps showing the first frame.
                        let p = ui.global::<Phone>();
                        p.set_frame_tick(p.get_frame_tick().wrapping_add(1) & 0x3fff_ffff);
                        ui.window().request_redraw();
                    }
                });
            };
            match u.mpv.as_ref().unwrap().create_gl_renderer(redraw) {
                Ok(r) => u.renderer = Some(r),
                Err(e) => {
                    crate::toast_problem(ui, &format!("Video renderer failed: {e}"));
                    return;
                }
            }
            if let Some(url) = u.pending.take() {
                let _ = u.mpv.as_ref().unwrap().command(&["loadfile", &url, "replace"]);
            }
            return;
        }
        let mpv = u.mpv.as_ref().unwrap();
        mpv.drain_events();
        let (dw, dh) = (mpv.get_i64("dwidth").unwrap_or(0), mpv.get_i64("dheight").unwrap_or(0));
        if dw <= 0 || dh <= 0 {
            return;
        }
        // Drawn at most at 1280 on its long side: the page shows it smaller than that.
        let scale = (1280.0 / dw.max(dh) as f64).min(1.0);
        let (w, h) = (((dw as f64) * scale).round() as i32, ((dh as f64) * scale).round() as i32);
        unsafe {
            if u.target.as_ref().is_none_or(|t| t.w != w || t.h != h) {
                if let Some(old) = u.target.take() {
                    gl.delete_framebuffer(old.fbo);
                    gl.delete_texture(old.tex);
                }
                match make_target(gl, w, h) {
                    Ok(t) => {
                        let image = slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                            t.tex.0,
                            [w as u32, h as u32].into(),
                        )
                        .build();
                        let p = ui.global::<Phone>();
                        p.set_frame(image);
                        p.set_has_frame(true);
                        p.set_frame_w(dw as i32);
                        p.set_frame_h(dh as i32);
                        u.target = Some(t);
                        u.shown_frame = true;
                        ui.window().request_redraw();
                    }
                    Err(_) => return,
                }
            }
            // Mirroring again at the same size: the texture is kept, but the page let go of it.
            if !u.shown_frame {
                let t = u.target.as_ref().unwrap();
                let image =
                    slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(t.tex.0, [w as u32, h as u32].into())
                        .build();
                let p = ui.global::<Phone>();
                p.set_frame(image);
                p.set_has_frame(true);
                p.set_frame_w(dw as i32);
                p.set_frame_h(dh as i32);
                u.shown_frame = true;
                ui.window().request_redraw();
            }
            let target = u.target.as_ref().unwrap();
            let prev_fbo = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
            let mut viewport = [0i32; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            if let Some(r) = &u.renderer {
                // Never wait for a frame's time: the raw stream's times are guesses (25 fps),
                // ahead of the clock whenever the phone sends faster, and this is the UI thread.
                r.render_with(target.fbo.0.get() as i32, w, h, false, false);
                DRAWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, NonZeroU32::new(prev_fbo as u32).map(glow::NativeFramebuffer));
            gl.viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        }
    });
}

/// The GL context goes: the renderer and the texture first (before the mpv they came from).
pub fn teardown(gl: &glow::Context) {
    UI.with(|u| {
        let mut u = u.borrow_mut();
        u.renderer = None;
        if let Some(t) = u.target.take() {
            unsafe {
                gl.delete_framebuffer(t.fbo);
                gl.delete_texture(t.tex);
            }
        }
    });
}

/// On quit: stop mirroring and Neria's adb (it's ours alone; the phone stays paired).
pub fn shutdown() {
    let (session, buffer, rec) = UI.with(|u| {
        let mut u = u.borrow_mut();
        u.speaker = None;
        (u.session.take(), u.buffer.take(), u.recording.take())
    });
    crate::present_ui::close();
    if let Some(r) = rec {
        let _ = r.stop();
    }
    drop(buffer);
    if let Some(s) = session {
        s.stop();
    }
    if UI.with(|u| u.borrow().server) {
        phone::stop_server();
    }
}

/// `NERIA_PHONE_FAKE`: mirror `phone::fake` (a test picture and a tone) through the real
/// way: relay, mpv, buffer, Clip the last. No adb, no phone.
fn fake(ui: &AppWindow) {
    let p = ui.global::<Phone>();
    p.set_tab(1);
    let f = std::env::temp_dir().join("neria-fake-phone.h264");
    if !f.is_file() {
        let _ = proc::command(paths::tool("ffmpeg"))
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=540x1200:rate=30",
                "-t",
                "6",
            ])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-bf",
                "0",
                "-g",
                "60",
                "-pix_fmt",
                "yuv420p",
                "-x264-params",
                "aud=1",
                "-f",
                "h264",
            ])
            .arg(&f)
            .status();
    }
    let started = std::fs::read(&f).map_err(anyhow::Error::from).and_then(|h264| {
        let stop = Arc::new(AtomicBool::new(false));
        std::mem::forget(stop.clone());
        let port = phone::fake::serve(h264, (540, 1200), 30, true, stop)?;
        let s = Arc::new(Session::connect(port, "fake", Options::default(), None, None)?);
        let b = Arc::new(PhoneBuffer::start(&s, prefs().keep)?);
        Ok((s, b))
    });
    match started {
        Ok((s, b)) => {
            UI.with(|u| u.borrow_mut().serial = "fake".into());
            set_state(ui, st::STARTING);
            running(ui, s, Some(b), false);
        }
        Err(e) => crate::toast_problem(ui, &format!("Fake phone: {e}")),
    }
}

// ---------------------------------------------------------------------------------------------
// Sample screens

/// A pretend phone screen for the design checks: a soft gradient with two cards.
fn demo_frame(w: u32, h: u32) -> slint::Image {
    let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
    let stops = [(0.0f32, [29u8, 53, 87]), (0.38, [69, 123, 157]), (0.75, [233, 196, 106]), (1.0, [244, 162, 97])];
    let pixels = buf.make_mut_slice();
    for y in 0..h {
        for x in 0..w {
            let t = ((y as f32 / h as f32) * 0.85 + (x as f32 / w as f32) * 0.15).clamp(0.0, 1.0);
            let k = stops.windows(2).find(|s| t >= s[0].0 && t <= s[1].0).unwrap_or(&stops[2..4]);
            let f = (t - k[0].0) / (k[1].0 - k[0].0).max(1e-3);
            let mut c: [u8; 3] =
                std::array::from_fn(|i| (k[0].1[i] as f32 + (k[1].1[i] as f32 - k[0].1[i] as f32) * f) as u8);
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let card = (0.1..0.9).contains(&fx) && ((0.2..0.52).contains(&fy) || (0.58..0.68).contains(&fy));
            let bar = fy < 0.035;
            if card || bar {
                let a = if bar {
                    0.75
                } else if fy < 0.55 {
                    1.18
                } else {
                    0.85
                };
                for v in c.iter_mut() {
                    *v = ((*v as f32) * a).min(255.0) as u8;
                }
            }
            pixels[(y * w + x) as usize] = slint::Rgba8Pixel { r: c[0], g: c[1], b: c[2], a: 255 };
        }
    }
    slint::Image::from_rgba8(buf)
}

/// `NERIA_PHONE_DEMO`: the Phone tab with sample data, nothing connects. `tools`, `getting`,
/// `connect`, `code`, `phones`, `mirror`, `landscape`, `rec`, `sound`, `problem`.
fn show_demo(ui: &AppWindow, what: &str) {
    let p = ui.global::<Phone>();
    p.set_tab(1);
    let rows = vec![
        PhoneRow {
            serial: "demo".into(),
            id: "a".into(),
            name: "Pixel 7".into(),
            hint: "Wi-Fi · Android 15".into(),
            here: true,
            wifi: true,
            visible: false,
            problem: "".into(),
        },
        PhoneRow {
            serial: "".into(),
            id: "b".into(),
            name: "Galaxy Tab S9".into(),
            hint: "USB · Android 14 · plug it in".into(),
            here: false,
            wifi: false,
            visible: false,
            problem: "".into(),
        },
    ];
    p.set_phones(ModelRc::new(VecModel::from(rows)));
    let mirror = |landscape: bool| {
        p.set_name("Pixel 7".into());
        let (w, h) = if landscape { (2400, 1080) } else { (1080, 2400) };
        p.set_frame(if landscape { demo_frame(480, 216) } else { demo_frame(216, 480) });
        p.set_has_frame(true);
        p.set_frame_w(w);
        p.set_frame_h(h);
        p.set_status("Mirroring Pixel 7".into());
        p.set_detail("Keeping the last 1 min".into());
        p.set_tech(format!("{w} × {h} · 60 fps · Wi-Fi").into());
        p.set_state(st::MIRROR);
    };
    match what {
        "tools" => p.set_state(st::NO_TOOLS),
        "getting" => {
            p.set_state(st::GETTING);
            p.set_tools_progress(0.46);
            p.set_tools_text(format!("Downloading scrcpy {}  ·  5.2 MB / 11.3 MB", phone::VERSION).into());
        }
        "connect" | "code" => {
            if let Some(img) = qr_image(&Pairing::new().qr_text()) {
                p.set_qr(img);
            }
            p.set_step(2);
            p.set_qr_note("A phone with Wireless debugging is on this Wi-Fi. Scan the code with it.".into());
            p.set_state(st::CONNECT);
            if what == "code" {
                p.set_code_open(true);
                p.set_code_target("a phone at 192.168.0.106".into());
                p.set_code_problem("That code didn't match. Check the 6 digits on the phone and try again.".into());
            }
        }
        "phones" => p.set_state(st::PHONES),
        "problem" => {
            p.set_state(st::PHONES);
            p.set_status("Pixel 7 stopped".into());
            p.set_detail("The phone stopped sending (unplugged, locked by a call, or Wi-Fi dropped).".into());
            p.set_problem(true);
        }
        "landscape" => mirror(true),
        "rec" => {
            mirror(true);
            p.set_recording(true);
            p.set_rec_time("2:14".into());
            p.set_presenting(true);
        }
        "switch" => {
            mirror(false);
            p.set_usb_offer(true);
        }
        "present" => {
            mirror(false);
            p.set_status("Presenting Pixel 7".into());
            p.set_detail("Keeping the last 1 min · sound in the window".into());
            p.set_presenting(true);
        }
        "sound" => {
            p.set_name("Pixel 7".into());
            p.set_status("Playing from Pixel 7".into());
            p.set_detail("Sound only · the phone's screen can stay off".into());
            p.set_tech("Sound only · Wi-Fi".into());
            p.set_volume(0.64);
            p.set_state(st::SOUND);
        }
        // Best was too much for the phone: the amber line under Quality.
        "stalled" => {
            mirror(false);
            UI.with(|u| u.borrow_mut().stepped_back = true);
            show_quality(ui);
        }
        _ => mirror(false),
    }
    let _ = Model::row_count(&p.get_phones());
}
