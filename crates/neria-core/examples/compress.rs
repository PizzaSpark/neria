//! Compression check without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example compress -- <file> <goal> [h264|hevc|av1] [cpu|gpu] [flags]`
//!
//! `<goal>`: `discord`, `email`, `whatsapp`, `sharp`, `tiny`, a size in MB (`7.5`) or `q0.0-1.0`.
//! Flags:
//! - `--range <start> <end>`: only that part (seconds)
//! - `--parts 10-20,60-75`: those parts joined into one file (the goal is for it); add
//!   `--each` for one file per part (each fits the goal on its own)
//! - `--even <LUFS>`: even out the sound (-14 online video, -16 podcast), measured first
//! - `--first-only`: the first sound track only, no captions, no chapters (default: keep them)
//! - `--bars`: look for black bars and cut them
//! - `--preview`: no encode; the size estimate and the before/after frames (paths printed)
//! - `--sample <at> <secs>`: no encode; a sample to watch (path printed)

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use neria_core::compress::{self, Codec, Encoders, Goal, Options, Target};
use neria_core::{fmt, media};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = std::path::PathBuf::from(&args[0]);
    let target = match args[1].as_str() {
        "discord" => Goal::Discord.target(),
        "email" => Goal::Email.target(),
        "whatsapp" => Goal::WhatsApp.target(),
        "sharp" => Goal::Sharp.target(),
        "tiny" => Goal::Tiny.target(),
        g => match g.strip_prefix('q') {
            Some(q) => Target::Quality(q.parse()?),
            None => Target::SizeMb(g.parse()?),
        },
    };
    let codec = match args.get(2).map(String::as_str) {
        Some("hevc") => Codec::Hevc,
        Some("av1") => Codec::Av1,
        _ => Codec::H264,
    };
    let use_gpu = args.get(3).map(String::as_str) != Some("cpu");
    let flag = |f: &str| args.iter().position(|a| a == f);
    let num = |i: usize| -> anyhow::Result<f64> {
        Ok(args.get(i).ok_or_else(|| anyhow::anyhow!("missing number"))?.parse()?)
    };

    let t = Instant::now();
    let encoders = Encoders::detect();
    println!("gpu encoders: {:?} ({:.1}s)", encoders.gpu, t.elapsed().as_secs_f32());

    let info = media::file_info(&input)?;
    println!(
        "input: {}x{} turned {} {:.2}fps {:.1}s {} · sound tracks {} · captions {:?} · chapters {} · interlaced {}",
        info.width,
        info.height,
        info.rotation,
        info.fps,
        info.duration,
        fmt::mb(info.size_bytes as f64),
        info.audio_tracks,
        info.subtitles,
        info.chapters.len(),
        info.interlaced
    );
    let mut opts = Options { target, codec, use_gpu, ..Options::for_goal(Goal::Discord) };
    opts.keep_tracks = flag("--first-only").is_none();
    if let Some(i) = flag("--range") {
        opts.range = Some((num(i + 1)?, num(i + 2)?));
    }
    if let Some(i) = flag("--even") {
        opts.even = Some(num(i + 1)? as f32);
    }
    // `--parts 10-20,60-75`: joined into one file, or with `--each` one file per part.
    let mut each = Vec::new();
    if let Some(i) = flag("--parts") {
        let parts: Vec<(f64, f64)> = args
            .get(i + 1)
            .map(String::as_str)
            .unwrap_or("")
            .split(',')
            .filter_map(|p| p.split_once('-'))
            .filter_map(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
            .collect();
        if flag("--each").is_some() {
            each = parts;
        } else {
            opts.parts = parts;
        }
    }
    if !each.is_empty() {
        let cancel = AtomicBool::new(false);
        for (n, part) in each.into_iter().enumerate() {
            let o = Options { range: Some(part), part_no: Some(n + 1), ..opts.clone() };
            let out = compress::run(&input, &info, &o, &encoders, &cancel, |_| {})?;
            println!("part {}: {} ({}) · {}", n + 1, out.path.display(), fmt::mb(out.bytes as f64), out.plan.summary());
        }
        return Ok(());
    }
    if flag("--bars").is_some() {
        let t = Instant::now();
        opts.crop = compress::detect_bars(&input, &info);
        println!("black bars: {:?} ({:.1}s)", opts.crop, t.elapsed().as_secs_f32());
    }
    match compress::plan(&info, &opts, &encoders) {
        Ok(plan) => {
            println!("plan: {}", plan.summary());
            println!("hint: {}", plan.hint());
            println!(
                "tracks: {} sound · captions {:?} ({} left out) · chapters {} · filters {:?}",
                plan.audio_tracks, plan.captions, plan.captions_dropped, plan.chapters, plan.filters
            );
        }
        Err(e) => println!("plan: {e}"),
    }
    if let Some(secs) = compress::longest_fit(&info, &opts, &encoders) {
        println!("longest part that fits: {}", fmt::duration(secs));
    }

    let cancel = AtomicBool::new(false);
    if flag("--preview").is_some() {
        let t = Instant::now();
        let p = compress::preview(&input, &info, &opts, &encoders, "example", 540, &cancel)?;
        println!(
            "preview: {}{} · before {} · after {} ({:.1}s)",
            if p.estimated { "about " } else { "" },
            fmt::mb(p.bytes as f64),
            p.before.display(),
            p.after.display(),
            t.elapsed().as_secs_f32()
        );
        return Ok(());
    }
    if let Some(i) = flag("--sample") {
        let plan = compress::plan(&info, &opts, &encoders)?;
        let out = neria_core::paths::cache_dir().join("compress-sample-example.mp4");
        let t = Instant::now();
        let s = compress::sample(&input, &info, &plan, num(i + 1)?, num(i + 2)?, &out, &cancel)?;
        println!("sample: {} · {} ({:.1}s)", s.path.display(), fmt::mb(s.bytes as f64), t.elapsed().as_secs_f32());
        return Ok(());
    }

    let t = Instant::now();
    let mut last = String::new();
    let out = compress::run(&input, &info, &opts, &encoders, &cancel, |p| {
        if p.stage != last {
            println!("  {}", p.stage);
            last = p.stage.clone();
        }
    })?;
    println!(
        "out: {} = {} bytes ({}) in {:.1}s",
        out.path.file_name().unwrap().to_string_lossy(),
        out.bytes,
        fmt::mb(out.bytes as f64),
        t.elapsed().as_secs_f32()
    );
    let o = media::file_info(&out.path)?;
    println!(
        "result: {}x{} turned {} {:.2}fps {:.1}s · sound tracks {} · captions {:?} · chapters {}",
        o.width,
        o.height,
        o.rotation,
        o.fps,
        o.duration,
        o.audio_tracks,
        o.subtitles,
        o.chapters.len()
    );
    Ok(())
}
