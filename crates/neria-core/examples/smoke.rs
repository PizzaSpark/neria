//! End-to-end check without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example smoke -- <url> [audio|video] [out_dir] [start end [exact]]`
//!
//! The Download page's picks, as environment variables (single videos):
//! - `NERIA_PRESET=best|compatible|smallest|audio`
//! - `NERIA_CONTAINER=mp4|mkv|webm`, `NERIA_TEMPLATE=<yt-dlp -o template>`
//! - `NERIA_CAPTIONS=en,es` (creator-made), `NERIA_CAPTIONS_AUTO=en` (auto-generated),
//!   `NERIA_SUB_FORMAT=srt|vtt|ass|text`, `NERIA_SUB_MODE=separate|embed|both`, `NERIA_SUBS_ONLY=1`
//! - `NERIA_AUDIO_LANGS=en,es` (audio tracks; with audio + several: one file each)
//! - `NERIA_CHAPTERS=2,3,5` (1-based): only those parts, joined into one file
//! - `NERIA_SOUND=even:-14,fade-in:1,fade-out:2` (or `gain:6`, `mute`): the Clip page's
//!   Sound, changed after the download as a link clip gets it
//! - `NERIA_GIF=gif|webp|mp4`: Make a GIF from the link clip after the download
//! - `NERIA_TRANSCRIPT=text|srt|both`: a transcript after the download (speech to text
//!   must be set up: `--example transcribe -- --get`)

use std::sync::{Arc, Mutex};
use std::time::Duration;

use neria_core::download::DownloadRequest;
use neria_core::queue::{JobState, Queue};
use neria_core::settings::{Preset, Quality, Settings};
use neria_core::tracks::{CaptionChoice, SubFormat, SubMode};
use neria_core::{fmt, probe, tracks};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

