//! Live chat on the Clip page (`ClipChat` global, `components/chat-panel.slint`,
//! `dialogs/chat.slint`): the panel beside the player, the activity strip over the
//! timeline, Chat in this clip, and the chat a clip's export gets.
//!
//! Where the chat comes from:
//! - a live link (watching, or a recording being watched): read live (`chat::LiveChat`),
//!   timed on the clock and put on the player's timeline with the live anchor;
//! - a past stream: its saved chat, loaded on request ("Load chat": it takes a minute or two
//!   for a long stream);
//! - a file: its `.chat.json` next to it (recordings and past streams Neria saved).
//!
//! The panel is a window of messages Rust fills to the panel's height, newest at the bottom,
//! drawn by core `chat_draw` (the same drawing a burned-in clip gets).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use neria_core::chat::{self, ChatFile, Filter, LiveChat, LiveState, Message, Which};
use neria_core::chat_draw::{self, Fonts, Look, Pictures, RowExtras};
use neria_core::chat_out::{self, ChatJob, How, Spec};
use slint::{ComponentHandle, ModelRc, SharedPixelBuffer, VecModel};

use crate::{AppWindow, ChatRow, ClipChat, ClipView};

/// What a past stream's chat load has got so far.
#[derive(Default)]
struct Loading {
    count: usize,
    frac: Option<f64>,
    done: Option<Result<ChatFile, String>>,
}

/// What the rows were last drawn for; the same key means nothing to draw again.
type DrawKey = (i64, u64, i32, String, u32, u32, usize, bool, usize);
#[derive(Default)]
struct State {
    url: Option<String>,
    file: Option<PathBuf>,
    /// Live: where the player's timeline 0 is on the clock.
    anchor: Option<f64>,
    lives: Vec<LiveChat>,
    lives_gen: Vec<u64>,
    /// A past stream's chat or a file's (`t`: seconds into the video).
    replay: Vec<Message>,
    loading: Option<(Arc<AtomicBool>, Arc<Mutex<Loading>>)>,
    /// Looking at whether the link kept its chat.
    checking: Option<Arc<Mutex<Option<bool>>>>,
    /// Looking for the same stream live on the other site (a multi-stream); what was found.
    seeking: Option<Arc<Mutex<Option<Option<chat::AlsoLive>>>>>,
    also: Option<chat::AlsoLive>,
    /// Everything on the player's timeline (`t`: seconds on it), oldest first.
    msgs: Vec<Message>,
    msgs_gen: u64,
    picked: HashSet<String>,
    /// Rows scrolled back from the newest shown.
    back: usize,
    last_key: Option<DrawKey>,
    fonts: Option<Fonts>,
    pics: Pictures,
    /// Drawn rows: (id, width px, dark) → image and its height in px.
    cache: HashMap<(String, u32, bool), (slint::Image, f32)>,
    /// Rows drawn while some of their pictures were still on the way.
    unfinished: HashSet<String>,
    fetching: HashSet<String>,
    fetched: Arc<Mutex<Vec<String>>>,
    activity_key: Option<(u64, i64, i64, u32)>,
    ticks: u32,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Set from the Download page's preview: the previewed video is a past stream that kept
/// its chat (saved with it when Settings say so).
static PREVIEW_HAS_CHAT: AtomicBool = AtomicBool::new(false);

pub fn set_preview_has_chat(yes: bool) {
    PREVIEW_HAS_CHAT.store(yes, Ordering::SeqCst);
}

/// The Download page's download of a past stream: its chat is saved with it (Settings →
/// Download → Save live chat).
pub fn add_to_download(req: &mut neria_core::download::DownloadRequest, settings: &neria_core::settings::Settings) {
    if PREVIEW_HAS_CHAT.load(Ordering::SeqCst)
        && settings.chat_save
        && req.section.is_none()
        && req.live.is_none()
        && chat::supported(&req.url)
    {
        req.chat = Some(ChatJob { save: true, ..ChatJob::default() });
    }
}

thread_local! {
    /// The next link clip is a live "Save the last": its chat goes next to it.
    static BESIDE_NEXT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The next link clip's export is "Save the last" while watching live.
pub fn beside_next() {
    BESIDE_NEXT.with(|b| b.set(true));
}

/// Everything said `from`–`to` on the player's timeline (`t` from `from`), for a live clip's
/// `.chat.json`: only while live chat is read and Settings → Save live chat is on.
pub fn said_during(ui: &AppWindow, from: f64, to: f64) -> Vec<Message> {
    if !ui.global::<ClipChat>().get_save() {
        return Vec::new();
    }
    STATE.with(|s| {
        let s = s.borrow();
        if s.lives.is_empty() {
            return Vec::new();
        }
        chat::for_clip(&s.msgs, from, to, &Filter::default())
    })
}

/// The same as a chat file, for a clip saved here (the replay buffer).
pub fn file_for(ui: &AppWindow, from: f64, to: f64) -> Option<ChatFile> {
    let messages = said_during(ui, from, to);
    if messages.is_empty() {
        return None;
    }
    let anchor = STATE.with(|s| s.borrow().anchor);
    let mut sites: Vec<chat::Site> = Vec::new();
    for m in &messages {
        if !sites.contains(&m.site) {
            sites.push(m.site);
        }
    }
    Some(ChatFile { version: 1, sites, zero: anchor.map(|a| a + from), messages, ..Default::default() })
}

fn number(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Something new opened in the clipper: what was there goes.
fn reset(ui: &AppWindow) {
    BESIDE_NEXT.with(|b| b.set(false));
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some((cancel, _)) = &s.loading {
            cancel.store(true, Ordering::SeqCst);
        }
        let fonts = s.fonts.take();
        let cache_pics = std::mem::take(&mut s.pics);
        *s = State { fonts, pics: cache_pics, ..State::default() };
    });
    let g = ui.global::<ClipChat>();
    g.set_available(false);
    g.set_state(0);
    g.set_line("".into());
    g.set_count("".into());
    g.set_sites("".into());
    g.set_rows(ModelRc::from(Rc::new(VecModel::<ChatRow>::default())));
    g.set_picked_count(0);
    g.set_scrolled(false);
    g.set_find("".into());
    g.set_view(0);
    g.set_adding(false);
    g.set_can_add(false);
    g.set_also_text("".into());
    g.set_open(false);
    g.set_sheet(false);
    g.set_activity(slint::Image::default());
    refresh_dialog(ui);
}

/// A file opened: its saved chat, if it has one.
pub fn opened_file(ui: &AppWindow, path: &Path) {
    reset(ui);
    let Some(f) = chat::load_for(path) else { return };
    let n = f.messages.len();
    let sites: Vec<&str> = f.sites.iter().map(|s| s.name()).collect();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.file = Some(path.to_path_buf());
        s.replay = f.messages;
        rebuild(&mut s);
    });
    let g = ui.global::<ClipChat>();
    g.set_available(true);
    g.set_state(3);
    g.set_count(number(n).into());
    g.set_sites(if sites.len() > 1 { sites.join(" + ").into() } else { "".into() });
    refresh_dialog(ui);
}

