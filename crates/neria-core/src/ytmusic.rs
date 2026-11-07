//! Finding a Spotify track on YouTube Music.
//!
//! Searches YouTube Music's "Songs" (official audio, not music videos) through
//! its public web API, the same endpoint `ytmusicapi`/spotDL use, then scores
//! candidates by title, artist and duration. Matching ideas follow spotDL's
//! `spotdl/utils/matching.py` (MIT); see docs/UPSTREAM.md.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::spotify::Track;
use crate::web;

const SEARCH_URL: &str = "https://music.youtube.com/youtubei/v1/search?prettyPrint=false";
const CLIENT_VERSION: &str = "1.20251101.01.00";
/// "Songs" filter for search (same value ytmusicapi uses).
const SONGS_FILTER: &str = "EgWKAQIIAWoMEA4QChADEAQQCRAF";
/// Below this, we'd rather fail than download the wrong song.
pub const MIN_SCORE: f64 = 0.6;

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub video_id: String,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub duration: Option<f64>,
    pub score: f64,
}

impl Candidate {
    pub fn url(&self) -> String {
        format!("https://music.youtube.com/watch?v={}", self.video_id)
    }
}

pub fn search_songs(query: &str) -> Result<Vec<Candidate>> {
    let body = json!({
        "context": { "client": { "clientName": "WEB_REMIX", "clientVersion": CLIENT_VERSION, "hl": "en", "gl": "US" } },
        "query": query,
        "params": SONGS_FILTER,
    });
    let resp = web::client()?
        .post(SEARCH_URL)
        .header("Content-Type", "application/json")
        .header("Origin", "https://music.youtube.com")
        .header("Referer", "https://music.youtube.com/")
        .body(body.to_string())
        .send()?;
    if !resp.status().is_success() {
        bail!("YouTube Music search failed (HTTP {})", resp.status());
    }
    let v: Value = serde_json::from_slice(&resp.bytes()?)?;
    Ok(parse_results(&v))
}

/// Collect every `musicResponsiveListItemRenderer` (robust to layout shuffles).
fn parse_results(v: &Value) -> Vec<Candidate> {
    let mut out = Vec::new();
    walk(v, &mut |node| {
        if let Some(r) = node.get("musicResponsiveListItemRenderer") {
            if let Some(c) = parse_item(r) {
                out.push(c);
            }
        }
    });
    out
}

fn walk(v: &Value, f: &mut impl FnMut(&Value)) {
    f(v);
    match v {
        Value::Array(a) => a.iter().for_each(|x| walk(x, f)),
        Value::Object(o) => o.values().for_each(|x| walk(x, f)),
        _ => {}
    }
}

