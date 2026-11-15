//! The clipper's timeline when zoomed (Clip track 9): which part of the video is on screen,
//! zooming around a point, the ruler's step, the filmstrip's tiles, and the waveform drawn
//! from peaks for any part of it. Plain maths, no UI, so it's tested here and shared with a
//! future Android front-end.

use std::path::Path;

/// Shortest stretch on screen, in seconds: about a frame per 20 px.
pub const MIN_SPAN: f64 = 1.0;
/// A live stream's shortest: its pieces are seconds long, closer means nothing.
pub const MIN_SPAN_LIVE: f64 = 10.0;
/// A live stream opens on its last five minutes.
pub const LIVE_SPAN: f64 = 300.0;

/// The part of the timeline on screen: `start` to `start + span`, in seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct View {
    pub start: f64,
    pub span: f64,
}

impl View {
    pub fn whole(duration: f64) -> View {
        View { start: 0.0, span: duration.max(0.0) }
    }

    /// A live stream's opening view: the last five minutes (or all of it, if less).
    pub fn live(duration: f64) -> View {
        let span = LIVE_SPAN.min(duration.max(0.0));
        View { start: duration - span, span }
    }

    pub fn end(&self) -> f64 {
        self.start + self.span
    }

    /// Kept inside the video, never shorter than `min_span` (unless the video is).
    pub fn clamp(self, duration: f64, min_span: f64) -> View {
        let duration = duration.max(0.0);
        let span = self.span.max(min_span.min(duration)).min(duration);
        let start = self.start.clamp(0.0, (duration - span).max(0.0));
        View { start, span }
    }

    /// Shows the whole video (nothing to scroll to).
    pub fn is_whole(&self, duration: f64) -> bool {
        self.span >= duration - 1e-3
    }

    /// Zoom by `factor` (under 1 zooms in) keeping `anchor` where it is on screen.
    pub fn zoom(self, factor: f64, anchor: f64, duration: f64, min_span: f64) -> View {
        let span = (self.span * factor).max(min_span.min(duration)).min(duration);
        let f = if self.span > 0.0 { ((anchor - self.start) / self.span).clamp(0.0, 1.0) } else { 0.5 };
        View { start: anchor - f * span, span }.clamp(duration, min_span)
    }

    /// The same zoom, moved to start at `start`.
    pub fn pan_to(self, start: f64, duration: f64, min_span: f64) -> View {
        View { start, span: self.span }.clamp(duration, min_span)
    }

    /// Frames `a`–`b` with a little room on both sides.
    pub fn fit(a: f64, b: f64, duration: f64, min_span: f64) -> View {
        let (a, b) = (a.min(b), a.max(b));
        let pad = (b - a) * 0.15 + 0.5;
        View { start: a - pad, span: b - a + 2.0 * pad }.clamp(duration, min_span)
    }

    /// Live: the edge sits on the right.
    pub fn at_edge(self, duration: f64, min_span: f64) -> View {
        View { start: duration - self.span, span: self.span }.clamp(duration, min_span)
    }

    /// Shows the live edge (so it keeps following it as the stream grows).
    pub fn shows_edge(&self, duration: f64) -> bool {
        self.end() >= duration - 0.5
    }

    /// The view moved so the playhead stays in sight: while playing it pages along as the
    /// playhead runs off the right (it shows up near the left again); a jump (a seek, Home,
    /// a chapter) out of sight centres it. A playhead that was already out of sight and only
    /// moved a little (you scrolled away while it plays) is left alone.
    pub fn follow_playhead(self, prev: f64, pos: f64, duration: f64, min_span: f64) -> Option<View> {
        let inside = |t: f64| t >= self.start - 1e-3 && t <= self.end() + 1e-3;
        if inside(pos) || self.is_whole(duration) {
            return None;
        }
        let jumped = (pos - prev).abs() > 1.5;
        if !jumped && !inside(prev) {
            return None;
        }
        let start = if !jumped && pos > self.end() { pos - self.span * 0.05 } else { pos - self.span / 2.0 };
        Some(View { start, span: self.span }.clamp(duration, min_span))
    }
}