fn list(k: &str) -> Vec<String> {
    env(k).map(|v| v.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default()
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("usage: smoke <url> [audio] [out_dir]");
    let mut quality = if args.next().as_deref() == Some("audio") { Quality::Audio } else { Quality::P720 };
    let preset = match env("NERIA_PRESET").as_deref() {
        Some("compatible") => Preset::Compatible,
        Some("smallest") => Preset::Smallest,
        Some("audio") => Preset::Audio,
        _ => Preset::Best,
    };
    if preset == Preset::Audio {
        quality = Quality::Audio;
    }

    let mut settings = Settings::default();
    if let Some(c) = env("NERIA_CONTAINER") {
        settings.container = c;
    }
    if let Some(t) = env("NERIA_TEMPLATE") {
        settings.filename_template = t;
    }
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
        req.preset = preset;
        req.key = info.key.clone();
        req.section = section;
        req.exact_cuts = exact_cuts;
        // `NERIA_SOUND=even:-14,fade-out:1` / `gain:6` / `mute`: a link clip's Sound dialog.
        if let Some(spec) = env("NERIA_SOUND") {
            let mut e = neria_core::sound::AudioEdit::default();
            for part in spec.split(',') {
                let (k, v) = part.split_once(':').unwrap_or((part, ""));
                let n = v.parse::<f64>().unwrap_or(0.0);
                match k.trim() {
                    "mute" => e.level = neria_core::sound::Level::Mute,
                    "gain" => e.level = neria_core::sound::Level::Gain(n as f32),
                    "even" => e.level = neria_core::sound::Level::Normalize(n as f32),
                    "fade-in" => e.fade_in = n,
                    "fade-out" => e.fade_out = n,
                    _ => {}
                }
            }
            println!("sound: {}", e.summary());
            req.audio_edit = Some(e);
        }
        // `NERIA_ASPECT=9:16` (and `NERIA_TURN=1`): the Clip page's Picture, a centred crop of
        // a 16:9 source, done after the download.
        if env("NERIA_ASPECT").is_some() || env("NERIA_TURN").is_some() {
            let turns: u8 = env("NERIA_TURN").and_then(|t| t.parse().ok()).unwrap_or(0);
            let (w, h) = if turns % 2 == 1 { (9.0, 16.0) } else { (16.0, 9.0) };
            let crop = env("NERIA_ASPECT").and_then(|r| {
                let (a, b) = r.split_once(':')?;
                Some(neria_core::picture::Crop::centred(a.parse::<f64>().ok()? / b.parse::<f64>().ok()?, w, h))
            });
            req.picture = Some(neria_core::picture::Picture { turns, crop, ..Default::default() });
        }
        // `NERIA_GIF=gif|webp|mp4`: Make a GIF from a link clip (480 px, 15 fps, looping).
        if let Some(kind) = env("NERIA_GIF") {
            req.gif_spec = Some(neria_core::gif::GifSpec {
                kind: match kind.as_str() {
                    "webp" => neria_core::gif::Kind::Webp,
                    "mp4" => neria_core::gif::Kind::Mp4,
                    _ => neria_core::gif::Kind::Gif,
                },
                ..Default::default()
            });
        }
        // `NERIA_TRANSCRIPT=text|srt|both`: Extras → Transcript.
        if let Some(kind) = env("NERIA_TRANSCRIPT") {
            req.transcribe = Some(neria_core::transcribe::Spec {
                output: match kind.as_str() {
                    "srt" => neria_core::transcribe::Output::Captions,
                    "both" => neria_core::transcribe::Output::Both,
                    _ => neria_core::transcribe::Output::Text,
                },
                ..Default::default()
            });
        }
        let (langs, auto_langs) = (list("NERIA_CAPTIONS"), list("NERIA_CAPTIONS_AUTO"));
        if !langs.is_empty() || !auto_langs.is_empty() {
            req.captions = Some(CaptionChoice {
                langs,
                auto_langs,
                format: match env("NERIA_SUB_FORMAT").as_deref() {
                    Some("vtt") => SubFormat::Vtt,
                    Some("ass") => SubFormat::Ass,
                    Some("text") => SubFormat::Txt,
                    _ => SubFormat::Srt,
                },
                mode: match env("NERIA_SUB_MODE").as_deref() {
                    Some("embed") => SubMode::Embed,
                    Some("both") => SubMode::Both,
                    _ => SubMode::Separate,
                },
                only: env("NERIA_SUBS_ONLY").is_some(),
            });
        }
        println!(
            "captions: {}",
            info.captions
                .iter()
                .map(|c| format!("{}{}", c.lang, if c.auto { "(auto)" } else { "" }))
                .take(12)
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!("audio tracks: {}", info.audio_tracks.iter().map(|t| t.lang.clone()).collect::<Vec<_>>().join(" "));
        println!("chapters: {}", info.chapters.len());
        let picked: Vec<usize> =
            list("NERIA_CHAPTERS").iter().filter_map(|n| n.parse::<usize>().ok()?.checked_sub(1)).collect();
        if !picked.is_empty() {
            match tracks::ranges(&info.chapters, &picked).as_slice() {
                [one] => {
                    req.section = Some(*one);
                    req.exact_cuts = true;
                }
                many => {
                    req.sections = many.to_vec();
                    req.name_tag = Some(" (chapters)".into());
                }
            }
        }
        let audio = list("NERIA_AUDIO_LANGS");
        if quality == Quality::Audio && audio.len() > 1 {
            audio
                .iter()
                .map(|lang| {
                    let mut r = req.clone();
                    r.audio_langs = vec![lang.clone()];
                    r.name_tag = Some(format!(" ({})", tracks::language_name(lang)));
                    r
                })
                .collect()
        } else {
            req.audio_langs = audio;
            vec![req]
        }
    } else {
        // `NERIA_PICK=1,2:audio,3:480` - an item can carry its own quality.
        let picked: Vec<neria_core::post::Pick> = match std::env::var("NERIA_PICK") {
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
                    Some((n.parse::<usize>().ok()?.checked_sub(1)?, q, None))
                })
                .collect(),
            Err(_) => (0..info.items.len()).map(|i| (i, None, None)).collect(),
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
    while let Ok(s) = rx.recv_timeout(Duration::from_secs(300)) {
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
