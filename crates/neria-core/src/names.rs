//! File names without yt-dlp template syntax: a name is a line of pieces, fields (Title,
//! Channel, Upload date…) and plain text between them; a `/` makes a folder. Turned into
//! yt-dlp's `-o` template and back, with an example from real values and warnings about
//! characters Windows doesn't allow and names that would collide.

use std::collections::HashMap;

/// A field you can put in a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Title,
    Channel,
    UploadDate,
    VideoId,
    Playlist,
    PlaylistIndex,
    Resolution,
    Duration,
    Site,
}

impl Field {
    pub const ALL: [Field; 9] = [
        Self::Title,
        Self::Channel,
        Self::UploadDate,
        Self::VideoId,
        Self::Playlist,
        Self::PlaylistIndex,
        Self::Resolution,
        Self::Duration,
        Self::Site,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Channel => "Channel",
            Self::UploadDate => "Upload date",
            Self::VideoId => "Video ID",
            Self::Playlist => "Playlist",
            Self::PlaylistIndex => "Playlist #",
            Self::Resolution => "Resolution",
            Self::Duration => "Duration",
            Self::Site => "Site",
        }
    }

    /// The yt-dlp template for this field. Fields a video may not have fall back to
    /// nothing instead of "NA".
    pub fn template(self) -> &'static str {
        match self {
            Self::Title => "%(title)s",
            Self::Channel => "%(channel,uploader|)s",
            Self::UploadDate => "%(upload_date>%Y-%m-%d|)s",
            Self::VideoId => "%(id)s",
            Self::Playlist => "%(playlist_title|)s",
            Self::PlaylistIndex => "%(playlist_index|)s",
            Self::Resolution => "%(height|)sp",
            Self::Duration => "%(duration_string|)s",
            Self::Site => "%(extractor_key)s",
        }
    }

    /// Older or hand-written templates for the same field, so they still open as pieces.
    fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Title => &["%(title)s"],
            Self::Channel => &["%(uploader)s", "%(channel)s"],
            Self::UploadDate => &["%(upload_date)s"],
            Self::VideoId => &["%(id)s"],
            Self::Playlist => &["%(playlist_title)s", "%(playlist)s"],
            Self::PlaylistIndex => &["%(playlist_index)s", "%(playlist_index)02d"],
            Self::Resolution => &["%(resolution)s"],
            Self::Duration => &["%(duration_string)s"],
            Self::Site => &["%(extractor)s"],
        }
    }

    pub fn from_index(i: usize) -> Option<Field> {
        Self::ALL.get(i).copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Field(Field),
    /// Plain text; a `/` in it makes a folder.
    Text(String),
}

/// The `-o` template for these pieces (`.%(ext)s` added).
pub fn to_template(pieces: &[Piece]) -> String {
    let mut out = String::new();
    for p in pieces {
        match p {
            Piece::Field(f) => out.push_str(f.template()),
            // `%` is special in templates; `\` would be a folder too.
            Piece::Text(t) => out.push_str(&t.replace('%', "%%").replace('\\', "/")),
        }
    }
    out.push_str(".%(ext)s");
    out
}

/// A template as pieces, if it only uses fields the builder knows (else it stays a raw
/// template, editable under Advanced).
pub fn from_template(template: &str) -> Option<Vec<Piece>> {
    let t = template.trim();
    let t = t.strip_suffix(".%(ext)s").unwrap_or(t);
    let mut pieces: Vec<Piece> = Vec::new();
    let mut rest = t;
    let mut text = String::new();
    'outer: while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("%%") {
            text.push('%');
            rest = r;
            continue;
        }
        if rest.starts_with("%(") {
            for f in Field::ALL {
                for pat in std::iter::once(f.template()).chain(f.aliases().iter().copied()) {
                    if let Some(r) = rest.strip_prefix(pat) {
                        if !text.is_empty() {
                            pieces.push(Piece::Text(std::mem::take(&mut text)));
                        }
                        pieces.push(Piece::Field(f));
                        rest = r;
                        continue 'outer;
                    }
                }
            }
            return None;
        }
        let c = rest.chars().next()?;
        text.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Some(pieces)
}

/// Ready-made names. `Playlist / 01 Title` makes a folder per playlist.
pub fn presets() -> Vec<(&'static str, Vec<Piece>)> {
    use Field::*;
    let f = Piece::Field;
    let t = |s: &str| Piece::Text(s.into());
    vec![
        ("Title", vec![f(Title)]),
        ("Channel - Title", vec![f(Channel), t(" - "), f(Title)]),
        ("Date - Title", vec![f(UploadDate), t(" - "), f(Title)]),
        ("Title [ID]", vec![f(Title), t(" ["), f(VideoId), t("]")]),
        ("Playlist / 01 Title", vec![f(Playlist), t("/"), f(PlaylistIndex), t(" "), f(Title)]),
    ]
}

