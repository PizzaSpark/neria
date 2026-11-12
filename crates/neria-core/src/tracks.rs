//! What's inside one video besides the picture: captions, audio tracks (dubbed languages)
//! and chapters, read from yt-dlp's `-J`, and the yt-dlp options for the ones picked.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One captions language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caption {
    pub lang: String,
    /// "English", "Spanish (auto)".
    pub name: String,
    /// Made by YouTube's speech recognition (or translated by it), not by the creator.
    pub auto: bool,
}

/// One audio track of a video with several (dubbed languages, descriptive audio).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTrack {
    pub lang: String,
    pub name: String,
    /// The language the video was made in (the default).
    pub original: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub start: f64,
    pub end: f64,
    pub title: String,
}

/// A readable name for a language code yt-dlp didn't name.
pub fn language_name(code: &str) -> String {
    let base = code.split(['-', '_']).next().unwrap_or(code).to_ascii_lowercase();
    let name = match base.as_str() {
        "en" => "English",
        "es" => "Spanish",
        "fr" => "French",
        "de" => "German",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "pl" => "Polish",
        "ru" => "Russian",
        "uk" => "Ukrainian",
        "tr" => "Turkish",
        "ar" => "Arabic",
        "hi" => "Hindi",
        "id" => "Indonesian",
        "ja" => "Japanese",
        "ko" => "Korean",
        "zh" => "Chinese",
        "vi" => "Vietnamese",
        "th" => "Thai",
        "sv" => "Swedish",
        "da" => "Danish",
        "no" | "nb" => "Norwegian",
        "fi" => "Finnish",
        "cs" => "Czech",
        "el" => "Greek",
        "he" | "iw" => "Hebrew",
        "ro" => "Romanian",
        "hu" => "Hungarian",
        "bn" => "Bengali",
        "ta" => "Tamil",
        "te" => "Telugu",
        "ms" => "Malay",
        "fil" | "tl" => "Filipino",
        _ => return code.to_string(),
    };
    match code.split_once(['-', '_']) {
        Some((_, region)) if !region.eq_ignore_ascii_case("orig") => format!("{name} ({region})"),
        _ => name.to_string(),
    }
}

/// The video's own language (`language`), lower case, if yt-dlp knows it.
pub fn video_language(v: &Value) -> Option<String> {
    v.get("language").and_then(Value::as_str).filter(|l| !l.is_empty()).map(str::to_ascii_lowercase)
}

/// Same language, ignoring region (`en-US` ~ `en`).
fn same_lang(a: &str, b: &str) -> bool {
    let base = |s: &str| s.split(['-', '_']).next().unwrap_or(s).to_ascii_lowercase();
    base(a) == base(b)
}

/// Every captions language: the creator's first, then auto-generated; in each group the
/// video's own language on top, then by name.
pub fn captions(v: &Value) -> Vec<Caption> {
    let own = video_language(v);
    let mut out = Vec::new();
    for (key, auto) in [("subtitles", false), ("automatic_captions", true)] {
        let Some(map) = v.get(key).and_then(Value::as_object) else { continue };
        let mut group: Vec<Caption> = map
            .iter()
            .filter(|(lang, list)| *lang != "live_chat" && list.as_array().is_some_and(|l| !l.is_empty()))
            .map(|(lang, list)| {
                let named = list.as_array().and_then(|l| l.iter().find_map(|f| f.get("name")?.as_str()));
                let name = named.map(str::to_string).unwrap_or_else(|| language_name(lang));
                Caption { lang: lang.clone(), name, auto }
            })
            .collect();
        let rank = |c: &Caption| match &own {
            // The original speech first ("en-orig"), then the video's language.
            Some(o) if c.lang.ends_with("-orig") && same_lang(&c.lang, o) => 0,
            Some(o) if c.lang.eq_ignore_ascii_case(o) => 1,
            Some(o) if same_lang(&c.lang, o) => 2,
            _ => 3,
        };
        group.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        out.extend(group);
    }
    out
}

