//! Live streams (Livestreams milestone): the Download page's "Records from the start / from
//! now" line and its switch, the recording request it makes, a recording's row in the queue
//! (the tape, Stop, "Clip the last…"), and saving recordings when Neria closes.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use neria_core::download::DownloadRequest;
use neria_core::probe::LiveInfo;
use neria_core::queue::{JobSnapshot, JobState, Queue};
use neria_core::record::{self, LiveJob, Phase};
use neria_core::{fmt, live};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppWindow, Live, LiveRow, QueueActions};

thread_local! {
    /// The previewed live stream and the start picked for it (None: Neria's pick).
    static PREVIEW: RefCell<Option<(LiveInfo, Option<bool>)>> = const { RefCell::new(None) };
    /// Its picture, for the recording's row (yt-dlp doesn't say it while recording).
    static THUMB: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Keeps "Starts in 42 min" / "about 20 min to catch up" current while it's shown.
    static TICK: slint::Timer = slint::Timer::default();
    /// The last live link looked up (link, what the site said, its picture), so the player
    /// can record it after the preview has gone.
    static LAST: RefCell<Option<(String, LiveInfo, Option<String>)>> = const { RefCell::new(None) };
    /// Titles of recordings started just now, before their row exists (for watching them).
    static TITLES: RefCell<Vec<(u64, String)>> = const { RefCell::new(Vec::new()) };
}

/// A link was looked up: a live stream gets its line, anything else clears it.
pub fn set_info(ui: &AppWindow, info: Option<&LiveInfo>, thumbnail: Option<String>) {
    // Dev hook: `NERIA_LIVE_FROM=now` / `start` picks where a recording starts, no click.
    let want = std::env::var("NERIA_LIVE_FROM").ok().map(|v| v == "start");
    PREVIEW.with(|p| *p.borrow_mut() = info.cloned().map(|i| (i, want)));
    if let (Some(i), Some(url)) = (info, crate::link_of(&ui.get_url())) {
        LAST.with(|l| *l.borrow_mut() = Some((url, i.clone(), thumbnail.clone())));
    }
    THUMB.with(|t| *t.borrow_mut() = thumbnail);
    show(ui);
    let weak = ui.as_weak();
    TICK.with(|t| {
        if info.is_some() {
            t.start(slint::TimerMode::Repeated, Duration::from_secs(20), move || {
                if let Some(ui) = weak.upgrade() {
                    show(&ui);
                }
            });
        } else {
            t.stop();
        }
    });
}

pub fn clear(ui: &AppWindow) {
    set_info(ui, None, None);
}

fn show(ui: &AppWindow) {
    let g = ui.global::<Live>();
    PREVIEW.with(|p| match p.borrow().as_ref() {
        Some((info, want)) => {
            let c = record::start_choice(info, *want, live::now());
            g.set_on(true);
            g.set_upcoming(info.upcoming);
            if info.upcoming {
                g.set_line(c.line.into());
                g.set_hint(c.hint.into());
                g.set_hint_short(c.hint_short.into());
            } else {
                // Record opens the player on it (from now: clipping works at once).
                g.set_line("Records from now".into());
                g.set_hint("the player opens, so you can clip as it happens".into());
                g.set_hint_short("clip as it happens".into());
            }
            g.set_switch("".into());
            g.set_switch_short("".into());
        }
        None => g.set_on(false),
    });
}

/// The previewed link is a live stream: make `req` a recording of it (from the start or
/// from now, as the preview says). False for anything else.
pub fn make_recording(req: &mut DownloadRequest) -> bool {
    PREVIEW.with(|p| {
        let p = p.borrow();
        let Some((info, want)) = p.as_ref() else { return false };
        let c = record::start_choice(info, *want, live::now());
        req.live = Some(LiveJob {
            // Live now: from now (the player opens on it, so clips can start at once). Not on
            // yet: from its first second where the site keeps it.
            from_start: info.upcoming && c.from_start,
            started_at: info.started_at,
            starts_at: info.starts_at,
            work: None,
        });
        req.thumbnail = THUMB.with(|t| t.borrow().clone());
        true
    })
}

/// The previewed live stream isn't on yet (Record waits for it; nothing to watch).
pub fn upcoming() -> bool {
    PREVIEW.with(|p| p.borrow().as_ref().is_some_and(|(i, _)| i.upcoming))
}

/// A recording just added: its title, for watching it before its row exists.
pub fn note_title(id: u64, title: &str) {
    TITLES.with(|t| {
        let mut t = t.borrow_mut();
        t.retain(|(i, _)| *i != id);
        t.push((id, title.to_string()));
        let n = t.len();
        if n > 20 {
            t.drain(..n - 20);
        }
    });
}

