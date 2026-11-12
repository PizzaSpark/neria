//! Follow a channel or playlist (Download track 12): "Follow" under its preview, the list
//! in Settings → Following, and the checks. One check runs at a time; due ones (a missed
//! check included) run once the tools are ready, one after the other.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use neria_core::follow::{self, Every};
use neria_core::queue::Queue;
use neria_core::settings::Settings;
use neria_core::{post, probe};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{AppWindow, FollowItem, Following};

/// The previewed channel or playlist, for "Follow".
struct Preview {
    url: String,
    name: String,
    thumbnail: Option<String>,
    items: Vec<post::Item>,
}

thread_local! {
    static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
    /// The list being checked right now.
    static CHECKING: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn ago(secs: u64) -> String {
    match secs {
        0..=89 => "just now".into(),
        90..=5_399 => format!("{} min ago", (secs + 30) / 60),
        5_400..=129_599 => format!("{} h ago", (secs + 1_800) / 3_600),
        _ => format!("{} days ago", (secs + 43_200) / 86_400),
    }
}

/// "daily · checked 2 h ago · 1 new", "weekly · paused".
fn detail(f: &follow::Followed, now: u64) -> String {
    let mut parts = vec![f.every.label().to_string()];
    if f.paused {
        parts.push("paused".into());
    } else if f.last_check > 0 {
        parts.push(format!("checked {}", ago(now.saturating_sub(f.last_check))));
    }
    if let Some(e) = &f.error {
        parts.push(format!("couldn't check: {e}"));
    } else if f.last_new > 0 {
        parts.push(format!("{} new", f.last_new));
    }
    if !f.pending.is_empty() {
        parts.push(format!("{} downloading", f.pending.len()));
    }
    parts.join(" · ")
}

fn refresh(ui: &AppWindow) {
    let now = now_secs();
    let checking = CHECKING.with(|c| c.borrow().clone());
    let items: Vec<FollowItem> = follow::list()
        .iter()
        .map(|f| FollowItem {
            url: f.url.as_str().into(),
            name: f.name.as_str().into(),
            detail: detail(f, now).into(),
            every: f.every.index() as i32,
            paused: f.paused,
            checking: checking.as_deref() == Some(f.url.as_str()),
        })
        .collect();
    let g = ui.global::<Following>();
    g.set_items(ModelRc::from(Rc::new(VecModel::from(items))));
    // The preview's button follows the list (unfollowed in Settings, say).
    let followed = PREVIEW.with(|p| p.borrow().as_ref().is_some_and(|p| follow::find(&p.url).is_some()));
    g.set_followed(followed);
    g.set_note(note(followed).into());
}

fn note(followed: bool) -> String {
    if followed {
        "New uploads download by themselves · Settings → Following".into()
    } else {
        "Get its new uploads by themselves, checked daily".into()
    }
}

/// A preview came in: offer "Follow" when it's a channel or playlist.
pub(crate) fn set_preview(ui: &AppWindow, url: &str, info: &probe::MediaInfo) {
    let g = ui.global::<Following>();
    if !info.followable {
        clear(ui);
        return;
    }
    // A channel's own name (its tab is "<channel> - Videos").
    let channel = probe::channel_videos_url(url).is_some();
    let name = match (&info.uploader, channel) {
        (Some(u), true) => u.clone(),
        _ => info.title.clone(),
    };
    PREVIEW.with(|p| {
        *p.borrow_mut() = Some(Preview {
            url: url.to_string(),
            name,
            thumbnail: info.thumbnail_url.clone(),
            items: info.items.clone(),
        })
    });
    let followed = follow::find(url).is_some();
    g.set_can_follow(true);
    g.set_followed(followed);
    g.set_note(note(followed).into());
}

pub(crate) fn clear(ui: &AppWindow) {
    PREVIEW.with(|p| *p.borrow_mut() = None);
    ui.global::<Following>().set_can_follow(false);
}

/// Check `url` now (if nothing else is being checked). Due ones follow when it's done.
fn run(ui: &AppWindow, url: String, queue: &Queue, settings: &Arc<Mutex<Settings>>) {
    if !ui.get_tools_ready() || CHECKING.with(|c| c.borrow().is_some()) {
        return;
    }
    CHECKING.with(|c| *c.borrow_mut() = Some(url.clone()));
    refresh(ui);
    let (weak, queue2, settings2) = (ui.as_weak(), queue.clone(), settings.clone());
    let s = settings.lock().unwrap().clone();
    let q = queue.clone();
    std::thread::spawn(move || {
        let result = follow::check(&url, &s, &q);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            CHECKING.with(|c| *c.borrow_mut() = None);
            refresh(&ui);
            if let Err(e) = &result
                && ui.get_drawer_open()
            {
                crate::show_toast(&ui, &format!("Couldn't check it: {e}"));
            }
            run_due(&ui, &queue2, &settings2);
        });
    });
}