/// The audio tracks of a video, when there's a choice (two or more languages). The
/// original first.
pub fn audio_tracks(v: &Value) -> Vec<AudioTrack> {
    let Some(formats) = v.get("formats").and_then(Value::as_array) else { return Vec::new() };
    let mut out: Vec<AudioTrack> = Vec::new();
    for f in formats {
        let has_audio = f.get("acodec").and_then(Value::as_str).is_some_and(|a| a != "none");
        let Some(lang) = f.get("language").and_then(Value::as_str).filter(|l| !l.is_empty()) else { continue };
        if !has_audio {
            continue;
        }
        let note = f.get("format_note").and_then(Value::as_str).unwrap_or("").to_lowercase();
        let original =
            note.contains("original") || f.get("language_preference").and_then(Value::as_i64).is_some_and(|p| p >= 10);
        let descriptive = note.contains("descriptive");
        let mut name = language_name(lang);
        if descriptive {
            name.push_str(" (descriptive)");
        }
        match out.iter_mut().find(|t| t.lang == lang) {
            Some(t) => t.original |= original,
            None => out.push(AudioTrack { lang: lang.to_string(), name, original }),
        }
    }
    if out.len() < 2 {
        return Vec::new();
    }
    out.sort_by(|a, b| b.original.cmp(&a.original).then_with(|| a.name.cmp(&b.name)));
    out
}

pub fn chapters(v: &Value) -> Vec<Chapter> {
    let duration = v.get("duration").and_then(Value::as_f64);
    let list = v.get("chapters").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out: Vec<Chapter> = list
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let start = c.get("start_time").and_then(Value::as_f64)?;
            let end = c.get("end_time").and_then(Value::as_f64).or(duration)?;
            let title = c
                .get("title")
                .and_then(Value::as_str)
                .filter(|t| !t.trim().is_empty())
                .map_or_else(|| format!("Chapter {}", i + 1), |t| t.trim().to_string());
            (end > start).then_some(Chapter { start, end, title })
        })
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    // One "chapter" covering everything isn't a choice.
    if out.len() < 2 { Vec::new() } else { out }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubFormat {
    #[default]
    Srt,
    Vtt,
    Ass,
    /// Plain text: the words only, no timings.
    Txt,
}

impl SubFormat {
    pub const ALL: [SubFormat; 4] = [Self::Srt, Self::Vtt, Self::Ass, Self::Txt];
    pub fn from_index(i: i32) -> Self {
        Self::ALL.get(i.max(0) as usize).copied().unwrap_or_default()
    }
    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|f| *f == self).unwrap_or(0) as i32
    }
    /// What yt-dlp converts to (text is made from SRT afterwards).
    fn convert_to(self) -> &'static str {
        match self {
            Self::Srt | Self::Txt => "srt",
            Self::Vtt => "vtt",
            Self::Ass => "ass",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubMode {
    /// `Title.en.srt` next to the video.
    #[default]
    Separate,
    /// Inside the video as switchable tracks.
    Embed,
    Both,
}

impl SubMode {
    pub const ALL: [SubMode; 3] = [Self::Separate, Self::Embed, Self::Both];
    pub fn from_index(i: i32) -> Self {
        Self::ALL.get(i.max(0) as usize).copied().unwrap_or_default()
    }
    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|f| *f == self).unwrap_or(0) as i32
    }
}

/// The captions to save with a download.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct CaptionChoice {
    /// Creator-made languages.
    pub langs: Vec<String>,
    /// Auto-generated languages.
    pub auto_langs: Vec<String>,
    pub format: SubFormat,
    pub mode: SubMode,
    /// Captions only: skip the video itself.
    pub only: bool,
}

impl CaptionChoice {
    pub fn is_empty(&self) -> bool {
        self.langs.is_empty() && self.auto_langs.is_empty()
    }

