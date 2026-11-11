//! End-to-end check without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example smoke -- <url> [audio|video] [out_dir] [start end [exact]]`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use neria_core::download::DownloadRequest;
use neria_core::queue::{JobState, Queue};
use neria_core::settings::{Quality, Settings};
use neria_core::{fmt, probe};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("usage: smoke <url> [audio] [out_dir]");
    let quality = if args.next().as_deref() == Some("audio") { Quality::Audio } else { Quality::P720 };

    let mut settings = Settings::default();
    if let Some(dir) = args.next() {
        settings.output_dir = dir.into();
    }
    let section = match (args.next(), args.next()) {
        (Some(a), Some(b)) => Some((a.parse::<f64>()?, b.parse::<f64>()?)),
        _ => None,
    };
    let exact_cuts = args.next().as_deref() == Some("exact");

    let info = probe::probe(&url, &settings)?;
    println!("probe: {} | {}", info.title, info.meta_line());
    if let Some(note) = &info.note {
        println!("note: {note}");
    }
    for (i, it) in info.items.iter().enumerate() {
        println!("  {:>2} {:<5} {} {}", i + 1, it.kind.label(), it.title, it.direct_url.as_deref().unwrap_or(""));
    }

    // Posts/playlists: NERIA_PICK="1,3" picks items (1-based); default all of them.
    let reqs = if info.items.is_empty() {
        let mut req = DownloadRequest::new(url, quality, info.title.clone());
        req.section = section;
        req.exact_cuts = exact_cuts;
        vec![req]
    } else {
        // `NERIA_PICK=1,2:audio,3:480` - an item can carry its own quality.
        let picked: Vec<(usize, Option<Quality>)> = match std::env::var("NERIA_PICK") {
            Ok(p) => p
                .split(',')
                .filter_map(|part| {
                    let (n, q) = part.trim().split_once(':').map_or((part.trim(), None), |(n, q)| (n, Some(q)));
                    let q = q.map(|q| match q {
                        "audio" => Quality::Audio,
                        "1080" => Quality::P1080,
                        "720" => Quality::P720,
                        "480" => Quality::P480,
                        _ => Quality::Best,
                    });
                    Some((n.parse::<usize>().ok()?.checked_sub(1)?, q))
                })
                .collect(),
            Err(_) => (0..info.items.len()).map(|i| (i, None)).collect(),
        };
        neria_core::post::requests(&info.title, &info.items, &picked, quality, &settings)
    };

    let settings = Arc::new(Mutex::new(settings));
    let (tx, rx) = std::sync::mpsc::channel();
    let queue = Queue::new(settings, move |s| {
        let _ = tx.send(s);
    });
    let mut left = reqs.len();
    queue.add_batch(reqs);

    let mut failed = 0;
    while let Ok(s) = rx.recv_timeout(Duration::from_secs(120)) {
        let (status, stats) = fmt::job_lines(&s);
        println!("#{} [{:?} {:>3.0}%] {status}  {stats}", s.id, s.state, s.progress * 100.0);
        if !s.state.is_active() {
            if s.state != JobState::Done {
                failed += 1;
            }
            println!("file: {:?}", s.file);
            left -= 1;
            if left == 0 {
                break;
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} job(s) didn't finish");
    Ok(())
}