/// Start the most overdue check, if any.
fn run_due(ui: &AppWindow, queue: &Queue, settings: &Arc<Mutex<Settings>>) {
    if let Some(url) = follow::due(now_secs()).into_iter().next() {
        run(ui, url, queue, settings);
    }
}

pub(crate) fn install(ui: &AppWindow, queue: Queue, settings: Arc<Mutex<Settings>>) -> slint::Timer {
    let g = ui.global::<Following>();
    g.on_follow({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(name) = PREVIEW.with(|p| {
                let p = p.borrow();
                let p = p.as_ref()?;
                let (quality, preset) = crate::chosen_quality(&ui);
                follow::follow(&p.url, &p.name, p.thumbnail.clone(), &p.items, quality, preset);
                Some(p.name.clone())
            }) else {
                return;
            };
            refresh(&ui);
            crate::show_toast(&ui, &format!("Following {name}. New uploads download by themselves (checked daily)."));
        }
    });
    g.on_open_list({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            refresh(&ui);
            ui.set_settings_section(1);
            ui.set_drawer_open(true);
        }
    });
    g.on_check_now({
        let (weak, queue, settings) = (ui.as_weak(), queue.clone(), settings.clone());
        move |url| {
            if let Some(ui) = weak.upgrade() {
                run(&ui, url.to_string(), &queue, &settings);
            }
        }
    });
    g.on_pause({
        let weak = ui.as_weak();
        move |url, paused| {
            follow::set_paused(&url, paused);
            if let Some(ui) = weak.upgrade() {
                refresh(&ui);
            }
        }
    });
    g.on_unfollow({
        let weak = ui.as_weak();
        move |url| {
            follow::unfollow(&url);
            if let Some(ui) = weak.upgrade() {
                refresh(&ui);
            }
        }
    });
    g.on_every({
        let weak = ui.as_weak();
        move |url, i| {
            follow::set_every(&url, Every::from_index(i.max(0) as usize));
            if let Some(ui) = weak.upgrade() {
                refresh(&ui);
            }
        }
    });
    refresh(ui);

    // Missed checks run soon after start (once the tools are ready), then every minute
    // looks for due ones. Times in the list ("checked 2 h ago") move along with it.
    {
        let (weak, queue, settings) = (ui.as_weak(), queue.clone(), settings.clone());
        slint::Timer::single_shot(Duration::from_secs(20), move || {
            if let Some(ui) = weak.upgrade() {
                run_due(&ui, &queue, &settings);
            }
        });
    }
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    timer.start(slint::TimerMode::Repeated, Duration::from_secs(60), move || {
        if let Some(ui) = weak.upgrade() {
            refresh(&ui);
            run_due(&ui, &queue, &settings);
        }
    });
    timer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_it_goes() {
        let f = follow::Followed { last_check: 1_000, last_new: 2, ..Default::default() };
        assert_eq!(detail(&f, 1_000 + 7_200), "daily · checked 2 h ago · 2 new");
        let p = follow::Followed { paused: true, every: Every::Weekly, ..Default::default() };
        assert_eq!(detail(&p, 0), "weekly · paused");
    }
}
