//! Compare audio files with a Spotify track's 30 s preview:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example verify -- <spotify track id> <file|url>...`

use std::time::Instant;

use neria_core::{fingerprint, spotify};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let track = spotify::track_details(&args[0])?;
    let preview = track.preview_url.clone().expect("track has no preview");
    println!("{} - {} | preview {}", track.artist_line(), track.title, preview);
    for input in &args[1..] {
        let t = Instant::now();
        let a = fingerprint::compare_with_preview(&preview, std::path::Path::new(input))?;
        println!(
            "  {:.3} {:?} at {:.1}s  [{:.1}s]  {}",
            a.similarity,
            fingerprint::verdict(a.similarity),
            a.offset_secs,
            t.elapsed().as_secs_f32(),
            input
        );
    }
    Ok(())
}
