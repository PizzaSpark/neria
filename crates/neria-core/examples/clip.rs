//! Export check without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example clip -- <file> <start> <end> [fast|exact]`

use std::sync::atomic::AtomicBool;

use neria_core::clip::{self, ClipMode, ClipRequest};
use neria_core::media;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = std::path::PathBuf::from(&args[0]);
    let info = media::file_info(&input)?;
    println!("info: {info:?}");

    let req = ClipRequest {
        input,
        start: args[1].parse()?,
        end: args[2].parse()?,
        mode: if args.get(3).map(String::as_str) == Some("exact") { ClipMode::Exact } else { ClipMode::Fast },
        has_video: info.has_video,
    };
    let mut last = -1;
    let out = clip::export(&req, &AtomicBool::new(false), |p| {
        let pct = (p * 100.0) as i32;
        if pct / 25 != last / 25 {
            println!("{pct}%");
        }
        last = pct;
    })?;
    let check = media::file_info(&out)?;
    println!("out: {} ({:.2}s)", out.display(), check.duration);
    Ok(())
}