/// A link opened: nothing yet (once it plays, `length_known` says whether it's live).
pub fn opened_link(ui: &AppWindow, url: &str) {
    reset(ui);
    if chat::supported(url) {
        STATE.with(|s| s.borrow_mut().url = Some(url.to_string()));
    }
}

/// Nothing open any more.
pub fn closed(ui: &AppWindow) {
    reset(ui);
}

/// The open link's length is known: live (read its chat now, on the clock from `anchor`),
/// or not (look whether it kept its chat).
pub fn length_known(ui: &AppWindow, live: bool, anchor: Option<f64>) {
    let Some(url) = STATE.with(|s| s.borrow().url.clone()) else { return };
    if live {
        let settings_off = std::env::var_os("NERIA_CHAT_OFF").is_some();
        if settings_off {
            return;
        }
        if let Some(c) = LiveChat::start(&url, None) {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.anchor = anchor;
                s.lives = vec![c];
                s.lives_gen = vec![0];
            });
            let g = ui.global::<ClipChat>();
            g.set_available(true);
            g.set_state(4);
            g.set_line("Connecting to the chat…".into());
            g.set_can_add(true);
            refresh_dialog(ui);
            // The same stream on the other site: its chat is one click away.
            let found = Arc::new(Mutex::new(None));
            let (f, u) = (found.clone(), url.clone());
            std::thread::spawn(move || *f.lock().unwrap() = Some(chat::also_live(&u)));
            STATE.with(|s| s.borrow_mut().seeking = Some(found));
        }
        return;
    }
    let found = Arc::new(Mutex::new(None));
    let f = found.clone();
    std::thread::spawn(move || {
        *f.lock().unwrap() = Some(chat::has_replay(&url));
    });
    STATE.with(|s| s.borrow_mut().checking = Some(found));
}

