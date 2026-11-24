//! Put in folders (`ui/dialogs/organize.slint`, mock `design/audio-tools.html` 5A): the
//! Library's picked songs (or a playlist's) moved into Artist / Album folders by their tags.
//! Every move is listed first, nothing is overwritten, the Library's records follow the files,
//! and Undo puts it all back. Planning and moving: `neria_core::organize`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use neria_core::library::{self, Item};
use neria_core::organize::{self, Arrange, Done, Move};
use neria_core::settings::Settings;
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, OrgMove, Organize, show_toast};

#[derive(Default)]
struct State {
    files: Vec<PathBuf>,
    /// The records (to follow the files).
    ids: Vec<(u64, PathBuf)>,
    root: PathBuf,
    moves: Vec<Move>,
    done: Option<Done>,
    settings: Option<Arc<Mutex<Settings>>>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

/// A path as the list shows it: under the folder, from there; else its folder and name.
fn short(p: &Path, root: &Path) -> String {
    match p.strip_prefix(root) {
        Ok(r) => r.display().to_string(),
        // Elsewhere: its folder's name and its own ("Chill Mix.mp3").
        Err(_) => {
            let n = p.components().count();
            p.components().skip(n.saturating_sub(2)).collect::<PathBuf>().display().to_string()
        }
    }
}

/// Open it on these records (songs here).
pub fn open(ui: &AppWindow, items: Vec<Item>) {
    let files: Vec<PathBuf> = items.iter().map(|i| i.file.clone()).filter(|f| library::is_audio_file(f)).collect();
    if files.is_empty() {
        show_toast(ui, "Only songs and sound files go in folders.");
        return;
    }
    let root = with(|s| s.settings.clone())
        .map(|s| neria_core::folders::for_kind(&s.lock().unwrap(), neria_core::folders::Kind::Audio))
        .unwrap_or_else(|| files[0].parent().map(Path::to_path_buf).unwrap_or_default());
    with(|s| {
        s.files = files.clone();
        s.ids = items.iter().map(|i| (i.id, i.file.clone())).collect();
        s.root = root;
        s.done = None;
        s.moves.clear();
    });
    let o = ui.global::<Organize>();
    o.set_done(false);
    o.set_status("".into());
    o.set_open(true);
    replan(ui);
}

/// Work out the moves again (arrangement or folder changed), off the UI thread (tags).
fn replan(ui: &AppWindow) {
    let o = ui.global::<Organize>();
    let how = Arrange::from_index(o.get_arrange());
    let (files, root) = with(|s| (s.files.clone(), s.root.clone()));
    o.set_root(root.display().to_string().into());
    o.set_busy(true);
    o.set_hint("Reading their tags…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let moves = organize::plan(&files, &root, how);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            show(&ui, &moves, &files, &root);
            with(|s| s.moves = moves);
        });
    });
}

fn show(ui: &AppWindow, moves: &[Move], files: &[PathBuf], root: &Path) {
    let o = ui.global::<Organize>();
    let rows: Vec<OrgMove> = moves
        .iter()
        .map(|m| OrgMove {
            from: short(&m.from, root).into(),
            to: if m.blocked {
                format!("{} (a file is already there: it stays)", short(&m.to, root)).into()
            } else {
                short(&m.to, root).into()
            },
            warn: m.blocked,
        })
        .collect();
    o.set_moves(ModelRc::from(Rc::new(VecModel::from(rows))));
    let go = moves.iter().filter(|m| !m.blocked).count();
    let n = files.len();
    o.set_subtitle(
        format!("{n} song{} · lyrics move with them", if n == 1 { "" } else { "s" }).into(),
    );
    o.set_can_go(go > 0);
    o.set_go_text(if go == 1 { "Move 1".into() } else { format!("Move {go}").into() });
    o.set_busy(false);
    o.set_hint(
        if moves.is_empty() {
            "They're all where this would put them already.".to_string()
        } else {
            "Playlists that list them are changed to the new places. Undo puts everything back.".into()
        }
        .into(),
    );
}

