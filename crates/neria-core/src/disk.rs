//! Free disk space, so big jobs stop early with a clear message instead of a
//! half-written file and a cryptic error.

use std::path::{Component, Path};

use crate::fmt;

/// Kept free on top of what a job needs (temp files, the merge step, Windows itself).
pub const MARGIN: u64 = 200_000_000;

/// Bytes free on the drive holding `path`. The folder doesn't have to exist yet.
pub fn free_space(path: &Path) -> Option<u64> {
    // Dev hook: pretend the drive has this many MB free.
    if let Some(mb) = std::env::var("NERIA_FAKE_FREE_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()) {
        return Some(mb * 1_000_000);
    }
    let mut dir = path;
    while !dir.exists() {
        dir = dir.parent()?;
    }
    imp::free(dir)
}

/// "D:" for `D:\Videos\Neria`, else the folder itself.
pub fn drive_name(path: &Path) -> String {
    match path.components().next() {
        Some(Component::Prefix(p)) => p.as_os_str().to_string_lossy().into_owned(),
        _ => path.display().to_string(),
    }
}

/// Is there room for `needed` more bytes (plus [`MARGIN`]) in `dir`? `Err` holds a
/// sentence for the user (add what to do). Unknown free space counts as enough.
pub fn check(dir: &Path, needed: u64) -> Result<(), String> {
    let Some(free) = free_space(dir) else { return Ok(()) };
    if free >= needed.saturating_add(MARGIN) {
        return Ok(());
    }
    Err(message(&drive_name(dir), free, needed))
}

fn message(drive: &str, free: u64, needed: u64) -> String {
    if needed < MARGIN {
        format!("Not enough space: drive {drive} is almost full ({} free).", fmt::bytes(free as f64))
    } else {
        format!(
            "Not enough space on {drive} ({} free, needs about {}).",
            fmt::bytes(free as f64),
            fmt::bytes(needed as f64)
        )
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
    }

    pub fn free(dir: &Path) -> Option<u64> {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        // SAFETY: `wide` is NUL-terminated; the out pointers may be null except the one we read.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
        (ok != 0).then_some(free)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn free(_dir: &std::path::Path) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_free_space_of_missing_folders() {
        let dir = std::env::temp_dir().join("neria-no-such-folder").join("deeper");
        let free = free_space(&dir);
        if cfg!(windows) {
            assert!(free.unwrap() > 0);
        }
        assert!(check(&dir, 0).is_ok() || free.unwrap_or(u64::MAX) < MARGIN);
        assert!(check(&dir, u64::MAX / 2).unwrap_err().starts_with("Not enough space on"));
    }

    #[test]
    fn names_drives() {
        if cfg!(windows) {
            assert_eq!(drive_name(Path::new(r"D:\Videos\Neria")), "D:");
        }
        assert!(message("D:", 50_000_000, 0).contains("almost full"));
    }
}