/// "Load chat": the past stream's whole chat, in the background.
fn load(ui: &AppWindow) {
    let Some(url) = STATE.with(|s| s.borrow().url.clone()) else { return };
    let duration = Some(ui.get_clip_duration() as f64).filter(|d| *d > 0.0);
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new(Loading::default()));
    let (c, p) = (cancel.clone(), progress.clone());
    std::thread::spawn(move || {
        let r = chat::fetch_replay(&url, duration, &c, &mut |n, frac| {
            let mut l = p.lock().unwrap();
            l.count = n;
            l.frac = frac;
        });
        p.lock().unwrap().done = Some(r.map_err(|e| e.to_string()));
    });
    STATE.with(|s| s.borrow_mut().loading = Some((cancel, progress)));
    let g = ui.global::<ClipChat>();
    g.set_state(2);
    g.set_line("Starting…".into());
}

/// Rebuild the messages on the timeline from what's read (live chats merged, by time).
fn rebuild(s: &mut State) {
    let mut msgs: Vec<Message> = s.replay.clone();
    if let Some(anchor) = s.anchor {
        for c in &s.lives {
            msgs.extend(c.messages().into_iter().map(|m| Message { t: m.t - anchor, ..m }));
        }
    }
    msgs.sort_by(|a, b| a.t.total_cmp(&b.t));
    s.msgs = msgs;
    s.msgs_gen += 1;
}

fn filter_of(ui: &AppWindow, s: &State) -> Filter {
    let g = ui.global::<ClipChat>();
    Filter {
        which: match g.get_which() {
            1 => Which::Highlights,
            2 => Which::Picked,
            _ => Which::All,
        },
        no_bots: g.get_no_bots(),
        words: g.get_words().split(',').map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).collect(),
        picked: s.picked.clone(),
    }
}

/// How chat goes into the clip, from the dialog.
pub fn spec(ui: &AppWindow) -> Spec {
    let g = ui.global::<ClipChat>();
    if !g.get_available() {
        return Spec::default();
    }
    Spec {
        how: match (g.get_how(), g.get_place()) {
            (1, _) => How::Track,
            (2, 1) => How::Overlay,
            (2, _) => How::Panel,
            _ => How::None,
        },
        light: g.get_light(),
        emotes: g.get_pictures(),
    }
}

/// The chat a clip of `from`–`to` (seconds on the player's timeline) gets, `t` from its start.
pub fn messages_for(ui: &AppWindow, from: f64, to: f64) -> Vec<Message> {
    STATE.with(|s| {
        let s = s.borrow();
        let f = filter_of(ui, &s);
        chat::for_clip(&s.msgs, from, to, &f)
    })
}

/// A link clip's chat job (none when Chat is left out). Several ranges joined into one file
/// come one after the other.
pub fn job_for(ui: &AppWindow, ranges: &[(f64, f64)]) -> Option<ChatJob> {
    let spec = spec(ui);
    // "Save the last" while live: everything said goes next to the clip too.
    let beside = match (BESIDE_NEXT.with(|b| b.replace(false)), ranges) {
        (true, [(a, b)]) => said_during(ui, *a, *b),
        _ => Vec::new(),
    };
    if ranges.is_empty() || (spec.is_none() && beside.is_empty()) {
        return None;
    }
    if spec.is_none() {
        return Some(ChatJob { clip_len: Some(ranges[0].1 - ranges[0].0), beside, ..ChatJob::default() });
    }
    let mut messages = Vec::new();
    let mut offset = 0.0;
    for (a, b) in ranges {
        messages.extend(messages_for(ui, *a, *b).into_iter().map(|m| Message { t: m.t + offset, ..m }));
        offset += b - a;
    }
    Some(ChatJob { save: false, spec, messages, clip_len: Some(offset), beside })
}

