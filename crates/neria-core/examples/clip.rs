//! Export check without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example clip -- <file> <start> <end> [fast|exact] [sound…]`
//!
//! Sound (the Clip page's Sound dialog): `--mute`, `--gain <dB>`, `--even <LUFS>` (e.g. -14),
//! `--fade-in <s>`, `--fade-out <s>`, `--audio-only <mp3|m4a|opus|flac|wav>`.
//! `--in-place <file>`: apply the sound to a whole file instead (what a link clip gets).
//! Make a GIF: `--gif <gif|webp|mp4> [--width <px, 0 = as is>] [--fps <n, 0 = as is>] [--once]`
//! (prints the size estimate first, then makes it).

use std::sync::atomic::AtomicBool;

use neria_core::clip::{self, ClipMode, ClipRequest};
use neria_core::{gif, media};
use neria_core::sound::{self, AudioEdit, Level};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let mut edit = AudioEdit::default();
    if args.iter().any(|a| a == "--mute") {
        edit.level = Level::Mute;
    }
    if let Some(db) = flag("--gain") {
        edit.level = Level::Gain(db.parse()?);
    }
    if let Some(t) = flag("--even") {
        edit.level = Level::Normalize(t.parse()?);
    }
    edit.fade_in = flag("--fade-in").map(|s| s.parse()).transpose()?.unwrap_or(0.0);
    edit.fade_out = flag("--fade-out").map(|s| s.parse()).transpose()?.unwrap_or(0.0);
    println!("sound: {} ({edit:?})", edit.summary());
    let progress = {
        let mut last = -1;
        move |p: f32| {
            let pct = (p * 100.0) as i32;
            if pct / 25 != last / 25 {
                println!("{pct}%");
            }
            last = pct;
        }
    };

    if let Some(file) = flag("--in-place") {
        let file = std::path::PathBuf::from(file);
        sound::apply_in_place(&file, &edit, &AtomicBool::new(false), progress)?;
        println!("changed: {} ({:.2}s)", file.display(), media::file_info(&file)?.duration);
        return Ok(());
    }

    let input = std::path::PathBuf::from(&args[0]);
    let info = media::file_info(&input)?;
    println!("info: {info:?}");

    if let Some(kind) = flag("--gif") {
        let spec = gif::GifSpec {
            kind: match kind.as_str() {
                "webp" => gif::Kind::Webp,
                "mp4" => gif::Kind::Mp4,
                _ => gif::Kind::Gif,
            },
            width: flag("--width").map(|s| s.parse()).transpose()?.unwrap_or(480),
            fps: flag("--fps").map(|s| s.parse()).transpose()?.unwrap_or(15.0),
            loop_forever: !args.iter().any(|a| a == "--once"),
        };
        let (start, end): (f64, f64) = (args[1].parse()?, args[2].parse()?);
        let t = std::time::Instant::now();
        let guess = gif::estimate(&input, start, end, &spec, &AtomicBool::new(false))?;
        println!("estimate: {} ({} ms)", neria_core::fmt::bytes(guess as f64), t.elapsed().as_millis());
        let out = gif::make(&input, start, end, &spec, &AtomicBool::new(false), progress)?;
        let size = std::fs::metadata(&out)?.len();
        let check = media::file_info(&out)?;
        println!(
            "out: {} ({}, {:.2}s, {}x{}, {:.1} fps)",
            out.display(),
            neria_core::fmt::bytes(size as f64),
            check.duration,
            check.width,
            check.height,
            check.fps
        );
        return Ok(());
    }
    let mode = if args.get(3).map(String::as_str) == Some("exact") { ClipMode::Exact } else { ClipMode::Fast };
    let mut req = ClipRequest::new(input, args[1].parse()?, args[2].parse()?, mode, info.has_video);
    req.audio = edit;
    req.audio_only = flag("--audio-only");
    let out = clip::export(&req, &AtomicBool::new(false), progress)?;
    let check = media::file_info(&out)?;
    println!("out: {} ({:.2}s, video {}, audio {})", out.display(), check.duration, check.has_video, check.has_audio);
    Ok(())
}
