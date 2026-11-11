//! Where downloads land.
//!
//! ```text
//! Videos\Neria\              <- Settings → Save to (the root)
//! ├─ Audio\                  audio-only downloads and Spotify songs
//! │  └─ Chill Mix\           a playlist/album downloaded as audio
//! ├─ Video\                  videos and GIFs
//! │  └─ My Playlist\         a playlist downloaded as video
//! ├─ Photos\                 photos picked from a post
//! └─ seann - Just testing\   several picked items of different kinds (photo + video…)
//! ```
//!
//! With "Sort into Audio / Video / Photos folders" off, everything goes into the root as
//! before (groups still get their own folder).

use std::path::PathBuf;

use crate::music::sanitize;
use crate::settings::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Audio,
    Video,
    Photo,
}

impl Kind {
    fn folder(self) -> &'static str {
        match self {
            Self::Audio => "Audio",
            Self::Video => "Video",
            Self::Photo => "Photos",
        }
    }
}

/// Folder for one download of this kind.
pub fn for_kind(s: &Settings, kind: Kind) -> PathBuf {
    if s.sort_folders { s.output_dir.join(kind.folder()) } else { s.output_dir.clone() }
}

/// Folder for several items downloaded together (a playlist, or items picked from a
/// post): inside the kind's folder when they're all one kind, else next to those folders.
pub fn for_group(s: &Settings, kinds: &[Kind], name: &str) -> PathBuf {
    let base = match kinds.split_first() {
        Some((first, rest)) if rest.iter().all(|k| k == first) => for_kind(s, *first),
        _ => s.output_dir.clone(),
    };
    // The same playlist/post again reuses its folder; items that are already there are skipped.
    base.join(sanitize(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(sort: bool) -> Settings {
        Settings { output_dir: PathBuf::from("C:/N"), sort_folders: sort, ..Settings::default() }
    }

    #[test]
    fn sorts_by_kind() {
        let s = settings(true);
        assert_eq!(for_kind(&s, Kind::Audio), PathBuf::from("C:/N/Audio"));
        assert_eq!(for_group(&s, &[Kind::Audio, Kind::Audio], "Mix"), PathBuf::from("C:/N/Audio/Mix"));
        assert_eq!(for_group(&s, &[Kind::Video, Kind::Video], "P"), PathBuf::from("C:/N/Video/P"));
        assert_eq!(for_group(&s, &[Kind::Photo, Kind::Video], "Post: 1"), PathBuf::from("C:/N/Post 1"));
    }

    #[test]
    fn unsorted_keeps_the_root() {
        let s = settings(false);
        assert_eq!(for_kind(&s, Kind::Photo), PathBuf::from("C:/N"));
        assert_eq!(for_group(&s, &[Kind::Audio], "Mix"), PathBuf::from("C:/N/Mix"));
    }
}
