//! The command palette's matching (Ctrl+K, mock `design/palette.html`): how well what's typed
//! fits a name and its other words ("shrink" finds Make a file smaller), and what was picked
//! before (`Used`, `palette.json` next to the exe) so the things you use come first.
//! The list of actions and what they do live in the desktop app (`palette_ui.rs`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::paths;

/// How well `query` fits `name` (or one of `words`): `None` when it doesn't. Higher is better:
/// the whole name, then its start, a word's start, inside a word, then the other words; each
/// typed word has to be found somewhere. Case and accents of ASCII letters don't matter.
pub fn score(query: &str, name: &str, words: &[&str]) -> Option<i32> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Some(0);
    }
    let name = name.to_lowercase();
    let words: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let mut total = 0;
    for part in q.split_whitespace() {
        total += part_score(part, &name, &words)?;
    }
    // The whole thing typed as it's written reads as the best fit.
    if name == q {
        total += 200;
    } else if name.starts_with(&q) {
        total += 100;
    }
    Some(total)
}

fn part_score(part: &str, name: &str, words: &[String]) -> Option<i32> {
    let at_word_start = |text: &str| {
        text.match_indices(part).any(|(i, _)| i == 0 || !text[..i].ends_with(|c: char| c.is_alphanumeric()))
    };
    if name.starts_with(part) {
        Some(60)
    } else if at_word_start(name) {
        Some(45)
    } else if words.iter().any(|w| w.starts_with(part)) {
        Some(35)
    } else if name.contains(part) {
        Some(20)
    } else if words.iter().any(|w| w.contains(part)) && part.len() >= 3 {
        Some(10)
    } else {
        None
    }
}

/// Where the match is in `name`, to show it in bold: (start, end) byte offsets of the first
/// typed word found at a word's start, else anywhere.
pub fn highlight(query: &str, name: &str) -> Option<(usize, usize)> {
    let part = query.split_whitespace().next()?.to_lowercase();
    let lower = name.to_lowercase();
    // Lowercasing keeps byte offsets for ASCII; anything else isn't highlighted.
    if lower.len() != name.len() {
        return None;
    }
    let starts =
        lower.match_indices(&part).find(|(i, _)| *i == 0 || !lower[..*i].ends_with(|c: char| c.is_alphanumeric()));
    let (i, _) = starts.or_else(|| lower.match_indices(&part).next())?;
    Some((i, i + part.len()))
}

/// How often each action was picked from the palette (by its key), for the order before
/// anything is typed and between equal matches.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Used {
    pub counts: HashMap<String, u32>,
}

fn file() -> std::path::PathBuf {
    paths::app_dir().join("palette.json")
}

impl Used {
    pub fn load() -> Used {
        std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// One more pick of `key`, saved.
    pub fn picked(key: &str) {
        let mut u = Used::load();
        *u.counts.entry(key.to_string()).or_default() += 1;
        if let Ok(json) = serde_json::to_vec(&u) {
            let _ = std::fs::write(file(), json);
        }
    }

    pub fn count(&self, key: &str) -> u32 {
        self.counts.get(key).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_then_their_other_words() {
        let compress = ("Make a file smaller…", &["compress", "shrink", "discord"][..]);
        let gif = ("Make a GIF…", &["webp", "meme"][..]);
        // Start of the name beats a word's start beats another word.
        assert!(score("make", compress.0, compress.1).is_some());
        assert!(score("gif", gif.0, gif.1).unwrap() > score("gif", compress.0, compress.1).unwrap_or(0));
        assert!(score("shrink", compress.0, compress.1).is_some());
        assert!(score("disc", compress.0, compress.1).is_some());
        assert_eq!(score("zzz", compress.0, compress.1), None);
        // Every typed word has to fit.
        assert!(score("make gif", gif.0, gif.1).is_some());
        assert_eq!(score("make tokyo", gif.0, gif.1), None);
        // Case doesn't matter; nothing typed fits everything.
        assert!(score("GIF", gif.0, gif.1).is_some());
        assert_eq!(score("  ", gif.0, gif.1), Some(0));
    }

    #[test]
    fn a_word_start_beats_inside_a_word() {
        let tokyo = score("tok", "Day 3 in Tokyo", &[]).unwrap();
        let inside = score("tok", "Stokes live", &[]).unwrap();
        assert!(tokyo > inside);
        // Short typing doesn't match inside the other words (too many false hits).
        assert_eq!(score("ab", "Clip", &["drawable"]), None);
    }

    #[test]
    fn the_bold_part() {
        assert_eq!(highlight("tok", "Day 3 in Tokyo"), Some((9, 12)));
        assert_eq!(highlight("gif", "Make a GIF…"), Some((7, 10)));
        assert_eq!(highlight("zz", "Make a GIF…"), None);
        assert_eq!(highlight("", "Make a GIF…"), None);
    }
}
