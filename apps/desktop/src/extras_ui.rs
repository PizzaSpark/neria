//! Captions, audio tracks and chapters of the previewed video: the buttons under the
//! quality chips, the shared picker dialog, and what the picks add to the download.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use neria_core::download::DownloadRequest;
use neria_core::probe::MediaInfo;
use neria_core::settings::Settings;
use neria_core::tracks::{Caption, CaptionChoice, SubFormat, SubMode};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppWindow, Extras, TrackRow};

#[derive(Default)]
struct State {
    captions: Vec<Caption>,
    /// Picked captions, as row keys: `c:<lang>` (creator) / `a:<lang>` (auto-generated).
    cap_picked: HashSet<String>,
    /// A site where videos usually have captions (YouTube): say when this one has none.
    expect_captions: bool,
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
    });
    vec![base]
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
                if open == 1 {
                    for k in shown {
                        if on {
                            s.cap_picked.insert(k);
                        } else {
                            s.cap_picked.remove(&k);
                        }
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
