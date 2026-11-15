//! Clip page → the timeline when zoomed (Clip track 9). Owns the view (what part of the
//! video is on screen, core `timeline::View`), the sound levels it draws the waveform from,
//! the keyframes it thins out for what's on screen, and the close-up filmstrip frames made
//! for the zoom (a fixed grid per zoom step, so scrolling back reuses them). The Slint
//! timeline only maps the view to pixels and sends gestures to `ClipView`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use neria_core::media;
use neria_core::timeline::{self, Peaks, View};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::{AppWindow, ClipLive, ClipView, Fmt};

/// Where close-up frames come from.
#[derive(Clone, Debug, Default)]
pub enum TileSource {
    #[default]
    None,
    File(PathBuf),
    /// A link's storyboard (not for live streams: they have none).
    Link(String),
}

#[derive(Default)]
struct State {
    view: View,
    /// A live stream's view sticks to the edge while it shows it.
    follow: bool,
    peaks: Option<Rc<Peaks>>,
    keyframes: Vec<f64>,
    source: TileSource,
    /// Which opened video this is (frames still being made for an older one are dropped).
    source_id: u64,
    /// Close-up frames made so far: (step in ms, grid index) → frame.
    tiles: HashMap<(u64, u64), Image>,
    /// The tiles on screen now: their step and first index (to drop late results).
    shown: Option<(u64, u64, usize)>,
    /// The playhead at the previous tick, to page along as it plays.
    prev_pos: f64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    /// Waits a moment after the last zoom or scroll before making close-ups.
    static TILE_TIMER: slint::Timer = slint::Timer::default();
}

/// Bumped whenever another source opens or another set of tiles is wanted.
static TILE_GEN: AtomicU64 = AtomicU64::new(0);
static SOURCE_ID: AtomicU64 = AtomicU64::new(0);

/// Close-up frame height, in pixels (about twice the strip's, for sharp high-DPI frames).
const TILE_H: u32 = 96;
/// A filmstrip frame's width on screen, logical px (the strip's height × 16:9).
const TILE_PX: f64 = 85.0;
/// The waveform's colour (warm grey, reads on both themes; as `showwavespic` drew it).
const WAVE_RGB: [u8; 3] = [0x8e, 0x8c, 0x88];

fn duration(ui: &AppWindow) -> f64 {
    ui.get_clip_duration() as f64
}

fn min_span(ui: &AppWindow) -> f64 {
    if ui.global::<ClipLive>().get_on() { timeline::MIN_SPAN_LIVE } else { timeline::MIN_SPAN }
}

/// Another video opened: nothing known yet.
pub fn reset(ui: &AppWindow, source: TileSource) {
    TILE_GEN.fetch_add(1, Ordering::SeqCst);
    let source_id = SOURCE_ID.fetch_add(1, Ordering::SeqCst) + 1;
    STATE.with_borrow_mut(|s| *s = State { source, source_id, ..State::default() });
    let g = ui.global::<ClipView>();
    g.set_start(0.0);
    g.set_span(0.0);
    g.set_zoomed(false);
    g.set_can_zoom_in(true);
    g.set_span_text("".into());
    g.set_wave(Image::default());
    g.set_tiles(ModelRc::from(Rc::new(VecModel::<Image>::default())));
    g.set_dragging(false);
    ui.set_clip_wave(Image::default());
    ui.set_clip_keyframe_ticks(ModelRc::from(Rc::new(VecModel::<f32>::default())));
}

/// The length is known (and whether it's live): the whole video, or a live stream's last
/// five minutes. `NERIA_CLIP_VIEW=start,span` opens zoomed (for screenshots).
pub fn length_known(ui: &AppWindow, live: bool) {
    let d = duration(ui);
    let mut v = if live { View::live(d) } else { View::whole(d) };
    if let Some((a, b)) = std::env::var("NERIA_CLIP_VIEW").ok().and_then(|s| {
        let (a, b) = s.split_once(',')?;
        Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
    }) {
        v = View { start: a, span: b }.clamp(d, min_span(ui));
    }
    STATE.with_borrow_mut(|s| {
        s.follow = live && v.shows_edge(d);
        s.prev_pos = 0.0;
    });
    set_view(ui, v);
}

/// The sound levels arrived: the waveform can be drawn (and the overview's, for sound-only
/// files).
pub fn peaks_ready(ui: &AppWindow, peaks: Peaks) {
    let whole = View::whole(peaks.duration().max(duration(ui)));
    let (w, h) = (800, 40);
    ui.set_clip_wave(image(&peaks.render(whole, w, h, WAVE_RGB), w, h));
    STATE.with_borrow_mut(|s| s.peaks = Some(Rc::new(peaks)));
    draw_wave(ui);
}

pub fn keyframes_ready(ui: &AppWindow, keyframes: Vec<f64>) {
    STATE.with_borrow_mut(|s| s.keyframes = keyframes);
    update_ticks(ui);
}

