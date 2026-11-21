//! Clip page → Sound (`ClipSound` global, `dialogs/sound.slint`): reading the dialog into a
//! `sound::AudioEdit`, its button text and note. The player side is `Clipper::apply_sound`.

use neria_core::clip::TrackMix;
use neria_core::sound::{AudioEdit, Level};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppWindow, ClipSound, ClipTrack};

/// An instant replay's own tracks (its sound track names, "Everything" first): the rest
/// become the Tracks group, all on at 100%. Any other file: none.
pub fn set_tracks(ui: &AppWindow, titles: &[String]) {
    let rows: Vec<ClipTrack> = if titles.len() >= 2 && titles[0] == "Everything" {
        titles[1..].iter().map(|t| ClipTrack { name: t.into(), on: true, volume: 1.0 }).collect()
    } else {
        Vec::new()
    };
    let g = ui.global::<ClipSound>();
    g.set_tracks(ModelRc::new(VecModel::from(rows)));
    g.set_tracks_apart(false);
}

/// The Tracks group as the export takes it (indices count from the file's second track).
pub fn tracks(ui: &AppWindow) -> Vec<TrackMix> {
    let g = ui.global::<ClipSound>();
    g.get_tracks()
        .iter()
        .enumerate()
        .map(|(i, t)| TrackMix { index: i as u32 + 1, name: t.name.to_string(), on: t.on, volume: t.volume })
        .collect()
}

/// The tracks are mixed again (one off, louder or quieter, or kept apart).
pub fn mixes_tracks(ui: &AppWindow) -> bool {
    let mut req = neria_core::clip::ClipRequest::new("", 0.0, 1.0, neria_core::clip::ClipMode::Fast, true);
    req.audio = edit(ui);
    req.tracks = tracks(ui);
    req.tracks_apart = ui.global::<ClipSound>().get_tracks_apart();
    req.mixes_tracks()
}

/// For the player: the tracks mixed as set (mpv's `lavfi-complex`), "" when as they are.
pub fn player_mix(ui: &AppWindow) -> String {
    if !mixes_tracks(ui) {
        return String::new();
    }
    neria_core::clip::tracks_graph(&tracks(ui), false, None, true).unwrap_or_default()
}

pub fn install(ui: &AppWindow) {
    let g = ui.global::<ClipSound>();
    g.on_track_set({
        let weak = ui.as_weak();
        move |i, on, volume| {
            let ui = weak.unwrap();
            let g = ui.global::<ClipSound>();
            let rows = g.get_tracks();
            if let Some(mut row) = rows.row_data(i as usize) {
                row.on = on;
                row.volume = (volume * 10.0).round() / 10.0;
                rows.set_row_data(i as usize, row);
            }
            g.invoke_changed();
        }
    });
}

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
    let mixing = mixes_tracks(ui);
    let button = match (&only, e.is_none()) {
        (None, true) if mixing => "Sound: tracks mixed".to_string(),
        (None, false) if mixing => format!("Sound: {} · tracks", e.summary()),
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
        _ if mixing => "Tracks".into(),
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
    } else if (e.reencodes() || mixing) && fast {
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
    let rows = g.get_tracks();
    for i in 0..rows.row_count() {
        if let Some(mut r) = rows.row_data(i) {
            r.on = true;
            r.volume = 1.0;
            rows.set_row_data(i, r);
        }
    }
    g.set_tracks_apart(false);
    refresh(ui);
}
