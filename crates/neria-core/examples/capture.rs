//! Instant replay of the screen without the UI (prototype, picture only):
//! `NERIA_HOME=target/debug cargo run -p neria-core --example capture -- [options]`
//!
//! - `--window <game.exe>`: one program's window (Windows Graphics Capture); else
//!   `--monitor <n>` (Desktop Duplication, default 0).
//! - Sound, a track each: the game & PC, Discord on its own when it's running, the mic.
//!   `--mic <part of its name>` (else Windows' default; `--mics` lists them), `--no-sound`,
//!   or exactly `--sound pc,mic,mic:fifine,app:Discord.exe,except:Discord.exe`.
//! - `--keep <s>` (60), `--fps <n>` (60), `--mbps <n>` (16), `--height <px>` (scaled down on
//!   the graphics card).
//! - `--quality smaller|standard|best`: as the Capture page's Quality (size, frame rate and bit
//!   rate worked out for the source; prints them).
//! - `--route nvenc|amf|qsv|nvenc-copy|cpu`: force a route instead of trying them.
//! - `--routes`: only say which routes work here, then stop.
//! - `--encoder auto|nvidia|amd|intel|cpu`: as the Capture page's Encoder (a window is grabbed
//!   on that chip); `--chip <n>` with `--route` grabs a window on chip n.
//! - `--run <s>`: stop after that long and save the last `--keep` seconds. Without it:
//!   Enter saves the last `--keep` seconds, `q` + Enter stops.
//! - `--hotkey <key>` (F8; e.g. `Alt+F10`): saves the last `--keep` seconds from anywhere,
//!   even inside a game, with a short sound; `--no-hotkey`.
//! - `--list`: the monitors (by name), programs with a window open (games first), what's in
//!   front and the apps playing sound, as the Capture page sees them, then stop.
//! - `--tracks`: sound as the Capture page plans it (Everything, the game when recording one,
//!   each app playing sound, the mic); `--never a.exe,b.exe` leaves those apps out.
//! - `--out <dir>` (default: the current folder).
//!
//! While it runs it prints how it keeps up every 2 s: play a game with it and compare the
//! game's fps and 1% lows with and without it.

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use neria_core::capture::{Options, Route, ScreenBuffer, Sound, Source};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let mut o = Options::default();
    if let Some(exe) = val("--window") {
        o.source = Source::Window(exe);
    } else if let Some(m) = val("--monitor") {
        o.source = Source::Monitor(m.parse()?);
    }
    if let Some(v) = val("--keep") {
        o.keep = v.parse()?;
    }
    if let Some(v) = val("--fps") {
        o.fps = v.parse()?;
    }
    if let Some(v) = val("--mbps") {
        o.mbps = v.parse()?;
    }
    if let Some(h) = val("--height") {
        let size = o.source.size().map(|s| s.0).unwrap_or((1920, 1080));
        let l = neria_core::capture::Look { height: h.parse()?, fps: o.fps, mbps: o.mbps };
        o.size = l.scale(size);
    }
    if let Some(q) = val("--quality") {
        let (size, hz) = o.source.size().unwrap_or(((1920, 1080), 60));
        let l = neria_core::capture::look(neria_core::capture::Quality::from_name(&q), size, hz);
        o.fps = l.fps;
        o.mbps = l.mbps;
        o.size = l.scale(size);
        let (w, h) = l.size(size);
        println!(
            "{q}: {w} × {h} (from {} × {}, {hz} Hz) · {} fps · {} Mbit/s · ≈ {} MB a minute",
            size.0,
            size.1,
            l.fps,
            l.mbps,
            l.mb_per_min(0)
        );
    }
    if let Some(r) = val("--route") {
        o.chip = val("--chip").map(|c| c.parse()).transpose()?;
        o.route = Some(match r.as_str() {
            "nvenc" => Route::Nvenc,
            "amf" => Route::Amf,
            "qsv" => Route::Qsv,
            "nvenc-copy" => Route::NvencCopy,
            "cpu" => Route::Cpu,
            _ => anyhow::bail!("unknown route {r}"),
        });
    }
    if let Some(e) = val("--encoder") {
        o.encoder = neria_core::capture::Encoder::from_name(&e);
    }
    let out: PathBuf = val("--out").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if args.iter().any(|a| a == "--list") {
        for m in neria_core::capture_pick::monitors() {
            println!("monitor {}: {} · {}", m.index, m.name, m.detail());
        }
        for p in neria_core::capture_pick::programs() {
            println!("{} {} ({})", if p.game { "game  " } else { "window" }, p.name, p.exe);
        }
        for c in neria_core::capture_pick::chips() {
            println!("chip {}: {} ({:?})", c.index, c.name, c.vendor);
        }
        let f = neria_core::capture_pick::front();
        println!("in front: game {:?}, monitor {:?}", f.game.map(|g| g.name), f.monitor);
        for a in neria_core::capture_sound::playing_apps() {
            println!("sound  {} ({}) peak {:.2}", a.name, a.exe, a.peak);
        }
        return Ok(());
    }
    if args.iter().any(|a| a == "--mics") {
        for m in neria_core::capture::mics() {
            println!("{m}");
        }
        return Ok(());
    }
    // Sound: Windows' default mic unless `--mic`; `--no-sound`; `--sound pc,app:Discord.exe,…`.
    o.sounds = neria_core::capture::default_sounds(val("--mic"));
    if args.iter().any(|a| a == "--no-sound") {
        o.sounds.clear();
    }
    if let Some(list) = val("--sound") {
        o.sounds = list
            .split(',')
            .map(|s| match s.split_once(':') {
                None if s == "pc" => Ok(Sound::Pc),
                None if s == "mic" => Ok(Sound::Mic(None)),
                Some(("mic", m)) => Ok(Sound::Mic(Some(m.into()))),
                Some(("app", a)) => Ok(Sound::App(a.into())),
                Some(("except", a)) => Ok(Sound::PcExcept(a.into())),
                _ => Err(anyhow::anyhow!("unknown sound {s}")),
            })
            .collect::<anyhow::Result<_>>()?;
    }
    if args.iter().any(|a| a == "--tracks") {
        let prefs = neria_core::capture::Prefs {
            never: val("--never").map(|n| n.split(',').map(String::from).collect()).unwrap_or_default(),
            mic_name: val("--mic").unwrap_or_default(),
            ..Default::default()
        };
        let game = match &o.source {
            Source::Window(exe) => Some(exe.clone()),
            _ => None,
        };
        let plan = prefs.sound_plan(game.as_deref().map(|g| (g, g.trim_end_matches(".exe"))));
        o.sounds = plan.sounds;
        o.tracks = plan.tracks;
        println!(
            "tracks: {}{}",
            o.titles().join(" · "),
            if plan.by_app { " (everything built app by app)" } else { "" }
        );
    }

    if args.iter().any(|a| a == "--routes") {
        for r in Route::ALL {
            match r.check(&o.source, o.fps, o.size) {
                Ok(()) => println!("{:<40} works", r.label()),
                Err(e) => println!("{:<40} no: {e}", r.label()),
            }
        }
        return Ok(());
    }

    let t = Instant::now();
    let buffer = ScreenBuffer::start(o.clone())?;
    println!(
        "recording {:?} through {} on chip {:?} (picked in {:.1}s), keeping {} s",
        o.source,
        buffer.route.label(),
        buffer.options.chip,
        t.elapsed().as_secs_f64(),
        o.keep
    );
    let titles = o.titles();
    println!("sound tracks: {}", if titles.is_empty() { "none".into() } else { titles.join(" · ") });

    // The hotkey, working while a game has focus: a sound at once, the save in the background
    // (one at a time), recording going on.
    let key_text = val("--hotkey").unwrap_or_else(|| neria_core::hotkey::DEFAULT.into());
    let _hotkey = if args.iter().any(|a| a == "--no-hotkey") {
        None
    } else {
        let key = neria_core::hotkey::Hotkey::parse(&key_text)?;
        let (dir, keep, out) = (buffer.dir.clone(), o.keep, out.clone());
        let titles = o.titles();
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let l = neria_core::hotkey::listen(key, move || {
            if busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
                println!("{key_text}: still saving the last one");
                return;
            }
            neria_core::hotkey::confirm_sound();
            let (dir, out, titles, busy) = (dir.clone(), out.clone(), titles.clone(), busy.clone());
            std::thread::spawn(move || {
                let t = Instant::now();
                match neria_core::capture::save_from(&dir, keep, &out, &titles, "Replay") {
                    Ok(p) => println!("hotkey: saved {} in {:.1}s", p.display(), t.elapsed().as_secs_f64()),
                    Err(e) => println!("hotkey: couldn't save: {e}"),
                }
                busy.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        });
        match l {
            Ok(l) => {
                println!(
                    "{}: save the last {} s, from anywhere",
                    val("--hotkey").unwrap_or_else(|| neria_core::hotkey::DEFAULT.into()),
                    o.keep
                );
                Some(l)
            }
            Err(e) => {
                println!("hotkey not available: {e}");
                None
            }
        }
    };

    let run = val("--run").map(|s| s.parse::<u64>()).transpose()?;
    let (tx, rx) = mpsc::channel::<String>();
    if run.is_none() {
        println!("Enter: save the last {} s · q + Enter: stop", o.keep);
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(|l| l.ok()) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    let began = Instant::now();
    let mut last_print = Instant::now();
    loop {
        if !buffer.running() {
            anyhow::bail!("stopped: {}", buffer.problem.lock().unwrap());
        }
        if let Some(r) = run {
            if began.elapsed() >= Duration::from_secs(r) {
                save(&buffer, o.keep, &out);
                break;
            }
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(l) if l.trim() == "q" => break,
            Ok(_) => save(&buffer, o.keep, &out),
            Err(_) => {}
        }
        if last_print.elapsed() >= Duration::from_secs(2) {
            last_print = Instant::now();
            let s = buffer.stats.lock().unwrap().clone();
            let (kept, bytes) = buffer.kept();
            println!(
                "{:>5.0}s  {:>5.1} fps  speed {:.2}x  dropped {}  repeated {}  kept {:.0}s ({:.0} MB)",
                began.elapsed().as_secs_f64(),
                s.fps,
                s.speed,
                s.dropped,
                s.repeated,
                kept,
                bytes as f64 / 1e6
            );
        }
    }
    buffer.stop();
    Ok(())
}

fn save(buffer: &ScreenBuffer, secs: u32, out: &std::path::Path) {
    let t = Instant::now();
    match buffer.save(secs, out) {
        Ok(p) => println!("saved {} in {:.1}s", p.display(), t.elapsed().as_secs_f64()),
        Err(e) => println!("couldn't save: {e}"),
    }
}
