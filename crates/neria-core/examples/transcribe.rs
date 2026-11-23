//! Speech to text without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example transcribe -- <file>... [flags]`
//!
//! Several files are joined in order into one transcript (parts of one talk).
//! Flags:
//! - `--status`: what's installed, the drive space it takes, whether an NVIDIA card is seen
//! - `--get [cpu|gpu]`: download what's missing (the NVIDIA pack with `gpu`); no files needed
//! - `--remove`: take it all off the drive
//! - `--lang <code>`: `en`, `tl`… (default: detect)
//! - `--srt` (captions only) / `--both` (text and captions); default: text
//! - `--no-times`: no `[01:23]` before each line of the text
//! - `--translate`: into English, whatever is spoken
//! - `--burn`: also a copy of the video with the captions burned into the picture
//! - `--range <start> <end>`: only that part of the (one) file, in seconds
//! - `--out <dir>`: save there (default: next to the first file)

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use neria_core::fmt;
use neria_core::transcribe::{self, Output, Request, Spec, Stage};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let after = |f: &str, n: usize| args.iter().position(|a| a == f).and_then(|i| args.get(i + n)).cloned();

    if flag("--status") {
        let st = transcribe::status();
        println!(
            "engine: {} ({})  model: {}  ready: {}  on drive: {}  NVIDIA card: {}",
            st.engine,
            if st.gpu { "NVIDIA" } else { "CPU" },
            st.model,
            st.ready(),
            fmt::bytes(transcribe::disk_bytes() as f64),
            transcribe::has_nvidia()
        );
        return Ok(());
    }
    if flag("--remove") {
        transcribe::remove()?;
        println!("removed");
        return Ok(());
    }
    if flag("--get") {
        let gpu = after("--get", 1).as_deref() == Some("gpu");
        let mut last = String::new();
        transcribe::install(gpu, &AtomicBool::new(false), |p| {
            let t = format!("{} {:.0}%", p.text(), p.fraction() * 100.0);
            if t.split('·').next() != last.split('·').next() || p.fraction() >= 1.0 {
                println!("{t}");
            }
            last = t;
        })?;
        println!("ready: {}", transcribe::ready());
        return Ok(());
    }

    let mut skip = std::collections::HashSet::new();
    for (f, n) in [("--lang", 1), ("--range", 2), ("--out", 1)] {
        if let Some(i) = args.iter().position(|a| a == f) {
            (i..=i + n).for_each(|j| {
                skip.insert(j);
            });
        }
    }
    let inputs: Vec<PathBuf> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !skip.contains(i) && !a.starts_with("--"))
        .map(|(_, a)| a.into())
        .collect();
    let range = match (after("--range", 1), after("--range", 2)) {
        (Some(a), Some(b)) => Some((a.parse()?, b.parse()?)),
        _ => None,
    };
    let output = if flag("--both") {
        Output::Both
    } else if flag("--srt") {
        Output::Captions
    } else {
        Output::Text
    };
    let req = Request {
        inputs,
        range,
        spec: Spec {
            language: after("--lang", 1),
            output,
            times: !flag("--no-times"),
            translate: flag("--translate"),
            burn: flag("--burn"),
        },
        out_dir: after("--out", 1).map(PathBuf::from),
    };
    let t0 = Instant::now();
    let mut shown = -1i32;
    let done = transcribe::run(&req, &AtomicBool::new(false), |p| match p.stage {
        Stage::Preparing => println!("preparing the sound…"),
        Stage::Listening(line) if !line.is_empty() => println!("  {line}"),
        Stage::Listening(_) => {
            let pc = (p.fraction * 100.0) as i32;
            if pc / 10 != shown / 10 {
                println!("{pc}%");
                shown = pc;
            }
        }
    })?;
    println!(
        "done in {:.1}s · {} · {} lines",
        t0.elapsed().as_secs_f64(),
        transcribe::language_name(&done.language),
        done.segments
    );
    for f in done.files {
        println!("saved: {}", f.display());
    }
    Ok(())
}
