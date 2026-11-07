//! Spotify end-to-end check without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example spotify -- <spotify link> <out_dir> [max_tracks] [--match-only]`

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use neria_core::download::DownloadRequest;
use neria_core::queue::Queue;
use neria_core::settings::Settings;
use neria_core::{fmt, music, spotify, ytmusic};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let link = spotify::parse_link(&args[0]).expect("not a Spotify link");
    let mut settings = Settings::default();
    settings.output_dir = args[1].clone().into();
    let max: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
    let match_only = args.iter().any(|a| a == "--match-only");

    let t = Instant::now();
    let mut col = spotify::fetch(&link, settings.spotify_credentials().as_ref())?;
    println!(
        "{} \"{}\" by {:?}: {} tracks (truncated={}) in {:.1}s",
        col.kind.label(),
        col.name,
        col.owner,
        col.tracks.len(),
        col.truncated,
        t.elapsed().as_secs_f32()
    );
    col.tracks.truncate(max);

    if match_only {
        for tr in &col.tracks {
            let t = Instant::now();
            let cands = ytmusic::candidates(tr)?;
            match cands.first() {
                Some(c) => println!(
                    "{:>5.2} {} - {} ({}) => {} | {} | {:?} | {:?}s  [{:.1}s]",
                    c.score,
                    tr.main_artist(),
                    tr.title,
                    fmt::duration(tr.duration_secs()),
                    c.title,
                    c.artists,
                    c.album,
                    c.duration,
                    t.elapsed().as_secs_f32()
                ),
                None => println!("  --  {} - {}: no candidates", tr.main_artist(), tr.title),
            }
        }
        return Ok(());
    }

    let (dir, jobs) = music::prepare(&col, &settings)?;
    println!("folder: {}", dir.display());
    let settings = Arc::new(Mutex::new(settings));
    let (tx, rx) = std::sync::mpsc::channel();
    let queue = Queue::new(settings, move |s| {
        let _ = tx.send(s);
    });
    let n = jobs.len();
    for j in jobs {
        queue.add(DownloadRequest::music(j));
    }
    let mut finished = std::collections::HashSet::new();
    while finished.len() < n {
        let s = rx.recv_timeout(Duration::from_secs(300))?;
        if !s.state.is_active() && finished.insert(s.id) {
            let (status, stats) = fmt::job_lines(&s);
            println!("[{:?}] {} :: {status} {stats} {}", s.state, s.title, s.error.clone().unwrap_or_default());
        }
    }
    Ok(())
}
