//! A sound file's tags (Library → Edit tags, mock `design/audio-tools.html` 3A): read them,
//! write the ones asked for (the rest stay as they are), and look them up online: Spotify by
//! the song's id when Neria knows it (its downloads, its folders' manifests), else YouTube
//! Music by the title and artist it has (or its file name).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use crate::{music, paths, probe, proc, spotify, ytmusic};

/// The fields Edit tags shows. On writing, `None` leaves that tag as the file has it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// "3" or "3/12".
    pub track: Option<String>,
    /// "1985" or "1985-04-01".
    pub year: Option<String>,
}

/// The file's tags now (missing ones are None).
pub fn read(file: &Path) -> Tags {
    let Ok(out) = proc::command(paths::tool("ffprobe"))
        .args(["-v", "error", "-show_entries", "format_tags", "-of", "json"])
        .arg(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return Tags::default();
    };
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let tags = v["format"]["tags"].as_object().cloned().unwrap_or_default();
    // Names differ in case between formats (TITLE in FLAC / Opus, title in MP3).
    let tag = |k: &str| {
        tags.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(k))
            .and_then(|(_, v)| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Tags {
        title: tag("title"),
        artist: tag("artist").or_else(|| tag("album_artist")),
        album: tag("album"),
        track: tag("track"),
        year: tag("date").or_else(|| tag("year")),
    }
}

/// The cover inside the file, as a picture in Neria's cache (to show it), if it has one.
pub fn cover(file: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    file.hash(&mut h);
    std::fs::metadata(file).ok()?.len().hash(&mut h);
    let out = paths::cache_dir().join(format!("cover-{:016x}.jpg", h.finish()));
    if out.is_file() {
        return Some(out);
    }
    let ok = proc::command(paths::tool("ffmpeg"))
        .args(["-hide_banner", "-v", "error", "-y", "-i"])
        .arg(file)
        .args(["-an", "-map", "0:v:0", "-frames:v", "1"])
        .arg(&out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    (ok && out.is_file()).then_some(out)
}

/// Write `tags` (the `Some` ones) into `file`, and `cover` as its picture when given. The
/// file is rewritten without re-encoding, through a temporary file next to it.
pub fn write(file: &Path, tags: &Tags, cover: Option<&Path>) -> Result<()> {
    let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    // ffmpeg can attach a cover to these; others keep the one they have.
    let cover = cover.filter(|_| matches!(ext.as_str(), "mp3" | "m4a" | "flac"));
    let tmp = file.with_extension(format!("tagging.{ext}"));
    let mut a: Vec<String> = ["-hide_banner", "-v", "error", "-y", "-i"].map(String::from).to_vec();
    a.push(file.to_string_lossy().into_owned());
    if let Some(c) = cover {
        a.extend(["-i".into(), c.to_string_lossy().into_owned()]);
        a.extend(["-map", "0:a", "-map", "1:v", "-disposition:v:0", "attached_pic"].map(String::from));
    } else {
        a.extend(["-map", "0"].map(String::from));
    }
    a.extend(["-map_metadata", "0", "-c", "copy"].map(String::from));
    for (k, v) in [
        ("title", &tags.title),
        ("artist", &tags.artist),
        ("album", &tags.album),
        ("track", &tags.track),
        ("date", &tags.year),
    ] {
        if let Some(v) = v {
            a.extend(["-metadata".into(), format!("{k}={}", v.trim())]);
        }
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
        bail!("{}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("ffmpeg failed"));
    }
    // Windows refuses while a player has the file open.
    std::fs::rename(&tmp, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// What a lookup found: the tags, its cover (a picture in the cache), and where from.
#[derive(Clone, Debug, Default)]
pub struct Found {
    pub tags: Tags,
    pub cover: Option<PathBuf>,
    pub from: &'static str,
}

/// Find this song's tags online: Spotify by its id (`spotify_url`, or the folder's manifest),
/// else the best YouTube Music match for the title and artist it has (or its file name).
pub fn find_online(file: &Path, spotify_url: &str) -> Result<Found> {
    let id = spotify::parse_link(spotify_url)
        .filter(|l| matches!(l.kind, spotify::Kind::Track))
        .map(|l| l.id)
        .or_else(|| music::track_from_file(file).map(|t| t.id).filter(|id| !id.is_empty()));
    if let Some(t) = id.and_then(|id| spotify::track_details(&id).ok()) {
        let cover = t.cover_url.as_deref().and_then(|u| probe::fetch_thumbnail(u).ok());
        return Ok(Found {
            tags: Tags {
                title: Some(t.title.clone()),
                artist: Some(t.artist_line()),
                album: t.album.clone(),
                track: t.track_number.map(|n| match t.track_total {
                    Some(total) => format!("{n}/{total}"),
                    None => n.to_string(),
                }),
                year: t.release_date.clone(),
            },
            cover,
            from: "Spotify",
        });
    }
    let now = read(file);
    let query = match (&now.artist, &now.title) {
        (Some(a), Some(t)) => format!("{a} {t}"),
        (None, Some(t)) => t.clone(),
        _ => file.file_stem().map(|s| s.to_string_lossy().replace(['_', '-'], " ")).unwrap_or_default(),
    };
    if query.trim().is_empty() {
        bail!("Nothing to look it up by: give it a title first");
    }
    let found = ytmusic::search_songs(&query)?;
    let Some(c) = found.into_iter().next() else { bail!("Nothing found for \"{query}\"") };
    Ok(Found {
        tags: Tags { title: Some(c.title), artist: Some(c.artists), album: c.album, track: None, year: None },
        cover: None,
        from: "YouTube Music",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_leaves_a_tag_alone() {
        let t = Tags { title: Some("A".into()), ..Tags::default() };
        assert_eq!((t.artist, t.album, t.track, t.year), (None, None, None, None));
    }
}
