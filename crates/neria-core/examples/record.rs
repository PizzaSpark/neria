//! Recording a live stream without the UI (Livestreams milestone):
//! `NERIA_HOME=target/debug cargo run -p neria-core --example record -- <live link> <now|start|both> <seconds> [out_dir]`
//!
//! Records for `seconds`, printing the row's numbers every few seconds, then stops (as the
//! Stop button does) and saves. Options:
//! - `--clip <s>@<t>`: at `t` seconds in, "Clip the last" `s` seconds (while it records).
//! - `--audio`: sound only. `--height <h>`: the picture size cap (default 480).
//! - `--started <unix>`: when the stream went live (else what yt-dlp says).
//! - `--starts <unix>`: a stream that isn't on yet (waits for it).
//! - `--kill-at <t>`: kill yt-dlp at `t` seconds without stopping (a dropped connection).
//! - `--crash-at <t>`: give up at `t` as if Neria died: the parts are left; then
//!   `--save <work dir>` saves them as Neria does at its next start.
//! - `--earlier-at <t>`: at `t` seconds in, "Also get the part before" (a from-now
//!   recording): fetched next to it, joined in front where the sound matches.
//! - `--queue`: through the download queue instead (as the app does): the row's state,
//!   "Clip the last" (`--clip`) and Stop go through it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use neria_core::record::{self, LiveJob, RecEvent, Recording};
use neria_core::settings::Settings;
use neria_core::{fmt, live};