/// Every player tick: a live stream's view keeps up with its edge, and the view pages along
/// with the playhead (not while something on the timeline is being dragged).
pub fn tick(ui: &AppWindow, pos: f64) {
    let d = duration(ui);
    let (view, follow, prev) = STATE.with_borrow(|s| (s.view, s.follow, s.prev_pos));
    STATE.with_borrow_mut(|s| s.prev_pos = pos);
    if view.span <= 0.0 || d <= 0.0 {
        return;
    }
    let min = min_span(ui);
    let live = ui.global::<ClipLive>().get_on();
    if live && follow && (view.end() - d).abs() > 1e-3 {
        set_view(ui, view.at_edge(d, min));
        return;
    }
    if ui.global::<ClipView>().get_dragging() {
        return;
    }
    if let Some(v) = view.follow_playhead(prev, pos, d, min) {
        set_view(ui, v);
        if live {
            STATE.with_borrow_mut(|s| s.follow = v.shows_edge(d));
        }
    }
}

/// Show `v` (clamped): the ruler, the zoom buttons, the waveform and the keyframe ticks
/// follow at once; close-ups a moment later.
fn set_view(ui: &AppWindow, v: View) {
    let d = duration(ui);
    let v = v.clamp(d, min_span(ui));
    STATE.with_borrow_mut(|s| s.view = v);
    let g = ui.global::<ClipView>();
    g.set_start(v.start as f32);
    g.set_span(v.span as f32);
    g.set_zoomed(!v.is_whole(d));
    g.set_can_zoom_in(v.span > min_span(ui).min(d) + 1e-3);
    g.set_span_text(timeline::span_text(v.span).into());
    g.set_ruler_step(timeline::ruler_step(v.span, g.get_width_px() as f64, 70.0) as f32);
    draw_wave(ui);
    update_ticks(ui);
    schedule_tiles(ui);
}

/// After a zoom or scroll by hand: a live stream follows its edge again once it's in view.
fn set_view_by_hand(ui: &AppWindow, v: View) {
    set_view(ui, v);
    let d = duration(ui);
    let live = ui.global::<ClipLive>().get_on();
    STATE.with_borrow_mut(|s| s.follow = live && s.view.shows_edge(d));
}

fn image(px: &[u8], w: u32, h: u32) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(px, w, h))
}

/// The waveform of what's on screen, at the timeline's width in real pixels.
fn draw_wave(ui: &AppWindow) {
    let (peaks, view) = STATE.with_borrow(|s| (s.peaks.clone(), s.view));
    let g = ui.global::<ClipView>();
    let Some(peaks) = peaks.filter(|_| view.span > 0.0) else {
        g.set_wave(Image::default());
        return;
    };
    let scale = ui.window().scale_factor() as f64;
    let w = ((g.get_width_px() as f64 * scale).round() as u32).clamp(16, 8192);
    let h = (if ui.get_clip_has_video() { 30.0 } else { 50.0 } * scale).round() as u32;
    g.set_wave(image(&peaks.render(view, w, h, WAVE_RGB), w, h));
}

/// Keyframe ticks for what's on screen (at most 300).
fn update_ticks(ui: &AppWindow) {
    let ticks: Vec<f32> = STATE.with_borrow(|s| {
        if s.view.span <= 0.0 {
            return Vec::new();
        }
        let (a, b) = (s.view.start, s.view.end());
        let lo = s.keyframes.partition_point(|k| *k < a);
        let hi = s.keyframes.partition_point(|k| *k <= b);
        media::decimate(&s.keyframes[lo..hi], s.view.span, 300).into_iter().map(|k| k as f32).collect()
    });
    ui.set_clip_keyframe_ticks(ModelRc::from(Rc::new(VecModel::from(ticks))));
}

fn schedule_tiles(ui: &AppWindow) {
    let weak = ui.as_weak();
    TILE_TIMER.with(|t| {
        t.start(slint::TimerMode::SingleShot, Duration::from_millis(250), move || {
            if let Some(ui) = weak.upgrade() {
                request_tiles(&ui);
            }
        })
    });
}