/// Record the stream the player shows, from now (the player's Record / Record again).
pub fn record_now(ui: &AppWindow, url: &str, title: &str) -> Option<u64> {
    let queue = crate::QUEUE.get()?;
    let (quality, preset) = crate::chosen_quality(ui);
    let mut req = DownloadRequest::new(url.to_string(), quality, title.to_string());
    req.preset = preset;
    let (started_at, thumb) = LAST.with(|l| match l.borrow().as_ref() {
        Some((u, info, thumb)) if u == url => (info.started_at, thumb.clone()),
        _ => (None, None),
    });
    req.live = Some(LiveJob { from_start: false, started_at, starts_at: None, work: None });
    req.thumbnail = thumb;
    let id = queue.add(req);
    note_title(id, title);
    Some(id)
}

/// A recording's row: the tape and its lines (both widths), its tag and what it offers.
pub fn row(s: &JobSnapshot) -> Option<LiveRow> {
    let now = live::now();
    let wide = fmt::live_row(s, now, false)?;
    let short = fmt::live_row(s, now, true).unwrap_or_default();
    let st = s.live.clone().unwrap_or_default();
    let running = s.state == JobState::Downloading;
    let recording = matches!(st.phase, Phase::CatchingUp | Phase::Recording | Phase::Reconnecting);
    let (tag, tag_kind) = match (running, st.phase) {
        (true, Phase::Waiting) => ("Waiting", 0),
        (true, Phase::CatchingUp | Phase::Recording) => ("REC", 5),
        _ => ("", 0),
    };
    Some(LiveRow {
        on: true,
        tape: running && wide.tape,
        joined: wide.joined,
        done: wide.done,
        holes: ModelRc::new(VecModel::from(wide.holes.clone())),
        catching_up: wide.catching_up,
        left: wide.left.into(),
        middle: wide.middle.into(),
        middle_bad: wide.middle_bad,
        right: wide.right.into(),
        can_clip: wide.can_clip,
        can_stop: running && (recording || st.recorded > 0.0),
        tag: tag.into(),
        tag_kind,
        status: wide.status.into(),
        stats: wide.stats.into(),
        short_status: short.status.into(),
        short_stats: short.stats.into(),
        short_middle: short.middle.into(),
        clipping: wide.clipping,
    })
}

pub fn install(ui: &AppWindow, queue: &Queue) {
    ui.global::<Live>().on_flip({
        let weak = ui.as_weak();
        move || {
            PREVIEW.with(|p| {
                if let Some((info, want)) = p.borrow_mut().as_mut() {
                    let now_from_start = record::start_choice(info, *want, live::now()).from_start;
                    *want = Some(!now_from_start);
                }
            });
            show(&weak.unwrap());
        }
    });
    // Dev hook for scripted checks: press Stop on every recording after this many seconds.
    if let Some(secs) = std::env::var("NERIA_REC_STOP").ok().and_then(|s| s.trim().parse::<u64>().ok()) {
        let queue = queue.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            queue.stop_recordings();
        });
    }
    // Dev hook: "Clip the last <s>" on the first recording at <t> seconds (`10@40`), as the
    // menu does (through the UI thread, so the toast shows too).
    if let Some((secs, at)) = std::env::var("NERIA_REC_CLIP").ok().and_then(|v| {
        let (s, t) = v.split_once('@')?;
        Some((s.trim().parse::<f32>().ok()?, t.trim().parse::<u64>().ok()?))
    }) {
        let (queue, weak) = (queue.clone(), ui.as_weak());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(at));
            if let Some(id) = queue.first_recording() {
                let _ =
                    weak.upgrade_in_event_loop(move |ui| ui.global::<QueueActions>().invoke_clip_last(id as i32, secs));
            }
        });
    }
    // Dev hooks: open the first recording in the player at <s> (`NERIA_REC_WATCH`); press
    // Start clip and End clip at <a>,<b> seconds (`NERIA_REC_MARK`), through the UI thread.
    let at = |name: &str| -> Vec<u64> {
        std::env::var(name)
            .ok()
            .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
            .unwrap_or_default()
    };
    for (secs, what) in at("NERIA_REC_WATCH")
        .into_iter()
        .map(|s| (s, 0))
        .chain(at("NERIA_REC_MARK").into_iter().zip([1, 2]))
        .chain(at("NERIA_REC_EARLIER").into_iter().map(|s| (s, 3)))
    {
        let (queue, weak) = (queue.clone(), ui.as_weak());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            let Some(id) = queue.first_recording() else { return };
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let a = ui.global::<QueueActions>();
                match what {
                    0 => a.invoke_watch(id as i32),
                    1 => a.invoke_clip_start(id as i32),
                    3 => ui.global::<crate::Watch>().invoke_earlier(),
                    _ => a.invoke_clip_end(id as i32),
                }
            });
        });
    }
    ui.global::<crate::Watch>().on_earlier({
        let (queue, weak) = (queue.clone(), ui.as_weak());
        move || {
            let id = weak.unwrap().global::<crate::Watch>().get_recording_id();
            queue.get_earlier(id as u64);
        }
    });
    let actions = ui.global::<QueueActions>();
    actions.on_clip_start({
        let (queue, weak) = (queue.clone(), ui.as_weak());
        move |id| {
            queue.clip_start(id as u64);
            crate::show_toast(&weak.unwrap(), "Clip started 10 s back. Press End clip when it's over.");
        }
    });
    actions.on_clip_end({
        let (queue, weak) = (queue.clone(), ui.as_weak());
        move |id| {
            queue.clip_end(id as u64);
            crate::show_toast(&weak.unwrap(), "Saving the clip… the recording goes on.");
        }
    });
    actions.on_clip_drop({
        let queue = queue.clone();
        move |id| queue.clip_drop(id as u64)
    });
    actions.on_stop({
        let queue = queue.clone();
        move |id| queue.stop(id as u64)
    });
    actions.on_clip_last({
        let (queue, weak) = (queue.clone(), ui.as_weak());
        move |id, secs| {
            queue.clip_last(id as u64, secs as f64);
            crate::show_toast(&weak.unwrap(), &format!("Saving the last {}…", fmt::duration(secs as f64)));
        }
    });
}