/// Steps the ruler's marks and the filmstrip's tiles snap to, in seconds.
const STEPS: &[f64] = &[
    0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0,
];

/// The ruler's step: the smallest that leaves `min_px` between labels.
pub fn ruler_step(span: f64, width_px: f64, min_px: f64) -> f64 {
    let fits = (width_px / min_px).max(1.0);
    STEPS.iter().copied().find(|s| span / s <= fits).unwrap_or(*STEPS.last().unwrap())
}

/// A ruler label: "1:05:00", "4:30", "12.5 s"-style times where the step is under a second.
pub fn ruler_label(t: f64, step: f64) -> String {
    let t = t.max(0.0);
    if step < 1.0 {
        // Tenths: "0:12.5"
        let tenths = (t * 10.0).round() as u64;
        let (whole, d) = (tenths / 10, tenths % 10);
        return format!("{}.{d}", clock(whole));
    }
    clock(t.round() as u64)
}

fn clock(s: u64) -> String {
    let (h, m, x) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{x:02}") } else { format!("{m}:{x:02}") }
}

/// "12 s on screen", "4:30 on screen".
pub fn span_text(span: f64) -> String {
    if span < 90.0 { format!("{} s on screen", span.round().max(1.0)) } else { format!("{} on screen", clock(span.round() as u64)) }
}

/// The filmstrip's tile step for a zoom: the largest grid step no longer than one tile's
/// worth of time (a tile is `tile_px` wide), so every tile on screen gets its own frame. The
/// grid is fixed per step, so scrolling reuses the frames already made.
pub fn tile_step(view: View, width_px: f64, tile_px: f64) -> f64 {
    let per_tile = view.span * tile_px / width_px.max(1.0);
    STEPS.iter().copied().rev().find(|s| *s <= per_tile).unwrap_or(STEPS[0])
}

/// Which tiles of that grid to have ready: the view and one screen either side, as a first
/// index and a count (capped). Tile `i` is the frame at `i * step`, shown for `step`.
pub fn tile_range(view: View, step: f64, duration: f64, cap: usize) -> (u64, usize) {
    let lo = (view.start - view.span).max(0.0);
    let hi = (view.end() + view.span).min(duration);
    let first = (lo / step).floor() as u64;
    let last = ((hi / step).ceil() as u64).max(first + 1);
    let count = ((last - first) as usize).min(cap);
    // Over the cap: keep the view itself, centred.
    if (last - first) as usize > cap {
        let mid = ((view.start + view.span / 2.0) / step) as u64;
        return (mid.saturating_sub(cap as u64 / 2), count);
    }
    (first, count)
}

/// The sound's loudness over time: one value (0–255, the loudest sample's level) for every
/// `1 / PEAKS_PER_SEC` seconds, so any part can be drawn at any zoom without decoding again.
#[derive(Clone, Debug, Default)]
pub struct Peaks {
    pub data: Vec<u8>,
}

pub const PEAKS_PER_SEC: f64 = 200.0;
/// Sample rate the sound is read at to measure the peaks.
pub const PEAKS_RATE: u32 = 8000;

impl Peaks {
    /// From mono signed 16-bit samples at [`PEAKS_RATE`].
    pub fn from_samples(samples: impl Iterator<Item = i16>) -> Peaks {
        let per = (PEAKS_RATE as f64 / PEAKS_PER_SEC) as usize;
        let mut data = Vec::new();
        let (mut n, mut max) = (0usize, 0u16);
        for s in samples {
            max = max.max(s.unsigned_abs());
            n += 1;
            if n == per {
                data.push((max >> 7).min(255) as u8);
                n = 0;
                max = 0;
            }
        }
        if n > 0 {
            data.push((max >> 7).min(255) as u8);
        }
        Peaks { data }
    }