    /// yt-dlp options. `audio_only`: nothing to embed into, so files only.
    pub fn args(&self, audio_only: bool) -> Vec<String> {
        if self.is_empty() {
            return Vec::new();
        }
        let mut a: Vec<String> = Vec::new();
        let embed = !audio_only && !self.only && matches!(self.mode, SubMode::Embed | SubMode::Both);
        let files = self.only || audio_only || matches!(self.mode, SubMode::Separate | SubMode::Both);
        let mut langs: Vec<String> = self.langs.iter().chain(&self.auto_langs).map(|l| regex_escape(l)).collect();
        langs.dedup();
        if !self.langs.is_empty() || files {
            // Also keeps the files when embedding (yt-dlp deletes them unless asked to write them).
            a.push("--write-subs".into());
        }
        if !self.auto_langs.is_empty() {
            a.push("--write-auto-subs".into());
        }
        a.push("--sub-langs".into());
        a.push(langs.join(","));
        a.push("--convert-subs".into());
        a.push(self.format.convert_to().into());
        if embed {
            a.push("--embed-subs".into());
        }
        if self.only {
            a.push("--skip-download".into());
        }
        a
    }

    /// Embedding only: the caption files yt-dlp wrote are leftovers to remove afterwards.
    pub fn files_are_leftovers(&self, audio_only: bool) -> bool {
        !audio_only && !self.only && self.mode == SubMode::Embed
    }
}

/// `--sub-langs` takes regexes: `en-US` is fine, but escape anything special.
fn regex_escape(s: &str) -> String {
    s.chars().fold(String::new(), |mut out, c| {
        if "\\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
        out
    })
}

/// yt-dlp `-f` for these audio languages. `video`: with the picture (several languages =
/// several audio tracks in one file, `--audio-multistreams`), else audio only (one language).
pub fn audio_format(langs: &[String], video: bool, max_height: Option<u32>) -> String {
    let lim = max_height.map(|h| format!("[height<={h}]")).unwrap_or_default();
    let tracks: String = langs.iter().map(|l| format!("+ba[language={l}]")).collect();
    if video {
        format!("bv*{lim}{tracks}/bv*{lim}+ba/b")
    } else {
        let l = langs.first().map(String::as_str).unwrap_or("");
        format!("ba[language={l}]/ba/b")
    }
}

