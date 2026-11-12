//! Captions, audio tracks and chapters of the previewed video: the buttons under the
//! quality chips, the shared picker dialog, and what the picks add to the download.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use neria_core::download::DownloadRequest;
use neria_core::probe::MediaInfo;
use neria_core::settings::Quality;
use neria_core::settings::Settings;
use neria_core::tracks::{AudioTrack, Caption, CaptionChoice, SubFormat, SubMode};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppWindow, Extras, TrackRow};

#[derive(Default)]
struct State {
    captions: Vec<Caption>,
    /// Picked captions, as row keys: `c:<lang>` (creator) / `a:<lang>` (auto-generated).
    cap_picked: HashSet<String>,
    /// A site where videos usually have captions (YouTube): say when this one has none.
    expect_captions: bool,
    audio: Vec<AudioTrack>,
    /// Picked audio languages (the original when none is).
    audio_picked: HashSet<String>,
}

/// The audio tracks to download, original first; empty = just the default track.
fn picked_audio(s: &State) -> Vec<&AudioTrack> {
    let picked: Vec<&AudioTrack> = s.audio.iter().filter(|t| s.audio_picked.contains(&t.lang)).collect();
    let only_original = picked.len() == 1 && picked[0].original;
    if only_original || picked.is_empty() { Vec::new() } else { picked }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn cap_key(c: &Caption) -> String {
    format!("{}:{}", if c.auto { "a" } else { "c" }, c.lang)
}

/// "English", "English +2", "none".
fn summary(names: &[String]) -> String {
    match names.len() {
        0 => "none".into(),
        1 => names[0].clone(),
        n => format!("{} +{}", names[0], n - 1),
    }
}

fn picked_captions(s: &State) -> Vec<&Caption> {
    s.captions.iter().filter(|c| s.cap_picked.contains(&cap_key(c))).collect()
}

/// The button texts under the quality chips.
fn refresh_buttons(ui: &AppWindow) {
    let ex = ui.global::<Extras>();
    STATE.with(|s| {
        let s = s.borrow();
        let caps: Vec<String> = picked_captions(&s).iter().map(|c| c.name.clone()).collect();
        let only = ex.get_subs_only() && !caps.is_empty();
        ex.set_captions_text(if s.captions.is_empty() {
            "".into()
        } else {
            format!("{}: {}", if only { "Captions only" } else { "Captions" }, summary(&caps)).into()
        });
        ex.set_no_captions(s.captions.is_empty() && s.expect_captions);
        let audio: Vec<String> = picked_audio(&s).iter().map(|t| t.name.clone()).collect();
        ex.set_audio_text(match audio.len() {
            _ if s.audio.is_empty() => "".into(),
            0 => {
                let original = s.audio.iter().find(|t| t.original).map(|t| t.name.clone());
                format!("Audio: Original{}", original.map(|n| format!(" ({n})")).unwrap_or_default()).into()
            }
            1 => format!("Audio: {}", audio[0]).into(),
            2 => format!("Audio: {} + {}", audio[0], audio[1]).into(),
            n => format!("Audio: {n} languages").into(),
        });
    });
}

/// The picker's rows for what's open, filtered by the search box.
fn refresh_rows(ui: &AppWindow) {
    let ex = ui.global::<Extras>();
    let query = ex.get_query().to_lowercase();
    let mut rows: Vec<TrackRow> = Vec::new();
    let (mut total, mut picked) = (0, 0);
    STATE.with(|s| {
        let s = s.borrow();
        if ex.get_open() == 1 {
            total = s.captions.len() as i32;
            picked = s.cap_picked.len() as i32;
            let own = s.captions.first().map(|c| c.lang.clone());
            let mut group = None;
            for c in &s.captions {
                if !query.is_empty()
                    && !c.name.to_lowercase().contains(&query)
                    && !c.lang.to_lowercase().starts_with(&query)
                {
                    continue;
                }
                if group != Some(c.auto) {
                    group = Some(c.auto);
                    rows.push(TrackRow {
                        label: if c.auto { "AUTO-GENERATED" } else { "MADE BY THE CREATOR" }.into(),
                        header: true,
                        ..Default::default()
                    });
                }
                let detail = if Some(&c.lang) == own.as_ref() && !c.auto { "video's language" } else { "" };
                rows.push(TrackRow {
                    key: cap_key(c).into(),
                    label: c.name.as_str().into(),
                    detail: detail.into(),
                    checked: s.cap_picked.contains(&cap_key(c)),
                    header: false,
                });
            }
        }
        if ex.get_open() == 2 {
            total = s.audio.len() as i32;
            picked = s.audio_picked.len() as i32;
            for t in &s.audio {
                rows.push(TrackRow {
                    key: format!("l:{}", t.lang).into(),
                    label: t.name.as_str().into(),
                    detail: if t.original { "Original" } else { "" }.into(),
                    checked: s.audio_picked.contains(&t.lang),
                    header: false,
                });
            }
        }
    });
    ex.set_total(total);
    ex.set_picked(picked);
    ex.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
}

/// A new preview: what this video has, with last time's captions languages ticked.
pub fn set_info(ui: &AppWindow, info: &MediaInfo, settings: &Settings) {
    let ex = ui.global::<Extras>();
    ex.set_sub_format(settings.caption_format.index());
    ex.set_sub_mode(settings.caption_mode.index());
    ex.set_subs_only(false);
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        *s = State::default();
        if !info.items.is_empty() {
            return;
        }
        s.captions = info.captions.clone();
        s.expect_captions = info.key.as_deref().is_some_and(|k| k.starts_with("Youtube:"));
        s.audio = info.audio_tracks.clone();
        let original = s.audio.iter().find(|t| t.original).or(s.audio.first()).map(|t| t.lang.clone());
        s.audio_picked = original.into_iter().collect();
        // Last time's languages: the creator's version if there is one, else auto-generated.
        for lang in &settings.caption_langs {
            let found = s
                .captions
                .iter()
                .find(|c| !c.auto && c.lang.eq_ignore_ascii_case(lang))
                .or_else(|| s.captions.iter().find(|c| c.auto && c.lang.eq_ignore_ascii_case(lang)))
                .map(cap_key);
            if let Some(k) = found {
                s.cap_picked.insert(k);
            }
        }
    });
    refresh_buttons(ui);
}

