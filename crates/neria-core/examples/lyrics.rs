//! What each lyrics source finds for Spotify songs:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example lyrics -- <spotify link>... [--only lrclib|spotify|genius] [--synced-only] [--show]`
//! `... --example lyrics -- --token-check`: reads Spotify's web-player secret and asks for an
//! anonymous token (proves the TOTP step without an account; no lyrics come back that way).
//!
//! An album/playlist link checks each of its songs. The Spotify source needs
//! `NERIA_HOME/cookies/spotify.com.txt` (Settings → Spotify → Sign in); the cookie and the
//! token are never printed.

use std::time::Instant;

use neria_core::lyrics::{self, Prefs, spotify_lyrics};
use neria_core::spotify;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--token-check") {
        let t = Instant::now();
        let (key, version) = spotify_lyrics::load_secret().map_err(anyhow::Error::msg)?;
        println!("web-player secret: version {version}, key {} bytes [{:.1}s]", key.len(), t.elapsed().as_secs_f32());
        let (token, expires, anonymous) = spotify_lyrics::token(&key, version, None).map_err(anyhow::Error::msg)?;
        println!("token: {} chars, anonymous {anonymous}, expires in {} min", token.len(), expires.saturating_sub(now_ms()) / 60_000);
        return Ok(());
    }
    let only = args.iter().position(|a| a == "--only").and_then(|i| args.get(i + 1)).cloned();
    let mut prefs = Prefs { spotify: true, ..Prefs::default() };
    if let Some(o) = &only {
        prefs = Prefs { lrclib: o == "lrclib", spotify: o == "spotify", genius: o == "genius", synced_only: false };
    }
    prefs.synced_only = args.iter().any(|a| a == "--synced-only");
    let show = args.iter().any(|a| a == "--show");
    if prefs.spotify && !spotify_lyrics::signed_in() {
        println!("(not signed in to Spotify: that source will say so)");
    }
    for link in args.iter().filter(|a| a.contains("spotify")) {
        let link = spotify::parse_link(link).ok_or_else(|| anyhow::anyhow!("not a Spotify link: {link}"))?;
        let col = spotify::fetch(&link, None)?;
        println!("{} · {} ({} songs)", col.kind.label(), col.name, col.tracks.len());
        let album = (col.kind == spotify::Kind::Album).then(|| col.name.clone());
        for t in &col.tracks {
            let started = Instant::now();
            let found = lyrics::fetch(t, t.album.as_deref().or(album.as_deref()), prefs);
            println!(
                "  {} - {}: {} [{:.1}s]",
                t.main_artist(),
                t.title,
                found.status(),
                started.elapsed().as_secs_f32()
            );
            if show && let Some(l) = &found.lyrics {
                let text = l.synced.clone().or(l.plain.clone()).unwrap_or_default();
                for line in text.lines().take(8) {
                    println!("      {line}");
                }
            }
        }
    }
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}