/// SRT → the words only (no numbers, timings or tags), one caption per line, repeats dropped.
pub fn srt_to_text(srt: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in srt.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.contains("-->") || line.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // Strip <i>, {\an8} and the like.
        let mut text = String::new();
        let mut depth = 0;
        for c in line.chars() {
            match c {
                '<' | '{' => depth += 1,
                '>' | '}' if depth > 0 => depth -= 1,
                c if depth == 0 => text.push(c),
                _ => {}
            }
        }
        let text = text.trim().to_string();
        if !text.is_empty() && out.last() != Some(&text) {
            out.push(text);
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Picked chapters as ranges to download: neighbours join into one range.
/// `picked` are chapter indexes. Returns (start, end) in order.
pub fn ranges(chapters: &[Chapter], picked: &[usize]) -> Vec<(f64, f64)> {
    let mut idx: Vec<usize> = picked.iter().copied().filter(|i| *i < chapters.len()).collect();
    idx.sort_unstable();
    idx.dedup();
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut prev: Option<usize> = None;
    for i in idx {
        let c = &chapters[i];
        match (prev, out.last_mut()) {
            (Some(p), Some(last)) if p + 1 == i => last.1 = c.end,
            _ => out.push((c.start, c.end)),
        }
        prev = Some(i);
    }
    out
}

/// For "One file": the chapters left out between picked ones ("skips chapter 3",
/// "skips chapters 3 and 5", "skips 4 chapters"). `None` when nothing in between is skipped.
pub fn skipped_note(total: usize, picked: &[usize]) -> Option<String> {
    let (Some(first), Some(last)) = (picked.iter().min(), picked.iter().max()) else { return None };
    let gaps: Vec<usize> = (*first..=*last).filter(|i| !picked.contains(i)).map(|i| i + 1).collect();
    let _ = total;
    match gaps.len() {
        0 => None,
        1 => Some(format!("skips chapter {}", gaps[0])),
        2 => Some(format!("skips chapters {} and {}", gaps[0], gaps[1])),
        n => Some(format!("skips {n} chapters")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_captions() {
        let v = json!({
            "language": "en",
            "subtitles": { "es": [{"ext": "vtt", "name": "Spanish"}], "en": [{"ext": "vtt", "name": "English"}], "live_chat": [{}] },
            "automatic_captions": { "fr": [{"name": "French"}], "en-orig": [{"name": "English (Original)"}], "de": [] }
        });
        let c = captions(&v);
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["English", "Spanish", "English (Original)", "French"]);
        assert!(!c[0].auto && c[2].auto);
    }

    #[test]
    fn reads_audio_tracks() {
        let v = json!({ "formats": [
            {"acodec": "opus", "language": "es", "format_note": "Spanish (dubbed-auto), medium"},
            {"acodec": "opus", "language": "en", "format_note": "English original (default), medium"},
            {"acodec": "opus", "language": "en", "format_note": "English original (default), low"},
            {"acodec": "none", "language": "de", "vcodec": "avc1"}
        ]});
        let t = audio_tracks(&v);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].lang, "en");
        assert!(t[0].original && !t[1].original);
        let single = json!({ "formats": [{"acodec": "opus", "language": "en"}] });
        assert!(audio_tracks(&single).is_empty());
        assert_eq!(
            audio_format(&["en".into(), "es".into()], true, Some(720)),
            "bv*[height<=720]+ba[language=en]+ba[language=es]/bv*[height<=720]+ba/b"
        );
        assert_eq!(audio_format(&["es".into()], false, None), "ba[language=es]/ba/b");
    }

    #[test]
    fn chapter_ranges() {
        let ch: Vec<Chapter> = (0..5)
            .map(|i| Chapter { start: i as f64 * 10.0, end: i as f64 * 10.0 + 10.0, title: format!("C{i}") })
            .collect();
        assert_eq!(ranges(&ch, &[1, 2, 4]), vec![(10.0, 30.0), (40.0, 50.0)]);
        assert_eq!(skipped_note(5, &[1, 2, 4]).as_deref(), Some("skips chapter 4"));
        assert_eq!(skipped_note(5, &[0, 4]).as_deref(), Some("skips 3 chapters"));
        assert_eq!(skipped_note(5, &[1, 2]), None);
        let v = json!({"duration": 30.0, "chapters": [{"start_time": 0.0, "end_time": 12.0, "title": "Intro"}, {"start_time": 12.0, "title": ""}]});
        let c = chapters(&v);
        assert_eq!(c[1].end, 30.0);
        assert_eq!(c[1].title, "Chapter 2");
    }

    #[test]
    fn caption_args() {
        let c = CaptionChoice {
            langs: vec!["en".into()],
            auto_langs: vec!["fr".into()],
            format: SubFormat::Srt,
            mode: SubMode::Both,
            only: false,
        };
        let a = c.args(false);
        assert!(a.contains(&"--embed-subs".to_string()) && a.contains(&"--write-auto-subs".to_string()));
        assert!(a.windows(2).any(|w| w[0] == "--sub-langs" && w[1] == "en,fr"));
        let only = CaptionChoice { only: true, ..c };
        assert!(only.args(false).contains(&"--skip-download".to_string()));
        assert!(!only.args(false).contains(&"--embed-subs".to_string()));
        assert_eq!(
            srt_to_text(
                "1\n00:00:01,000 --> 00:00:02,000\n<i>Hello</i>\n\n2\n00:00:02,000 --> 00:00:03,000\nHello\nworld\n"
            ),
            "Hello\nworld\n"
        );
    }
}
