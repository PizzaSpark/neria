//! Live chat without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example chat -- <link> <mode> [flags]`
//!
//! Modes:
//! - `live <seconds>`: read a live stream's chat that long, print each message
//! - `also`: whether the same stream is live on the other site now (a multi-stream), and where
//! - `replay [out.chat.json]`: a past stream's whole chat (YouTube replay, Twitch past broadcast)
//! - `draw <file.chat.json> <out.png> [--width 420] [--height 900] [--at <s>] [--overlay] [--light]`:
//!   the burned-in look of the chat at second `at` (the link is ignored, pass `-`)
//! - `into <video> <file.chat.json> <out> [--from a] [--to b] [--track] [--overlay] [--light]
//!   [--highlights] [--no-bots] [--words a,b]`: put that chat into a video (burned in, side
//!   panel unless `--overlay`; `--track` as a track you turn on)

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use neria_core::chat::{self, ChatFile, Filter, LiveChat, Which};
use neria_core::{chat_draw, chat_out};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let after = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let num = |f: &str| after(f).and_then(|v| v.parse::<f64>().ok());
    let (Some(url), Some(mode)) = (args.first(), args.get(1)) else {
        eprintln!("usage: chat <link> live <seconds> | replay [out] | draw … | into …");
        std::process::exit(2);
    };
    let look = chat_draw::Look { light: flag("--light"), overlay: flag("--overlay"), ..chat_draw::Look::default() };
    match mode.as_str() {
        "also" => {
            let t = Instant::now();
            println!("{:?} ({:.1} s)", chat::also_live(url), t.elapsed().as_secs_f64());
        }
        "live" => {
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20);
            let live = LiveChat::start(url, None).ok_or_else(|| anyhow::anyhow!("not a YouTube or Twitch link"))?;
            let end = Instant::now() + Duration::from_secs(secs);
            let mut shown = 0;
            let mut last_state = None;
            while Instant::now() < end {
                let st = live.state();
                if last_state.as_ref() != Some(&st) {
                    println!("-- {st:?}");
                    last_state = Some(st.clone());
                }
                let msgs = live.messages();
                for m in &msgs[shown.min(msgs.len())..] {
                    print_msg(m);
                }
                shown = msgs.len();
                if matches!(st, chat::LiveState::Off(_)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            println!("-- {} messages from {}", live.len(), live.channel());
        }
        "replay" => {
            let cancel = AtomicBool::new(false);
            let t0 = Instant::now();
            let f = chat::fetch_replay(url, None, &cancel, &mut |n, frac| {
                eprint!("\r{n} messages {}", frac.map(|f| format!("{:.0}%", f * 100.0)).unwrap_or_default());
            })?;
            eprintln!();
            for m in f.messages.iter().take(15) {
                print_msg(m);
            }
            let kinds = f.messages.iter().filter(|m| m.kind != chat::Kind::Chat).count();
            let emotes =
                f.messages.iter().filter(|m| m.parts.iter().any(|p| matches!(p, chat::Part::Emote { .. }))).count();
            println!(
                "-- {} messages ({kinds} paid/member/notice, {emotes} with pictures), last at {:.0}s, in {:.1}s",
                f.messages.len(),
                f.messages.last().map(|m| m.t).unwrap_or(0.0),
                t0.elapsed().as_secs_f64()
            );
            if let Some(out) = args.get(2) {
                f.save(&PathBuf::from(out))?;
                println!("saved {out}");
            }
        }
        "draw" => {
            let f = ChatFile::load(&PathBuf::from(&args[2]))?;
            let out = PathBuf::from(&args[3]);
            let (w, h) = (num("--width").unwrap_or(420.0) as u32, num("--height").unwrap_or(900.0) as u32);
            let at = num("--at").unwrap_or_else(|| f.messages.last().map(|m| m.t).unwrap_or(0.0));
            let fonts = chat_draw::Fonts::load();
            let mut pics = chat_draw::Pictures::fetching();
            let shown: Vec<_> = f.messages.iter().filter(|m| m.t <= at).cloned().collect();
            let px = chat_draw::panel(&fonts, &mut pics, &shown, at, w, h, &look, h as f32 / 46.0);
            px.save_png(&out)?;
            println!("drew {} messages up to {at:.0}s into {}", shown.len(), out.display());
        }
        "into" => {
            let video = PathBuf::from(&args[2]);
            let f = ChatFile::load(&PathBuf::from(&args[3]))?;
            let out = PathBuf::from(&args[4]);
            let from = num("--from").unwrap_or(0.0);
            let to = num("--to").unwrap_or(f64::MAX);
            let filter = Filter {
                which: if flag("--highlights") { Which::Highlights } else { Which::All },
                no_bots: flag("--no-bots"),
                words: after("--words").map(|w| w.split(',').map(String::from).collect()).unwrap_or_default(),
                ..Filter::default()
            };
            let msgs = chat::for_clip(&f.messages, from, to, &filter);
            let how = if flag("--track") {
                chat_out::How::Track
            } else if flag("--overlay") {
                chat_out::How::Overlay
            } else {
                chat_out::How::Panel
            };
            let spec = chat_out::Spec { how, light: flag("--light"), emotes: !flag("--no-pictures") };
            let cancel = AtomicBool::new(false);
            let t0 = Instant::now();
            chat_out::put_into(&video, &msgs, &spec, &out, &cancel, &mut |p| eprint!("\r{:.0}%", p * 100.0))?;
            eprintln!();
            println!("{} messages into {} in {:.1}s", msgs.len(), out.display(), t0.elapsed().as_secs_f64());
        }
        _ => anyhow::bail!("unknown mode {mode}"),
    }
    Ok(())
}

fn print_msg(m: &chat::Message) {
    let extra = match (&m.amount, &m.note) {
        (Some(a), _) => format!(" [{a}]"),
        (_, Some(n)) => format!(" [{n}]"),
        _ => String::new(),
    };
    let badges = if m.badges.is_empty() { String::new() } else { format!(" ({})", m.badges.join(",")) };
    println!("{:>12.1} {} {}{badges}{extra}: {}", m.t, m.site.mark(), m.name, m.text());
}
