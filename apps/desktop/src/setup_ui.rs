//! First-run screen: one row per tool with its own progress, plain-language errors with
//! Retry, and a clear "Move Neria somewhere else" when the folder isn't writable.

use std::rc::Rc;

use neria_core::{fmt, paths, report, tools};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{AppWindow, ToolRow};

const WAITING: i32 = 0;
const WORKING: i32 = 1;
const READY: i32 = 2;
const FAILED: i32 = 3;

/// Reset the screen for a (re)start. Returns false if Neria can't write next to itself,
/// in which case the screen says so and nothing gets downloaded.
pub fn begin(ui: &AppWindow) -> bool {
    ui.set_setup_error("".into());
    ui.set_setup_detail("".into());
    ui.set_setup_copied(false);
    if let Err((dir, e)) = paths::check_writable() {
        ui.set_setup_blocked_dir(dir.to_string_lossy().as_ref().into());
        ui.set_setup_detail(format!("Neria {} can't write to {}: {e}", report::VERSION, dir.display()).into());
        return false;
    }
    ui.set_setup_blocked_dir("".into());
    let rows: Vec<ToolRow> = tools::TOOLS
        .iter()
        .map(|t| {
            let ready = t.installed();
            ToolRow {
                name: t.name().into(),
                purpose: t.purpose.into(),
                state: if ready { READY } else { WAITING },
                progress: if ready { 1.0 } else { 0.0 },
                detail: "".into(),
            }
        })
        .collect();
    let missing = rows.iter().filter(|r| r.state != READY).count();
    ui.set_setup_missing(missing as i32);
    ui.set_setup_label("Starting up…".into());
    ui.set_setup_tools(ModelRc::from(Rc::new(VecModel::from(rows))));
    true
}

fn update_row(ui: &AppWindow, idx: usize, f: impl FnOnce(&mut ToolRow)) {
    let model = ui.get_setup_tools();
    if let Some(mut row) = model.row_data(idx) {
        f(&mut row);
        model.set_row_data(idx, row);
    }
}

/// Show one progress report from `tools::ensure_all`.
pub fn progress(ui: &AppWindow, p: &tools::Progress) {
    let model = ui.get_setup_tools();
    // Tools download one after another: a row that was busy and isn't this one is done.
    for i in 0..model.row_count() {
        if i != p.tool && model.row_data(i).is_some_and(|r| r.state == WORKING) {
            update_row(ui, i, |r| {
                r.state = READY;
                r.progress = 1.0;
                r.detail = "".into();
            });
        }
    }
    let extracting = p.label.ends_with("(extracting)");
    let detail = if extracting {
        "Unpacking…".to_string()
    } else {
        match p.total {
            Some(t) if t > 0 => format!("{} / {}", fmt::bytes(p.downloaded as f64), fmt::bytes(t as f64)),
            _ if p.downloaded > 0 => fmt::bytes(p.downloaded as f64),
            _ => "Starting…".to_string(),
        }
    };
    let frac = match p.total {
        Some(t) if t > 0 => (p.downloaded as f32 / t as f32).min(1.0),
        _ => 0.0,
    };
    update_row(ui, p.tool, |r| {
        r.state = WORKING;
        r.progress = if extracting { 1.0 } else { frac };
        r.detail = detail.into();
    });
}

/// Everything downloaded: tick off the last row.
pub fn finished(ui: &AppWindow) {
    let model = ui.get_setup_tools();
    for i in 0..model.row_count() {
        update_row(ui, i, |r| {
            r.state = READY;
            r.progress = 1.0;
            r.detail = "".into();
        });
    }
    ui.set_setup_missing(0);
}

/// A download failed: mark its row and explain in plain words.
pub fn failed(ui: &AppWindow, err: &anyhow::Error) {
    let model = ui.get_setup_tools();
    let mut tool = "the tools".to_string();
    for i in 0..model.row_count() {
        if let Some(r) = model.row_data(i).filter(|r| r.state == WORKING) {
            tool = r.name.to_string();
            update_row(ui, i, |r| {
                r.state = FAILED;
                r.detail = "".into();
            });
        }
    }
    ui.set_setup_error(tools::explain(err, &tool).into());
    ui.set_setup_detail(format!("Neria {} setup failed while getting {tool}: {err:#}", report::VERSION).into());
}

pub fn install(ui: &AppWindow) {
    ui.on_setup_copy_details({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let text = ui.get_setup_detail().to_string();
            if arboard::Clipboard::new().and_then(|mut c| c.set_text(text)).is_ok() {
                ui.set_setup_copied(true);
            }
        }
    });
    ui.on_setup_show_folder(|| {
        let exe = std::env::current_exe().unwrap_or_else(|_| paths::app_dir());
        if std::env::var_os("NERIA_HOME").is_some() {
            let _ = open::that(paths::app_dir());
        } else {
            crate::reveal(&exe);
        }
    });
}