/// No video previewed (cleared, a playlist, an error).
pub fn clear(ui: &AppWindow) {
    STATE.with(|s| *s.borrow_mut() = State::default());
    ui.global::<Extras>().set_open(0);
    refresh_buttons(ui);
}

/// The picks, applied to the download of the previewed video. Usually one request; some
/// picks make several (they then share a group titled after the video).
pub fn requests(ui: &AppWindow, mut base: DownloadRequest) -> Vec<DownloadRequest> {
    let ex = ui.global::<Extras>();
    STATE.with(|s| {
        let s = s.borrow();
        let audio: Vec<(String, String)> = picked_audio(&s).iter().map(|t| (t.lang.clone(), t.name.clone())).collect();
        if !s.captions.is_empty() {
            let picked = picked_captions(&s);
            base.captions = Some(CaptionChoice {
                langs: picked.iter().filter(|c| !c.auto).map(|c| c.lang.clone()).collect(),
                auto_langs: picked.iter().filter(|c| c.auto).map(|c| c.lang.clone()).collect(),
                format: SubFormat::from_index(ex.get_sub_format()),
                mode: SubMode::from_index(ex.get_sub_mode()),
                only: ex.get_subs_only(),
            });
        }
        if base.captions_only() || audio.is_empty() {
            return vec![base];
        }
        if base.quality != Quality::Audio || audio.len() == 1 {
            // One file: one audio track per language (or just the one picked).
            base.audio_langs = audio.into_iter().map(|a| a.0).collect();
            return vec![base];
        }
        // Audio only, several languages: one file each, "Title (Spanish).mp3". Captions
        // come with the first.
        audio
            .into_iter()
            .enumerate()
            .map(|(i, (lang, name))| {
                let mut r = base.clone();
                r.audio_langs = vec![lang];
                r.name_tag = Some(format!(" ({name})"));
                r.title_hint = format!("{} ({name})", base.title_hint);
                if i > 0 {
                    r.captions = None;
                }
                r
            })
            .collect()
    })
}

/// Keep this download's caption choices for next time.
pub fn remember(ui: &AppWindow, settings: &Arc<Mutex<Settings>>) {
    let ex = ui.global::<Extras>();
    let langs: Option<Vec<String>> = STATE.with(|s| {
        let s = s.borrow();
        (!s.captions.is_empty()).then(|| picked_captions(&s).iter().map(|c| c.lang.clone()).collect())
    });
    let mut st = settings.lock().unwrap();
    if let Some(langs) = langs {
        st.caption_langs = langs;
    }
    st.caption_format = SubFormat::from_index(ex.get_sub_format());
    st.caption_mode = SubMode::from_index(ex.get_sub_mode());
    let _ = st.save();
}

pub fn install(ui: &AppWindow) {
    let ex = ui.global::<Extras>();
    ex.on_show({
        let weak = ui.as_weak();
        move |kind| {
            let ui = weak.unwrap();
            let ex = ui.global::<Extras>();
            ex.set_query("".into());
            ex.set_note("".into());
            ex.set_open(kind);
            refresh_rows(&ui);
        }
    });
    ex.on_toggle({
        let weak = ui.as_weak();
        move |key, on| {
            let ui = weak.unwrap();
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                let key = key.to_string();
                if key.starts_with("c:") || key.starts_with("a:") {
                    if on {
                        s.cap_picked.insert(key);
                    } else {
                        s.cap_picked.remove(&key);
                    }
                } else if let Some(lang) = key.strip_prefix("l:") {
                    if on {
                        s.audio_picked.insert(lang.to_string());
                    } else {
                        s.audio_picked.remove(lang);
                    }
                }
            });
            refresh_rows(&ui);
            refresh_buttons(&ui);
        }
    });
    ex.on_all({
        let weak = ui.as_weak();
        move |on| {
            let ui = weak.unwrap();
            let open = ui.global::<Extras>().get_open();
            // "All" ticks what the search shows (all of it when there's no search).
            let shown: Vec<String> =
                ui.global::<Extras>().get_rows().iter().filter(|r| !r.header).map(|r| r.key.to_string()).collect();
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                for k in shown {
                    match (open, k.strip_prefix("l:")) {
                        (2, Some(lang)) if on => {
                            s.audio_picked.insert(lang.to_string());
                        }
                        (2, Some(lang)) => {
                            s.audio_picked.remove(lang);
                        }
                        (1, _) if on => {
                            s.cap_picked.insert(k);
                        }
                        (1, _) => {
                            s.cap_picked.remove(&k);
                        }
                        _ => {}
                    }
                }
            });
            refresh_rows(&ui);
            refresh_buttons(&ui);
        }
    });
    ex.on_search({
        let weak = ui.as_weak();
        move |_| refresh_rows(&weak.unwrap())
    });
    ex.on_options_changed({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            refresh_rows(&ui);
            refresh_buttons(&ui);
        }
    });
}