/// Watching a recording: keep the player's bar in step with its row (can it clip yet, is a
/// clip open, what it says).
pub fn sync_watch(ui: &AppWindow, s: &JobSnapshot) {
    let w = ui.global::<crate::Watch>();
    if w.get_recording_id() != s.id as i32 {
        return;
    }
    let st = s.live.clone().unwrap_or_default();
    let running = s.state == JobState::Downloading;
    let done = matches!(s.state, JobState::Done | JobState::Failed | JobState::Cancelled);
    w.set_rec_done(done);
    if done {
        let ended = s.note.as_deref() == Some("the stream ended");
        w.set_rec_saved(s.state == JobState::Done);
        w.set_rec_again(!ended);
        w.set_rec_can_clip(false);
        w.set_rec_clipping(false);
        let text = match s.state {
            JobState::Done => {
                let why = s.note.clone().unwrap_or_else(|| "you stopped it".into());
                [fmt::duration(st.recorded), s.downloaded.map(fmt::bytes).unwrap_or_default(), why]
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
            JobState::Cancelled => "Thrown away. The stream is still on.".into(),
            _ => format!("Couldn't record it: {}", s.error.clone().unwrap_or_default()),
        };
        w.set_rec_text(text.into());
        return;
    }
    w.set_rec_can_clip(running && st.can_clip);
    // The part before a from-now recording: offered, coming in, in, or not to be had.
    let url = crate::QUEUE.get().and_then(|q| q.url_of(s.id)).unwrap_or_default();
    let before = st.joined.unwrap_or(0.0);
    let (earlier, link) = if let Some((got, of)) = st.earlier {
        (format!("Getting the part before · {} of {}", fmt::duration(got), fmt::duration(of)), false)
    } else if st.earlier_done {
        (format!("The {} before is in: it goes in front when saved", record::age_words(before)), false)
    } else if let Some(e) = &st.earlier_failed {
        (format!("Couldn't get the part before: {e}"), false)
    } else if running && before > 60.0 && record::can_record_from_start(&url) && !st.rewind_failed {
        (format!("Also get the {} before", record::age_words(before)), true)
    } else {
        (String::new(), false)
    };
    w.set_rec_earlier(earlier.into());
    w.set_rec_earlier_link(link);
    w.set_rec_clipping(running && s.clip_from.is_some());
    let text = match s.clip_from {
        Some(from) if running => {
            format!("Clipping from {} · {} so far", fmt::duration(from), fmt::duration((st.recorded - from).max(0.0)))
        }
        _ if running && st.phase == Phase::CatchingUp => {
            "Catching up with the live stream: you can clip once it has.".to_string()
        }
        _ if running => format!("Recording · {} so far", fmt::duration(st.recorded)),
        _ => "Starting the recording…".to_string(),
    };
    w.set_rec_text(text.into());
}

/// A recording's link and title (for watching it from its row).
pub fn link_of(id: u64) -> Option<(String, String)> {
    let url = crate::QUEUE.get()?.url_of(id)?;
    let title = match crate::rows().iter().find(|j| j.id == id as i32 && !j.header) {
        Some(row) => row.title.to_string(),
        // Just added: its row comes a moment later.
        None => TITLES.with(|t| t.borrow().iter().find(|(i, _)| *i == id).map(|(_, t)| t.clone()))?,
    };
    Some((url, title))
}

/// Neria is closing: stop every recording and wait (a while) until they're saved, so the
/// files are whole when it's gone. What isn't saved by then is saved at the next start.
pub fn save_before_quit(queue: &Queue) {
    if queue.stop_recordings() == 0 {
        return;
    }
    let started = Instant::now();
    while queue.recording_count() > 0 && started.elapsed() < Duration::from_secs(120) {
        std::thread::sleep(Duration::from_millis(100));
    }
}