/// The same through `Queue`, as the app runs it.
fn through_queue(url: &str, args: &[String]) -> anyhow::Result<()> {
    use neria_core::download::DownloadRequest;
    use neria_core::queue::{JobState, Queue};
    use neria_core::settings::Quality;
    let secs: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let mut settings = Settings::default();
    if let Some(dir) = args.get(3).filter(|a| !a.starts_with("--")) {
        settings.output_dir = dir.into();
    }
    let clip = opt(args, "--clip").and_then(|c| {
        let (s, t) = c.split_once('@')?;
        Some((s.parse::<f64>().ok()?, t.parse::<f64>().ok()?))
    });
    let last: Arc<Mutex<Vec<neria_core::queue::JobSnapshot>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = last.clone();
    let printed = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(10)));
    let queue = Queue::new(Arc::new(Mutex::new(settings)), move |s| {
        let mut p = printed.lock().unwrap();
        if p.elapsed() >= Duration::from_secs(3) || s.state != JobState::Downloading {
            *p = Instant::now();
            let (status, stats) = fmt::job_lines(&s);
            println!("#{} {:?} | {status} | {stats} | note {:?} | err {:?}", s.id, s.state, s.note, s.error);
        }
        let mut l = seen.lock().unwrap();
        l.retain(|x| x.id != s.id);
        l.push(s);
    });
    let mut req = DownloadRequest::new(url, Quality::P480, "queue record test");
    // `both`: from now with the part before fetched next to it at once (the app's "From the
    // start" on a stream already live).
    req.live = Some(LiveJob {
        from_start: args.get(1).is_some_and(|m| m == "start"),
        with_earlier: args.get(1).is_some_and(|m| m == "both"),
        // As the app passes it from the link's preview.
        started_at: opt(args, "--started").and_then(|v| v.parse().ok()),
        ..Default::default()
    });
    let id = queue.add(req);
    let t0 = Instant::now();
    let mut clipped = false;
    while t0.elapsed().as_secs_f64() < secs {
        std::thread::sleep(Duration::from_millis(200));
        if let Some((s, _)) = clip.filter(|(_, at)| !clipped && t0.elapsed().as_secs_f64() >= *at) {
            clipped = true;
            queue.clip_last(id, s);
        }
    }
    if let Some(r) = queue.report(id) {
        println!(
            "-- log
{r}"
        );
    }
    println!("-- Stop (recordings running: {})", queue.recording_count());
    queue.stop(id);
    while queue.recording_count() > 0 {
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_millis(500));
    for s in last.lock().unwrap().iter() {
        println!("final #{} {:?}: {:?} note {:?} err {:?}", s.id, s.state, s.file, s.note, s.error);
    }
    Ok(())
}

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(work) = opt(&args, "--save") {
        let out = opt(&args, "--out").unwrap_or_else(|| ".".into());
        let saved = record::save(std::path::Path::new(&work), std::path::Path::new(&out))?;
        println!("saved {:?} ({})", saved.files, fmt::duration(saved.recorded));
        return Ok(());
    }
    let url = args.first().expect("usage: record <link> <now|start> <seconds> [out_dir]").clone();
    if args.iter().any(|a| a == "--queue") {
        return through_queue(&url, &args);
    }
    let from_start = args.get(1).is_some_and(|m| m == "start");
    let secs: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let out_dir = args.get(3).filter(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| ".".into());
    let out_dir = std::path::PathBuf::from(out_dir);
    let clip = opt(&args, "--clip").and_then(|c| {
        let (s, t) = c.split_once('@')?;
        Some((s.parse::<f64>().ok()?, t.parse::<f64>().ok()?))
    });
    let kill_at: Option<f64> = opt(&args, "--kill-at").and_then(|v| v.parse().ok());
    let crash_at: Option<f64> = opt(&args, "--crash-at").and_then(|v| v.parse().ok());
    let settings = Settings::default();
    let title = format!("record test {}", live::now() as u64);
    let rec = Recording {
        url: &url,
        settings: &settings,
        audio: args.iter().any(|a| a == "--audio"),
        max_height: Some(opt(&args, "--height").and_then(|h| h.parse().ok()).unwrap_or(480)),
        live: LiveJob {
            from_start,
            started_at: opt(&args, "--started").and_then(|v| v.parse().ok()),
            starts_at: opt(&args, "--starts").and_then(|v| v.parse().ok()),
            ..Default::default()
        },
        title: &title,
        out_dir: &out_dir,
        work: record::work_dir(&out_dir, &title),
    };
    println!("work: {}", rec.work.display());
    let slot = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let cancel = Arc::new(AtomicBool::new(false));
    // "Also get the part before" at `t` seconds in (a from-now recording): fetched next to
    // it, joined in front on saving.
    if let Some(at) = opt(&args, "--earlier-at").and_then(|v| v.parse::<f64>().ok()) {
        let (url, settings, stop) = (url.clone(), settings.clone(), stop.clone());
        let (out_dir, title, work, live) = (out_dir.clone(), title.clone(), rec.work.clone(), rec.live.clone());
        let (audio, max_height) = (rec.audio, rec.max_height);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(at));
            let rec = Recording {
                url: &url,
                settings: &settings,
                audio,
                max_height,
                live,
                title: &title,
                out_dir: &out_dir,
                work,
            };
            let slot = Mutex::new(None);
            let started = Instant::now();
            let mut printed = Instant::now();
            let never = AtomicBool::new(false);
            let result = record::fetch_earlier(&rec, &slot, &stop, &never, |got, of, bytes| {
                if printed.elapsed() >= Duration::from_secs(10) {
                    printed = Instant::now();
                    println!("earlier: {} of {} · {}", fmt::duration(got), fmt::duration(of), fmt::bytes(bytes as f64));
                }
            });
            println!("earlier done in {:.0} s: {result:?}", started.elapsed().as_secs_f64());
        });
    }
    // The clock: stop (or clip, kill, crash) on time from another thread, as the UI would.
    {
        let (slot, stop, cancel, work) = (slot.clone(), stop.clone(), cancel.clone(), rec.work.clone());
        let (out_dir, title) = (out_dir.clone(), title.clone());
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let (mut clipped, mut killed) = (false, false);
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let t = t0.elapsed().as_secs_f64();
                if let Some((s, at)) = clip.filter(|(_, at)| !clipped && t >= *at) {
                    clipped = true;
                    let started = Instant::now();
                    match record::clip_last(&work, s, &out_dir, &title) {
                        Ok(p) => println!(
                            "clip: {} ({}, took {:.1} s)",
                            p.display(),
                            fmt::duration(neria_core::media::duration(&p).unwrap_or(0.0)),
                            started.elapsed().as_secs_f64()
                        ),
                        Err(e) => println!("clip failed: {e}"),
                    }
                    let _ = at;
                }
                if kill_at.is_some_and(|k| !killed && t >= k) {
                    killed = true;
                    println!("-- killing yt-dlp (a dropped connection)");
                    if let Some(c) = slot.lock().unwrap().as_mut() {
                        neria_core::proc::kill_tree(c);
                    }
                }
                if crash_at.is_some_and(|c| t >= c) {
                    println!("-- crash: leaving the parts in {}", work.display());
                    if let Some(c) = slot.lock().unwrap().as_mut() {
                        neria_core::proc::kill_tree(c);
                    }
                    std::process::exit(3);
                }
                if t >= secs {
                    println!("-- Stop");
                    stop.store(true, Ordering::SeqCst);
                    if let Some(c) = slot.lock().unwrap().as_mut() {
                        neria_core::proc::kill_tree(c);
                    }
                    let _ = &cancel;
                    return;
                }
            }
        });
    }
    let t0 = Instant::now();
    let mut last = Instant::now() - Duration::from_secs(10);
    let saved = record::record(&rec, &slot, &stop, &cancel, |ev| match ev {
        RecEvent::Stats(s) => {
            if last.elapsed() >= Duration::from_secs(3) {
                last = Instant::now();
                let len = s.stream_len(live::now());
                println!(
                    "{:5.1}s {:?}: recorded {} · {} · {}/s · at {} of {} (joined {}) · {}p · holes {:?} · clip {}",
                    t0.elapsed().as_secs_f64(),
                    s.phase,
                    fmt::duration(s.recorded),
                    fmt::bytes(s.bytes as f64),
                    fmt::bytes(s.speed),
                    fmt::duration(s.position),
                    fmt::duration(len),
                    s.joined.map(fmt::duration).unwrap_or_else(|| "?".into()),
                    s.height.unwrap_or(0),
                    s.holes,
                    s.can_clip
                );
            }
        }
        RecEvent::Log(l) => println!("  log: {l}"),
    })?;
    println!("saved in {:.1} s: {:?}", t0.elapsed().as_secs_f64() - secs, saved.files);
    println!("note: {} · recorded {} · holes {:?}", saved.note, fmt::duration(saved.recorded), saved.holes);
    for f in &saved.files {
        println!("  {} plays {}", f.display(), fmt::duration(neria_core::media::duration(f).unwrap_or(0.0)));
    }
    Ok(())
}