/// A short name for saved names: the pieces in words ("Channel - Title").
pub fn describe(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Field(f) => f.label().to_string(),
            Piece::Text(t) => t.clone(),
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Sample values for the example when there's no link to take them from.
pub fn sample_values() -> HashMap<&'static str, String> {
    HashMap::from([
        ("title", "How the Deep Sea Works, Explained".to_string()),
        ("channel", "Kurzgesagt".to_string()),
        ("upload_date", "2024-03-19".to_string()),
        ("id", "dQw4w9WgXcQ".to_string()),
        ("playlist", "Science".to_string()),
        ("playlist_index", "3".to_string()),
        ("height", "1080".to_string()),
        ("duration", "18:42".to_string()),
        ("site", "Youtube".to_string()),
        ("ext", "mp4".to_string()),
    ])
}

/// What the file would be called: the pieces filled in with `values` (keys as in
/// `sample_values`), the way yt-dlp cleans field values for Windows.
pub fn example(pieces: &[Piece], values: &HashMap<&str, String>) -> String {
    let v = |k: &str| values.get(k).cloned().unwrap_or_default();
    let mut out = String::new();
    for p in pieces {
        match p {
            Piece::Field(f) => {
                let raw = match f {
                    Field::Title => v("title"),
                    Field::Channel => v("channel"),
                    Field::UploadDate => v("upload_date"),
                    Field::VideoId => v("id"),
                    Field::Playlist => v("playlist"),
                    Field::PlaylistIndex => v("playlist_index"),
                    Field::Resolution => {
                        let h = v("height");
                        if h.is_empty() { h } else { format!("{h}p") }
                    }
                    Field::Duration => v("duration"),
                    Field::Site => v("site"),
                };
                // yt-dlp swaps characters Windows can't take for look-alikes.
                out.push_str(&raw.replace(['/', '\\'], "⧸").replace(':', "：").replace('?', "？").replace('"', "＂"));
            }
            Piece::Text(t) => out.push_str(t),
        }
    }
    format!("{}.{}", out.replace('\\', "/"), v("ext"))
}

/// What to warn about: characters Windows doesn't allow in the typed text, an empty name,
/// a folder with no name, names that would collide.
pub fn warnings(pieces: &[Piece]) -> Vec<String> {
    let mut w = Vec::new();
    let text: String =
        pieces.iter().filter_map(|p| if let Piece::Text(t) = p { Some(t.as_str()) } else { None }).collect();
    let bad: Vec<char> = text.chars().filter(|c| "<>:\"|?*".contains(*c)).collect();
    if !bad.is_empty() {
        let mut bad = bad;
        bad.dedup();
        let list: String = bad.iter().map(|c| format!("{c} ")).collect();
        w.push(format!("Windows doesn't allow {}in file names: they'll be swapped for look-alikes.", list));
    }
    if !pieces.iter().any(|p| matches!(p, Piece::Field(_))) {
        w.push("Add at least one field, or every file gets the same name.".into());
    } else {
        let has = |f: Field| pieces.contains(&Piece::Field(f));
        let unique = has(Field::VideoId) || has(Field::PlaylistIndex) || (has(Field::UploadDate) && has(Field::Title));
        if !unique {
            w.push("Two videos with the same title would get the same name (the second is skipped). Add Video ID or Upload date?".into());
        }
    }
    // `a//b`, a name starting or ending with a folder slash.
    let line = to_template(pieces);
    let line = line.strip_suffix(".%(ext)s").unwrap_or(&line);
    if line.starts_with('/') || line.ends_with('/') || line.contains("//") {
        w.push("A / needs a name on both sides (it makes a folder).".into());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for (_, p) in presets() {
            assert_eq!(from_template(&to_template(&p)).unwrap(), p);
        }
        // Neria's old default opens as pieces.
        let p = from_template("%(title)s [%(id)s].%(ext)s").unwrap();
        assert_eq!(describe(&p), "Title [Video ID]");
        assert_eq!(from_template("%(title)s - %(view_count)s.%(ext)s"), None);
        assert_eq!(to_template(&[Piece::Text("100% ".into()), Piece::Field(Field::Title)]), "100%% %(title)s.%(ext)s");
    }

    #[test]
    fn examples_and_warnings() {
        let p = &presets()[4].1;
        assert_eq!(example(p, &sample_values()), "Science/3 How the Deep Sea Works, Explained.mp4");
        assert!(warnings(p).is_empty());
        let title_only = &presets()[0].1;
        assert_eq!(warnings(title_only).len(), 1);
        let bad = vec![Piece::Field(Field::Title), Piece::Text(" : ".into()), Piece::Field(Field::VideoId)];
        assert!(warnings(&bad)[0].contains("doesn't allow"));
        assert!(!warnings(&[Piece::Text("x".into())]).is_empty());
    }
}
