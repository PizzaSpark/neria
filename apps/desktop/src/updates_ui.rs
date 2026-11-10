//! "Check for updates": a banner on the Download page when a new Neria is out,
//! and Settings → About for checking by hand and refreshing the tools.

use neria_core::{fmt, update};
use slint::ComponentHandle;

use crate::AppWindow;

// update-state values shared with app.slint
const IDLE: i32 = 0;
const CHECKING: i32 = 1;
const DOWNLOADING: i32 = 2;
const READY: i32 = 3;
const FAILED: i32 = 4;

pub fn install(ui: &AppWindow) {
    ui.on_update_check({
        let weak = ui.as_weak();
        move || check(&weak.unwrap(), true)
    });

    ui.on_update_install({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let Some(rel) = pending(&ui) else { return };
            ui.set_update_state(DOWNLOADING);
            ui.set_update_progress(0.0);
            ui.set_update_status(format!("Downloading Neria {}…", rel.version).into());
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let w = weak.clone();
                let result = update::install_neria(&rel, |done, total| {
                    let (frac, text) = match total {
                        Some(t) if t > 0 => (done as f32 / t as f32, format!("{} / {}", fmt::bytes(done as f64), fmt::bytes(t as f64))),
                        _ => (0.0, fmt::bytes(done as f64)),
                    };
                    let _ = w.upgrade_in_event_loop(move |ui| {
                        ui.set_update_progress(frac);
                        ui.set_update_status(format!("Downloading the update…  {text}").into());
                    });
                });
                let _ = weak.upgrade_in_event_loop(move |ui| match result {
                    Ok(()) => {
                        ui.set_update_state(READY);
                        ui.set_update_status(format!("Neria {} is ready. Restart to start using it.", rel.version).into());
                        if std::env::var("NERIA_UPDATE_INSTALL").as_deref() == Ok("restart") {
                            ui.invoke_update_restart();
                        }
                    }
                    Err(e) => {
                        ui.set_update_state(FAILED);
                        ui.set_update_status(format!("The update didn't work: {e:#}. Neria is unchanged.").into());
                    }
                });
            });
        }
    });

    ui.on_update_restart({
        let weak = ui.as_weak();
        move || match update::restart() {
            Ok(()) => {
                let _ = slint::quit_event_loop();
            }
            Err(e) => {
                let ui = weak.unwrap();
                ui.set_update_state(FAILED);
                ui.set_update_status(format!("{e:#}. Close Neria and open it again to finish.").into());
            }
        }
    });

    ui.on_update_later({
        let weak = ui.as_weak();
        move || weak.unwrap().set_update_banner(false)
    });

    ui.on_tools_update({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let labels: Vec<String> = ui.get_tools_outdated().split('\n').filter(|s| !s.is_empty()).map(str::to_string).collect();
            if labels.is_empty() {
                return;
            }
            ui.set_tools_updating(true);
            ui.set_tools_status("Starting…".into());
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let names: Vec<&str> = labels.iter().map(String::as_str).collect();
                let w = weak.clone();
                let result = update::update_tools(&names, |p| {
                    let text = p.text();
                    let _ = w.upgrade_in_event_loop(move |ui| ui.set_tools_status(text.into()));
                });
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_tools_updating(false);
                    match result {
                        Ok(()) => {
                            ui.set_tools_outdated("".into());
                            ui.set_tools_status("Tools updated. New versions show after a restart.".into());
                        }
                        Err(e) => ui.set_tools_status(format!("Tool update failed: {e:#}. The old versions are still in place.").into()),
                    }
                });
            });
        }
    });
}

/// The release found by the last check, if any.
fn pending(ui: &AppWindow) -> Option<update::Release> {
    let version = ui.get_update_version().to_string();
    let url = ui.get_update_url().to_string();
    (!version.is_empty() && !url.is_empty()).then(|| update::Release {
        version,
        notes: String::new(),
        zip_url: url,
        size: None,
    })
}

/// Look for a new Neria and outdated tools in the background. `by_hand`:
/// the user pressed "Check now", so also report "up to date" and errors.
pub fn check(ui: &AppWindow, by_hand: bool) {
    if matches!(ui.get_update_state(), CHECKING | DOWNLOADING | READY) {
        return;
    }
    ui.set_update_state(CHECKING);
    ui.set_update_status("Checking for updates…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let neria = update::check_neria();
        let tools = update::check_tools();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_update_state(IDLE);
            match neria {
                Ok(Some(rel)) => {
                    ui.set_update_version(rel.version.as_str().into());
                    ui.set_update_url(rel.zip_url.as_str().into());
                    ui.set_update_notes(update::plain_notes(&rel.notes).into());
                    let size = rel.size.map(|s| format!(" ({})", fmt::bytes(s as f64))).unwrap_or_default();
                    ui.set_update_status(format!("Neria {} is out{size}.", rel.version).into());
                    ui.set_update_banner(true);
                    // Dev hook: install right away (`restart` also restarts).
                    if std::env::var_os("NERIA_UPDATE_INSTALL").is_some() {
                        ui.invoke_update_install();
                    }
                }
                Ok(None) => ui.set_update_status("Neria is up to date.".into()),
                Err(e) => {
                    let msg = format!("Couldn't check for a new Neria: {e:#}.");
                    ui.set_update_status(if by_hand { msg.into() } else { "".into() });
                    if by_hand {
                        ui.set_update_state(FAILED);
                    }
                }
            }
            match tools {
                Ok(list) => {
                    let labels: Vec<&str> = list.iter().map(|t| t.label).collect();
                    ui.set_tools_outdated(labels.join("\n").into());
                    ui.set_tools_status(match list.len() {
                        0 => "".into(),
                        _ => format!(
                            "Newer builds of {} are available.",
                            list.iter().map(|t| short_label(t.label)).collect::<Vec<_>>().join(", ")
                        )
                        .into(),
                    });
                }
                Err(e) if by_hand => ui.set_tools_status(format!("Couldn't check the tools: {e:#}.").into()),
                Err(_) => {}
            }
        });
    });
}

/// "Deno (needed by yt-dlp for YouTube)" → "Deno".
fn short_label(label: &str) -> &str {
    label.split(" (").next().unwrap_or(label)
}
