//! Links with several things in them: posts with photos, videos and GIFs (X, Instagram,
//! Facebook…) and playlists. Lists every item so the user can pick what to download.
//!
//! yt-dlp sees videos (and on Instagram, photos as items without formats); gallery-dl sees
//! photos and, on X, everything in order. [`merge`] lines the two up.

use std::process::Stdio;

use serde_json::Value;

use crate::download::DownloadRequest;
use crate::folders::{self, Kind};
use crate::music::sanitize;
use crate::settings::{Quality, Settings};
use crate::{paths, proc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Video,
    /// X "GIF": a short silent MP4, saved as a real .gif.
    Gif,
    Photo,
}

impl ItemKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Gif => "GIF",
            Self::Photo => "Photo",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    /// The item's own title (playlist entries); empty for post items.
    pub title: String,
    pub duration: Option<f64>,
    /// Small picture for the picker.
    pub thumbnail: Option<String>,
    /// What yt-dlp downloads: the item's own link, or the post link with `playlist_index`.
    pub url: String,
    /// 1-based position among yt-dlp's items (`--playlist-items`).
    pub playlist_index: Option<usize>,
    /// A file address saved as is, without yt-dlp (photos; videos yt-dlp didn't see).
    pub direct_url: Option<String>,
    /// File extension for `direct_url`.
    pub ext: String,
}

/// Sites where posts often hold photos yt-dlp can't see.
pub fn wants_gallery(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("").to_lowercase();
    let host = host.strip_prefix("www.").or_else(|| host.strip_prefix("m.")).unwrap_or(&host);
    ["x.com", "twitter.com", "mobile.twitter.com", "facebook.com", "web.facebook.com"].contains(&host)
}

/// Items from yt-dlp's `-J` output (run with `--ignore-no-formats-error`).
/// `None` when the link is a single video (no picker needed).
pub fn from_ytdlp(v: &Value, url: &str) -> Option<Vec<Item>> {
    if v.get("_type").and_then(Value::as_str) != Some("playlist") {
        return None;
    }
    let entries = v.get("entries")?.as_array()?;
    // Post items are often just "Post title #2": no title of their own.
    let parent = v.get("title").and_then(Value::as_str).unwrap_or("");
    let items = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.is_object())
        .map(|(i, e)| {
            let title = e.get("title").and_then(Value::as_str).unwrap_or("");
            let title = if !parent.is_empty() && title.starts_with(parent) { String::new() } else { title.to_string() };
            let duration = e.get("duration").and_then(Value::as_f64);
            let thumbnail = crate::probe::pick_thumbnail(e);
            // Flat playlist entry (YouTube…): a link of its own.
            if e.get("_type").and_then(Value::as_str) == Some("url")
                && let Some(link) = e.get("url").and_then(Value::as_str)
            {
                return Item {
                    kind: ItemKind::Video,
                    title,
                    duration,
                    thumbnail,
                    url: link.to_string(),
                    playlist_index: None,
                    direct_url: None,
                    ext: String::new(),
                };
            }
            let formats = e.get("formats").and_then(Value::as_array).map_or(0, |f| f.len());
            if formats == 0 && e.get("url").is_none() {
                // A photo (Instagram): the biggest "thumbnail" is the photo itself.
                let full = biggest_thumbnail(e);
                return Item {
                    kind: ItemKind::Photo,
                    title: String::new(),
                    duration: None,
                    thumbnail: thumbnail.or_else(|| full.clone()),
                    url: url.to_string(),
                    playlist_index: Some(i + 1),
                    ext: full.as_deref().map_or("jpg".into(), ext_of),
                    direct_url: full,
                };
            }
            let gif = e.get("url").and_then(Value::as_str).is_some_and(crate::gif::is_x_gif_url);
            Item {
                kind: if gif { ItemKind::Gif } else { ItemKind::Video },
                title,
                duration,
                thumbnail,
                url: url.to_string(),
                playlist_index: Some(i + 1),
                direct_url: None,
                ext: String::new(),
            }
        })
        .collect();
    Some(items)
}

/// yt-dlp sorts thumbnails worst to best, so the last is the full-size picture.
fn biggest_thumbnail(e: &Value) -> Option<String> {
    let list = e.get("thumbnails")?.as_array()?;
    list.iter()
        .rev()
        .find_map(|t| t.get("url")?.as_str())
        .map(str::to_string)
        .or_else(|| e.get("thumbnail").and_then(Value::as_str).map(str::to_string))
}