fn column_runs(r: &Value, i: usize) -> Vec<&Value> {
    r.pointer(&format!("/flexColumns/{i}/musicResponsiveListItemFlexColumnRenderer/text/runs"))
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn parse_item(r: &Value) -> Option<Candidate> {
    let title_runs = column_runs(r, 0);
    let title: String = title_runs.iter().filter_map(|x| x["text"].as_str()).collect();
    let video_id = r
        .pointer("/playlistItemData/videoId")
        .or_else(|| title_runs.first()?.pointer("/navigationEndpoint/watchEndpoint/videoId"))?
        .as_str()?
        .to_string();

    // "Artist, Other & Third • Album • 6:10" (album missing for singles sometimes)
    let sub: String = column_runs(r, 1).iter().filter_map(|x| x["text"].as_str()).collect();
    let mut parts: Vec<&str> = sub.split(" • ").map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.first().is_some_and(|p| matches!(*p, "Song" | "Video")) {
        parts.remove(0);
    }
    let duration = parts.last().and_then(|p| parse_clock(p));
    if duration.is_some() {
        parts.pop();
    }
    let artists = parts.first().map(|s| s.to_string()).unwrap_or_default();
    let album = parts.get(1).map(|s| s.to_string());

    (!title.is_empty()).then_some(Candidate { video_id, title, artists, album, duration, score: 0.0 })
}

fn parse_clock(s: &str) -> Option<f64> {
    let mut total = 0.0;
    for part in s.split(':') {
        total = total * 60.0 + part.parse::<u32>().ok()? as f64;
    }
    s.contains(':').then_some(total)
}

// ───────────── Scoring ─────────────

const VERSION_WORDS: &[&str] = &[
    "live", "remix", "cover", "karaoke", "instrumental", "acoustic", "sped", "slowed", "reverb", "nightcore",
    "8d", "edit", "version", "demo", "remaster", "remastered", "mix", "rework", "extended", "recorded",
    "rerecorded", "tribute", "lofi",
];

/// Lowercase, drop "feat./ft./with …" credits and punctuation, collapse spaces.
pub fn normalize(s: &str) -> String {
    let lower = s.to_lowercase().replace('&', " and ");
    // Cut "(feat. X)", "[ft. X]", " feat. X" credits.
    let mut cut = lower.clone();
    for marker in ["(feat", "[feat", "(ft.", "[ft.", "(with ", " feat.", " feat ", " ft. ", " featuring "] {
        if let Some(i) = cut.find(marker) {
            let close = match marker.chars().next() {
                Some('(') => cut[i..].find(')').map(|j| i + j + 1),
                Some('[') => cut[i..].find(']').map(|j| i + j + 1),
                _ => None,
            };
            cut = match close {
                Some(end) => format!("{}{}", &cut[..i], &cut[end..]),
                None => cut[..i].to_string(),
            };
        }
    }
    cut.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Title without "(feat. …)" credits, original casing kept (for lyrics lookups).
pub fn normalize_title(title: &str) -> String {
    let lower = title.to_lowercase();
    for marker in ["(feat", "[feat", "(ft.", "[ft.", "(with ", " feat.", " ft. "] {
        if let Some(i) = lower.find(marker) {
            return title[..i].trim().to_string();
        }
    }
    title.trim().to_string()
}

fn words(s: &str) -> Vec<&str> {
    s.split(' ').filter(|w| !w.is_empty()).collect()
}

fn title_similarity(a: &str, b: &str) -> f64 {
    let (na, nb) = (normalize(a), normalize(b));
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }
    if na == nb {
        return 1.0;
    }
    let (wa, wb) = (words(&na), words(&nb));
    let common = wa.iter().filter(|w| wb.contains(w)).count() as f64;
    let jaccard = common / (wa.len() + wb.len()) as f64 * 2.0;
    // "Get Lucky" vs "Get Lucky (Radio Edit)" etc: containment is a strong signal.
    let contained = (nb.contains(&na) || na.contains(&nb)) as u8 as f64 * 0.85;
    jaccard.max(contained)
}

fn artist_similarity(track: &Track, cand: &Candidate) -> f64 {
    let hay = format!("{} {}", normalize(&cand.artists), normalize(&cand.title));
    let present = |a: &str| {
        let n = normalize(a);
        !n.is_empty() && hay.contains(&n)
    };
    if present(track.main_artist()) {
        1.0
    } else if track.artists.iter().any(|a| present(a)) {
        0.7
    } else {
        0.0
    }
}

/// Penalise "live", "remix", … when the Spotify title doesn't say so.
fn version_penalty(track: &Track, cand: &Candidate) -> f64 {
    let (want, got) = (normalize(&track.title), normalize(&cand.title));
    let (want, got) = (words(&want), words(&got));
    let extra = VERSION_WORDS.iter().filter(|w| got.contains(w) && !want.contains(w)).count();
    (extra as f64 * 0.25).min(0.5)
}

/// Score in 0..1, or None if the duration rules it out.
pub fn score(track: &Track, cand: &Candidate) -> Option<f64> {
    let want = track.duration_secs();
    let dur_score = match cand.duration {
        Some(d) if want > 0.0 => {
            let diff = (d - want).abs();
            if diff > 30.0 {
                return None;
            }
            if diff <= 3.0 { 1.0 } else { (1.0 - (diff - 3.0) / 15.0).max(0.0) }
        }
        _ => 0.5,
    };
    let s = 0.45 * title_similarity(&track.title, &cand.title) + 0.35 * artist_similarity(track, cand) + 0.2 * dur_score
        - version_penalty(track, cand);
    Some(s.clamp(0.0, 1.0))
}

/// Candidates for a track, best first (scored; unusable ones dropped).
pub fn candidates(track: &Track) -> Result<Vec<Candidate>> {
    let mut queries = Vec::new();
    if let Some(isrc) = &track.isrc {
        queries.push(isrc.clone()); // exact hit when YouTube Music knows the ISRC
    }
    queries.push(format!("{} {}", track.main_artist(), track.title));

    let mut all: Vec<Candidate> = Vec::new();
    let mut last_err = None;
    for q in &queries {
        match search_songs(q) {
            Ok(found) => {
                for mut c in found {
                    if all.iter().any(|x| x.video_id == c.video_id) {
                        continue;
                    }
                    if let Some(s) = score(track, &c) {
                        c.score = s;
                        all.push(c);
                    }
                }
                if all.iter().any(|c| c.score >= 0.9) {
                    break;
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    if all.is_empty() {
        if let Some(e) = last_err {
            return Err(e);
        }
    }
    all.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(all)
}

/// The best candidate, if it's good enough to download without asking.
pub fn best_match(track: &Track) -> Result<Option<Candidate>> {
    Ok(candidates(track)?.into_iter().next().filter(|c| c.score >= MIN_SCORE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_lucky() -> Track {
        Track {
            title: "Get Lucky (feat. Pharrell Williams and Nile Rodgers)".into(),
            artists: vec!["Daft Punk".into(), "Pharrell Williams".into(), "Nile Rodgers".into()],
            duration_ms: 369_626,
            ..Default::default()
        }
    }

    fn cand(title: &str, artists: &str, dur: f64) -> Candidate {
        Candidate { video_id: "x".into(), title: title.into(), artists: artists.into(), album: None, duration: Some(dur), score: 0.0 }
    }

    #[test]
    fn normalizes_credits() {
        assert_eq!(normalize("Get Lucky (feat. Pharrell Williams and Nile Rodgers)"), "get lucky");
        assert_eq!(normalize("Song [ft. Someone] - Live"), "song live");
        assert_eq!(normalize("Rock & Roll"), "rock and roll");
    }

    #[test]
    fn picks_album_version_over_edits_and_namesakes() {
        let t = get_lucky();
        let album = score(&t, &cand("Get Lucky (feat. Pharrell Williams and Nile Rodgers)", "Daft Punk, Pharrell Williams & Nile Rodgers", 370.0)).unwrap();
        let radio = score(&t, &cand("Get Lucky (Radio Edit - feat. Pharrell Williams and Nile Rodgers)", "Daft Punk, Pharrell Williams & Nile Rodgers", 249.0));
        let other = score(&t, &cand("Get Lucky", "Phoenix Moon", 259.0));
        assert!(album > 0.95, "{album}");
        assert!(radio.is_none(), "radio edit is 2 min shorter");
        assert!(other.is_none());
        let same_len_cover = score(&t, &cand("Get Lucky (Cover)", "Someone Else", 368.0)).unwrap();
        assert!(same_len_cover < MIN_SCORE, "{same_len_cover}");
    }

    #[test]
    fn parses_search_item() {
        let item = json!({ "musicResponsiveListItemRenderer": {
            "playlistItemData": { "videoId": "4D7u5KF7SP8" },
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [ { "text": "Get Lucky" } ] } } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Daft Punk" }, { "text": " • " }, { "text": "Random Access Memories" }, { "text": " • " }, { "text": "6:10" }
                ] } } }
            ]
        }});
        let found = parse_results(&json!({ "contents": [item] }));
        assert_eq!(found.len(), 1);
        let c = &found[0];
        assert_eq!(c.video_id, "4D7u5KF7SP8");
        assert_eq!(c.artists, "Daft Punk");
        assert_eq!(c.album.as_deref(), Some("Random Access Memories"));
        assert_eq!(c.duration, Some(370.0));
    }
}
