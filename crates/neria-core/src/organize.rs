//! Put in folders (Library → several songs or a playlist, mock `design/audio-tools.html` 5A):
//! songs moved into Artist / Album folders (or Artist, Album, one folder) by their tags, with
//! their `.lrc` / `.txt` lyrics, every move listed before it's done, nothing overwritten, the
//! playlists that list them rewritten to the new places, and Undo. The one place Neria moves
//! files the user has.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{music, tags};

/// How to arrange them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrange {
    ArtistAlbum,
    Artist,
    Album,
    /// All of them in the folder itself.
    One,
}

impl Arrange {
    pub fn from_index(i: i32) -> Arrange {
        [Arrange::ArtistAlbum, Arrange::Artist, Arrange::Album, Arrange::One][i.clamp(0, 3) as usize]
    }
}

/// One planned move: the song and where it goes; `blocked` when a file is already there
/// (it stays where it is).
#[derive(Clone, Debug, PartialEq)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
    pub blocked: bool,
}

/// The folder for a song with these tags under `root`. Missing tags: "Unknown artist" /
/// "Unknown album"; names made safe for Windows.
pub fn folder_for(root: &Path, how: Arrange, artist: Option<&str>, album: Option<&str>) -> PathBuf {
    // The first artist of "A, B & C" (as Neria writes them) names the folder.
    let artist = artist
        .map(|a| a.split(", ").next().unwrap_or(a).split(" & ").next().unwrap_or(a).trim().to_string())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "Unknown artist".into());
    let album = album.map(str::trim).filter(|a| !a.is_empty()).unwrap_or("Unknown album").to_string();
    match how {
        Arrange::ArtistAlbum => root.join(music::sanitize(&artist)).join(music::sanitize(&album)),
        Arrange::Artist => root.join(music::sanitize(&artist)),
        Arrange::Album => root.join(music::sanitize(&album)),
        Arrange::One => root.to_path_buf(),
    }
}

/// Where each of `files` would go (their tags read now). Files already in place are left
/// out; two songs headed for the same name: the second is blocked.
pub fn plan(files: &[PathBuf], root: &Path, how: Arrange) -> Vec<Move> {
    let mut out: Vec<Move> = Vec::new();
    for f in files {
        let t = tags::read(f);
        let Some(name) = f.file_name() else { continue };
        let to = folder_for(root, how, t.artist.as_deref(), t.album.as_deref()).join(name);
        if same(&to, f) {
            continue;
        }
        let blocked = to.exists() || out.iter().any(|m| same(&m.to, &to));
        out.push(Move { from: f.clone(), to, blocked });
    }
    out
}

fn same(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

/// The lyrics files that go along with a song.
fn companions(song: &Path) -> Vec<PathBuf> {
    ["lrc", "txt"].iter().map(|e| song.with_extension(e)).filter(|p| p.is_file()).collect()
}

/// Move one file, across drives too (copy, then delete).
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::rename(from, to).or_else(|_| {
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)
    })
}

/// What was done, to say it and to undo it: every file moved (songs and their lyrics), in
/// order, and the playlists rewritten (with what they said before).
#[derive(Clone, Debug, Default)]
pub struct Done {
    pub moved: Vec<(PathBuf, PathBuf)>,
    /// The songs moved (for the Library's records).
    pub songs: Vec<(PathBuf, PathBuf)>,
    pub playlists: Vec<(PathBuf, String)>,
    pub failed: Vec<String>,
}