/// Chat into a clip saved from a file: the clip's messages, timed to where the file really
/// starts (a lossless cut starts on the keyframe before In). Returns where the file is now.
pub fn put_into_export(
    path: &Path,
    msgs: &[Message],
    spec: &Spec,
    wanted_len: f64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> anyhow::Result<PathBuf> {
    let early = neria_core::media::duration(path).map(|d| (d - wanted_len).max(0.0)).unwrap_or(0.0);
    let msgs: Vec<Message> = msgs.iter().map(|m| Message { t: m.t + early, ..m.clone() }).collect();
    chat_out::put_in_place(path, &msgs, spec, cancel, progress)
}

/// The dialog's lines and the bottom bar's button.
pub fn refresh_dialog(ui: &AppWindow) {
    let g = ui.global::<ClipChat>();
    let (a, b) = (ui.get_clip_in() as f64, ui.get_clip_out() as f64);
    let clips = crate::clips_ui::clips();
    let ranges: Vec<(f64, f64)> =
        if clips.is_empty() { vec![(a, b)] } else { clips.iter().map(|c| (c.start, c.end)).collect() };
    let (all, high, picked) = STATE.with(|s| {
        let s = s.borrow();
        let words: Vec<String> =
            g.get_words().split(',').map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).collect();
        let no_bots = g.get_no_bots();
        let inside = |m: &&Message| ranges.iter().any(|(x, y)| m.t >= *x && m.t <= *y) && !(no_bots && chat::is_bot(m));
        let all = s.msgs.iter().filter(inside).count();
        let high = s.msgs.iter().filter(inside).filter(|m| chat::is_highlight(m, &words)).count();
        let picked = s.msgs.iter().filter(inside).filter(|m| s.picked.contains(&m.id)).count();
        (all, high, picked)
    });
    let range = if clips.is_empty() {
        format!("{} – {}", fmt_t(ui, a), fmt_t(ui, b))
    } else {
        format!("the {} clips", clips.len())
    };
    g.set_range_text(
        if all == 1 {
            format!("1 message falls inside {range}")
        } else {
            format!("{} messages fall inside {range}", number(all))
        }
        .into(),
    );
    g.set_all_count(number(all).into());
    g.set_highlight_count(number(high).into());
    g.set_picked_in_clip(number(picked).into());
    let (long, short) = match spec(ui).how {
        How::None => ("Chat: left out", "Chat"),
        How::Track => ("Chat · as a track", "Chat · track"),
        How::Panel => ("Chat · burned in beside", "Chat · beside"),
        How::Overlay => ("Chat · burned in over", "Chat · over"),
    };
    g.set_button_text(long.into());
    g.set_short_text(short.into());
    g.set_set(!spec(ui).is_none());
}

/// A time on the player's timeline as the panel and dialog say it: the clock for a live
/// stream, else into the video.
fn fmt_t(ui: &AppWindow, t: f64) -> String {
    match STATE.with(|s| s.borrow().anchor) {
        Some(anchor) if ui.global::<ClipChat>().get_state() == 4 => {
            chrono::DateTime::from_timestamp((anchor + t) as i64, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
                .unwrap_or_default()
        }
        _ => neria_core::fmt::duration(t.max(0.0)),
    }
}

fn row_time(live_anchor: Option<f64>, t: f64) -> String {
    match live_anchor {
        Some(anchor) => chrono::DateTime::from_timestamp((anchor + t) as i64, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default(),
        None => {
            let s = t.max(0.0) as u64;
            if s >= 3600 {
                format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
            } else {
                format!("{}:{:02}", s / 60, s % 60)
            }
        }
    }
}

/// Every frame: live chat coming in, a load going, the player moving; the panel's rows and
/// the activity strip follow (only when something they show changed).
pub fn tick(ui: &AppWindow, position: f64) {
    let g = ui.global::<ClipChat>();
    // The same stream live on the other site: "Also live on Twitch · Add its chat".
    let sought = STATE.with(|s| s.borrow().seeking.as_ref().and_then(|c| c.lock().unwrap().take()));
    if let Some(found) = sought {
        STATE.with(|s| s.borrow_mut().seeking = None);
        if let Some(a) = found {
            g.set_also_text(format!("Also live on {}", a.site.name()).into());
            STATE.with(|s| s.borrow_mut().also = Some(a));
        }
    }
    // Whether the link kept its chat (a past stream): offer to load it.
    let checked = STATE.with(|s| s.borrow().checking.as_ref().and_then(|c| *c.lock().unwrap()));
    if let Some(found) = checked {
        STATE.with(|s| s.borrow_mut().checking = None);
        if found {
            g.set_available(true);
            g.set_state(1);
            refresh_dialog(ui);
        }
    }
    // A load going: how far, then its messages.
    let progress = STATE.with(|s| {
        s.borrow().loading.as_ref().map(|(_, p)| {
            let mut l = p.lock().unwrap();
            (l.count, l.frac, l.done.take())
        })
    });
    if let Some((count, frac, done)) = progress {
        match done {
            None => g.set_line(
                match frac {
                    Some(f) => format!("{} messages · {:.0}%", number(count), f * 100.0),
                    None => format!("{} messages", number(count)),
                }
                .into(),
            ),
            Some(Ok(f)) => {
                let n = f.messages.len();
                STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    s.loading = None;
                    s.replay = f.messages;
                    rebuild(&mut s);
                });
                g.set_state(3);
                g.set_line("".into());
                g.set_count(number(n).into());
                refresh_dialog(ui);
            }
            Some(Err(e)) => {
                STATE.with(|s| s.borrow_mut().loading = None);
                g.set_state(5);
                g.set_line(e.into());
            }
        }
    }
    // Live: new messages, and how the connection is.
    let live_line = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.lives.is_empty() {
            return None;
        }
        let gens: Vec<u64> = s.lives.iter().map(|c| c.generation()).collect();
        if gens != s.lives_gen {
            s.lives_gen = gens;
            rebuild(&mut s);
        }
        let states: Vec<LiveState> = s.lives.iter().map(|c| c.state()).collect();
        Some((states, s.lives.iter().map(|c| c.len()).sum::<usize>()))
    });
    if let Some((states, n)) = live_line {
        let line = states
            .iter()
            .find_map(|st| match st {
                LiveState::Connecting => Some("Connecting to the chat…".to_string()),
                LiveState::Retrying(why) => Some(format!("{why} · trying again")),
                LiveState::Off(why) => Some(why.clone()),
                LiveState::On => None,
            })
            .unwrap_or_default();
        let all_off = states.iter().all(|s| matches!(s, LiveState::Off(_)));
        if all_off && n == 0 {
            g.set_state(5);
        }
        g.set_line(line.into());
        g.set_count(number(n).into());
    }
    // Pictures fetched since: rows that waited for them are drawn again.
    let fetched: Vec<String> = STATE.with(|s| std::mem::take(&mut *s.borrow().fetched.lock().unwrap()));
    if !fetched.is_empty() {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            for u in &fetched {
                s.fetching.remove(u);
            }
            let unfinished: Vec<String> = s.unfinished.drain().collect();
            s.cache.retain(|(id, _, _), _| !unfinished.contains(id));
            s.pics = Pictures::default();
            s.last_key = None;
        });
    }
    let state = g.get_state();
    if state != 3 && state != 4 {
        return;
    }
    rows(ui, position);
    let ticks = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.ticks += 1;
        s.ticks
    });
    if ticks.is_multiple_of(15) {
        activity(ui);
    }
    // The dialog's counts follow In / Out while it's open.
    if g.get_open() && ticks.is_multiple_of(10) {
        refresh_dialog(ui);
    }
}

