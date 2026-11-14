//! Clipping live streams without the UI (Clip track 8):
//! `NERIA_HOME=target/debug cargo run -p neria-core --example live -- <live link> <command> [out_dir]`
//!
//! - `window`: how far back the site keeps the stream.
//! - `rewind <seconds ago> <length>`: download that part through the queue, as the clipper's
//!   live mode does (the mark is pinned to the clock; the queue finds it in the window).
//!   `NERIA_WAIT=<s>` waits that long between marking and downloading (the window slides).
//! - `buffer <minutes> <seconds to run> <clip seconds>`: keep the last minutes with the
//!   replay buffer for a while, then save the last `clip seconds`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use neria_core::download::DownloadRequest;
use neria_core::queue::{JobState, Queue};
use neria_core::settings::{Quality, Settings};
use neria_core::{fmt, live};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = args.first().expect("usage: live <link> <window|rewind|buffer> …").clone();
    let mut settings = Settings::default();
    match args.get(1).map(String::as_str) {
        Some("window") => {
            println!("window: {}", fmt::duration(live::window_len(&url, &settings)?));
        }
        Some("rewind") => {
            let ago: f64 = args.get(2).expect("seconds ago").parse()?;
            let len: f64 = args.get(3).expect("length").parse()?;
            if let Some(dir) = args.get(4) {
                settings.output_dir = dir.into();
            }
            // `NERIA_NOW=<Unix s>`: mark as if it were then (the same mark, downloaded twice).
            let now = std::env::var("NERIA_NOW").ok().and_then(|n| n.parse().ok()).unwrap_or_else(live::now);
            let mut req = DownloadRequest::new(url, Quality::P720, "live");
            req.live_window = Some((now - ago, now - ago + len));
            println!("marked: {:.0} – {:.0} (Unix)", now - ago, now - ago + len);
            if let Ok(w) = std::env::var("NERIA_WAIT") {
                let w: u64 = w.parse()?;
                println!("waiting {w} s…");
                std::thread::sleep(Duration::from_secs(w));
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let queue = Queue::new(Arc::new(Mutex::new(settings)), move |s| {
                let _ = tx.send(s);
            });
            queue.set_ready(true);
            queue.add(req);
            while let Ok(s) = rx.recv_timeout(Duration::from_secs(300)) {
                let (status, stats) = fmt::job_lines(&s);
                println!("[{:?} {:>3.0}%] {status}  {stats}", s.state, s.progress * 100.0);
                if !s.state.is_active() {
                    println!("file: {:?}", s.file);
                    if s.state != JobState::Done {
                        println!("{}", queue.report(s.id).unwrap_or_default());
                    }
                    anyhow::ensure!(s.state == JobState::Done, "didn't finish: {:?}", s.error);
                    break;
                }
            }
        }
        Some("buffer") => {
            let minutes: u32 = args.get(2).expect("minutes").parse()?;
            let run: u64 = args.get(3).expect("seconds to run").parse()?;
            let clip: f64 = args.get(4).expect("clip seconds").parse()?;
            let out = std::path::PathBuf::from(args.get(5).cloned().unwrap_or_else(|| ".".into()));
            let buffer = live::ReplayBuffer::start(&url, &settings, minutes)?;
            for i in 0..run {
                std::thread::sleep(Duration::from_secs(1));
                if i % 10 == 9 {
                    let (secs, bytes) = buffer.kept();
                    println!("kept {} ({}), running {}", fmt::duration(secs), fmt::bytes(bytes as f64), buffer.running());
                }
            }
            let problem = buffer.problem.lock().unwrap().clone();
            if !problem.is_empty() {
                println!("problem: {problem}");
            }
            let file = buffer.clip_last(clip, &out, "live")?;
            println!("saved: {}", file.display());
            buffer.stop();
        }
        _ => anyhow::bail!("usage: live <link> <window|rewind|buffer> …"),
    }
    Ok(())
}
