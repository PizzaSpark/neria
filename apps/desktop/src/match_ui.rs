//! "Pick another match" for a Spotify track: list YouTube Music candidates,
//! let the user search, and re-download the chosen one in place.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use neria_core::fmt;
use neria_core::queue::Queue;
use neria_core::spotify::Track;
use neria_core::ytmusic::{self, Candidate};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, MatchItem};

#[derive(Default)]
struct State {
    job: u64,
    track: Track,
    items: Vec<Candidate>,
    chosen: Option<String>,
}

fn to_item(c: &Candidate, track: &Track, chosen: Option<&str>) -> MatchItem {
    let mut detail = vec![c.artists.clone()];
    if let Some(a) = &c.album {
        detail.push(a.clone());
    }
    if let Some(d) = c.duration {
        let diff = d - track.duration_secs();
        let delta = if diff.abs() < 1.5 { String::new() } else { format!(" ({:+.0}s)", diff) };
        detail.push(format!("{}{delta}", fmt::duration(d)));
    }
    let url = c.url();
    MatchItem {
        title: c.title.as_str().into(),
        detail: detail.join("  ·  ").into(),
        score: (c.score * 100.0).round() as i32,
        ruled_out: c.score == 0.0,
        current: chosen == Some(url.as_str()),
        url: url.into(),
    }
}

fn show(ui: &AppWindow, st: &State) {
    let items: Vec<MatchItem> = st.items.iter().map(|c| to_item(c, &st.track, st.chosen.as_deref())).collect();
    ui.set_match_items(ModelRc::from(Rc::new(VecModel::from(items))));
}

/// Search in the background and show ranked results.
fn search(ui: &AppWindow, state: &Arc<Mutex<State>>, generation: &Arc<AtomicU64>, query: Option<String>) {
    let my_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;
    ui.set_match_loading(true);
    ui.set_match_status("".into());
    let (state, generation, weak) = (state.clone(), generation.clone(), ui.as_weak());
    let track = state.lock().unwrap().track.clone();
    std::thread::spawn(move || {
        let result = match &query {
            Some(q) => ytmusic::search_songs(q).map(|found| ytmusic::rank(&track, found)),
            None => ytmusic::candidates(&track),
        };
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if generation.load(Ordering::SeqCst) != my_gen {
                return;
            }
            ui.set_match_loading(false);
            let mut st = state.lock().unwrap();
            match result {
                Ok(items) => {
                    if items.is_empty() {
                        ui.set_match_status("Nothing found. Try different words.".into());
                    }
                    st.items = items;
                    show(&ui, &st);
                }
                Err(e) => ui.set_match_status(format!("Search failed: {e}").into()),
            }
        });
    });
}

pub fn install(ui: &AppWindow, queue: Queue) {
    let state = Arc::new(Mutex::new(State::default()));
    let generation = Arc::new(AtomicU64::new(0));

    ui.on_match_pick({
        let (weak, queue, state, generation) = (ui.as_weak(), queue.clone(), state.clone(), generation.clone());
        move |id| {
            let ui = weak.unwrap();
            let Some(info) = queue.match_info(id as u64) else { return };
            let t = &info.track;
            ui.set_match_heading(format!("Spotify: {} – {}  ·  {}", t.artist_line(), t.title, fmt::duration(t.duration_secs())).into());
            ui.set_match_sub("Best matches first. Length difference from Spotify in brackets.".into());
            ui.set_match_query(format!("{} {}", t.main_artist(), t.title).into());
            let have = !info.candidates.is_empty();
            {
                let mut st = state.lock().unwrap();
                *st = State { job: id as u64, track: info.track, items: info.candidates, chosen: info.chosen };
                show(&ui, &st);
            }
            ui.set_match_open(true);
            if !have {
                search(&ui, &state, &generation, None);
            }
        }
    });

    ui.on_match_search({
        let (weak, state, generation) = (ui.as_weak(), state.clone(), generation.clone());
        move || {
            let ui = weak.unwrap();
            let q = ui.get_match_query().trim().to_string();
            if !q.is_empty() {
                search(&ui, &state, &generation, Some(q));
            }
        }
    });

    ui.on_match_use({
        let (weak, queue, state) = (ui.as_weak(), queue.clone(), state.clone());
        move |i| {
            let ui = weak.unwrap();
            let st = state.lock().unwrap();
            let Some(c) = st.items.get(i as usize) else { return };
            if queue.rematch(st.job, c.url()) {
                ui.set_match_open(false);
            } else {
                ui.set_match_status("That song is still downloading. Try again when it's done.".into());
            }
        }
    });

    ui.on_match_open_url(|url| {
        let _ = open::that(url.as_str());
    });
}