/// The panel's rows: the newest messages up to the playhead (live at the edge: everything),
/// as many as fit, `back` rows scrolled back.
fn rows(ui: &AppWindow, position: f64) {
    let g = ui.global::<ClipChat>();
    let scale = ui.window().scale_factor();
    let width = (g.get_width_px().max(80.0) * scale).round() as u32;
    let height = g.get_height_px();
    let dark = ui.global::<crate::Theme>().get_dark();
    let view = g.get_view();
    let find = g.get_find().to_lowercase();
    // Live at the edge (as the player's "Live" says), or a stream that can't be rewound
    // (Twitch): the newest chat. Gone back further: the chat of that moment.
    let cl = ui.global::<crate::ClipLive>();
    let live_edge = g.get_state() == 4
        && (!cl.get_can_rewind() || (ui.get_clip_duration() as f64 - position) < cl.get_live_slack() as f64);
    let now = if live_edge { f64::MAX } else { position + 0.5 };
    let key_pos = if live_edge { i64::MAX } else { (position * 4.0) as i64 };
    let built = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let key = (key_pos, s.msgs_gen, view, find.clone(), width, height as u32, s.back, dark, s.picked.len());
        if s.last_key.as_ref() == Some(&key) {
            return None;
        }
        s.last_key = Some(key);
        if s.fonts.is_none() {
            s.fonts = Some(Fonts::load());
        }
        let words: Vec<String> = Vec::new();
        let upto = s.msgs.partition_point(|m| m.t <= now);
        let keep = |m: &Message| -> bool {
            (match view {
                1 => chat::is_highlight(m, &words),
                2 => s.picked.contains(&m.id),
                _ => true,
            }) && (find.is_empty() || m.name.to_lowercase().contains(&find) || m.text().to_lowercase().contains(&find))
        };
        let shown: Vec<usize> = (0..upto).rev().filter(|i| keep(&s.msgs[*i])).take(400 + s.back).collect();
        let back = s.back.min(shown.len().saturating_sub(1));
        s.back = back;
        let mixed = s.msgs.first().is_some_and(|f| s.msgs.iter().any(|m| m.site != f.site));
        let look = Look { light: !dark, overlay: false, show_site: mixed, pictures: true };
        let size = 13.0 * scale;
        let anchor = if g.get_state() == 4 { s.anchor } else { None };
        let mut used = 0.0f32;
        let mut out: Vec<ChatRow> = Vec::new();
        for &i in shown.iter().skip(back) {
            let m = s.msgs[i].clone();
            let ck = (m.id.clone(), width, dark);
            let (img, h) = match s.cache.get(&ck) {
                Some(v) => v.clone(),
                None => {
                    let before = s.pics.wanted.len();
                    let State { fonts, pics, .. } = &mut *s;
                    let px = chat_draw::message(
                        fonts.as_ref().unwrap(),
                        pics,
                        &m,
                        width,
                        &look,
                        size,
                        &RowExtras { time: Some(row_time(anchor, m.t)), dim: chat::is_bot(&m) },
                    );
                    if s.pics.wanted.len() > before {
                        s.unfinished.insert(m.id.clone());
                    }
                    let buf =
                        SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(px.data(), px.width(), px.height());
                    let img = slint::Image::from_rgba8_premultiplied(buf);
                    let h = px.height() as f32 / scale;
                    s.cache.insert(ck, (img.clone(), h));
                    (img, h)
                }
            };
            // Rows until the panel is full: the last one may be cut off at the top (the panel
            // clips it), so a long message doesn't leave a gap above the newer ones.
            if used >= height {
                break;
            }
            used += h + 2.0;
            out.push(ChatRow {
                id: m.id.clone().into(),
                image: img,
                h,
                picked: s.picked.contains(&m.id),
                text: format!("{}: {}", m.name, m.text()).into(),
            });
        }
        out.reverse();
        // Keep the cache from growing without end on a long live stream.
        if s.cache.len() > 3000 {
            let keepers: HashSet<String> = out.iter().map(|r| r.id.to_string()).collect();
            s.cache.retain(|(id, _, _), _| keepers.contains(id));
        }
        let asked: Vec<String> = std::mem::take(&mut s.pics.wanted);
        let wanted: Vec<String> = asked.into_iter().filter(|u| !s.fetching.contains(u)).collect();
        for u in &wanted {
            s.fetching.insert(u.clone());
        }
        Some((out, back > 0, wanted, s.fetched.clone()))
    });
    let Some((out, scrolled, wanted, fetched)) = built else { return };
    g.set_rows(ModelRc::from(Rc::new(VecModel::from(out))));
    g.set_scrolled(scrolled);
    // Pictures not had yet: fetched off the UI thread, a few at a time.
    if !wanted.is_empty() {
        std::thread::spawn(move || {
            for chunk in wanted.chunks(8) {
                let handles: Vec<_> = chunk
                    .iter()
                    .cloned()
                    .map(|u| {
                        std::thread::spawn(move || {
                            let _ = chat::emote_file(&u);
                            u
                        })
                    })
                    .collect();
                let done: Vec<String> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
                fetched.lock().unwrap().extend(done);
            }
        });
    }
}

