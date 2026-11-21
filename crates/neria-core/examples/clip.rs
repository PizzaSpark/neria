//! Export check without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example clip -- <file> <start> <end> [fast|exact] [sound…]`
//!
//! Sound (the Clip page's Sound dialog): `--mute`, `--gain <dB>`, `--even <LUFS>` (e.g. -14),
//! `--fade-in <s>`, `--fade-out <s>`, `--audio-only <mp3|m4a|opus|flac|wav>`.
//! Picture (Clip page → Picture): `--turn <1|2|3>` (clockwise quarter turns), `--flip-h`,
//! `--flip-v`, `--aspect <9:16|1:1|4:5|16:9>` (centred crop of the turned picture) or
//! `--crop <x,y,w,h>` (fractions 0..1).
//! `--in-place <file>`: apply the sound and picture to a whole file instead (a link clip).
//! Make a GIF: `--gif <gif|webp|mp4> [--width <px, 0 = as is>] [--fps <n, 0 = as is>] [--once]`
//! (prints the size estimate first, then makes it).
//! An instant replay's tracks: `--mix <n:vol|off,…>` (n counts from 1 after "Everything",
//! e.g. `2:off,3:1.5`), `--apart` keeps them as tracks of their own too.
//! The clip list: `--clips <a-b,c-d,…> [--names A,B,…] [--join]` instead of <start> <end>
//! (pass `0 0` for them).

use std::sync::atomic::AtomicBool;

use neria_core::clip::{self, ClipMode, ClipRequest};
use neria_core::picture::{Crop, Picture};
use neria_core::sound::{self, AudioEdit, Level};
use neria_core::{gif, media};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let has = |name: &str| args.iter().any(|a| a == name);
    let mut edit = AudioEdit::default();
    if has("--mute") {
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

    let target = flag("--in-place").unwrap_or_else(|| args[0].clone());
    let info = media::file_info(std::path::Path::new(&target))?;
    println!("info: {info:?}");
    let mut picture = Picture {
        turns: flag("--turn").map(|s| s.parse()).transpose()?.unwrap_or(0),
        flip_h: has("--flip-h"),
        flip_v: has("--flip-v"),
        crop: None,
    };
    // As it's shown: phone footage stored on its side is turned upright first.
    let (src_w, src_h) = if info.rotation % 180 == 90 { (info.height, info.width) } else { (info.width, info.height) };
    if let Some(r) = flag("--aspect") {
        let (a, b) = r.split_once(':').unwrap_or(("1", "1"));
        let (w, h) = if picture.turns % 2 == 1 { (src_h, src_w) } else { (src_w, src_h) };
        picture.crop = Some(Crop::centred(a.parse::<f64>()? / b.parse::<f64>()?, w as f64, h as f64));
    }
    if let Some(c) = flag("--crop") {
        let v: Vec<f64> = c.split(',').map(|x| x.parse()).collect::<Result<_, _>>()?;
        picture.crop = Some(Crop { x: v[0], y: v[1], w: v[2], h: v[3] });
    }
    if !picture.is_none() {
        let (w, h) = picture.output_size(src_w, src_h);
        println!("picture: {} -> {w}x{h} ({:?})", picture.summary(), picture.filter());
    }
    let report = |out: &std::path::Path| -> anyhow::Result<()> {
        let size = std::fs::metadata(out)?.len();
        let c = media::file_info(out)?;
        println!(
            "out: {} ({}, {:.2}s, {}x{}, {:.1} fps, video {}, audio {})",
            out.display(),
            neria_core::fmt::bytes(size as f64),
            c.duration,
            c.width,
            c.height,
            c.fps,
            c.has_video,
            c.has_audio
        );
        Ok(())
    };

    if flag("--in-place").is_some() {
        let out =
            sound::edit_in_place(std::path::Path::new(&target), &edit, &picture, &AtomicBool::new(false), progress)?;
        return report(&out);
    }

    let input = std::path::PathBuf::from(&args[0]);
    if let Some(kind) = flag("--gif") {
        let spec = gif::GifSpec {
            kind: match kind.as_str() {
                "webp" => gif::Kind::Webp,
                "mp4" => gif::Kind::Mp4,
                _ => gif::Kind::Gif,
            },
            width: flag("--width").map(|s| s.parse()).transpose()?.unwrap_or(480),
            fps: flag("--fps").map(|s| s.parse()).transpose()?.unwrap_or(15.0),
            loop_forever: !has("--once"),
            picture,
        };
        let (start, end): (f64, f64) = (args[1].parse()?, args[2].parse()?);
        let t = std::time::Instant::now();
        let guess = gif::estimate(&input, start, end, &spec, &AtomicBool::new(false))?;
        println!("estimate: {} ({} ms)", neria_core::fmt::bytes(guess as f64), t.elapsed().as_millis());
        let out = gif::make(&input, start, end, &spec, &AtomicBool::new(false), progress)?;
        return report(&out);
    }
    let mode = if args.get(3).map(String::as_str) == Some("exact") { ClipMode::Exact } else { ClipMode::Fast };
    let make = |start: f64, end: f64, name: Option<String>| {
        let mut req = ClipRequest::new(input.clone(), start, end, mode, info.has_video);
        req.audio = edit;
        req.audio_only = flag("--audio-only");
        req.picture = picture;
        req.name = name;
        if let Some(mix) = flag("--mix") {
            // Every track after the first ("Everything"), as it is unless named.
            req.tracks = (1..info.audio_tracks)
                .map(|i| neria_core::clip::TrackMix {
                    index: i,
                    name: info.audio_titles.get(i as usize).cloned().unwrap_or_default(),
                    on: true,
                    volume: 1.0,
                })
                .collect();
            for part in mix.split(',') {
                let (n, v) = part.split_once(':').expect("n:vol");
                let n: u32 = n.parse().expect("track number");
                if let Some(t) = req.tracks.iter_mut().find(|t| t.index == n) {
                    if v == "off" {
                        t.on = false;
                    } else {
                        t.volume = v.parse().expect("volume");
                    }
                }
            }
            req.tracks_apart = has("--apart");
        }
        req
    };
    if let Some(list) = flag("--clips") {
        let names: Vec<String> = flag("--names").map(|n| n.split(',').map(String::from).collect()).unwrap_or_default();
        let mut reqs = Vec::new();
        for (i, r) in list.split(',').enumerate() {
            let (a, b) = r.split_once('-').expect("a-b");
            reqs.push(make(a.parse()?, b.parse()?, names.get(i).cloned()));
        }
        let outs = clip::export_many(&reqs, has("--join"), &AtomicBool::new(false), progress)?;
        for o in outs {
            report(&o)?;
        }
        return Ok(());
    }
    let out = clip::export(&make(args[1].parse()?, args[2].parse()?, None), &AtomicBool::new(false), progress)?;
    report(&out)
}
