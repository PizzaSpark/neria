//! Spotify end-to-end check without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example spotify -- <spotify link> <out_dir> [max_tracks] [--match-only] [--rematch]`
//!
//! `--rematch`: after downloading, re-download the first song from its 2nd-best
//! candidate (exercises "pick another match").
//! `--lyrics-only`: "Lyrics only" (.lrc / .txt, no audio). `--number`: "Number playlist
//! songs". `--no-lyrics` / `--no-verify`: those settings off. `--add-lyrics`: afterwards,
//! "Add lyrics to songs I already have" on the folder. `--spotify-lyrics`: Spotify's own
//! lyrics on (needs `NERIA_HOME/cookies/spotify.com.txt`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use neria_core::download::DownloadRequest;
use neria_core::queue::Queue;
use neria_core::settings::Settings;
use neria_core::{fmt, music, spotify, ytmusic};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let link = spotify::parse_link(&args[0]).expect("not a Spotify link");
    let flag = |f: &str| args.iter().any(|a| a == f);
    let settings = Settings {
        output_dir: args[1].clone().into(),
        number_spotify: flag("--number"),
        lyrics: !flag("--no-lyrics"),
        verify_audio: !flag("--no-verify"),
        lyrics_spotify: flag("--spotify-lyrics"),
        ..Settings::default()
    };
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

    let (dir, jobs) =
        if flag("--lyrics-only") { music::prepare_lyrics_only(&col, &settings)? } else { music::prepare(&col, &settings)? };
    println!("folder: {}", dir.display());
    let plain_settings = settings.clone();
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
    let mut first_id = None;
    while finished.len() < n {
        let s = rx.recv_timeout(Duration::from_secs(300))?;
        first_id = first_id.or(Some(s.id));
        if !s.state.is_active() && finished.insert(s.id) {
            let (status, stats) = fmt::job_lines(&s);
            println!("[{:?}] {} :: {status} {stats} {}", s.state, s.title, s.error.clone().unwrap_or_default());
        }
    }

    if flag("--add-lyrics") {
        let (jobs, have, untagged) = music::add_lyrics_jobs(&dir, &plain_settings);
        println!("add lyrics: {} songs ({have} have a .lrc, {untagged} untagged)", jobs.len());
        let n = jobs.len();
        let mut done = std::collections::HashSet::new();
        for j in jobs {
            queue.add(DownloadRequest::music(j));
        }
        while done.len() < n {
            let s = rx.recv_timeout(Duration::from_secs(120))?;
            if !s.state.is_active() && done.insert(s.id) {
                let (status, stats) = fmt::job_lines(&s);
                println!("[{:?}] {} :: {status} {stats} {}", s.state, s.title, s.error.clone().unwrap_or_default());
            }
        }
    }

    if flag("--rematch") {
        let id = first_id.expect("a job");
        let info = queue.match_info(id).expect("music job");
        let alt = match info.candidates.get(1) {
            Some(c) => c.clone(),
            None => ytmusic::candidates(&info.track)?.get(1).cloned().expect("a 2nd candidate"),
        };
        println!("rematch -> {} | {} | {:?}", alt.title, alt.album.clone().unwrap_or_default(), alt.duration);
        assert!(queue.rematch(id, alt.url()));
        loop {
            let s = rx.recv_timeout(Duration::from_secs(300))?;
            if s.id == id && !s.state.is_active() {
                let (status, stats) = fmt::job_lines(&s);
                println!("[{:?}] {} :: {status} {stats} {}", s.state, s.title, s.error.clone().unwrap_or_default());
                break;
            }
        }
        let after = queue.match_info(id).unwrap();
        println!("chosen now: {:?}", after.chosen);
    }
    Ok(())
}