thread_local! {
    /// Watching a live stream: the tape's stretch of the player's timeline (start, span),
    /// which the activity strip follows instead of the timeline's view.
    static TAPE_RANGE: std::cell::Cell<Option<(f64, f64)>> = const { std::cell::Cell::new(None) };
}

/// The live tape shows (its stretch of the player's timeline), or not.
pub fn set_tape_range(range: Option<(f64, f64)>) {
    TAPE_RANGE.with(|r| r.set(range));
}

/// The strip over the timeline: how much is said along what's on screen.
fn activity(ui: &AppWindow) {
    let v = ui.global::<ClipView>();
    let (start, span) = TAPE_RANGE.with(|r| r.get()).unwrap_or((v.get_start() as f64, v.get_span() as f64));
    let span = if span > 0.0 { span } else { ui.get_clip_duration() as f64 };
    let scale = ui.window().scale_factor();
    let w = ((v.get_width_px() * scale) as u32).clamp(16, 8192);
    let h = (18.0 * scale) as u32;
    let (a, b) = (ui.get_clip_in() as f64, ui.get_clip_out() as f64);
    let key_gen = STATE.with(|s| s.borrow().msgs_gen);
    let key = (key_gen, (start * 10.0) as i64, (span * 10.0) as i64 ^ ((a * 4.0) as i64) << 20 ^ (b * 4.0) as i64, w);
    if STATE.with(|s| s.borrow().activity_key == Some(key)) {
        return;
    }
    STATE.with(|s| s.borrow_mut().activity_key = Some(key));
    let buckets = (w / (3.0 * scale).max(1.0) as u32).max(1) as usize;
    let counts = STATE.with(|s| chat::activity(&s.borrow().msgs, start, span, buckets));
    let max = counts.iter().copied().max().unwrap_or(0).max(1) as f32;
    let dark = ui.global::<crate::Theme>().get_dark();
    let (bar, hot) = if dark {
        ([0x46, 0x46, 0x4c, 255], [0xff, 0xd2, 0x3f, 255])
    } else {
        ([0xcf, 0xc7, 0xb4, 255], [0xc9, 0x9a, 0x00, 255])
    };
    let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
    let px = buf.make_mut_slice();
    let bw = w as f32 / buckets as f32;
    for (i, c) in counts.iter().enumerate() {
        if *c == 0 {
            continue;
        }
        let t = start + (i as f64 + 0.5) / buckets as f64 * span;
        let col = if t >= a && t <= b { hot } else { bar };
        let bh = ((*c as f32 / max).sqrt() * h as f32).round().max(1.0) as u32;
        let x0 = (i as f32 * bw).round() as u32;
        let x1 =
            (((i + 1) as f32 * bw).round() as u32).saturating_sub((scale.round() as u32).max(1)).max(x0 + 1).min(w);
        for y in h - bh.min(h)..h {
            for x in x0..x1 {
                let p = &mut px[(y * w + x) as usize];
                *p = slint::Rgba8Pixel { r: col[0], g: col[1], b: col[2], a: col[3] };
            }
        }
    }
    ui.global::<ClipChat>().set_activity(slint::Image::from_rgba8(buf));
}

