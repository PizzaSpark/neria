//! Portable layout: everything lives next to the executable.
//!
//! ```text
//! Neria.exe
//! settings.json
//! bin/  yt-dlp.exe, ffmpeg.exe, ffprobe.exe, deno.exe
//! ```

use std::path::PathBuf;

/// Directory containing the running executable (override with `NERIA_HOME`).
pub fn app_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("NERIA_HOME") {
        return PathBuf::from(home);
    }
    std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| PathBuf::from("."))
}

/// Where yt-dlp, ffmpeg and friends are stored.
pub fn bin_dir() -> PathBuf {
    app_dir().join("bin")
}

/// Full path of a bundled tool, e.g. `tool("yt-dlp")` -> `bin/yt-dlp.exe`.
pub fn tool(name: &str) -> PathBuf {
    bin_dir().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

pub fn settings_file() -> PathBuf {
    app_dir().join("settings.json")
}

/// Scratch space for thumbnails and other throwaway files.
pub fn cache_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("neria");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Can Neria write next to itself? Not when it was unzipped into Program Files or a
/// read-only drive. Returns the folder and the error when it can't.
pub fn check_writable() -> Result<(), (PathBuf, std::io::Error)> {
    let dir = app_dir();
    let probe = dir.join(".neria-write-test");
    std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(&probe, b"ok"))
        .and_then(|_| std::fs::remove_file(&probe))
        .map_err(|e| (dir, e))
}