/// Do the moves that aren't blocked, then fix the playlists in the songs' old folders.
pub fn apply(moves: &[Move]) -> Done {
    let mut done = Done::default();
    for m in moves.iter().filter(|m| !m.blocked) {
        let with = companions(&m.from);
        match move_file(&m.from, &m.to) {
            Ok(()) => {
                done.moved.push((m.from.clone(), m.to.clone()));
                done.songs.push((m.from.clone(), m.to.clone()));
                for c in with {
                    let dest = m.to.with_extension(c.extension().unwrap_or_default());
                    if !dest.exists() && move_file(&c, &dest).is_ok() {
                        done.moved.push((c, dest));
                    }
                }
            }
            Err(e) => done.failed.push(format!("{}: {e}", m.from.display())),
        }
    }
    // Playlists next to the songs that listed them: each line to where its song is now.
    let mut dirs: Vec<PathBuf> = done.songs.iter().filter_map(|(f, _)| f.parent().map(Path::to_path_buf)).collect();
    dirs.dedup();
    for d in dirs {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("m3u8") || x.eq_ignore_ascii_case("m3u")) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let mut changed = false;
            let lines: Vec<String> = text
                .lines()
                .map(|l| {
                    let listed = if Path::new(l).is_absolute() { PathBuf::from(l) } else { d.join(l) };
                    match done.songs.iter().find(|(f, _)| !l.starts_with('#') && same(f, &listed)) {
                        Some((_, to)) => {
                            changed = true;
                            to.strip_prefix(&d).map_or_else(|_| to.display().to_string(), |r| r.display().to_string())
                        }
                        None => l.to_string(),
                    }
                })
                .collect();
            if changed && std::fs::write(&p, lines.join("\n") + "\n").is_ok() {
                done.playlists.push((p, text));
            }
        }
    }
    done
}

/// Put everything back as it was.
pub fn undo(done: &Done) -> Result<()> {
    for (p, text) in &done.playlists {
        std::fs::write(p, text)?;
    }
    for (from, to) in done.moved.iter().rev() {
        move_file(to, from)?;
    }
    // Folders the moves made and left empty go.
    for (_, to) in &done.moved {
        let mut d = to.parent();
        while let Some(dir) = d {
            if std::fs::remove_dir(dir).is_err() {
                break;
            }
            d = dir.parent();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_from_tags() {
        let r = Path::new("D:/Music");
        assert_eq!(
            folder_for(r, Arrange::ArtistAlbum, Some("Daft Punk, Pharrell Williams"), Some("Random Access Memories")),
            Path::new("D:/Music/Daft Punk/Random Access Memories")
        );
        assert_eq!(folder_for(r, Arrange::Artist, None, None), Path::new("D:/Music/Unknown artist"));
        assert_eq!(
            folder_for(r, Arrange::Album, Some("AC/DC"), Some("Back in Black")),
            Path::new("D:/Music/Back in Black")
        );
        assert_eq!(folder_for(r, Arrange::One, Some("a"), Some("b")), r);
        assert_eq!(folder_for(r, Arrange::Artist, Some("AC/DC"), None), Path::new("D:/Music/AC DC"));
    }

    #[test]
    fn moves_lyrics_playlists_and_undoes() {
        let dir = std::env::temp_dir().join(format!("neria-organize-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("Chill Mix");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.mp3"), b"a").unwrap();
        std::fs::write(src.join("a.lrc"), b"la").unwrap();
        std::fs::write(src.join("b.mp3"), b"b").unwrap();
        std::fs::write(src.join("Chill Mix.m3u8"), "#EXTM3U\n#EXTINF:1,A\na.mp3\nb.mp3\n").unwrap();
        let to = dir.join("Out").join("X");
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("b.mp3"), b"other").unwrap(); // already there: b stays
        let moves = vec![
            Move { from: src.join("a.mp3"), to: to.join("a.mp3"), blocked: false },
            Move { from: src.join("b.mp3"), to: to.join("b.mp3"), blocked: true },
        ];
        let done = apply(&moves);
        assert!(to.join("a.mp3").is_file() && to.join("a.lrc").is_file() && !src.join("a.mp3").exists());
        assert_eq!(std::fs::read(to.join("b.mp3")).unwrap(), b"other");
        let m3u = std::fs::read_to_string(src.join("Chill Mix.m3u8")).unwrap();
        assert!(m3u.contains(&to.join("a.mp3").display().to_string()) && m3u.contains("\nb.mp3"));
        undo(&done).unwrap();
        assert!(src.join("a.mp3").is_file() && src.join("a.lrc").is_file());
        assert!(std::fs::read_to_string(src.join("Chill Mix.m3u8")).unwrap().contains("\na.mp3\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
