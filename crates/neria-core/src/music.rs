//! Spotify → music file: naming, resume, matching, tags, cover art, lyrics,
//! M3U playlists and the folder manifest used for sync.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::spotify::{Collection, Kind, Track};
use crate::ytmusic::{self, Candidate};
use crate::lyrics;
use crate::{paths, probe, proc};

/// Matches scoring below this are downloaded but flagged for the user to check
/// (e.g. only a re-recording exists on YouTube Music).
pub const CONFIDENT: f64 = 0.8;

pub const AUDIO_EXTS: &[&str] = &["mp3", "m4a", "opus", "ogg", "flac", "wav", "webm", "aac"];

/// Everything a queue job needs to turn one Spotify track into a tagged file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MusicJob {
    pub track: Track,
    pub dir: PathBuf,
    pub stem: String,
    pub lyrics: bool,
    /// Which lyrics sources to ask (Settings → Spotify).
    #[serde(default)]
    pub lyrics_prefs: crate::lyrics::Prefs,
    /// What to make: the song (default), or only its lyrics.
    #[serde(default)]
    pub mode: Mode,
    /// The user picked this YouTube Music URL ("pick another match"):
    /// skip matching and overwrite the existing file.
    pub forced: Option<String>,
    /// Check the download against Spotify's 30 s preview (and retry on mismatch).
    pub verify: bool,
}

/// What a Spotify job makes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// The song: matched on YouTube Music, downloaded, tagged, with lyrics if on.
    #[default]
    Song,
    /// "Lyrics only": `.lrc` / `.txt` named like the song would be, no audio.
    LyricsOnly,
    /// "Add lyrics to songs I already have": the song is `dir/stem.<ext>` already; add
    /// its lyrics inside it and next to it.
    AddLyrics,
}

/// Remove characters Windows forbids in file names and tidy the result.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    let cleaned: String = cleaned.chars().take(150).collect();
    if cleaned.is_empty() { "Untitled".into() } else { cleaned }
}

/// `Artist - Title`, with a `01 ` prefix for album tracks so they sort in order.
pub fn file_stem(track: &Track, kind: Kind) -> String {
    let base = format!("{} - {}", track.main_artist(), track.title);
    match (kind, track.track_number) {
        (Kind::Album, Some(n)) => sanitize(&format!("{n:02} {base}")),
        _ => sanitize(&base),
    }
}

/// Folder for a collection: the Audio folder itself for single songs, else a folder per
/// album/playlist inside it. Albums/playlists synced before the Audio folder existed keep
/// their old place (straight in the output folder), so resume and sync still find them.
pub fn collection_dir(settings: &crate::settings::Settings, col: &Collection) -> PathBuf {
    let audio = crate::folders::for_kind(settings, crate::folders::Kind::Audio);
    match col.kind {
        Kind::Track => audio,
        _ => {
            let name = sanitize(&col.name);
            let old = settings.output_dir.join(&name);
            if old.join(MANIFEST).is_file() { old } else { audio.join(name) }
        }
    }
}

/// An already-downloaded file for this stem, in any audio format.
pub fn existing_file(dir: &Path, stem: &str) -> Option<PathBuf> {
    AUDIO_EXTS.iter().map(|e| dir.join(format!("{stem}.{e}"))).find(|p| p.is_file())
}

/// All candidates (best first) and the one to download, if any is good enough.
pub fn find_match(job: &MusicJob) -> Result<(Vec<Candidate>, Option<Candidate>)> {
    let all = ytmusic::candidates(&job.track)?;
    let best = all.first().filter(|c| c.score >= ytmusic::MIN_SCORE).cloned();
    Ok((all, best))
}

// ───────────── Tagging ─────────────

/// Read the album name yt-dlp embedded from YouTube Music (used when Spotify had none).
fn embedded_album(file: &Path) -> Option<String> {
    let out = proc::command(paths::tool("ffprobe"))
        .args(["-v", "error", "-show_entries", "format_tags=album", "-of", "default=nw=1:nk=1"])
        .arg(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let a = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!a.is_empty()).then_some(a)
}

/// Tag `file` with Spotify metadata, cover art and lyrics (+ a `.lrc` next to it). Returns
/// what the row says about the lyrics (None when lyrics are off).
pub fn finish(file: &Path, job: &MusicJob) -> Result<Option<String>> {
    let mut track = job.track.clone();
    // Playlist tracks come without cover/date: one cheap embed lookup fills them in.
    if (track.cover_url.is_none() || track.release_date.is_none())
        && !track.id.is_empty()
        && let Ok(d) = crate::spotify::track_details(&track.id)
    {
        track.cover_url = track.cover_url.or(d.cover_url);
        track.release_date = track.release_date.or(d.release_date);
    }
    let album = track.album.clone().or_else(|| embedded_album(file));
    let cover = track.cover_url.as_deref().and_then(|u| probe::fetch_thumbnail(u).ok());
    let found = job.lyrics.then(|| lyrics::fetch(&track, album.as_deref(), job.lyrics_prefs));
    let lyrics = found.as_ref().and_then(|f| f.lyrics.as_ref());

    if let Some(synced) = lyrics.and_then(|l| l.synced.as_ref()) {
        let _ = std::fs::write(file.with_extension("lrc"), synced);
    }
    write_tags(file, &track, album.as_deref(), cover.as_deref(), lyrics)?;
    Ok(found.map(|f| f.status()))
}

