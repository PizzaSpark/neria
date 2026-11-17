//! Clip page → Sound (`ClipSound` global, `dialogs/sound.slint`): reading the dialog into a
//! `sound::AudioEdit`, its button text and note. The player side is `Clipper::apply_sound`.

use neria_core::sound::{AudioEdit, Level};
use slint::ComponentHandle;

use crate::{AppWindow, ClipSound};

const FADES: [f64; 5] = [0.0, 0.5, 1.0, 2.0, 3.0];
pub const FORMATS: [&str; 5] = ["mp3", "m4a", "opus", "flac", "wav"];

/// Choices the open clip can't have go back to their default: Mute and Sound only need a
/// picture to keep, and links pick sound only with the Audio only chip instead.
pub fn fit_to_clip(ui: &AppWindow) {
    let g = ui.global::<ClipSound>();
    // A silent video (or an X GIF) has no Sound button: nothing may stay picked unseen.
    if !ui.get_clip_has_audio() || ui.get_clip_is_gif() {
        reset(ui);
        return;
    }
    let (video, link) = (ui.get_clip_has_video(), ui.get_clip_is_link());
    if !video || link {
        g.set_save(0);
    }
    if video || link {
        g.set_file_format(0);
    }
    if !video && g.get_level() == 3 {
        g.set_level(0);
    }
    refresh(ui);
}

/// What the dialog says, as the edit core applies.
pub fn edit(ui: &AppWindow) -> AudioEdit {
    let g = ui.global::<ClipSound>();
    let level = match g.get_level() {
        1 => Level::Gain(g.get_gain().round().clamp(-12.0, 12.0)),
        2 => Level::Normalize(if g.get_target() == 1 { -16.0 } else { -14.0 }),
        3 => Level::Mute,
        _ => Level::AsIs,
    };
    let fade = |i: i32| FADES.get(i.max(0) as usize).copied().unwrap_or(0.0);
    let muted = level == Level::Mute;
    AudioEdit {
        level,
        fade_in: if muted { 0.0 } else { fade(g.get_fade_in()) },
        fade_out: if muted { 0.0 } else { fade(g.get_fade_out()) },
    }
}

/// Sound only, in this format: a video's sound (Save → Sound only), or a sound file saved as
/// another format (Save as).
pub fn audio_only(ui: &AppWindow) -> Option<String> {
    let g = ui.global::<ClipSound>();
    if ui.get_clip_is_link() {
        return None;
    }
    let pick = |i: i32| FORMATS.get(i.max(0) as usize).unwrap_or(&"mp3").to_string();
    if ui.get_clip_has_video() {
        (g.get_save() == 1).then(|| pick(g.get_save_format()))
    } else {
        (g.get_file_format() > 0).then(|| pick(g.get_file_format() - 1))
    }
}

/// The Clip page's button and the dialog's note line.
pub fn refresh(ui: &AppWindow) {
    let g = ui.global::<ClipSound>();
    let e = edit(ui);
    let only = audio_only(ui);
    // A sound file saved as another format says the format, not "Sound only".
    let lead = if ui.get_clip_has_video() { "Sound only" } else { "Save as" };
    let button = match (&only, e.is_none()) {
        (Some(f), true) => format!("{lead} · {f}"),
        (Some(f), false) => format!("{lead} · {f} · {}", e.summary()),
        (None, _) => format!("Sound: {}", e.summary()),
    };
    g.set_button_text(button.into());
    let short = match (&only, e.level) {
        (Some(f), _) if !ui.get_clip_has_video() => f.clone(),
        (Some(_), _) => "Sound only".to_string(),
        (None, Level::Mute) => "Muted".into(),
        (None, Level::Gain(db)) if db.abs() >= 0.05 => format!("{}{db:.0} dB", if db > 0.0 { "+" } else { "" }),
        (None, Level::Normalize(_)) => "Evened out".into(),
        _ if !e.is_none() => "Fades".into(),
        _ => "Sound".into(),
    };
    g.set_short_text(short.into());

    let fast = ui.get_clip_mode() == 0 && ui.get_clip_has_video();
    let note = if ui.get_clip_is_link() && !e.is_none() {
        "Changed after the clip downloads; the picture isn't touched."
    } else if only.is_some() && !ui.get_clip_has_video() {
        "Converted to the new format, cut exactly at In and Out."
    } else if only.is_some() {
        "Only the sound is saved, cut exactly at In and Out."
    } else if e.mutes() {
        ""
    } else if e.reencodes() && fast {
        "Best quality still copies the picture as it is; only the sound is re-encoded."
    } else if e.fade_in > 0.0 || e.fade_out > 0.0 {
        "You hear the volume while it plays; fades are added when it's saved."
    } else {
        ""
    };
    g.set_note(note.into());
}

pub fn reset(ui: &AppWindow) {
    let g = ui.global::<ClipSound>();
    g.set_level(0);
    g.set_gain(0.0);
    g.set_target(0);
    g.set_fade_in(0);
    g.set_fade_out(0);
    g.set_save(0);
    g.set_file_format(0);
    refresh(ui);
}
