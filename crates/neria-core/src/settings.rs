use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    #[default]
    Best,
    P1080,
    P720,
    P480,
    Audio,
}

impl Quality {
    pub const ALL: [Quality; 5] = [Self::Best, Self::P1080, Self::P720, Self::P480, Self::Audio];

    pub fn from_index(i: i32) -> Self {
        Self::ALL.get(i.max(0) as usize).copied().unwrap_or_default()
    }

    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|q| *q == self).unwrap_or(0) as i32
    }

    pub fn max_height(self) -> Option<u32> {
        match self {
            Self::P1080 => Some(1080),
            Self::P720 => Some(720),
            Self::P480 => Some(480),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub output_dir: PathBuf,
    /// Sort downloads into Audio / Video / Photos folders inside `output_dir`.
    pub sort_folders: bool,
    /// Number items of a playlist/post by their place in it (`01 Title`).
    pub number_items: bool,
    pub quality: Quality,
    /// Container for video downloads: mp4, mkv or webm.
    pub container: String,
    /// Target format for audio-only downloads: mp3, m4a, opus, flac, wav.
    pub audio_format: String,
    /// Prefer H.264/AAC so files play on anything (caps YouTube at 1080p).
    pub prefer_h264: bool,
    pub embed_thumbnail: bool,
    /// Also keep the thumbnail as a .jpg next to the video.
    pub save_thumbnail: bool,
    pub embed_metadata: bool,
    pub embed_subs: bool,
    pub sponsorblock: bool,
    pub allow_playlists: bool,
    /// X (Twitter) "GIFs" are short silent MP4s: convert them to real .gif files.
    pub x_gifs_as_gif: bool,
    /// Browser to borrow cookies from (empty = none).
    pub cookies_browser: String,
    /// yt-dlp rate limit, e.g. "5M". Empty = unlimited.
    pub rate_limit: String,
    pub filename_template: String,
    pub extra_args: String,
    pub max_concurrent: u32,
    /// Unix seconds of the last yt-dlp self-update check.
    pub last_update_check: u64,
    /// Spotify songs: fetch lyrics (embedded + .lrc next to the file).
    pub lyrics: bool,
    /// Spotify songs: compare with Spotify's 30 s preview, retry other versions on mismatch.
    pub verify_audio: bool,
    /// Spotify songs: explain where the audio comes from on the first Download.
    pub spotify_explain: bool,
    /// Optional: the user's own Spotify app (developer.spotify.com) for full playlists.
    pub spotify_client_id: String,
    pub spotify_client_secret: String,
    /// Look for a new Neria and newer tools at start-up (GitHub releases).
    pub check_updates: bool,
    /// "system" (follow Windows), "dark" or "light".
    pub theme: String,
    /// The first-run "How it works" steps were seen through (a download started, or the
    /// user hid them).
    pub tips_done: bool,
    /// Captions languages picked last time (ticked again when a video has them).
    pub caption_langs: Vec<String>,
    pub caption_format: crate::tracks::SubFormat,
    pub caption_mode: crate::tracks::SubMode,
    /// Chapters: one file per chapter instead of one file.
    pub chapters_separate: bool,
    /// Closing with downloads going: "ask", "tray" (keep going there) or "quit".
    pub close_action: String,
    /// Windows notifications when something finishes or fails.
    pub notify: bool,
    /// ...only while Neria isn't the window you're looking at.
    pub notify_unfocused: bool,
    /// "chime" (Neria's own), "windows" or "none"; separately for failures.
    pub sound_done: String,
    pub sound_failed: String,
}

impl Default for Settings {
    fn default() -> Self {
        let base = dirs::video_dir().or_else(dirs::download_dir).unwrap_or_else(paths::app_dir);
        Self {
            output_dir: base.join("Neria"),
            sort_folders: true,
            number_items: true,
            quality: Quality::Best,
            container: "mp4".into(),
            audio_format: "mp3".into(),
            prefer_h264: false,
            embed_thumbnail: true,
            save_thumbnail: false,
            embed_metadata: true,
            embed_subs: false,
            sponsorblock: false,
            allow_playlists: false,
            x_gifs_as_gif: true,
            cookies_browser: String::new(),
            rate_limit: String::new(),
            filename_template: "%(title)s [%(id)s].%(ext)s".into(),
            extra_args: String::new(),
            max_concurrent: 2,
            last_update_check: 0,
            lyrics: true,
            verify_audio: true,
            spotify_explain: true,
            spotify_client_id: String::new(),
            spotify_client_secret: String::new(),
            check_updates: true,
            theme: "system".into(),
            tips_done: false,
            caption_langs: Vec::new(),
            caption_format: Default::default(),
            caption_mode: Default::default(),
            chapters_separate: false,
            close_action: "ask".into(),
            notify: true,
            notify_unfocused: true,
            sound_done: "chime".into(),
            sound_failed: "chime".into(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(paths::settings_file())
            .ok()
            .and_then(|b| {
                // Tolerate a UTF-8 BOM from hand-editing in Notepad/PowerShell.
                let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b);
                serde_json::from_slice(b).ok()
            })
            .unwrap_or_default()
    }

    pub fn spotify_credentials(&self) -> Option<crate::spotify::Credentials> {
        (!self.spotify_client_id.trim().is_empty() && !self.spotify_client_secret.trim().is_empty()).then(|| {
            crate::spotify::Credentials {
                client_id: self.spotify_client_id.trim().to_string(),
                client_secret: self.spotify_client_secret.trim().to_string(),
            }
        })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(paths::settings_file(), json)?;
        Ok(())
    }
}
