//! Saved workflows without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example workflow -- <file> <change> [--put beside|replace|<dir>]`
//!
//! `<change>`: `discord`, `email`, `whatsapp`, a size in MB (`7.5`), `mp3` / `m4a` / `opus` /
//! `flac` / `wav` (Sound only), `gif`, or `keep` (only moved: give `--put <dir>`).
//! Prints what the queue row would say and where the result went.
//!
//! `tool`: your own tool, `--program <exe or name> --words "<its words with {file} {out}…>"
//! [--makes <ext>] [--takes videos|sound|both]` (saved to NERIA_HOME's `tools.json`, then
//! run once; a failure prints the report).
//!
//! `--list`: the saved workflows (NERIA_HOME's `workflows.json`; the three starters if none),
//! each with its sentence, and which one a link would get (`--link <url>`).
//! `--watch <dir> <seconds> <change>`: watch a folder (files already there are left alone)
//! and run the change on each file that lands in it, results in its Done folder.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use neria_core::workflow::{self, Change, Put, Size, SoundAs, When, Workflow};

fn change_of(s: &str) -> Change {
    match s {
        "discord" => Change::Smaller(Size::Discord),
        "email" => Change::Smaller(Size::Email),
        "whatsapp" => Change::Smaller(Size::WhatsApp),
        "mp3" => Change::Sound(SoundAs::Mp3),
        "m4a" => Change::Sound(SoundAs::M4a),
        "opus" => Change::Sound(SoundAs::Opus),
        "flac" => Change::Sound(SoundAs::Flac),
        "wav" => Change::Sound(SoundAs::Wav),
        "gif" => Change::Gif,
        "keep" => Change::Keep,
        mb => Change::Smaller(Size::Mb(mb.parse().expect("a change or a size in MB"))),
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();

    if args.iter().any(|a| a == "--list") {
        for w in workflow::list() {
            println!("{:>3}  {:<16} [{}]  {}", w.id, w.name, w.when_label(), w.sentence());
        }
        if let Some(url) = flag("--link") {
            match workflow::auto_for(&url) {
                Some(w) => println!("\n{url} gets {}", w.name),
                None => println!("\n{url} gets none"),
            }
        }
        return Ok(());
    }

    if let Some(i) = args.iter().position(|a| a == "--watch") {
        let dir = PathBuf::from(&args[i + 1]);
        let secs: u64 = args[i + 2].parse()?;
        let wf = Workflow {
            name: "Watch check".into(),
            change: change_of(&args[i + 3]),
            when: When::Folder(dir.clone()),
            ..Workflow::default()
        };
        let id = workflow::save(wf);
        println!(
            "watching {} for {secs} s (workflow {id}); left alone: {}",
            dir.display(),
            workflow::waiting_in(&dir).len()
        );
        let mut watcher = workflow::Watcher::default();
        let end = Instant::now() + Duration::from_secs(secs);
        let cancel = AtomicBool::new(false);
        while Instant::now() < end {
            std::thread::sleep(Duration::from_secs(3));
            for (wf, file) in watcher.look() {
                println!("took {}", file.display());
                let done_dir = workflow::done_dir(&dir);
                match workflow::run(&wf, &file, Some(&done_dir), &cancel, |_, _| {}) {
                    Ok(d) => println!("  {}: {} → {}", wf.name, d.said, d.file.display()),
                    Err(e) => println!("  failed: {e}"),
                }
            }
        }
        workflow::delete(id);
        return Ok(());
    }

    let file = PathBuf::from(&args[0]);
    let change = if args[1] == "tool" {
        let t = neria_core::tool::Tool {
            name: flag("--name").unwrap_or_else(|| "My tool".into()),
            program: flag("--program").expect("--program"),
            words: flag("--words").expect("--words"),
            makes: flag("--makes").unwrap_or_default(),
            takes: match flag("--takes").as_deref() {
                Some("sound") => neria_core::tool::Takes::Sound,
                Some("both") => neria_core::tool::Takes::Both,
                _ => neria_core::tool::Takes::Videos,
            },
            ..Default::default()
        };
        if let Some(p) = t.problem() {
            println!("problem: {p}");
        }
        println!("it'll run: {}", neria_core::tool::sample(&t));
        Change::Tool(neria_core::tool::save(t))
    } else {
        change_of(&args[1])
    };
    let put = match flag("--put").as_deref() {
        None | Some("beside") => Put::Beside,
        Some("replace") => Put::Replace,
        Some(dir) => Put::Folder(PathBuf::from(dir)),
    };
    let wf = Workflow { name: "Check".into(), change, put, ..Workflow::default() };
    println!("{}", wf.sentence());
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let mut last = String::new();
    let done = workflow::run(&wf, &file, None, &cancel, |f, stage| {
        if stage != last {
            println!("  {stage}");
            last = stage.to_string();
        }
        let _ = f;
    })
    .inspect_err(|e| {
        if let Some(r) = neria_core::tool::report(e) {
            println!(
                "--- report ---
{r}"
            );
        }
    })?;
    println!(
        "{}: {} (made: {}, replaced: {}, moved: {:?}) in {:.1} s\n  → {}",
        wf.name,
        done.said,
        done.made,
        done.replaced,
        done.moved,
        started.elapsed().as_secs_f32(),
        done.file.display()
    );
    Ok(())
}