fn go(ui: &AppWindow) {
    let o = ui.global::<Organize>();
    let moves = with(|s| s.moves.clone());
    o.set_busy(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let done = organize::apply(&moves);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let o = ui.global::<Organize>();
            o.set_busy(false);
            o.set_done(true);
            follow(&done.songs);
            let n = done.songs.len();
            o.set_subtitle(format!("{n} song{} moved", if n == 1 { "" } else { "s" }).into());
            o.set_hint(match done.playlists.len() {
                0 => "".into(),
                1 => "1 playlist changed to the new places.".into(),
                p => format!("{p} playlists changed to the new places.").into(),
            });
            o.set_status(match done.failed.first() {
                Some(f) => format!("{} couldn't move (open in a player?): {f}", done.failed.len()).into(),
                None => "".into(),
            });
            with(|s| s.done = Some(done));
            crate::library_ui::refresh_now(&ui);
            if n > 0 {
                show_toast(&ui, "Moved. The Library shows their new places (File, in each one's details).");
            }
        });
    });
}

/// The Library's records go where their files went.
fn follow(songs: &[(PathBuf, PathBuf)]) {
    let ids = with(|s| s.ids.clone());
    let moves: Vec<(u64, PathBuf)> = songs
        .iter()
        .filter_map(|(from, to)| ids.iter().find(|(_, f)| f == from).map(|(id, _)| (*id, to.clone())))
        .collect();
    library::relocate(&moves);
}

fn undo(ui: &AppWindow) {
    let Some(done) = with(|s| s.done.take()) else { return };
    match organize::undo(&done) {
        Ok(()) => {
            // The records back where they were: the ids now point at the new places.
            let ids = with(|s| s.ids.clone());
            let moves: Vec<(u64, PathBuf)> = ids.iter().map(|(id, f)| (*id, f.clone())).collect();
            library::relocate(&moves);
            ui.global::<Organize>().set_open(false);
            show_toast(ui, "Put back where they were");
            crate::library_ui::refresh_now(ui);
        }
        Err(e) => ui.global::<Organize>().set_status(format!("Couldn't put everything back: {e}").into()),
    }
}

pub fn install(ui: &AppWindow, settings: Arc<Mutex<Settings>>) {
    with(|s| s.settings = Some(settings));
    let o = ui.global::<Organize>();
    o.on_changed({
        let weak = ui.as_weak();
        move || replan(&weak.unwrap())
    });
    o.on_change_root({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let start = with(|s| s.root.clone());
            if let Some(d) = rfd::FileDialog::new().set_title("Put them in which folder?").set_directory(start).pick_folder() {
                with(|s| s.root = d);
                replan(&ui);
            }
        }
    });
    o.on_go({
        let weak = ui.as_weak();
        move || go(&weak.unwrap())
    });
    o.on_undo({
        let weak = ui.as_weak();
        move || undo(&weak.unwrap())
    });
    o.on_close({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            ui.global::<Organize>().set_open(false);
            with(|s| s.done = None);
        }
    });
    // `NERIA_ORGANIZE`: these files (`;` between) in the dialog, for checks; with
    // `NERIA_ORGANIZE_ROOT` as the folder and `NERIA_ORGANIZE_GO` pressing Move.
    if let Ok(files) = std::env::var("NERIA_ORGANIZE") {
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
            let Some(ui) = weak.upgrade() else { return };
            let items: Vec<Item> = files
                .split(';')
                .filter(|s| !s.trim().is_empty())
                .map(|f| Item::new(library::Kind::Song, PathBuf::from(f), ""))
                .collect();
            if let Ok(r) = std::env::var("NERIA_ORGANIZE_ROOT") {
                with(|s| s.settings = None);
                open(&ui, items);
                with(|s| s.root = PathBuf::from(&r));
                replan(&ui);
            } else {
                open(&ui, items);
            }
            if std::env::var_os("NERIA_ORGANIZE_GO").is_some() {
                let weak = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                    if let Some(ui) = weak.upgrade() {
                        go(&ui);
                    }
                });
            }
            // `NERIA_ORGANIZE_UNDO`: Undo, a few seconds after.
            if std::env::var_os("NERIA_ORGANIZE_UNDO").is_some() {
                let weak = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_secs(8), move || {
                    if let Some(ui) = weak.upgrade() {
                        undo(&ui);
                    }
                });
            }
        });
    }
}
