//! What following a channel or playlist would see: the link a check lists, its preview
//! (followable?) and the entries a check gets, newest first for channels.
//! `NERIA_HOME=target/debug cargo run -p neria-core --example follow -- <channel or playlist link>`
//! Read-only: nothing is followed or downloaded.

use neria_core::settings::Settings;
use neria_core::{follow, probe};

fn main() -> anyhow::Result<()> {
    let url = std::env::args().nth(1).expect("usage: follow <channel or playlist link>");
    let settings = Settings::load();
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
    Ok(())
}
