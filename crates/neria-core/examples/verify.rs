//! Audio check scores against a track's reference preview:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example verify -- <spotify track id> [--no-spotify] <file|youtube url>...`
//!
//! `--no-spotify` ignores Spotify's own preview to exercise the Deezer / Apple fallbacks.

use std::time::Instant;

use neria_core::{fingerprint, spotify};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut track = spotify::track_details(&args[0])?;
    let no_spotify = args.iter().any(|a| a == "--no-spotify");
    if no_spotify {
        track.preview_url = None;
        track.id.clear(); // stop reference_for from looking it up again
    }
    let t = Instant::now();
    let reference = fingerprint::reference_for(&track).expect("no reference preview anywhere");
    println!(
        "{} - {} ({:.0}s) | reference: {} [{:.1}s]",
        track.artist_line(),
        track.title,
        track.duration_secs(),
        reference.source,
        t.elapsed().as_secs_f32()
    );
    let clip = fingerprint::reference_fingerprint(&reference)?;
    for input in args[1..].iter().filter(|a| !a.starts_with("--")) {
        let t = Instant::now();
        let a = if input.starts_with("http") {
            fingerprint::compare_candidate(&clip, input)?
        } else {
            fingerprint::compare_file(&clip, std::path::Path::new(input))?
        };
        println!(
            "  {:.3} {:?} at {:.1}s  [{:.1}s]  {}",
            a.similarity,
            fingerprint::verdict(a.similarity),
            a.offset_secs,
            t.elapsed().as_secs_f32(),
            input.rsplit(['\\', '/']).next().unwrap_or(input)
        );
    }
    Ok(())
}
