//! Compression check without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example compress -- <file> <mb|q0.0-1.0> [h264|hevc|av1] [cpu|gpu]`

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use neria_core::compress::{self, Codec, Encoders, Options, Resolution, Target};
use neria_core::{fmt, media};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = std::path::PathBuf::from(&args[0]);
    let target = match args[1].strip_prefix('q') {
        Some(q) => Target::Quality(q.parse()?),
        None => Target::SizeMb(args[1].parse()?),
    };
    let codec = match args.get(2).map(String::as_str) {
        Some("hevc") => Codec::Hevc,
        Some("av1") => Codec::Av1,
        _ => Codec::H264,
    };
    let use_gpu = args.get(3).map(String::as_str) != Some("cpu");

    let t = Instant::now();
    let encoders = Encoders::detect();
    println!("gpu encoders: {:?} ({:.1}s)", encoders.gpu, t.elapsed().as_secs_f32());

    let info = media::file_info(&input)?;
    println!(
        "input: {}x{} {:.2}fps {:.1}s {}",
        info.width,
        info.height,
        info.fps,
        info.duration,
        fmt::bytes(info.size_bytes as f64)
    );
    let opts = Options { target, codec, use_gpu, resolution: Resolution::Auto, max_fps: None, audio_kbps: Some(128) };
    println!("plan: {}", compress::plan(&info, &opts, &encoders)?.summary());

    let t = Instant::now();
    let mut last = String::new();
    let out = compress::run(&input, &info, &opts, &encoders, &AtomicBool::new(false), |p| {
        if p.stage != last {
            println!("  {}", p.stage);
            last = p.stage.clone();
        }
    })?;
    println!(
        "out: {} = {} bytes ({}) in {:.1}s",
        out.path.file_name().unwrap().to_string_lossy(),
        out.bytes,
        fmt::bytes(out.bytes as f64),
        t.elapsed().as_secs_f32()
    );
    Ok(())
}