fn ext_of(url: &str) -> String {
    // X: `...?format=jpg&name=orig`
    if let Some(f) = url.split(['?', '&']).find_map(|p| p.strip_prefix("format=")) {
        return f.to_string();
    }
    let path = url.split('?').next().unwrap_or(url);
    let file = path.rsplit('/').next().unwrap_or("");
    match file.rsplit_once('.') {
        Some((_, e)) if (2..=4).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_alphanumeric()) => e.to_lowercase(),
        _ => "jpg".into(),
    }
}

/// What gallery-dl found at the link.
#[derive(Clone, Debug, Default)]
pub struct Gallery {
    pub items: Vec<Item>,
    /// A name for the post ("Photos from Jackson Soriano's post"), if it gave one.
    pub title: Option<String>,
}

/// Ask gallery-dl what's in the post (no download). Empty when it isn't installed,
/// doesn't support the site or found nothing.
pub fn gallery(url: &str, settings: &Settings) -> Gallery {
    let exe = paths::tool("gallery-dl");
    if !exe.is_file() {
        return Gallery::default();
    }
    let mut cmd = proc::command(exe);
    cmd.args(["-j", "--no-colors"]);
    if !settings.cookies_browser.is_empty() {
        cmd.args(["--cookies-from-browser", &settings.cookies_browser]);
    }
    cmd.arg("--").arg(url);
    let Ok(out) = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).output() else { return Gallery::default() };
    serde_json::from_slice::<Value>(&out.stdout).map(|v| parse_gallery(&v, url)).unwrap_or_default()
}

/// gallery-dl `-j` output: `[2, meta]` per post, `[3, url, meta]` per file, `[-1, error]`.
pub fn parse_gallery(v: &Value, url: &str) -> Gallery {
    let mut g = Gallery::default();
    for msg in v.as_array().into_iter().flatten() {
        let Some(m) = msg.as_array() else { continue };
        if m.first().and_then(Value::as_i64) != Some(3) {
            continue;
        }
        let (Some(file), Some(meta)) = (m.get(1).and_then(Value::as_str), m.get(2)) else { continue };
        let ext = meta.get("extension").and_then(Value::as_str).map_or_else(|| ext_of(file), str::to_string);
        let kind = match meta.get("type").and_then(Value::as_str) {
            Some("animated_gif") => ItemKind::Gif,
            Some("video") => ItemKind::Video,
            Some("photo") => ItemKind::Photo,
            _ if ["mp4", "webm", "mov", "m4v"].contains(&ext.as_str()) => ItemKind::Video,
            _ => ItemKind::Photo,
        };
        if g.title.is_none() {
            g.title = meta.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string);
        }
        let thumbnail = (kind == ItemKind::Photo).then(|| small_photo(file));
        g.items.push(Item {
            kind,
            title: String::new(),
            duration: None,
            thumbnail,
            url: url.to_string(),
            playlist_index: None,
            direct_url: Some(file.to_string()),
            ext,
        });
    }
    g
}

/// A smaller copy for the picker where the site offers one (X: `name=small`).
fn small_photo(url: &str) -> String {
    if url.contains("pbs.twimg.com") && url.contains("name=") {
        let mut out = String::new();
        for (i, part) in url.split('&').enumerate() {
            if i > 0 {
                out.push('&');
            }
            out.push_str(if part.starts_with("name=") { "name=small" } else { part });
        }
        out
    } else {
        url.to_string()
    }
}

