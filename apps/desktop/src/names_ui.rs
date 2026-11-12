//! The filename builder (Settings → Download → File names → Change): pieces in, the
//! yt-dlp template out (kept in Settings' `filename_template`, as before).

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use neria_core::names::{self, Field, Piece};
use neria_core::settings::Settings;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{AppWindow, NamePiece, Names};

fn pieces_of(ui: &AppWindow) -> Option<Vec<Piece>> {
    names::from_template(&ui.get_filename_template())
}

/// Save these pieces as the template, then show them.
fn set_pieces(ui: &AppWindow, settings: &Arc<Mutex<Settings>>, pieces: Vec<Piece>) {
    ui.set_filename_template(names::to_template(&pieces).into());
    ui.invoke_settings_changed();
    refresh(ui, settings);
}

/// Everything the dialog and Settings' row show, from the current template.
pub fn refresh(ui: &AppWindow, settings: &Arc<Mutex<Settings>>) {
    let n = ui.global::<Names>();
    let template = ui.get_filename_template().to_string();
    let template = if template.trim().is_empty() { "%(title)s [%(id)s].%(ext)s".to_string() } else { template };
    let pieces = names::from_template(&template);
    let saved = settings.lock().unwrap().saved_names.clone();
    let mut presets: Vec<(String, String)> =
        names::presets().into_iter().map(|(name, p)| (name.to_string(), names::to_template(&p))).collect();
    presets.extend(saved);
    let current = presets.iter().position(|(_, t)| *t == template).map_or(-1, |i| i as i32);
    n.set_presets(ModelRc::from(Rc::new(VecModel::from(
        presets.iter().map(|(name, _)| SharedString::from(name.as_str())).collect::<Vec<_>>(),
    ))));
    n.set_preset(current);
    n.set_fields(ModelRc::from(Rc::new(VecModel::from(
        Field::ALL.iter().map(|f| SharedString::from(f.label())).collect::<Vec<_>>(),
    ))));
    n.set_raw(template.as_str().into());
    n.set_custom(pieces.is_none());
    // The example uses the previewed video's title when there is one.
    let mut values = names::sample_values();
    if ui.get_probe_state() == 2 && !ui.get_probe_title().is_empty() {
        values.insert("title", ui.get_probe_title().to_string());
    }
    let ext = if crate::chosen_quality(ui).0 == neria_core::settings::Quality::Audio {
        settings.lock().unwrap().audio_format.clone()
    } else {
        settings.lock().unwrap().container.clone()
    };
    values.insert("ext", ext);
    match &pieces {
        Some(p) => {
            let rows: Vec<NamePiece> = p
                .iter()
                .map(|piece| match piece {
                    Piece::Field(f) => NamePiece { text: f.label().into(), field: true },
                    Piece::Text(t) => NamePiece { text: t.as_str().into(), field: false },
                })
                .collect();
            n.set_pieces(ModelRc::from(Rc::new(VecModel::from(rows))));
            n.set_example(names::example(p, &values).into());
            n.set_warning(names::warnings(p).first().cloned().unwrap_or_default().into());
            n.set_summary(names::describe(p).into());
        }
        None => {
            n.set_pieces(ModelRc::from(Rc::new(VecModel::<NamePiece>::default())));
            n.set_example("(a custom yt-dlp template: no example)".into());
            n.set_warning("".into());
            n.set_summary(template.as_str().into());
        }
    }
}

pub fn install(ui: &AppWindow, settings: Arc<Mutex<Settings>>) {
    refresh(ui, &settings);
    let n = ui.global::<Names>();
    n.on_show({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move || {
            let ui = weak.unwrap();
            refresh(&ui, &settings);
            ui.global::<Names>().set_open(true);
        }
    });
    n.on_add_field({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move |i| {
            let ui = weak.unwrap();
            let (Some(mut p), Some(f)) = (pieces_of(&ui), Field::from_index(i.max(0) as usize)) else { return };
            // Fields next to each other would run together: a space between.
            if matches!(p.last(), Some(Piece::Field(_))) {
                p.push(Piece::Text(" ".into()));
            }
            p.push(Piece::Field(f));
            set_pieces(&ui, &settings, p);
        }
    });
    n.on_add_text({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move |text| {
            let ui = weak.unwrap();
            if text.is_empty() {
                return;
            }
            let Some(mut p) = pieces_of(&ui) else { return };
            match p.last_mut() {
                Some(Piece::Text(t)) => t.push_str(&text),
                _ => p.push(Piece::Text(text.to_string())),
            }
            ui.global::<Names>().set_typing("".into());
            set_pieces(&ui, &settings, p);
        }
    });
    n.on_remove({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move |i| {
            let ui = weak.unwrap();
            let Some(mut p) = pieces_of(&ui) else { return };
            if (i as usize) < p.len() {
                p.remove(i as usize);
                set_pieces(&ui, &settings, p);
            }
        }
    });
    n.on_move({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move |from, to| {
            let ui = weak.unwrap();
            let Some(mut p) = pieces_of(&ui) else { return };
            let (from, to) = (from.max(0) as usize, to.max(0) as usize);
            if from < p.len() && to < p.len() && from != to {
                let piece = p.remove(from);
                p.insert(to, piece);
                set_pieces(&ui, &settings, p);
            }
        }
    });
    n.on_use_preset({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move |i| {
            let ui = weak.unwrap();
            let ready = names::presets();
            let i = i.max(0) as usize;
            let template = match ready.get(i) {
                Some((_, p)) => names::to_template(p),
                None => match settings.lock().unwrap().saved_names.get(i - ready.len()) {
                    Some((_, t)) => t.clone(),
                    None => return,
                },
            };
            ui.set_filename_template(template.into());
            ui.invoke_settings_changed();
            refresh(&ui, &settings);
        }
    });
    n.on_save_preset({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move || {
            let ui = weak.unwrap();
            let Some(p) = pieces_of(&ui) else { return };
            let template = names::to_template(&p);
            {
                let mut s = settings.lock().unwrap();
                if !s.saved_names.iter().any(|(_, t)| *t == template) {
                    s.saved_names.push((names::describe(&p), template));
                    let _ = s.save();
                }
            }
            refresh(&ui, &settings);
        }
    });
    n.on_set_raw({
        let (weak, settings) = (ui.as_weak(), settings.clone());
        move |raw| {
            let ui = weak.unwrap();
            ui.set_filename_template(raw.trim().into());
            ui.invoke_settings_changed();
            // Keep the raw box as typed; refresh the rest.
            refresh(&ui, &settings);
        }
    });
}
