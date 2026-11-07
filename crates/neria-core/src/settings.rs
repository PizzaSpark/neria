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
    pub quality: Quality,
    /// Container for video downloads: mp4, mkv or webm.
    pub container: String,
    /// Target format for audio-only downloads: mp3, m4a, opus, flac, wav.
    pub audio_format: String,
    /// Prefer H.264/AAC so files play on anything (caps YouTube at 1080p).
    pub prefer_h264: bool,
    pub embed_thumbnail: bool,
    pub embed_metadata: bool,
    pub embed_subs: bool,
    pub sponsorblock: bool,
    pub allow_playlists: bool,
    /// Browser to borrow cookies from (empty = none).
    pub cookies_browser: String,
    /// yt-dlp rate limit, e.g. "5M". Empty = unlimited.
    pub rate_limit: String,
    pub filename_template: String,
    pub extra_args: String,
    pub max_concurrent: u32,
    /// Unix seconds of the last yt-dlp self-update check.
    pub last_update_check: u64,
}

impl Default for Settings {
    fn default() -> Self {
        let base = dirs::video_dir()
            .or_else(dirs::download_dir)
            .unwrap_or_else(paths::app_dir);
        Self {
            output_dir: base.join("Neria"),
            quality: Quality::Best,
            container: "mp4".into(),
            audio_format: "mp3".into(),
            prefer_h264: false,
            embed_thumbnail: true,
            embed_metadata: true,
            embed_subs: false,
            sponsorblock: false,
            allow_playlists: false,
            cookies_browser: String::new(),
            rate_limit: String::new(),
            filename_template: "%(title)s [%(id)s].%(ext)s".into(),
            extra_args: String::new(),
            max_concurrent: 2,
            last_update_check: 0,
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

    pub fn save(&self) -> anyhow::Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(paths::settings_file(), json)?;
        Ok(())
    }
}
