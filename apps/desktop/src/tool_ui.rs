//! External tools (mock `design/tools.html`; `ui/dialogs/tool.slint`, Settings → Tools):
//! the list, the editor (a pasted command line split once, the blanks, the command shown for
//! a sample file, Try it on a file…), and Run a tool… on files already saved. A run is a
//! one-off workflow (`workflow::Change::Tool`), so it goes through the queue like the others
//! (`workflow_ui::run_on`). The model and running a tool: `neria_core::tool`.

use std::cell::RefCell;
use std::path::PathBuf;

use neria_core::tool::{self, Takes, Tool};
use neria_core::workflow::{self, Change, Workflow};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, ExtTool, Tools, show_toast, toast_info};

#[derive(Default)]
struct State {
    /// The tool being edited (id 0: new).
    draft: Tool,
    /// Run a tool…: the files.
    run_files: Vec<PathBuf>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

pub fn rows() -> Vec<ExtTool> {
    tool::list()
        .iter()
        .map(|t| ExtTool {
            id: t.id as i32,
            name: t.name.as_str().into(),
            program: t.program_name().into(),
            summary: t.summary().into(),
            tried: t.tried,
        })
        .collect()
}

/// The list again (Settings, Run a tool…, the workflow editor's rows).
pub fn refresh(ui: &AppWindow) {
    ui.global::<Tools>().set_items(ModelRc::new(VecModel::from(rows())));
    crate::workflow_ui::refresh(ui);
}

/// The workflows that run this tool, by name ("Every TikTok, Shrink recordings").
fn used_by(id: u64) -> String {
    let names: Vec<String> =
        workflow::list().into_iter().filter(|w| w.change == Change::Tool(id)).map(|w| w.name).collect();
    names.join(", ")
}

fn open_editor(ui: &AppWindow, t: Tool) {
    let g = ui.global::<Tools>();
    g.set_edit_new(t.id == 0);
    g.set_name(t.name.as_str().into());
    g.set_program(t.program.as_str().into());
    g.set_words(t.words.as_str().into());
    g.set_makes_file(t.id == 0 || t.makes_file());
    g.set_makes(if t.makes_file() { t.out_ext().into() } else { "mp4".into() });
    g.set_takes(t.takes.index());
    g.set_used_by(if t.id == 0 { "".into() } else { used_by(t.id).into() });
    with(|s| s.draft = t);
    changed(ui);
    g.set_edit_open(true);
}

fn from_editor(ui: &AppWindow) -> Tool {
    let g = ui.global::<Tools>();
    let mut t = with(|s| s.draft.clone());
    t.name = g.get_name().trim().to_string();
    t.program = g.get_program().trim().trim_matches('"').to_string();
    t.words = g.get_words().trim().to_string();
    t.makes = if g.get_makes_file() { g.get_makes().trim().trim_start_matches('.').to_string() } else { String::new() };
    t.takes = Takes::from_index(g.get_takes());
    t
}

fn changed(ui: &AppWindow) {
    let g = ui.global::<Tools>();
    // A whole command line pasted into Program: split once, where it shows.
    if g.get_words().trim().is_empty()
        && let Some((program, rest)) = tool::split_pasted(&g.get_program())
    {
        g.set_program(program.into());
        g.set_words(rest.into());
    }
    let t = from_editor(ui);
    // "makes" left empty while A new file is picked: that's the problem to say.
    let problem = t.problem().or_else(|| {
        (g.get_makes_file() && t.makes.is_empty()).then(|| "Type the new file's ending, like mp4.".to_string())
    });
    g.set_command(if t.program.is_empty() { "".into() } else { tool::sample(&t).into() });
    g.set_can_save(problem.is_none());
    g.set_problem(problem.unwrap_or_default().into());
}

/// Save the editor's tool; returns it (None: something's missing).
fn save(ui: &AppWindow) -> Option<Tool> {
    let mut t = from_editor(ui);
    if t.problem().is_some() || (ui.global::<Tools>().get_makes_file() && t.makes.is_empty()) {
        return None;
    }
    t.id = tool::save(t.clone());
    let t = tool::get(t.id)?;
    with(|s| s.draft = t.clone());
    ui.global::<Tools>().set_edit_open(false);
    refresh(ui);
    Some(t)
}

/// The one-off workflow a tool runs through (next to the original, when picked).
fn as_workflow(t: &Tool) -> Workflow {
    Workflow { name: t.name.clone(), change: Change::Tool(t.id), ..Workflow::default() }
}

/// Run a tool on files (the files it doesn't take are left out, and said so).
fn run_on(ui: &AppWindow, t: &Tool, files: &[PathBuf]) {
    let (fit, skip): (Vec<PathBuf>, Vec<PathBuf>) = files.iter().cloned().partition(|f| t.takes.fits(f));
    if fit.is_empty() {
        let what = match t.takes {
            Takes::Sound => "sound files",
            _ => "videos",
        };
        crate::toast_info(ui, &format!("{} takes {what} only.", t.name));
        return;
    }
    crate::workflow_ui::run_on(ui, &as_workflow(t), &fit);
    if !skip.is_empty() {
        show_toast(
            ui,
            &format!("{} is running on {}; {} it doesn't take were left out", t.name, fit.len(), skip.len()),
        );
    }
}

/// Run a tool… on these files (the Library's rows): which one.
pub fn open_run(ui: &AppWindow, files: Vec<PathBuf>) {
    let files: Vec<PathBuf> = files.into_iter().filter(|f| f.is_file()).collect();
    if files.is_empty() {
        toast_info(ui, "Those files aren't there any more.");
        return;
    }
    let sub = match files.as_slice() {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        many => format!("{} files", many.len()),
    };
    with(|s| s.run_files = files);
    refresh(ui);
    let g = ui.global::<Tools>();
    g.set_run_subtitle(sub.into());
    g.set_run_open(true);
}

/// The palette: a tool on one file.
pub fn run_one(ui: &AppWindow, id: u64, file: PathBuf) {
    if let Some(t) = tool::get(id) {
        run_on(ui, &t, &[file]);
    }
}

pub fn install(ui: &AppWindow) {
    let g = ui.global::<Tools>();
    let w = ui.as_weak();
    g.on_new_one(move || {
        let t = Tool { takes: Takes::Videos, makes: "mp4".into(), ..Tool::default() };
        open_editor(&w.unwrap(), t);
    });
    let w = ui.as_weak();
    g.on_edit(move |id| {
        if let Some(t) = tool::get(id as u64) {
            open_editor(&w.unwrap(), t);
        }
    });
    let w = ui.as_weak();
    g.on_changed(move || changed(&w.unwrap()));
    let w = ui.as_weak();
    g.on_pick_program(move || {
        let ui = w.unwrap();
        let picked = rfd::FileDialog::new()
            .set_title("Which program?")
            .add_filter("Programs", &["exe", "bat", "cmd"])
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(p) = picked {
            let g = ui.global::<Tools>();
            g.set_program(p.display().to_string().into());
            if g.get_name().trim().is_empty() {
                let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                g.set_name(stem.into());
            }
            changed(&ui);
        }
    });
    let w = ui.as_weak();
    g.on_add_blank(move |blank| {
        let ui = w.unwrap();
        let g = ui.global::<Tools>();
        let words = g.get_words().trim_end().to_string();
        g.set_words(if words.is_empty() { blank } else { format!("{words} {blank}").into() });
        changed(&ui);
    });
    let w = ui.as_weak();
    g.on_try_it(move || {
        let ui = w.unwrap();
        let Some(t) = save(&ui) else {
            toast_info(&ui, "Fill in what's missing first.");
            return;
        };
        let picked = rfd::FileDialog::new().set_title(format!("Try {} on which file?", t.name)).pick_file();
        if let Some(file) = picked {
            run_on(&ui, &t, &[file]);
        }
    });
    let w = ui.as_weak();
    g.on_save(move || {
        let ui = w.unwrap();
        if let Some(t) = save(&ui) {
            show_toast(&ui, &format!("Saved {}", t.name));
        }
    });
    let w = ui.as_weak();
    g.on_delete(move || {
        let ui = w.unwrap();
        let (id, name) = with(|s| (s.draft.id, s.draft.name.clone()));
        let users = used_by(id);
        tool::delete(id);
        ui.global::<Tools>().set_edit_open(false);
        refresh(&ui);
        if users.is_empty() {
            show_toast(&ui, &format!("Deleted {name}"));
        } else {
            show_toast(&ui, &format!("Deleted {name}. {users} used it: they do nothing until you edit them."));
        }
    });
    let w = ui.as_weak();
    g.on_run(move |id| {
        let ui = w.unwrap();
        ui.global::<Tools>().set_run_open(false);
        let files = with(|s| std::mem::take(&mut s.run_files));
        if let Some(t) = tool::get(id as u64) {
            run_on(&ui, &t, &files);
        }
    });
    refresh(ui);
    demo(ui);
}

/// `NERIA_TOOL` for design checks: `new`, `edit` (the first), `run` (Run a tool… on a
/// made-up file), `sample` (a sample tool saved first, then its editor).
fn demo(ui: &AppWindow) {
    let Ok(what) = std::env::var("NERIA_TOOL") else { return };
    let sample = || Tool {
        name: "Upscale 2×".into(),
        program: neria_core::paths::tool("ffmpeg").display().to_string(),
        words: "-i {file} -vf scale=iw*2:ih*2:flags=lanczos {out}".into(),
        makes: "mp4".into(),
        ..Tool::default()
    };
    match what.as_str() {
        "new" => ui.global::<Tools>().invoke_new_one(),
        "sample" => open_editor(ui, sample()),
        "edit" => {
            if tool::list().is_empty() {
                tool::save(sample());
                refresh(ui);
            }
            if let Some(t) = tool::list().into_iter().next() {
                open_editor(ui, t);
            }
        }
        "run" => {
            if tool::list().is_empty() {
                tool::save(sample());
                refresh(ui);
            }
            with(|s| s.run_files = vec![PathBuf::from("demo.mp4")]);
            let g = ui.global::<Tools>();
            g.set_run_subtitle("The 10-minute crispy rice everyone asks for.mp4".into());
            g.set_run_open(true);
        }
        _ => {}
    }
}