/// Where a message is on the player's timeline.
fn time_of(id: &str) -> Option<f64> {
    STATE.with(|s| s.borrow().msgs.iter().find(|m| m.id == id).map(|m| m.t))
}

/// Another stream's chat alongside (the same channel streaming to YouTube and Twitch).
fn add_chat(ui: &AppWindow, link: &str) {
    let link = link.trim();
    let g = ui.global::<ClipChat>();
    if !chat::supported(link) || chat::twitch_video_id(link).is_some() {
        g.set_line("Paste a live YouTube or Twitch link".into());
        return;
    }
    let Some(c) = LiveChat::start(link, None) else { return };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.lives.push(c);
        s.lives_gen.push(0);
        let sites: Vec<&str> = s.lives.iter().map(|c| c.site.name()).collect();
        g.set_sites(sites.join(" + ").into());
    });
    g.set_adding(false);
}

pub fn install(ui: &AppWindow, seek: impl Fn(f64) + 'static) {
    let g = ui.global::<ClipChat>();
    g.on_add_also({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            if let Some(a) = STATE.with(|s| s.borrow_mut().also.take()) {
                add_chat(&ui, &a.link);
            }
            ui.global::<ClipChat>().set_also_text("".into());
        }
    });
    g.on_dismiss_also({
        let weak = ui.as_weak();
        move || {
            STATE.with(|s| s.borrow_mut().also = None);
            weak.unwrap().global::<ClipChat>().set_also_text("".into());
        }
    });
    g.on_load({
        let weak = ui.as_weak();
        move || load(&weak.unwrap())
    });
    g.on_pick({
        let weak = ui.as_weak();
        move |id, on| {
            let ui = weak.unwrap();
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                if on {
                    s.picked.insert(id.to_string());
                } else {
                    s.picked.remove(id.as_str());
                }
                ui.global::<ClipChat>().set_picked_count(s.picked.len() as i32);
            });
            // Picking is choosing what goes in: the dialog's choice follows the first one.
            let g = ui.global::<ClipChat>();
            if on && g.get_which() == 0 && STATE.with(|s| s.borrow().picked.len()) == 1 {
                g.set_which(2);
            }
            refresh_dialog(&ui);
        }
    });
    g.on_view_changed(|| STATE.with(|s| s.borrow_mut().back = 0));
    g.on_find_changed(|| STATE.with(|s| s.borrow_mut().back = 0));
    g.on_jump({
        let seek = Rc::new(seek);
        let weak = ui.as_weak();
        move |id| {
            let ui = weak.unwrap();
            if let Some(t) = time_of(&id) {
                // A little before: the moment it reacts to.
                let t = (t - 3.0).clamp(0.0, ui.get_clip_duration() as f64);
                seek(t);
            }
        }
    });
    g.on_scroll(|rows| {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let n = rows.round() as i64;
            s.back = (s.back as i64 + n).max(0) as usize;
        })
    });
    g.on_latest(|| STATE.with(|s| s.borrow_mut().back = 0));
    g.on_resized(|| STATE.with(|s| s.borrow_mut().last_key = None));
    g.on_add_chat({
        let weak = ui.as_weak();
        move |link| add_chat(&weak.unwrap(), &link)
    });
    g.on_remove_chats({
        let weak = ui.as_weak();
        move || {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.lives.truncate(1);
                s.lives_gen.truncate(1);
                rebuild(&mut s);
            });
            weak.unwrap().global::<ClipChat>().set_sites("".into());
        }
    });
    g.on_changed({
        let weak = ui.as_weak();
        move || refresh_dialog(&weak.unwrap())
    });
    // Dev hook: NERIA_CHAT_DEMO opens the panel on sample messages (design checks).
    // NERIA_CHAT_EXPORT=panel|overlay|track (`,light`): once a file with its chat is open,
    // set Chat in this clip that way and press Export (a scripted check of the whole path).
    if let Ok(how) = std::env::var("NERIA_CHAT_EXPORT") {
        let weak = ui.as_weak();
        let timer = Rc::new(slint::Timer::default());
        let t2 = timer.clone();
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(500), move || {
            let Some(ui) = weak.upgrade() else { return };
            let g = ui.global::<ClipChat>();
            if !(ui.get_clip_loaded() && ui.get_clip_duration() > 0.0 && g.get_state() == 3) {
                return;
            }
            g.set_how(if how.starts_with("track") { 1 } else { 2 });
            g.set_place(if how.starts_with("overlay") { 1 } else { 0 });
            g.set_light(how.contains("light"));
            refresh_dialog(&ui);
            ui.invoke_clip_export();
            t2.stop();
        });
        std::mem::forget(timer);
    }
    // Once what NERIA_OPEN opens is loaded (opening resets the chat).
    if std::env::var_os("NERIA_CHAT_DEMO").is_some() {
        let weak = ui.as_weak();
        let timer = Rc::new(slint::Timer::default());
        let t2 = timer.clone();
        timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(300), move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_clip_loaded() && ui.get_clip_duration() > 0.0 {
                demo(&ui);
                t2.stop();
            }
        });
        std::mem::forget(timer);
    }
}

