//! Clip page → Picture: rotate, flip and crop, applied when the clip is saved (the video is
//! re-encoded) and shown live in the player. One filter chain serves every path: export,
//! the player, Make a GIF and link clips after their download.
//!
//! Order: rotate, then flip, then crop. The crop is in the turned picture's own frame, so
//! "9:16" is the shape of the saved file. It's kept as fractions of that frame and written
//! with `iw`/`ih`, so it fits whatever size the file really is (links: known only later).

use serde::{Deserialize, Serialize};

/// Part of the (turned) picture to keep, as fractions 0..1 of its width and height.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Crop {
    /// Kept inside the picture, and never smaller than a sliver.
    pub fn clamped(self) -> Crop {
        let w = self.w.clamp(0.05, 1.0);
        let h = self.h.clamp(0.05, 1.0);
        Crop { x: self.x.clamp(0.0, 1.0 - w), y: self.y.clamp(0.0, 1.0 - h), w, h }
    }

    /// The whole picture: no crop at all.
    pub fn is_full(&self) -> bool {
        self.x <= 0.001 && self.y <= 0.001 && self.w >= 0.999 && self.h >= 0.999
    }

    /// The biggest crop of shape `ratio` (width / height) centred in a `pic_w` × `pic_h`
    /// picture (any units: only their ratio counts).
    pub fn centred(ratio: f64, pic_w: f64, pic_h: f64) -> Crop {
        let pic = pic_w / pic_h.max(1e-9);
        let (w, h) = if ratio < pic { (ratio / pic, 1.0) } else { (1.0, pic / ratio) };
        Crop { x: (1.0 - w) / 2.0, y: (1.0 - h) / 2.0, w, h }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Picture {
    /// Clockwise quarter turns: 0, 1 (90°), 2 (180°), 3 (270°).
    pub turns: u8,
    /// Mirror left-right / upside down (after turning).
    pub flip_h: bool,
    pub flip_v: bool,
    pub crop: Option<Crop>,
}

impl Picture {
    pub fn is_none(&self) -> bool {
        self.turns.is_multiple_of(4) && !self.flip_h && !self.flip_v && self.crop.is_none_or(|c| c.is_full())
    }

    /// The ffmpeg (and mpv lavfi) video filters; `None` when the picture isn't touched.
    pub fn filter(&self) -> Option<String> {
        let mut f: Vec<String> = Vec::new();
        match self.turns % 4 {
            1 => f.push("transpose=1".into()),
            2 => f.push("hflip,vflip".into()),
            3 => f.push("transpose=2".into()),
            _ => {}
        }
        if self.flip_h {
            f.push("hflip".into());
        }
        if self.flip_v {
            f.push("vflip".into());
        }
        if let Some(c) = self.crop.map(Crop::clamped).filter(|c| !c.is_full()) {
            // Even sides (H.264 wants them). No commas: mpv's lavfi=[…] takes it as is.
            f.push(format!(
                "crop=floor(iw*{w:.6}/2)*2:floor(ih*{h:.6}/2)*2:floor(iw*{x:.6}):floor(ih*{y:.6})",
                w = c.w,
                h = c.h,
                x = c.x,
                y = c.y
            ));
        }
        (!f.is_empty()).then(|| f.join(","))
    }

    /// Only the turning and flipping (the player while the crop frame is up).
    pub fn without_crop(&self) -> Picture {
        Picture { crop: None, ..*self }
    }

    /// In a few words ("as is", "9:16 crop · turned", "mirrored").
    pub fn summary(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.crop.is_some_and(|c| !c.is_full()) {
            parts.push("cropped");
        }
        match self.turns % 4 {
            0 => {}
            2 => parts.push("upside down"),
            _ => parts.push("turned"),
        }
        if self.flip_h || self.flip_v {
            parts.push("mirrored");
        }
        if parts.is_empty() { "as is".into() } else { parts.join(" · ") }
    }

    /// The saved picture's size for a `w` × `h` source (for messages and checks).
    pub fn output_size(&self, w: u32, h: u32) -> (u32, u32) {
        let (w, h) = if self.turns % 2 == 1 { (h, w) } else { (w, h) };
        match self.crop.map(Crop::clamped).filter(|c| !c.is_full()) {
            Some(c) => {
                let even = |v: f64| ((v / 2.0).floor() * 2.0).max(2.0) as u32;
                (even(w as f64 * c.w), even(h as f64 * c.h))
            }
            None => (w, h),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_has_no_filter() {
        assert!(Picture::default().is_none());
        assert_eq!(Picture::default().filter(), None);
        let full = Picture { crop: Some(Crop { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }), ..Default::default() };
        assert!(full.is_none());
        assert_eq!(full.summary(), "as is");
    }

    #[test]
    fn turn_then_flip_then_crop() {
        let p = Picture {
            turns: 1,
            flip_h: true,
            crop: Some(Crop { x: 0.25, y: 0.0, w: 0.5, h: 1.0 }),
            ..Default::default()
        };
        let f = p.filter().unwrap();
        assert!(f.starts_with("transpose=1,hflip,crop="), "{f}");
        assert!(f.contains("floor(iw*0.500000/2)*2") && f.contains("floor(iw*0.250000)"), "{f}");
        assert_eq!(p.summary(), "cropped · turned · mirrored");
        // 1920×1080 turned is 1080×1920; half its width is 540.
        assert_eq!(p.output_size(1920, 1080), (540, 1920));
    }

    #[test]
    fn centred_crops_fit_the_shape() {
        // 9:16 out of 16:9: full height, a sliver of the width.
        let c = Crop::centred(9.0 / 16.0, 1920.0, 1080.0);
        assert!((c.h - 1.0).abs() < 1e-9);
        assert!((c.w - (9.0 / 16.0) / (16.0 / 9.0)).abs() < 1e-9);
        assert!((c.x - (1.0 - c.w) / 2.0).abs() < 1e-9);
        // 16:9 out of 9:16: full width.
        let c = Crop::centred(16.0 / 9.0, 1080.0, 1920.0);
        assert!((c.w - 1.0).abs() < 1e-9 && c.h < 0.4);
        let p = Picture { crop: Some(Crop::centred(1.0, 1920.0, 1080.0)), ..Default::default() };
        assert_eq!(p.output_size(1920, 1080), (1080, 1080));
    }

    #[test]
    fn clamped_stays_inside() {
        let c = Crop { x: 0.9, y: -0.2, w: 0.5, h: 2.0 }.clamped();
        assert!((c.x - 0.5).abs() < 1e-9 && c.y == 0.0 && c.h == 1.0);
    }
}