/// Queue jobs for the picked items (`picked` holds positions in `items`, 0-based).
///
/// One item: saved like any single download (its kind's folder, the usual name). Several:
/// one folder named after the post/playlist (inside Audio/Video/Photos when they're all
/// one kind), files numbered by their place in the post so they sort in order:
/// `01 Photo.jpg`, `02 Video.mp4` for posts, `01 Title.mp4` for playlists. With
/// "Number files in playlist order" off: `Photo 1.jpg`, `Title.mp4`.
pub fn requests(
    title: &str,
    items: &[Item],
    picked: &[usize],
    quality: Quality,
    settings: &Settings,
) -> Vec<DownloadRequest> {
    let numbered = settings.number_items;
    let picked: Vec<(usize, &Item)> = picked.iter().filter_map(|&i| items.get(i).map(|it| (i, it))).collect();
    let audio = quality == Quality::Audio;
    let kinds: Vec<Kind> = picked
        .iter()
        .map(|(_, it)| match it.kind {
            ItemKind::Photo => Kind::Photo,
            ItemKind::Gif => Kind::Video,
            ItemKind::Video if audio => Kind::Audio,
            ItemKind::Video => Kind::Video,
        })
        .collect();
    let name: String = title.chars().take(80).collect();
    let group = (picked.len() > 1).then(|| folders::for_group(settings, &kinds, &name));
    let width = items.len().to_string().len().max(2);

    picked
        .iter()
        .map(|&(i, it)| {
            let n = i + 1;
            let stem = match &group {
                Some(_) if !it.title.is_empty() && numbered => Some(sanitize(&format!("{n:0width$} {}", it.title))),
                Some(_) if !it.title.is_empty() => Some(sanitize(&it.title)),
                Some(_) if numbered => Some(format!("{n:0width$} {}", it.kind.label())),
                Some(_) => Some(format!("{} {n}", it.kind.label())),
                // A lone photo/video only gallery-dl saw has no title to name it by.
                None if it.direct_url.is_some() => Some(sanitize(&format!("{name} ({} {n})", it.kind.label()))),
                None => None,
            };
            let label =
                if it.title.is_empty() { format!("{} · {} {n}", title, it.kind.label()) } else { it.title.clone() };
            let gif = it.kind == ItemKind::Gif;
            let mut req = DownloadRequest::new(&it.url, if gif { Quality::Best } else { quality }, label);
            req.playlist_index = it.playlist_index;
            req.dir = group.clone();
            req.stem = stem;
            req.gif = gif;
            req.thumbnail = it.thumbnail.clone();
            req.direct =
                it.direct_url.clone().map(|u| (u, if it.ext.is_empty() { "jpg".into() } else { it.ext.clone() }));
            req
        })
        .collect()
}