/// Sample chat on what's open (NERIA_CHAT_DEMO, with NERIA_OPEN): the panel and the dialog
/// as they look with every kind of message.
pub fn demo(ui: &AppWindow) {
    use chat::{Kind, Part, Site};
    let t = |s: &str| Part::Text { s: s.into() };
    let names = ["kaiyo", "mossbyte", "nullpointer", "tarte", "lumen", "Rin", "Ann", "Nightbot", "orbit", "pixelfox"];
    let colors =
        ["#FF9F43", "#5FD4FF", "#7DFF9B", "#FF7BD5", "#C4A8FF", "#F5B14A", "#1E90FF", "#8E8C88", "#FF6B6F", "#9ACD32"];
    let lines = [
        "that was CLEAN",
        "clip it clip it",
        "no way he hit that",
        "GG",
        "how did that work lol",
        "best play all week, keep going",
        "hype",
        "!socials → follow on all platforms",
        "again again",
        "chat is this real",
    ];
    let len = ui.get_clip_duration().max(60.0) as f64;
    let mut msgs = Vec::new();
    for i in 0..400 {
        let k = i % names.len();
        let mut m = Message {
            t: (i as f64 / 400.0) * len + ((i * 7919) % 13) as f64 * 0.1,
            id: format!("demo{i}"),
            site: if i % 3 == 0 { Site::YouTube } else { Site::Twitch },
            name: names[k].into(),
            color: Some(colors[k].into()),
            parts: vec![t(lines[(i * 3 + k) % lines.len()])],
            ..Message::default()
        };
        if k == 3 {
            m.badges = vec!["moderator".into()];
        }
        if k == 5 && i % 20 == 5 {
            m.kind = Kind::Paid;
            m.amount = Some("$20.00".into());
        }
        if k == 6 && i % 20 == 6 {
            m.kind = Kind::Member;
            m.note = Some("Ann subscribed for 12 months".into());
        }
        msgs.push(m);
    }
    // A burst in the middle: what the activity strip shows.
    for i in 0..120 {
        msgs.push(Message {
            t: len * 0.45 + i as f64 * 0.08,
            id: format!("burst{i}"),
            site: Site::Twitch,
            name: names[i % names.len()].into(),
            color: Some(colors[i % colors.len()].into()),
            parts: vec![t("OMG")],
            ..Message::default()
        });
    }
    msgs.sort_by(|a, b| a.t.total_cmp(&b.t));
    let n = msgs.len();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.replay = msgs;
        s.picked = ["demo101", "demo105", "demo110"].iter().map(|x| x.to_string()).collect();
        rebuild(&mut s);
    });
    let g = ui.global::<ClipChat>();
    g.set_available(true);
    g.set_state(3);
    g.set_count(number(n).into());
    g.set_sites("YouTube + Twitch".into());
    g.set_picked_count(3);
    if std::env::var("NERIA_CHAT_DEMO").is_ok_and(|v| v == "sheet") {
        g.set_sheet(true);
    }
    if std::env::var("NERIA_CHAT_DEMO").is_ok_and(|v| v == "dialog") {
        g.set_how(2);
        g.set_which(2);
        g.set_open(true);
    }
    // `also`: one site's chat, the same stream found live on the other.
    if std::env::var("NERIA_CHAT_DEMO").is_ok_and(|v| v == "also") {
        g.set_sites("Twitch".into());
        g.set_also_text("Also live on YouTube".into());
    }
    refresh_dialog(ui);
}
