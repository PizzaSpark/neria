//! Clip page → Picture (`ClipPicture` global): turn, mirror and crop, read into a
//! `picture::Picture`; the button texts; and what the player shows (`Clipper::apply_picture`).

use neria_core::picture::{Crop, Picture};
use slint::ComponentHandle;

use crate::{AppWindow, ClipPicture};

/// Width / height of each shape chip (0 whole picture, 1 free).
fn ratio(aspect: i32) -> Option<f64> {
    match aspect {
        2 => Some(9.0 / 16.0),
        3 => Some(1.0),
        4 => Some(4.0 / 5.0),
        5 => Some(16.0 / 9.0),
        _ => None,
    }
}

pub fn picture(ui: &AppWindow) -> Picture {
    let g = ui.global::<ClipPicture>();
    let crop = (g.get_aspect() != 0).then(|| {
        Crop { x: g.get_cx() as f64, y: g.get_cy() as f64, w: g.get_cw() as f64, h: g.get_ch() as f64 }.clamped()
    });
    Picture { turns: g.get_turns().rem_euclid(4) as u8, flip_h: g.get_flip_h(), flip_v: g.get_flip_v(), crop }
}

/// The filters for the player: while the crop frame is up, only the turning and mirroring
/// (the frame is drawn over that); otherwise the finished picture. "" = none.
/// `rotation` is the file's own (phone footage, clockwise degrees): mpv turns the picture
/// after its filters, ffmpeg before, so when there are filters the caller switches mpv's
/// turning off and the file's rotation goes first here, as in the saved file.
pub fn player_filter(ui: &AppWindow, rotation: i64) -> String {
    let g = ui.global::<ClipPicture>();
    let p = picture(ui);
    let p = if g.get_editing() { p.without_crop() } else { p };
    let Some(f) = p.filter() else { return String::new() };
    let upright = match rotation.rem_euclid(360) {
        90 => "transpose=1,",
        180 => "hflip,vflip,",
        270 => "transpose=2,",
        _ => "",
    };
    format!("{upright}{f}")
}

/// A shape chip or a turn: the biggest crop of that shape, centred in the turned picture
/// (`src_w` × `src_h` is the picture before turning). Free keeps the frame (or starts at 80%).
pub fn aspect_picked(ui: &AppWindow, src_w: f64, src_h: f64) {
    let g = ui.global::<ClipPicture>();
    let (w, h) = if g.get_turns().rem_euclid(2) == 1 { (src_h, src_w) } else { (src_w, src_h) };
    let crop = match (g.get_aspect(), ratio(g.get_aspect())) {
        (_, Some(r)) if w > 0.0 && h > 0.0 => Crop::centred(r, w, h),
        (1, _) => {
            let c = Crop { x: g.get_cx() as f64, y: g.get_cy() as f64, w: g.get_cw() as f64, h: g.get_ch() as f64 };
            if c.is_full() { Crop { x: 0.1, y: 0.1, w: 0.8, h: 0.8 } } else { c.clamped() }
        }
        _ => Crop { x: 0.0, y: 0.0, w: 1.0, h: 1.0 },
    };
    g.set_cx(crop.x as f32);
    g.set_cy(crop.y as f32);
    g.set_cw(crop.w as f32);
    g.set_ch(crop.h as f32);
    refresh(ui);
}

pub fn refresh(ui: &AppWindow) {
    let g = ui.global::<ClipPicture>();
    let p = picture(ui);
    let shape = match g.get_aspect() {
        2 => "9:16",
        3 => "1:1",
        4 => "4:5",
        5 => "16:9",
        _ => "",
    };
    let mut summary = p.summary();
    if !shape.is_empty() && p.crop.is_some_and(|c| !c.is_full()) {
        summary = summary.replacen("cropped", &format!("{shape} crop"), 1);
    }
    g.set_button_text(format!("Picture: {summary}").into());
    let short = if p.is_none() {
        "Picture".to_string()
    } else if !shape.is_empty() && p.crop.is_some() {
        shape.to_string()
    } else if p.crop.is_some_and(|c| !c.is_full()) {
        "Cropped".into()
    } else if p.turns % 4 != 0 {
        "Turned".into()
    } else {
        "Mirrored".into()
    };
    g.set_short_text(short.into());
    g.set_changed_any(!p.is_none());
}

pub fn reset(ui: &AppWindow) {
    let g = ui.global::<ClipPicture>();
    g.set_turns(0);
    g.set_flip_h(false);
    g.set_flip_v(false);
    g.set_aspect(0);
    g.set_cx(0.0);
    g.set_cy(0.0);
    g.set_cw(1.0);
    g.set_ch(1.0);
    refresh(ui);
}
