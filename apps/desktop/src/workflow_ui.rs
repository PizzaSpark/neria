//! Saved workflows (mock `design/workflows.html`; `ui/dialogs/workflow.slint`, the Then
//! button in `ui/components/extras-row.slint`, Settings → Workflows): the list, the Then
//! pick for the previewed link, the editor, Run a workflow… on files already saved, and the
//! watched folders (a thread looking every few seconds). The model, which one runs and running
//! it: `neria_core::workflow`; the queue runs them (`DownloadRequest::workflow` / `local`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use neria_core::download::DownloadRequest;
use neria_core::queue::Queue;
use neria_core::settings::Quality;
use neria_core::workflow::{self, Change, Pick, Put, Size, SoundAs, When, Workflow};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, WfRow, WfSite, Workflows, show_toast};

#[derive(Default)]
struct State {
    /// Then for the previewed link: None = as it was set by itself (Auto), Some(0) = Nothing.
    picked: Option<u64>,
    /// The automatic one for the previewed link.
    auto: Option<Workflow>,
    /// The editor: the workflow being edited (id 0: new), its folders and sites.
    draft: Workflow,
    sites: Vec<String>,
    put_folder: Option<PathBuf>,
    when_folder: Option<PathBuf>,
    /// The editor was opened from the Then menu: what's saved is picked for the link.
    from_then: bool,
    /// Run a workflow…: the files.
    run_files: Vec<PathBuf>,
    queue: Option<Queue>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

fn kind_of(w: &Workflow) -> i32 {
    match (&w.when, w.change) {
        (When::Folder(_), _) => 4,
        (_, Change::Smaller(_)) => 0,
        (_, Change::Sound(_)) => 1,
        (_, Change::Gif) => 2,
        (_, Change::Keep) => 3,
    }
}

/// The list again (Settings, the Then menu, Run a workflow…).
pub fn refresh(ui: &AppWindow) {
    let rows: Vec<WfRow> = workflow::list()
        .iter()
        .map(|w| {
            let watching = match &w.when {
                When::Folder(dir) => {
                    let n = workflow::done_today(w);
                    let mut s = format!("watching {}", dir.display());
                    if n > 0 {
                        s.push_str(&format!(" · {n} done today"));
                    }
                    s
                }
                _ => String::new(),
            };
            WfRow {
                id: w.id as i32,
                name: w.name.as_str().into(),
                what: w.what().into(),
                when: w.when_label().into(),
                auto: w.when != When::Picked,
                on: w.on,
                watching: watching.into(),
                kind: kind_of(w),
            }
        })
        .collect();
    ui.global::<Workflows>().set_items(ModelRc::new(VecModel::from(rows)));
    show_then(ui);
}

/// The Then button for the previewed link.
fn show_then(ui: &AppWindow) {
    let (picked, auto) = with(|s| (s.picked, s.auto.clone()));
    let wf = match picked {
        Some(0) => None,
        Some(id) => workflow::get(id),
        None => auto.clone(),
    };
    let g = ui.global::<Workflows>();
    g.set_picked(wf.as_ref().map_or(0, |w| w.id as i32));
    g.set_then_text(match &wf {
        Some(w) => format!("Then: {}", w.name).into(),
        None => "Then".into(),
    });
    let why = match (&wf, &auto) {
        (Some(w), Some(a)) if w.id == a.id => workflow::why(w),
        _ => String::new(),
    };
    g.set_picked_why(why.into());
}

/// A new link previewed: Then starts from the one that runs by itself for it, if any.
pub fn on_preview(ui: &AppWindow, url: &str) {
    let auto = workflow::auto_for(url);
    with(|s| {
        s.picked = None;
        s.auto = auto;
    });
    show_then(ui);
}

/// What the Download button queues with: the Then pick (Nothing too), else as by itself.
pub fn download_pick() -> Pick {
    with(|s| match s.picked {
        Some(0) => Pick::Nothing,
        Some(id) => workflow::get(id).map_or(Pick::Auto, |w| Pick::Use(Box::new(w))),
        None => s.auto.clone().map_or(Pick::Nothing, |w| Pick::Use(Box::new(w))),
    })
}

// ---------------------------------------------------------------------------------------
// The editor

fn open_editor(ui: &AppWindow, wf: Workflow, from_then: bool) {
    let g = ui.global::<Workflows>();
    g.set_edit_new(wf.id == 0);
    g.set_name(wf.name.as_str().into());
    let (change, size, sound) = match wf.change {
        Change::Keep => (0, 0, 0),
        Change::Smaller(s) => (
            1,
            match s {
                Size::Discord => 0,
                Size::Email => 1,
                Size::WhatsApp => 2,
                Size::Mb(mb) => {
                    g.set_size_mb(neria_core::compress::fmt_mb(mb).into());
                    3
                }
            },
            0,
        ),
        Change::Sound(s) => (2, 0, s.index() as i32),
        Change::Gif => (3, 0, 0),
    };
    g.set_change(change);
    g.set_size(size);
    g.set_sound_as(sound);
    let put_folder = match &wf.put {
        Put::Folder(d) => Some(d.clone()),
        _ => None,
    };
    g.set_put(match wf.put {
        Put::Beside => 0,
        Put::Folder(_) => 1,
        Put::Replace => 2,
    });
    let (when, sites, when_folder) = match &wf.when {
        When::Picked => (0, Vec::new(), None),
        When::Every => (1, Vec::new(), None),
        When::Sites(s) => (2, s.clone(), None),
        When::Followed => (3, Vec::new(), None),
        When::Folder(d) => (4, Vec::new(), Some(d.clone())),
    };
    g.set_when(when);
    g.set_do_waiting(false);
    with(|s| {
        s.draft = wf;
        s.sites = sites;
        s.put_folder = put_folder;
        s.when_folder = when_folder;
        s.from_then = from_then;
    });
    changed(ui);
    g.set_edit_open(true);
}

/// The workflow as the editor shows it now, or why it can't be saved.
fn from_editor(ui: &AppWindow) -> (Workflow, Option<String>) {
    let g = ui.global::<Workflows>();
    let (mut wf, sites, put_folder, when_folder) =
        with(|s| (s.draft.clone(), s.sites.clone(), s.put_folder.clone(), s.when_folder.clone()));
    let mut problem = None;
    wf.name = g.get_name().trim().to_string();
    let mb: f64 = g.get_size_mb().trim().replace(',', ".").parse().unwrap_or(0.0);
    wf.change = match g.get_change() {
        0 => Change::Keep,
        2 => Change::Sound(SoundAs::from_index(g.get_sound_as() as usize)),
        3 => Change::Gif,
        _ => Change::Smaller(match g.get_size() {
            1 => Size::Email,
            2 => Size::WhatsApp,
            3 => {
                if mb < 1.0 {
                    problem = Some("Type a size of 1 MB or more.".to_string());
                }
                Size::Mb(mb.max(1.0))
            }
            _ => Size::Discord,
        }),
    };
    let put = if wf.change == Change::Keep { 1 } else { g.get_put() };
    wf.put = match put {
        1 => match &put_folder {
            Some(d) => Put::Folder(d.clone()),
            None => {
                problem.get_or_insert_with(|| "Pick the folder it goes in.".into());
                Put::Beside
            }
        },
        2 => Put::Replace,
        _ => Put::Beside,
    };
    wf.when = match g.get_when() {
        1 => When::Every,
        2 => {
            if sites.is_empty() {
                problem.get_or_insert_with(|| "Tick the sites it's for.".into());
            }
            When::Sites(sites)
        }
        3 => When::Followed,
        4 => match &when_folder {
            Some(d) => When::Folder(d.clone()),
            None => {
                problem.get_or_insert_with(|| "Pick the folder to watch.".into());
                When::Folder(PathBuf::new())
            }
        },
        _ => When::Picked,
    };
    if let (When::Folder(w), Put::Folder(p)) = (&wf.when, &wf.put)
        && p.starts_with(w)
    {
        problem.get_or_insert_with(|| "Its results can't go back into the folder it watches.".into());
    }
    if wf.name.is_empty() {
        problem.get_or_insert_with(|| "Give it a name.".into());
    } else if workflow::list().iter().any(|w| w.id != wf.id && w.name.eq_ignore_ascii_case(&wf.name)) {
        problem.get_or_insert_with(|| format!("There's already one called {}.", wf.name));
    }
    (wf, problem)
}

fn changed(ui: &AppWindow) {
    let g = ui.global::<Workflows>();
    let (wf, problem) = from_editor(ui);
    let picked = with(|s| s.sites.clone());
    let sites: Vec<WfSite> = workflow::SITES
        .iter()
        .map(|n| WfSite { name: (*n).into(), on: picked.iter().any(|p| p == n) })
        .collect();
    g.set_sites(ModelRc::new(VecModel::from(sites)));
    g.set_put_folder(with(|s| s.put_folder.clone()).map(|p| p.display().to_string()).unwrap_or_default().into());
    let when_folder = with(|s| s.when_folder.clone());
    g.set_when_folder(when_folder.as_ref().map(|p| p.display().to_string()).unwrap_or_default().into());
    // Files already in the watched folder: left alone unless ticked (only for a new folder).
    let waiting = match (&when_folder, g.get_when()) {
        (Some(d), 4) if with(|s| s.draft.when != When::Folder(d.clone())) => workflow::waiting_in(d).len(),
        _ => 0,
    };
    g.set_waiting(waiting as i32);
    g.set_sentence(wf.sentence().into());
    g.set_problem(problem.clone().unwrap_or_default().into());
    g.set_can_save(problem.is_none());
}

fn save(ui: &AppWindow) {
    let (wf, problem) = from_editor(ui);
    if problem.is_some() {
        return;
    }
    let g = ui.global::<Workflows>();
    let do_waiting = g.get_do_waiting() && g.get_waiting() > 0;
    let name = wf.name.clone();
    let id = workflow::save(wf);
    if do_waiting {
        workflow::forget_seen(id);
    }
    if with(|s| s.from_then) {
        with(|s| s.picked = Some(id));
    }
    g.set_edit_open(false);
    refresh(ui);
    show_toast(ui, &format!("Saved {name}"));
}

fn pick_folder(title: &str, start: Option<&Path>) -> Option<PathBuf> {
    let mut d = rfd::FileDialog::new().set_title(title);
    if let Some(s) = start.filter(|s| s.is_dir()) {
        d = d.set_directory(s);
    }
    d.pick_folder()
}

// ---------------------------------------------------------------------------------------
// Run a workflow… on files already saved

/// Run a workflow… on these files (the Library's rows): which one.
pub fn open_run(ui: &AppWindow, files: Vec<PathBuf>) {
    let files: Vec<PathBuf> = files.into_iter().filter(|f| f.is_file()).collect();
    if files.is_empty() {
        show_toast(ui, "Those files aren't there any more.");
        return;
    }
    let sub = match files.as_slice() {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        many => format!("{} files", many.len()),
    };
    with(|s| s.run_files = files);
    refresh(ui);
    let g = ui.global::<Workflows>();
    g.set_run_subtitle(sub.into());
    g.set_run_open(true);
}

/// Queue `wf` on these files (each a row of its own on the Download page).
pub fn run_on(ui: &AppWindow, wf: &Workflow, files: &[PathBuf]) {
    let Some(queue) = with(|s| s.queue.clone()) else { return };
    let reqs: Vec<DownloadRequest> = files.iter().map(|f| local_request(wf, f)).collect();
    let n = reqs.len();
    if n == 1 {
        queue.add(reqs.into_iter().next().unwrap());
    } else {
        queue.add_group(&format!("{} · {n} files", wf.name), false, reqs);
    }
    let what = if n == 1 { "it".to_string() } else { format!("{n} files") };
    show_toast(ui, &format!("{} is running on {what}: it shows in the Download page's queue", wf.name));
}

fn local_request(wf: &Workflow, file: &Path) -> DownloadRequest {
    let title = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut r = DownloadRequest::new(file.display().to_string(), Quality::Best, title);
    r.local = Some(file.to_path_buf());
    r.workflow = Pick::Use(Box::new(wf.clone()));
    r
}

// ---------------------------------------------------------------------------------------

pub fn install(ui: &AppWindow, queue: Queue) {
    with(|s| s.queue = Some(queue.clone()));
    let g = ui.global::<Workflows>();
    let w = ui.as_weak();
    g.on_pick(move |id| {
        let ui = w.unwrap();
        with(|s| s.picked = Some(id.max(0) as u64));
        show_then(&ui);
    });
    let w = ui.as_weak();
    g.on_open_settings(move || {
        let ui = w.unwrap();
        ui.set_settings_section(7);
        ui.set_drawer_open(true);
    });
    let w = ui.as_weak();
    g.on_new_one(move || {
        let ui = w.unwrap();
        // From the Then menu when the Download page has a preview and Settings is closed.
        let from_then = !ui.get_drawer_open() && !ui.global::<Workflows>().get_run_open();
        open_editor(&ui, Workflow { name: String::new(), ..Workflow::default() }, from_then);
    });
    let w = ui.as_weak();
    g.on_edit(move |id| {
        let ui = w.unwrap();
        if let Some(wf) = workflow::get(id as u64) {
            open_editor(&ui, wf, false);
        }
    });
    let w = ui.as_weak();
    g.on_changed(move || changed(&w.unwrap()));
    let w = ui.as_weak();
    g.on_toggle_site(move |i| {
        let Some(name) = workflow::SITES.get(i as usize) else { return };
        with(|s| match s.sites.iter().position(|x| x == name) {
            Some(at) => {
                s.sites.remove(at);
            }
            None => s.sites.push((*name).to_string()),
        });
        changed(&w.unwrap());
    });
    let w = ui.as_weak();
    g.on_pick_put_folder(move || {
        let ui = w.unwrap();
        let start = with(|s| s.put_folder.clone());
        if let Some(dir) = pick_folder("Where should it go?", start.as_deref()) {
            with(|s| s.put_folder = Some(dir));
            let g = ui.global::<Workflows>();
            if g.get_change() != 0 {
                g.set_put(1);
            }
        }
        changed(&ui);
    });
    let w = ui.as_weak();
    g.on_pick_when_folder(move || {
        let ui = w.unwrap();
        let start = with(|s| s.when_folder.clone());
        if let Some(dir) = pick_folder("Which folder should Neria watch?", start.as_deref()) {
            with(|s| s.when_folder = Some(dir));
        }
        changed(&ui);
    });
    let w = ui.as_weak();
    g.on_save(move || save(&w.unwrap()));
    let w = ui.as_weak();
    g.on_delete(move || {
        let ui = w.unwrap();
        let (id, name) = with(|s| (s.draft.id, s.draft.name.clone()));
        workflow::delete(id);
        with(|s| {
            if s.picked == Some(id) {
                s.picked = Some(0);
            }
            if s.auto.as_ref().is_some_and(|a| a.id == id) {
                s.auto = None;
            }
        });
        ui.global::<Workflows>().set_edit_open(false);
        refresh(&ui);
        show_toast(&ui, &format!("Deleted {name}"));
    });
    let w = ui.as_weak();
    g.on_set_on(move |id, on| {
        workflow::set_on(id as u64, on);
        refresh(&w.unwrap());
    });
    let w = ui.as_weak();
    g.on_run(move |id| {
        let ui = w.unwrap();
        ui.global::<Workflows>().set_run_open(false);
        let files = with(|s| std::mem::take(&mut s.run_files));
        if let Some(wf) = workflow::get(id as u64) {
            run_on(&ui, &wf, &files);
        }
    });
    refresh(ui);

    // Watched folders: a look every 3 s; a file that stopped growing joins the queue.
    let w = ui.as_weak();
    std::thread::spawn(move || {
        let mut watcher = workflow::Watcher::default();
        loop {
            std::thread::sleep(Duration::from_secs(3));
            let ready = watcher.look();
            if ready.is_empty() {
                continue;
            }
            for (wf, file) in &ready {
                queue.add(local_request(wf, file));
            }
            let _ = w.upgrade_in_event_loop(|ui| refresh(&ui));
        }
    });

    demo(ui);
}

/// `NERIA_WORKFLOW` for design checks: `new`, `edit` (the first), `folder` (a watched
/// folder's editor), `run` (Run a workflow… on `NERIA_WORKFLOW_FILE` or a made-up file).
fn demo(ui: &AppWindow) {
    let Ok(what) = std::env::var("NERIA_WORKFLOW") else { return };
    match what.as_str() {
        "new" => ui.global::<Workflows>().invoke_new_one(),
        "edit" => {
            if let Some(wf) = workflow::list().into_iter().next() {
                open_editor(ui, wf, false);
            }
        }
        "folder" => {
            let dir = std::env::temp_dir();
            let wf = Workflow {
                name: "Shrink recordings".into(),
                change: Change::Smaller(Size::Mb(50.0)),
                ..Workflow::default()
            };
            open_editor(ui, wf, false);
            with(|s| s.when_folder = Some(dir));
            ui.global::<Workflows>().set_when(4);
            changed(ui);
        }
        "run" => {
            let file = std::env::var_os("NERIA_WORKFLOW_FILE").map(PathBuf::from);
            match file {
                Some(f) => open_run(ui, vec![f]),
                None => {
                    with(|s| s.run_files = vec![PathBuf::from("demo.mp4")]);
                    let g = ui.global::<Workflows>();
                    g.set_run_subtitle("The 10-minute crispy rice everyone asks for.mp4".into());
                    g.set_run_open(true);
                }
            }
        }
        _ => {}
    }
}