    pub fn duration(&self) -> f64 {
        self.data.len() as f64 / PEAKS_PER_SEC
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, &self.data)
    }

    pub fn load(path: &Path) -> Option<Peaks> {
        let data = std::fs::read(path).ok()?;
        (!data.is_empty()).then_some(Peaks { data })
    }

    /// The part `view` drawn as a `w` × `h` RGBA picture: a bar per pixel column, mirrored
    /// around the middle, `rgb` on transparent. Levels go through a square root, so quiet
    /// parts still show (like ffmpeg's `showwavespic` with `scale=sqrt`).
    pub fn render(&self, view: View, w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let (w, h) = (w.max(1) as usize, h.max(2) as usize);
        let mut px = vec![0u8; w * h * 4];
        if self.data.is_empty() || view.span <= 0.0 {
            return px;
        }
        let per_px = view.span / w as f64 * PEAKS_PER_SEC;
        let mid = h as f64 / 2.0;
        for x in 0..w {
            let a = view.start * PEAKS_PER_SEC + x as f64 * per_px;
            let (i0, i1) = (a.floor().max(0.0) as usize, ((a + per_px).ceil() as usize).max(a.floor() as usize + 1));
            let Some(slice) = self.data.get(i0.min(self.data.len())..i1.min(self.data.len())) else { continue };
            let Some(&level) = slice.iter().max() else { continue };
            let half = ((level as f64 / 255.0).sqrt() * (mid - 0.5)).max(if level > 0 { 0.5 } else { 0.0 });
            let (y0, y1) = ((mid - half).floor().max(0.0) as usize, ((mid + half).ceil() as usize).min(h));
            for y in y0..y1 {
                let o = (y * w + x) * 4;
                px[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        px
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: f64 = 3600.0;

    #[test]
    fn clamping_keeps_the_view_inside() {
        assert_eq!(View { start: -5.0, span: 10.0 }.clamp(100.0, 1.0), View { start: 0.0, span: 10.0 });
        assert_eq!(View { start: 95.0, span: 10.0 }.clamp(100.0, 1.0), View { start: 90.0, span: 10.0 });
        assert_eq!(View { start: 3.0, span: 0.1 }.clamp(100.0, 1.0).span, 1.0);
        assert_eq!(View { start: 3.0, span: 500.0 }.clamp(100.0, 1.0), View::whole(100.0));
        // A video shorter than the shortest zoom: the whole of it.
        assert_eq!(View { start: 0.0, span: 0.2 }.clamp(0.5, 1.0), View::whole(0.5));
    }

    #[test]
    fn zoom_keeps_the_anchor_in_place() {
        let v = View { start: 1000.0, span: 100.0 };
        let z = v.zoom(0.5, 1025.0, HOUR, MIN_SPAN);
        assert_eq!(z.span, 50.0);
        // 1025 was a quarter in, and still is.
        assert!(((1025.0 - z.start) / z.span - 0.25).abs() < 1e-9);
        // Out past the whole video stops at the whole video.
        assert_eq!(v.zoom(1000.0, 1025.0, HOUR, MIN_SPAN), View::whole(HOUR));
        // In past the shortest stops there.
        assert_eq!(v.zoom(1e-6, 1025.0, HOUR, MIN_SPAN).span, MIN_SPAN);
        // Near the end: kept inside.
        let e = View { start: HOUR - 10.0, span: 10.0 }.zoom(4.0, HOUR, HOUR, MIN_SPAN);
        assert_eq!(e, View { start: HOUR - 40.0, span: 40.0 });
    }

    #[test]
    fn fit_frames_the_clip_with_room() {
        let v = View::fit(3070.0, 3090.0, HOUR, MIN_SPAN);
        assert!(v.start < 3070.0 && v.end() > 3090.0);
        assert!((v.span - 27.0).abs() < 1e-9);
        assert_eq!(View::fit(0.0, HOUR, HOUR, MIN_SPAN), View::whole(HOUR));
    }

    #[test]
    fn live_view_follows_the_edge() {
        let v = View::live(HOUR);
        assert_eq!(v, View { start: HOUR - 300.0, span: 300.0 });
        assert!(v.shows_edge(HOUR));
        assert_eq!(v.at_edge(HOUR + 5.0, MIN_SPAN_LIVE).end(), HOUR + 5.0);
        assert!(!View { start: 100.0, span: 300.0 }.shows_edge(HOUR));
        // A short window: all of it.
        assert_eq!(View::live(120.0), View::whole(120.0));
    }

    #[test]
    fn the_view_follows_the_playhead() {
        let v = View { start: 100.0, span: 20.0 };
        // Inside: nothing.
        assert_eq!(v.follow_playhead(110.0, 111.0, HOUR, MIN_SPAN), None);
        // Played off the right edge: pages along, the playhead near the left.
        let p = v.follow_playhead(119.9, 120.1, HOUR, MIN_SPAN).unwrap();
        assert!(p.start < 120.1 && 120.1 - p.start < 2.0 && p.span == 20.0);
        // A jump out of sight: centred.
        let j = v.follow_playhead(110.0, 2000.0, HOUR, MIN_SPAN).unwrap();
        assert_eq!(j, View { start: 1990.0, span: 20.0 });
        // Scrolled away while it plays: left alone.
        assert_eq!(v.follow_playhead(500.0, 500.03, HOUR, MIN_SPAN), None);
        // The whole video shows everything already.
        assert_eq!(View::whole(HOUR).follow_playhead(0.0, 3000.0, HOUR, MIN_SPAN), None);
    }

    #[test]
    fn ruler_steps_suit_the_zoom() {
        assert_eq!(ruler_step(HOUR * 1.5, 1000.0, 70.0), 600.0);
        assert_eq!(ruler_step(60.0, 1000.0, 70.0), 5.0);
        assert_eq!(ruler_step(2.0, 1000.0, 70.0), 0.2);
        assert_eq!(ruler_label(65.0, 5.0), "1:05");
        assert_eq!(ruler_label(3725.0, 600.0), "1:02:05");
        assert_eq!(ruler_label(12.5, 0.5), "0:12.5");
        assert_eq!(span_text(12.4), "12 s on screen");
        assert_eq!(span_text(270.0), "4:30 on screen");
    }

    #[test]
    fn tiles_sit_on_a_grid() {
        // 60 s over 1000 px with 100 px tiles: 6 s per tile → 5 s grid.
        let v = View { start: 600.0, span: 60.0 };
        let step = tile_step(v, 1000.0, 100.0);
        assert_eq!(step, 5.0);
        let (first, count) = tile_range(v, step, HOUR, 200);
        assert_eq!(first, 108); // 540 s: one screen to the left
        assert_eq!(count, 36); // to 720 s
        // Scrolled a little: the same grid, mostly the same tiles.
        let (f2, _) = tile_range(v.pan_to(610.0, HOUR, 1.0), step, HOUR, 200);
        assert_eq!(f2, 110);
        // Capped around the view.
        let (f3, c3) = tile_range(View { start: 1000.0, span: 600.0 }, 0.1, HOUR, 40);
        assert_eq!(c3, 40);
        assert_eq!(f3, 13000 - 20);
    }

    #[test]
    fn peaks_measure_and_draw() {
        // One second loud, one second silent.
        let loud = std::iter::repeat_n(i16::MAX, PEAKS_RATE as usize);
        let quiet = std::iter::repeat_n(0i16, PEAKS_RATE as usize);
        let p = Peaks::from_samples(loud.chain(quiet));
        assert_eq!(p.data.len(), 400);
        assert!((p.duration() - 2.0).abs() < 1e-9);
        assert_eq!(p.data[0], 255);
        assert_eq!(p.data[399], 0);
        // Drawn over both seconds: the left half full height, the right half empty.
        let px = p.render(View::whole(2.0), 10, 8, [1, 2, 3]);
        let alpha = |x: usize, y: usize| px[(y * 10 + x) * 4 + 3];
        assert_eq!(alpha(2, 0), 255);
        assert_eq!(alpha(2, 7), 255);
        assert_eq!(alpha(8, 3), 0);
        // Zoomed into the loud second: every column drawn.
        let px = p.render(View { start: 0.2, span: 0.5 }, 10, 8, [1, 2, 3]);
        assert!((0..10).all(|x| px[(4 * 10 + x) * 4 + 3] == 255));
    }
}
