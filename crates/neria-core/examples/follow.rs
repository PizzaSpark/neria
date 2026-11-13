//! What following a channel or playlist would see: the link a check lists, its preview
//! (followable?) and the entries a check gets, newest first for channels.
//! `NERIA_HOME=<scratch home> cargo run -p neria-core --example follow -- <channel or playlist link> [--try-new]`
//! Read-only, unless `--try-new`: then it really follows the link (in NERIA_HOME's
//! `following.json`), pretends its newest upload is new, runs a check and downloads that
//! one upload as audio into NERIA_HOME/out. Use a scratch NERIA_HOME (with a `bin` junction).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use neria_core::queue::{JobState, Queue};
use neria_core::settings::{Preset, Quality, Settings};
use neria_core::{follow, paths, probe};

fn main() -> anyhow::Result<()> {
    let url = std::env::args().nth(1).expect("usage: follow <channel or playlist link> [--try-new]");
    let try_new = std::env::args().any(|a| a == "--try-new");
    let mut settings = Settings::load();
    let list = follow::list_url(&url);
    let channel = probe::channel_videos_url(&url).is_some();
    println!("checks list: {list} ({})", if channel { "channel, newest first" } else { "playlist" });
    let info = probe::probe(&url, &settings)?;
    println!("preview: {} · {} items · followable: {}", info.title, info.items.len(), info.followable);
    let entries = probe::list_entries(&list, &settings, channel.then_some(40))?;
    println!("a check sees {} entries:", entries.len());
    for it in entries.iter().take(5) {
        println!("  {} · {}", it.title, it.url);
    }
    if !try_new {
        return Ok(());
    }

    // Follow it, minus the newest upload, which the check should then find and download.
    let newest = if channel { entries.first() } else { entries.last() }.cloned();
    let Some(newest) = newest else { anyhow::bail!("nothing listed") };
    let seen: Vec<_> = info.items.iter().filter(|i| i.url != newest.url).cloned().collect();
    let name = if channel { info.uploader.clone().unwrap_or(info.title.clone()) } else { info.title.clone() };
    follow::follow(&url, &name, None, &seen, Quality::Audio, Preset::Audio);
    settings.output_dir = paths::app_dir().join("out");
    let (tx, rx) = std::sync::mpsc::channel();
    let queue = Queue::new(Arc::new(Mutex::new(settings.clone())), move |s| {
        let _ = tx.send(s);
    });
    let n = follow::check(&list, &settings, &queue)?;
    println!("check found {n} new (expected 1: {})", newest.title);
    let mut left = n;
    while left > 0 {
        let Ok(s) = rx.recv_timeout(Duration::from_secs(300)) else { anyhow::bail!("timed out") };
        if !s.state.is_active() {
            println!("{:?}: {} → {:?}", s.state, s.title, s.file);
            anyhow::ensure!(s.state == JobState::Done, "the new upload didn't download");
            left -= 1;
        }
    }
    // Once downloaded (the queue keeps it in the history) it's seen: the next check finds
    // nothing.
    let again = follow::check(&list, &settings, &queue)?;
    println!("second check found {again} new (expected 0)");
    let f = follow::find(&url).expect("still followed");
    println!("pending {:?}, seen has it: {}", f.pending, f.seen.contains(&newest.url));
    Ok(())
}