fn write_tags(
    file: &Path,
    t: &Track,
    album: Option<&str>,
    cover: Option<&Path>,
    lyrics: Option<&lyrics::Lyrics>,
) -> Result<()> {
    let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    // ffmpeg can attach cover art to these containers; others just get text tags.
    let cover = cover.filter(|_| matches!(ext.as_str(), "mp3" | "m4a" | "flac"));
    let tmp = file.with_extension(format!("tagging.{ext}"));

    let mut a: Vec<String> = ["-hide_banner", "-v", "error", "-y", "-i"].map(String::from).to_vec();
    a.push(file.to_string_lossy().into_owned());
    if let Some(c) = cover {
        a.extend([
            "-i".into(),
            c.to_string_lossy().into_owned(),
            "-map".into(),
            "0:a".into(),
            "-map".into(),
            "1:v".into(),
        ]);
        a.extend(
            [
                "-disposition:v:0",
                "attached_pic",
                "-metadata:s:v",
                "title=Album cover",
                "-metadata:s:v",
                "comment=Cover (front)",
            ]
            .map(String::from),
        );
    } else {
        a.extend(["-map", "0"].map(String::from));
    }
    a.extend(["-map_metadata", "0", "-c", "copy"].map(String::from));

    let mut meta = |k: &str, v: Option<String>| {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            a.extend(["-metadata".into(), format!("{k}={v}")]);
        }
    };
    meta("title", Some(t.title.clone()));
    meta("artist", Some(t.artist_line()));
    meta("album_artist", t.album_artist.clone().or_else(|| Some(t.main_artist().to_string())));
    meta("album", album.map(str::to_string));
    meta(
        "track",
        t.track_number.map(|n| match t.track_total {
            Some(total) => format!("{n}/{total}"),
            None => n.to_string(),
        }),
    );
    meta("disc", t.disc_number.map(|d| d.to_string()));
    meta("date", t.release_date.clone());
    meta("isrc", t.isrc.clone());
    meta("lyrics", lyrics.and_then(|l| l.plain.clone().or_else(|| l.synced.clone())));
    // yt-dlp leaves the YouTube description/URL in these; they're noise in a music library.
    for k in ["comment", "description", "synopsis", "purl"] {
        a.extend(["-metadata".into(), format!("{k}=")]);
    }
    if ext == "mp3" {
        a.extend(["-id3v2_version", "3"].map(String::from));
    }
    a.push(tmp.to_string_lossy().into_owned());

    let out = proc::command(paths::tool("ffmpeg"))
        .args(&a)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("failed to start ffmpeg")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!("tagging failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""));
    }
    std::fs::rename(&tmp, file)?;
    Ok(())
}

/// Turn a collection into queue jobs: makes the folder, writes the M3U and
/// the sync manifest, and returns one job per track (in playlist order).
pub fn prepare(col: &Collection, settings: &crate::settings::Settings) -> Result<(PathBuf, Vec<MusicJob>)> {
    let dir = collection_dir(settings, col);
    std::fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;

    let jobs: Vec<MusicJob> = col
        .tracks
        .iter()
        .map(|t| MusicJob {
            track: t.clone(),
            dir: dir.clone(),
            stem: file_stem(t, col.kind),
            lyrics: settings.lyrics,
            lyrics_prefs: lyrics::Prefs::from_settings(settings),
            mode: Mode::Song,
            forced: None,
            verify: settings.verify_audio,
        })
        .collect();

    if col.kind != Kind::Track {
        let ext = &settings.audio_format;
        let entries: Vec<(String, f64, String)> = jobs
            .iter()
            .map(|j| {
                (
                    format!("{} - {}", j.track.artist_line(), j.track.title),
                    j.track.duration_secs(),
                    format!("{}.{ext}", j.stem),
                )
            })
            .collect();
        write_m3u(&dir, &col.name, &entries)?;
        let manifest = Manifest {
            spotify_kind: col.kind.label().to_lowercase(),
            spotify_id: col.id.clone(),
            name: col.name.clone(),
            tracks: jobs
                .iter()
                .map(|j| ManifestTrack {
                    id: j.track.id.clone(),
                    stem: j.stem.clone(),
                    title: format!("{} - {}", j.track.artist_line(), j.track.title),
                })
                .collect(),
        };
        write_manifest(&dir, &manifest)?;
    }
    Ok((dir, jobs))
}

