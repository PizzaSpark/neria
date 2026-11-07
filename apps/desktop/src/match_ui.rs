//! "Pick another match" for a Spotify track: list YouTube Music candidates,
//! check them by sound against an official preview, let the user search, and
//! re-download the chosen one in place.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use neria_core::fingerprint::{self, Verdict};
use neria_core::fmt;
use neria_core::queue::Queue;
use neria_core::spotify::Track;
use neria_core::ytmusic::{self, Candidate};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, MatchItem};

/// Candidates checked automatically when the dialog opens.
const AUTO_CHECK: usize = 5;

#[derive(Clone, Debug)]
enum Check {
    Running,
    Done(f64),
    Failed,
}

#[derive(Default)]
struct State {
    /// Bumped on every open so results for an older dialog are ignored.
    session: u64,
    job: u64,
    track: Track,
    items: Vec<Candidate>,
    chosen: Option<String>,
    /// None = still looking; Some(None) = no preview anywhere.
    reference: Option<Option<(Arc<Vec<u32>>, &'static str)>>,
    checks: HashMap<String, Check>,
}

fn to_item(c: &Candidate, st: &State) -> MatchItem {
    let mut detail = vec![c.artists.clone()];
    if let Some(a) = &c.album {
        detail.push(a.clone());
    }
    if let Some(d) = c.duration {
        let diff = d - st.track.duration_secs();
        let delta = if diff.abs() < 1.5 { String::new() } else { format!(" ({:+.0}s)", diff) };
        detail.push(format!("{}{delta}", fmt::duration(d)));
    }
    let url = c.url();
    let (check_state, check_text) = match st.checks.get(&c.video_id) {
        None => (0, String::new()),
        Some(Check::Running) => (1, "Checking sound…".into()),
        Some(Check::Failed) => (5, "Couldn't check".into()),
        Some(Check::Done(sim)) => {
            let pct = (sim * 100.0).round();
            match fingerprint::verdict(*sim) {
                Verdict::Same => (2, format!("✓ Same recording ({pct}%)")),
                Verdict::Unsure => (3, format!("Close ({pct}%)")),
                Verdict::Different => (4, format!("Different recording ({pct}%)")),
            }
        }
    };
    MatchItem {
        title: c.title.as_str().into(),
        detail: detail.join("  ·  ").into(),
        score: (c.score * 100.0).round() as i32,
        ruled_out: c.score == 0.0,
        current: st.chosen.as_deref() == Some(url.as_str()),
        url: url.into(),
        check_state,
        check_text: check_text.into(),
    }
}

/// Same-recording results float to the top; otherwise keep the match order.
fn sorted(st: &State) -> Vec<Candidate> {
    let rank = |c: &Candidate| match st.checks.get(&c.video_id) {
        Some(Check::Done(s)) if fingerprint::verdict(*s) == Verdict::Same => 0,
        Some(Check::Done(s)) if fingerprint::verdict(*s) == Verdict::Different => 2,
        _ => 1,
    };
    let mut items = st.items.clone();
    items.sort_by(|a, b| rank(a).cmp(&rank(b)).then(b.score.total_cmp(&a.score)));
    items
}

fn show(ui: &AppWindow, st: &mut State) {
    st.items = sorted(st);
    let items: Vec<MatchItem> = st.items.iter().map(|c| to_item(c, st)).collect();
    ui.set_match_items(ModelRc::from(Rc::new(VecModel::from(items))));
    ui.set_match_can_check(matches!(st.reference, Some(Some(_))));
    let status = match &st.reference {
        None => "Finding an official preview to compare the sound with…".to_string(),
        Some(None) => "No official preview found (Spotify, Deezer or Apple), so versions can't be checked by sound.".into(),
        Some(Some((_, src))) => {
            let running = st.checks.values().filter(|c| matches!(c, Check::Running)).count();
            if running > 0 {
                format!("Comparing with the {src} preview… ({running} left)")
            } else {
                format!("Checked by sound against the {src} preview. Length difference from Spotify in brackets.")
            }
        }
    };
    ui.set_match_sub(status.into());
}

/// Which candidates to check automatically: the best few by title/length, plus
/// same-title versions ruled out by length (often the album version of the
/// same recording).
fn auto_targets(st: &State) -> Vec<Candidate> {
    let want = ytmusic::normalize(&st.track.title);
    let mut out: Vec<Candidate> = st.items.iter().filter(|c| c.score > 0.0).take(3).cloned().collect();
    for c in st.items.iter().filter(|c| c.score == 0.0 && ytmusic::normalize(&c.title) == want) {
        if out.len() >= AUTO_CHECK {
            break;
        }
        out.push(c.clone());
    }
    out.retain(|c| !st.checks.contains_key(&c.video_id));
    out
}

fn start_checks(ui: &AppWindow, state: &Arc<Mutex<State>>, targets: Vec<Candidate>) {
    let (session, clip) = {
        let mut st = state.lock().unwrap();
        let Some(Some((clip, _))) = st.reference.clone() else { return };
        for c in &targets {
            st.checks.insert(c.video_id.clone(), Check::Running);
        }
        show(ui, &mut st);
        (st.session, clip)
    };
    for c in targets {
        let (state, weak, clip) = (state.clone(), ui.as_weak(), clip.clone());
        std::thread::spawn(move || {
            let result = fingerprint::compare_candidate(&clip, &c.url());
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let mut st = state.lock().unwrap();
                if st.session != session {
                    return;
                }
                let check = match result {
                    Ok(a) => Check::Done(a.similarity),
                    Err(_) => Check::Failed,
                };
                st.checks.insert(c.video_id.clone(), check);
                show(&ui, &mut st);
            });
        });
    }
}