/// Line up yt-dlp's and gallery-dl's lists. gallery-dl's order wins (it sees photos too);
/// its videos are swapped for yt-dlp's in order, since yt-dlp picks better formats and adds
/// metadata. Videos only gallery-dl saw are saved from its file address.
pub fn merge(ytdlp: Vec<Item>, gallery: Vec<Item>) -> Vec<Item> {
    if gallery.is_empty() {
        return ytdlp;
    }
    // yt-dlp found photos itself (Instagram): its list is complete.
    if ytdlp.iter().any(|i| i.kind == ItemKind::Photo) {
        return ytdlp;
    }
    let mut videos = ytdlp.into_iter();
    let mut out: Vec<Item> = gallery
        .into_iter()
        .map(|g| match g.kind {
            ItemKind::Photo => g,
            _ => match videos.next() {
                // yt-dlp knows GIFs by address; gallery-dl by type.
                Some(mut v) => {
                    if g.kind == ItemKind::Gif {
                        v.kind = ItemKind::Gif;
                    }
                    v
                }
                None => g,
            },
        })
        .collect();
    out.extend(videos);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn yt_video(i: usize) -> Item {
        Item {
            kind: ItemKind::Video,
            title: String::new(),
            duration: Some(4.8),
            thumbnail: None,
            url: "post".into(),
            playlist_index: Some(i),
            direct_url: None,
            ext: String::new(),
        }
    }

    #[test]
    fn x_post_photo_video_gif() {
        let v = json!([
            [2, {"tweet_id": 1}],
            [3, "https://pbs.twimg.com/media/A?format=jpg&name=orig", {"type": "photo", "extension": "jpg"}],
            [3, "https://video.twimg.com/amplify_video/1/a.mp4", {"type": "video", "extension": "mp4"}],
            [3, "https://video.twimg.com/tweet_video/B.mp4", {"type": "animated_gif", "extension": "mp4"}]
        ]);
        let g = parse_gallery(&v, "post");
        assert_eq!(
            g.items.iter().map(|i| i.kind).collect::<Vec<_>>(),
            [ItemKind::Photo, ItemKind::Video, ItemKind::Gif]
        );
        assert_eq!(g.items[0].thumbnail.as_deref(), Some("https://pbs.twimg.com/media/A?format=jpg&name=small"));

        let merged = merge(vec![yt_video(1), yt_video(2)], g.items);
        assert_eq!(
            merged.iter().map(|i| i.kind).collect::<Vec<_>>(),
            [ItemKind::Photo, ItemKind::Video, ItemKind::Gif]
        );
        assert_eq!(merged[1].playlist_index, Some(1));
        assert_eq!(merged[2].playlist_index, Some(2));
        assert!(merged[0].direct_url.is_some());
    }

    #[test]
    fn facebook_photos_without_video() {
        let v = json!([
            [3, "https://scontent.x/v/1_n.jpg?stp=a", {"extension": "jpg", "title": "Photos from J's post"}],
            [3, "https://scontent.x/v/2_n.jpg?stp=a", {"extension": "jpg"}]
        ]);
        let g = parse_gallery(&v, "post");
        assert_eq!(g.title.as_deref(), Some("Photos from J's post"));
        let merged = merge(Vec::new(), g.items);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().all(|i| i.kind == ItemKind::Photo && i.ext == "jpg"));
    }

    #[test]
    fn instagram_photos_come_from_ytdlp() {
        let v = json!({
            "_type": "playlist",
            "entries": [
                {"id": "a", "formats": [], "thumbnails": [{"url": "https://i/s.jpg"}, {"url": "https://i/full.jpg"}]},
                {"id": "b", "formats": [{"url": "x"}], "duration": 6.0},
                {"id": "c", "formats": [], "thumbnails": [{"url": "https://i/c.webp?x=1"}]}
            ]
        });
        let items = from_ytdlp(&v, "post").unwrap();
        assert_eq!(
            items.iter().map(|i| i.kind).collect::<Vec<_>>(),
            [ItemKind::Photo, ItemKind::Video, ItemKind::Photo]
        );
        assert_eq!(items[0].direct_url.as_deref(), Some("https://i/full.jpg"));
        assert_eq!(items[1].playlist_index, Some(2));
        assert_eq!(items[2].ext, "webp");
        // gallery-dl output is ignored when yt-dlp already has the photos.
        assert_eq!(merge(items.clone(), vec![yt_video(9)]), items);
    }

    #[test]
    fn picked_items_become_jobs() {
        let s = Settings { output_dir: "C:/N".into(), ..Settings::default() };
        let photo = Item {
            kind: ItemKind::Photo,
            direct_url: Some("https://p/1.jpg".into()),
            ext: "jpg".into(),
            ..yt_video(0)
        };
        let gif = Item { kind: ItemKind::Gif, playlist_index: Some(2), ..yt_video(2) };
        let items = vec![photo, yt_video(1), gif];

        // All three: one mixed folder next to Audio/Video, numbered by place in the post.
        let all = requests("seann - Just: testing", &items, &[0, 1, 2], Quality::Best, &s);
        let dir = std::path::PathBuf::from("C:/N/seann - Just testing");
        assert!(all.iter().all(|r| r.dir.as_ref() == Some(&dir)));
        assert_eq!(all.iter().map(|r| r.stem.clone().unwrap()).collect::<Vec<_>>(), ["01 Photo", "02 Video", "03 GIF"]);
        assert!(all[0].direct.is_some() && all[2].gif && all[2].quality == Quality::Best);

        // Video + GIF only: inside Video.
        let vids = requests("Post", &items, &[1, 2], Quality::P720, &s);
        assert_eq!(vids[0].dir.as_deref(), Some(std::path::Path::new("C:/N/Video/Post")));

        // One item: no folder of its own; a photo gets a name from the post.
        let one = requests("Post", &items, &[0], Quality::Best, &s);
        assert_eq!(one[0].dir, None);
        assert_eq!(one[0].stem.as_deref(), Some("Post (Photo 1)"));
        assert_eq!(crate::download::out_dir(&one[0], &s), std::path::PathBuf::from("C:/N/Photos"));
    }

    #[test]
    fn single_video_has_no_items() {
        assert!(from_ytdlp(&json!({"_type": "video", "formats": [{}]}), "u").is_none());
    }

    #[test]
    fn gallery_sites() {
        assert!(wants_gallery("https://x.com/a/status/1"));
        assert!(wants_gallery("https://www.facebook.com/share/p/1/"));
        assert!(!wants_gallery("https://www.youtube.com/watch?v=1"));
    }
}