/// Close-up frames for the zoom: the grid's frames on screen and a screen either side, the
/// ones already made at once, the rest made off the UI thread (nearest the middle first) and
/// shown as each few arrive. Not needed while the whole video's 12 frames are close enough.
fn request_tiles(ui: &AppWindow) {
    let d = duration(ui);
    let (view, source, source_id) = STATE.with_borrow(|s| (s.view, s.source.clone(), s.source_id));
    let g = ui.global::<ClipView>();
    // The whole video's filmstrip has 12 frames (even before they've arrived).
    let thumbs = 12.0;
    let step = timeline::tile_step(view, g.get_width_px() as f64, TILE_PX);
    let wanted = !matches!(source, TileSource::None)
        && ui.get_clip_has_video()
        && !ui.global::<ClipLive>().get_on()
        && view.span > 0.0
        && step < d / thumbs / 1.5;
    if !wanted {
        STATE.with_borrow_mut(|s| s.shown = None);
        g.set_tiles(ModelRc::from(Rc::new(VecModel::<Image>::default())));
        return;
    }
    let (first, count) = timeline::tile_range(view, step, d, 60);
    let step_ms = (step * 1000.0).round() as u64;
    let generation = TILE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    STATE.with_borrow_mut(|s| s.shown = Some((step_ms, first, count)));
    let missing: Vec<u64> = STATE.with_borrow(|s| {
        (first..first + count as u64).filter(|i| !s.tiles.contains_key(&(step_ms, *i))).collect()
    });
    g.set_tiles_start((first as f64 * step) as f32);
    g.set_tiles_step(step as f32);
    show_tiles(ui);
    if missing.is_empty() {
        return;
    }
    // Nearest the middle of the screen first.
    let mid = (view.start + view.span / 2.0) / step;
    let mut missing = missing;
    missing.sort_by(|a, b| ((*a as f64 - mid).abs()).total_cmp(&(*b as f64 - mid).abs()));
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        for chunk in missing.chunks(6) {
            if TILE_GEN.load(Ordering::SeqCst) != generation {
                return;
            }
            let times: Vec<f64> = chunk.iter().map(|i| (*i as f64 + 0.5) * step).collect();
            let files = match &source {
                TileSource::File(path) => {
                    let frames: Vec<(String, f64)> =
                        chunk.iter().zip(&times).map(|(i, t)| (format!("{step_ms}-{i}"), *t)).collect();
                    media::tiles(path, &frames, TILE_H)
                }
                TileSource::Link(url) => media::storyboard_tiles(url, &times, TILE_H),
                TileSource::None => return,
            };
            let made: Vec<(u64, PathBuf)> =
                chunk.iter().zip(files).filter_map(|(i, f)| Some((*i, f?))).collect();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                // A newer source drops these; a newer zoom keeps them (scrolling back finds them).
                STATE.with_borrow_mut(|s| {
                    if s.source_id != source_id {
                        return;
                    }
                    for (i, f) in made {
                        if let Ok(img) = Image::load_from_path(&f) {
                            s.tiles.insert((step_ms, i), img);
                        }
                    }
                });
                show_tiles(&ui);
            });
        }
    });
}

/// The tiles on screen, from what's been made (missing ones are empty: the strip shows the
/// whole video's frame there until they arrive).
fn show_tiles(ui: &AppWindow) {
    let images: Option<Vec<Image>> = STATE.with_borrow(|s| {
        let (step_ms, first, count) = s.shown?;
        Some((first..first + count as u64).map(|i| s.tiles.get(&(step_ms, i)).cloned().unwrap_or_default()).collect())
    });
    if let Some(images) = images {
        ui.global::<ClipView>().set_tiles(ModelRc::from(Rc::new(VecModel::from(images))));
    }
}

pub fn install(ui: &AppWindow) {
    ui.global::<Fmt>().on_tick(|t, step| timeline::ruler_label(t as f64, step as f64).into());
    let g = ui.global::<ClipView>();
    g.on_zoom({
        let weak = ui.as_weak();
        move |factor, anchor| {
            let ui = weak.unwrap();
            let d = duration(&ui);
            let (view, follow) = STATE.with_borrow(|s| (s.view, s.follow));
            if view.span <= 0.0 {
                return;
            }
            let pos = ui.get_clip_position() as f64;
            let anchor = if anchor >= 0.0 {
                anchor as f64
            } else if pos >= view.start && pos <= view.end() {
                pos
            } else {
                view.start + view.span / 2.0
            };
            let mut v = view.zoom(factor as f64, anchor, d, min_span(&ui));
            // Following a live stream: zoom keeps the edge on the right.
            if follow && ui.global::<ClipLive>().get_on() {
                v = v.at_edge(d, min_span(&ui));
            }
            set_view_by_hand(&ui, v);
        }
    });
    g.on_pan_to({
        let weak = ui.as_weak();
        move |start| {
            let ui = weak.unwrap();
            let view = STATE.with_borrow(|s| s.view);
            set_view_by_hand(&ui, view.pan_to(start as f64, duration(&ui), min_span(&ui)));
        }
    });
    g.on_fit_clip({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let (a, b) = (ui.get_clip_in() as f64, ui.get_clip_out() as f64);
            set_view_by_hand(&ui, View::fit(a, b, duration(&ui), min_span(&ui)));
        }
    });
    g.on_whole({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            set_view_by_hand(&ui, View::whole(duration(&ui)));
        }
    });
    g.on_resized({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let view = STATE.with_borrow(|s| s.view);
            if view.span > 0.0 {
                set_view(&ui, view);
            }
        }
    });
}