/// Look up the reference preview once per dialog, then check the obvious candidates.
fn find_reference(ui: &AppWindow, state: &Arc<Mutex<State>>) {
    let (session, track) = {
        let st = state.lock().unwrap();
        (st.session, st.track.clone())
    };
    let (state, weak) = (state.clone(), ui.as_weak());
    std::thread::spawn(move || {
        let reference = fingerprint::reference_for(&track)
            .and_then(|r| fingerprint::reference_fingerprint(&r).ok().map(|fp| (Arc::new(fp), r.source)));
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let targets = {
                let mut st = state.lock().unwrap();
                if st.session != session {
                    return;
                }
                st.reference = Some(reference);
                show(&ui, &mut st);
                auto_targets(&st)
            };
            start_checks(&ui, &state, targets);
        });
    });
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
            let targets = {
                let mut st = state.lock().unwrap();
                match result {
                    Ok(items) => {
                        if items.is_empty() {
                            ui.set_match_status("Nothing found. Try different words.".into());
                        }
                        st.items = items;
                        show(&ui, &mut st);
                        if matches!(st.reference, Some(Some(_))) { auto_targets(&st) } else { Vec::new() }
                    }
                    Err(e) => {
                        ui.set_match_status(format!("Search failed: {e}").into());
                        Vec::new()
                    }
                }
            };
            start_checks(&ui, &state, targets);
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
            ui.set_match_query(format!("{} {}", t.main_artist(), t.title).into());
            ui.set_match_status("".into());
            let have = !info.candidates.is_empty();
            {
                let mut st = state.lock().unwrap();
                let session = st.session + 1;
                *st = State {
                    session,
                    job: id as u64,
                    track: info.track,
                    items: info.candidates,
                    chosen: info.chosen,
                    reference: None,
                    checks: HashMap::new(),
                };
                show(&ui, &mut st);
            }
            ui.set_match_open(true);
            if !have {
                search(&ui, &state, &generation, None);
            }
            find_reference(&ui, &state);
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

    ui.on_match_check({
        let (weak, state) = (ui.as_weak(), state.clone());
        move |i| {
            let ui = weak.unwrap();
            let target = state.lock().unwrap().items.get(i as usize).cloned();
            if let Some(c) = target {
                start_checks(&ui, &state, vec![c]);
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
