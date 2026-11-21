//! "Clip that" without the UI (prototype):
//! `NERIA_HOME=target/debug cargo run -p neria-core --example voice -- [options]`
//!
//! - `--get`: download the voice model into `bin/voice/` (~17 MB), then stop. `--remove`.
//! - `--phrase "<words>"` (repeat for several; default "clip that"): what to listen for.
//! - `--pieces "<words>"`: only print how the phrase is cut into the model's pieces.
//! - `--mic <part of its name>` (else Windows' default; `--mics` lists them).
//! - `--file <sound or video>`: listen to a file instead of the mic (every phrase heard, with
//!   its time), e.g. a recording with game sound and the phrase said now and then.
//! - `--threshold <0-1>` (0.25: lower hears more), `--boost <n>` (1.5).
//! - `--run <s>`: stop after that long (else Ctrl+C).
//!
//! On the mic it prints each phrase heard (with a beep), the mic's level, and what listening
//! costs: the share of one CPU core spent recognising, every 5 s.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use neria_core::voice::{self, Event, Pieces, Spotter, Tuning};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let has = |name: &str| args.iter().any(|a| a == name);

    if has("--mics") {
        for m in neria_core::capture_sound::mics() {
            println!("{m}");
        }
        return Ok(());
    }
    if has("--remove") {
        voice::remove()?;
        println!("removed {}", voice::dir().display());
        return Ok(());
    }
    if has("--get") || !voice::ready() {
        let stop = AtomicBool::new(false);
        let mut last = 0;
        voice::install(&stop, |d, t| {
            let pct = t.map(|t| d * 100 / t.max(1)).unwrap_or(0);
            if pct / 10 != last / 10 {
                eprintln!("downloading the voice model… {pct}%");
                last = pct;
            }
        })?;
        println!("voice model ready in {}", voice::dir().display());
        if has("--get") {
            return Ok(());
        }
    }

    if let Some(p) = val("--pieces") {
        println!("{}", Pieces::load()?.encode(&p)?.join(" "));
        return Ok(());
    }

    let mut phrases: Vec<String> = args.windows(2).filter(|w| w[0] == "--phrase").map(|w| w[1].clone()).collect();
    if phrases.is_empty() {
        phrases.push(voice::DEFAULT_PHRASE.into());
    }
    let mut tune = Tuning::default();
    if let Some(v) = val("--threshold") {
        tune.threshold = v.parse()?;
    }
    if let Some(v) = val("--boost") {
        tune.boost = v.parse()?;
    }
    let pieces = Pieces::load()?;
    for p in &phrases {
        println!("listening for \"{p}\"  ({})", pieces.encode(p)?.join(" "));
    }
    println!("threshold {} · boost {}", tune.threshold, tune.boost);

    // Voice samples: every saying in the file (found by loudness) compared with them.
    if let (Some(list), Some(f)) = (val("--samples"), val("--file")) {
        let sound: Vec<Vec<f32>> =
            list.split(',').map(|p| voice::read_file(&PathBuf::from(p))).collect::<anyhow::Result<_>>()?;
        let samples =
            neria_core::voice_match::Samples::from_sound(&sound).ok_or_else(|| anyhow::anyhow!("need 2+ samples"))?;
        println!("{} samples · limit {:.2} · self-check {:?}", samples.len(), samples.limit, samples.self_check());
        let audio = voice::read_file(&PathBuf::from(f))?;
        let mut say = voice::Sayings::new(16_000);
        for c in audio.chunks(1600) {
            say.feed(c);
        }
        let mut n = 0;
        for (a, b) in &say.done {
            let s = (((a - 0.15).max(0.0) * 16_000.0) as usize).min(audio.len());
            let e = (((b + 0.15) * 16_000.0) as usize).min(audio.len());
            let (hit, d) = samples.matches(&audio[s..e]);
            n += hit as usize;
            println!(
                "{:>8}–{:<8} {:>6.2}  {}",
                neria_core::transcribe::clock(*a),
                neria_core::transcribe::clock(*b),
                d,
                if hit { "MATCH" } else { "" }
            );
        }
        println!("{n} matched of {} sayings", say.done.len());
        return Ok(());
    }

    if let Some(f) = val("--file") {
        let samples = voice::read_file(&PathBuf::from(f))?;
        let mut s = Spotter::new(&phrases, tune)?;
        let mut n = 0;
        for chunk in samples.chunks(1600) {
            for h in s.feed(16_000, chunk) {
                n += 1;
                println!("{:>8}  heard \"{}\"", neria_core::transcribe::clock(h.at), h.phrase);
            }
        }
        println!(
            "{n} heard in {} · {:.1}% of one core",
            neria_core::transcribe::clock(s.fed()),
            s.busy.as_secs_f64() / s.fed().max(0.001) * 100.0
        );
        return Ok(());
    }

    let stop = Arc::new(AtomicBool::new(false));
    if let Some(run) = val("--run") {
        let secs: f64 = run.parse()?;
        let stop = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(secs));
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    }
    let started = Instant::now();
    let mut peak = 0f32;
    let mut last_report = Instant::now();
    let mut heard = 0;
    let spotter = voice::listen(val("--mic"), &phrases, tune, stop, |e| match e {
        Event::Heard(h) => {
            heard += 1;
            print!("\x07");
            println!("{:>8}  HEARD \"{}\"", neria_core::transcribe::clock(h.at), h.phrase);
        }
        Event::Level(l) => {
            peak = peak.max(l);
            if last_report.elapsed() >= Duration::from_secs(5) {
                println!(
                    "{:>8}  mic peak {:>3}%",
                    neria_core::transcribe::clock(started.elapsed().as_secs_f64()),
                    (peak * 100.0) as u32
                );
                peak = 0.0;
                last_report = Instant::now();
            }
        }
    })?;
    println!(
        "{heard} heard in {} · {:.1}% of one core",
        neria_core::transcribe::clock(spotter.fed()),
        spotter.busy.as_secs_f64() / spotter.fed().max(0.001) * 100.0
    );
    Ok(())
}