// ───────────── Playlist files and sync manifest ─────────────

/// Write `<name>.m3u8` listing the tracks in order (relative paths).
pub fn write_m3u(dir: &Path, name: &str, entries: &[(String, f64, String)]) -> Result<PathBuf> {
    let mut s = String::from("#EXTM3U\n");
    for (display, secs, file) in entries {
        s.push_str(&format!("#EXTINF:{},{}\n{}\n", secs.round() as i64, display, file));
    }
    let path = dir.join(format!("{}.m3u8", sanitize(name)));
    std::fs::write(&path, s)?;
    Ok(path)
}

/// The entries of an `.m3u8` written by [`write_m3u`] (display, seconds, file). Empty if
/// there's none.
pub fn read_m3u(path: &Path) -> Vec<(String, f64, String)> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut out = Vec::new();
    let mut info: Option<(String, f64)> = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let (secs, display) = rest.split_once(',').unwrap_or((rest, ""));
            info = Some((display.to_string(), secs.trim().parse().unwrap_or(-1.0)));
        } else if !line.starts_with('#') {
            let (display, secs) = info.take().unwrap_or_else(|| (line.to_string(), -1.0));
            out.push((display, secs, line.to_string()));
        }
    }
    out
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Manifest {
    pub spotify_kind: String,
    pub spotify_id: String,
    pub name: String,
    pub tracks: Vec<ManifestTrack>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ManifestTrack {
    pub id: String,
    pub stem: String,
    pub title: String,
}

const MANIFEST: &str = ".neria-sync.json";

pub fn read_manifest(dir: &Path) -> Option<Manifest> {
    serde_json::from_slice(&std::fs::read(dir.join(MANIFEST)).ok()?).ok()
}

pub fn write_manifest(dir: &Path, m: &Manifest) -> Result<()> {
    std::fs::write(dir.join(MANIFEST), serde_json::to_vec_pretty(m)?)?;
    Ok(())
}

/// A downloaded track that is no longer in the playlist.
#[derive(Clone, Debug)]
pub struct Removed {
    pub title: String,
    pub file: PathBuf,
}

/// Compare the folder's last manifest with the current playlist. Only meaningful
/// when we saw the *whole* playlist (not truncated), otherwise we'd flag tracks
/// that simply weren't shown.
pub fn removed_tracks(dir: &Path, col: &Collection) -> Vec<Removed> {
    if col.truncated || col.kind == Kind::Track {
        return Vec::new();
    }
    let Some(prev) = read_manifest(dir) else { return Vec::new() };
    prev.tracks
        .iter()
        .filter(|t| !col.tracks.iter().any(|c| c.id == t.id))
        .filter_map(|t| existing_file(dir, &t.stem).map(|file| Removed { title: t.title.clone(), file }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize("AC/DC - What? <Live> *"), "AC DC - What Live");
        assert_eq!(sanitize("trailing dots..."), "trailing dots");
        assert_eq!(sanitize("   "), "Untitled");
    }

    #[test]
    fn album_tracks_are_numbered() {
        let t = Track {
            title: "Get Lucky".into(),
            artists: vec!["Daft Punk".into()],
            track_number: Some(8),
            ..Default::default()
        };
        assert_eq!(file_stem(&t, Kind::Album), "08 Daft Punk - Get Lucky");
        assert_eq!(file_stem(&t, Kind::Playlist), "Daft Punk - Get Lucky");
    }

    #[test]
    fn detects_removed_tracks_only_for_full_playlists() {
        let dir = std::env::temp_dir().join(format!("neria-sync-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A - Kept.mp3"), b"x").unwrap();
        std::fs::write(dir.join("B - Gone.mp3"), b"x").unwrap();
        let m = Manifest {
            spotify_kind: "playlist".into(),
            spotify_id: "p".into(),
            name: "P".into(),
            tracks: vec![
                ManifestTrack { id: "1".into(), stem: "A - Kept".into(), title: "Kept".into() },
                ManifestTrack { id: "2".into(), stem: "B - Gone".into(), title: "Gone".into() },
            ],
        };
        write_manifest(&dir, &m).unwrap();
        let mut col = Collection {
            kind: Kind::Playlist,
            id: "p".into(),
            name: "P".into(),
            owner: None,
            cover_url: None,
            tracks: vec![Track { id: "1".into(), ..Default::default() }],
            truncated: false,
            source: crate::spotify::Source::Embed,
            notice: None,
        };
        let removed = removed_tracks(&dir, &col);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].title, "Gone");
        col.truncated = true;
        assert!(removed_tracks(&dir, &col).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
