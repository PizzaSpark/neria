//! What Neria downloaded before (`history.json` next to the exe), so pasting the same video
//! again can say "You already downloaded this" instead of quietly saving a second copy.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::paths;

/// Oldest records are dropped beyond this.
const MAX: usize = 5000;

static LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    /// `Extractor:id` (see `download::media_key`).
    pub key: String,
    pub url: String,
    pub file: PathBuf,
    pub title: String,
    /// Unix seconds.
    pub at: u64,
}

fn file() -> PathBuf {
    paths::app_dir().join("history.json")
}

fn load() -> Vec<Record> {
    std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Remember a finished download (replaces an older record of the same file).
pub fn record(r: Record) {
    let _guard = LOCK.lock().unwrap();
    let mut all = load();
    all.retain(|o| o.file != r.file);
    all.push(r);
    if all.len() > MAX {
        all.drain(..all.len() - MAX);
    }
    if let Ok(json) = serde_json::to_vec(&all) {
        let tmp = file().with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, file());
        }
    }
}

/// The latest download of this video (by key, else the same link) whose file still exists.
pub fn find(key: Option<&str>, url: &str) -> Option<Record> {
    let _guard = LOCK.lock().unwrap();
    find_in(&load(), key, url)
}

fn find_in(all: &[Record], key: Option<&str>, url: &str) -> Option<Record> {
    all.iter().rev().filter(|r| key.map_or(r.url == url, |k| r.key == k)).find(|r| r.file.is_file()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_key_and_skips_deleted_files() {
        let dir = std::env::temp_dir().join(format!("neria-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let kept = dir.join("a.mp4");
        std::fs::write(&kept, b"x").unwrap();
        let rec =
            |key: &str, file: PathBuf| Record { key: key.into(), url: "u".into(), file, title: "t".into(), at: 1 };
        let all = vec![rec("Youtube:a", kept.clone()), rec("Youtube:a", dir.join("gone.mp4"))];
        assert_eq!(find_in(&all, Some("Youtube:a"), "x").map(|r| r.file), Some(kept));
        assert!(find_in(&all, Some("Youtube:b"), "u").is_none());
        // No key (site didn't give an id): the same link counts.
        assert!(find_in(&all, None, "u").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
