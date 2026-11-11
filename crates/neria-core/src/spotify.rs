//! Reading Spotify links: track / album / playlist / artist metadata.
//!
//! Default: Spotify's public embed pages (no login, no API key; the approach
//! Sunnify uses). Limits: playlists show at most 100 tracks, artists only their
//! top 10, and playlist tracks carry no album name (YouTube Music fills it in).
//!
//! Optional: the user's *own* Spotify app credentials (Client ID + secret) use
//! the Web API for complete data; if that fails we fall back to the embed page.
//!
//! Audio never comes from Spotify. See `ytmusic` for matching.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::web;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Track,
    Album,
    Playlist,
    Artist,
}

impl Kind {
    fn path(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Playlist => "playlist",
            Self::Artist => "artist",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Track => "Song",
            Self::Album => "Album",
            Self::Playlist => "Playlist",
            Self::Artist => "Artist",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub kind: Kind,
    pub id: String,
}

fn is_spotify_id(s: &str) -> bool {
    s.len() == 22 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Recognise `https://open.spotify.com/(intl-xx/)(embed/)track/<id>?si=…` and `spotify:track:<id>`.
pub fn parse_link(input: &str) -> Option<Link> {
    let s = input.trim();
    let (kind, id) = if let Some(rest) = s.strip_prefix("spotify:") {
        let mut parts = rest.split(':');
        (parts.next()?, parts.next()?.to_string())
    } else {
        let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"))?;
        let (host, path) = rest.split_once('/')?;
        if !matches!(host, "open.spotify.com" | "play.spotify.com") {
            return None;
        }
        let path = path.split(['?', '#']).next()?;
        let mut segs = path.split('/').filter(|p| !p.is_empty() && !p.starts_with("intl-") && *p != "embed");
        (segs.next()?, segs.next()?.to_string())
    };
    let kind = match kind {
        "track" => Kind::Track,
        "album" => Kind::Album,
        "playlist" => Kind::Playlist,
        "artist" => Kind::Artist,
        _ => return None,
    };
    is_spotify_id(&id).then_some(Link { kind, id })
}

/// For a Spotify link Neria can't handle (podcast, audiobook, profile, short
/// link…), say why in plain words. `None` if it isn't a Spotify link or it's
/// one [`parse_link`] accepts. Returns (what's wrong, what to do).
pub fn unsupported_reason(input: &str) -> Option<(&'static str, &'static str)> {
    const WORKS: &str = "Neria downloads Spotify songs, albums, playlists and artists' top songs.";
    let s = input.trim();
    if parse_link(s).is_some() {
        return None;
    }
    let kind = if let Some(rest) = s.strip_prefix("spotify:") {
        rest.split(':').next().unwrap_or("")
    } else {
        let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = host.to_ascii_lowercase();
        if matches!(host.as_str(), "spotify.link" | "spotify.app.link") {
            return Some((
                "Short Spotify links can't be opened here",
                "Open it in your browser, then copy the full open.spotify.com link and paste that.",
            ));
        }
        if !(host == "spotify.com" || host.ends_with(".spotify.com")) {
            return None;
        }
        let path = path.split(['?', '#']).next().unwrap_or("");
        path.split('/').find(|p| !p.is_empty() && !p.starts_with("intl-") && *p != "embed").unwrap_or("")
    };
    Some(match kind {
        "episode" | "show" => ("Neria can't download Spotify podcasts", WORKS),
        "audiobook" | "chapter" => ("Neria can't download Spotify audiobooks", WORKS),
        "track" | "album" | "playlist" | "artist" => {
            ("This Spotify link looks incomplete", "Copy the link again from Spotify (Share → Copy link).")
        }
        "user" => {
            ("Spotify profiles can't be downloaded", "Open one of their public playlists and paste that link instead.")
        }
        "collection" => (
            "Your Liked Songs can't be read without a login",
            "Add them to a public playlist and paste that link instead.",
        ),
        _ => ("This kind of Spotify link isn't supported", WORKS),
    })
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_number: Option<u32>,
    /// `YYYY-MM-DD` or `YYYY`.
    pub release_date: Option<String>,
    pub duration_ms: u64,
    pub cover_url: Option<String>,
    pub isrc: Option<String>,
    /// Spotify's public 30 s preview MP3 (used to verify the YouTube Music match).
    pub preview_url: Option<String>,
}

impl Track {
    pub fn artist_line(&self) -> String {
        self.artists.join(", ")
    }

    pub fn main_artist(&self) -> &str {
        self.artists.first().map(String::as_str).unwrap_or("")
    }

    pub fn duration_secs(&self) -> f64 {
        self.duration_ms as f64 / 1000.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Embed,
    Api,
}

#[derive(Clone, Debug)]
pub struct Collection {
    pub kind: Kind,
    pub id: String,
    pub name: String,
    pub owner: Option<String>,
    pub cover_url: Option<String>,
    pub tracks: Vec<Track>,
    /// The public page only showed part of it (playlists > 100 tracks, artists).
    pub truncated: bool,
    pub source: Source,
    /// Something the user should know (e.g. the API key failed, used the public page).
    pub notice: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
}

/// Fetch a link's metadata: the user's API key first (if any), else the public embed page.
pub fn fetch(link: &Link, creds: Option<&Credentials>) -> Result<Collection> {
    if let Some(c) = creds.filter(|c| !c.client_id.trim().is_empty() && !c.client_secret.trim().is_empty()) {
        match api::fetch(link, c) {
            Ok(col) => return Ok(col),
            Err(e) => {
                let mut col = embed_fetch(link)?;
                col.notice = Some(format!("Your Spotify key didn't work ({e}), so the public page was used instead."));
                return Ok(col);
            }
        }
    }
    embed_fetch(link)
}

/// Cover art and release date for one track (used for playlist tracks, which lack them).
pub fn track_details(id: &str) -> Result<Track> {
    let col = embed_fetch(&Link { kind: Kind::Track, id: id.to_string() })?;
    col.tracks.into_iter().next().ok_or_else(|| anyhow!("track not found"))
}

// ───────────── Public embed pages ─────────────

fn embed_fetch(link: &Link) -> Result<Collection> {
    let url = format!("https://open.spotify.com/embed/{}/{}", link.kind.path(), link.id);
    let html = web::get_text(&url).context("couldn't reach Spotify")?;
    let entity = embed_entity(&html)?;
    parse_embed(link, &entity)
}

fn embed_entity(html: &str) -> Result<Value> {
    const START: &str = r#"<script id="__NEXT_DATA__" type="application/json">"#;
    let start = html.find(START).ok_or_else(|| anyhow!("Spotify page format changed (no data block)"))? + START.len();
    let end = html[start..].find("</script>").ok_or_else(|| anyhow!("Spotify page format changed"))? + start;
    let data: Value = serde_json::from_str(&html[start..end])?;
    let entity = data.pointer("/props/pageProps/state/data/entity").cloned().unwrap_or(Value::Null);
    if entity.is_null() {
        bail!("This Spotify link is private, removed or not available.");
    }
    Ok(entity)
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Largest image from `visualIdentity.image` or `coverArt.sources`.
fn embed_cover(entity: &Value) -> Option<String> {
    let lists = [entity.pointer("/visualIdentity/image"), entity.pointer("/coverArt/sources")];
    lists
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|img| {
            let w = img.get("maxWidth").or_else(|| img.get("width")).and_then(Value::as_u64).unwrap_or(0);
            Some((w, img.get("url")?.as_str()?.to_string()))
        })
        .max_by_key(|(w, _)| *w)
        .map(|(_, url)| url)
}

fn embed_date(entity: &Value) -> Option<String> {
    entity
        .pointer("/releaseDate/isoString")
        .and_then(Value::as_str)
        .filter(|s| s.len() >= 4)
        .map(|s| s.get(..10).unwrap_or(s).to_string())
}

/// "Daft Punk,\u{a0}Pharrell Williams" -> ["Daft Punk", "Pharrell Williams"]
fn split_artists(s: &str) -> Vec<String> {
    s.split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect()
}

fn preview_of(v: &Value) -> Option<String> {
    v.pointer("/audioPreview/url").and_then(Value::as_str).filter(|u| u.starts_with("https://")).map(str::to_string)
}

fn id_from_uri(uri: &str) -> String {
    uri.rsplit(':').next().unwrap_or("").to_string()
}

fn parse_embed(link: &Link, e: &Value) -> Result<Collection> {
    let name = str_of(e, "name").or_else(|| str_of(e, "title")).unwrap_or_else(|| "Untitled".into());
    let cover = embed_cover(e);
    let date = embed_date(e);

    if link.kind == Kind::Track {
        let artists: Vec<String> = e
            .get("artists")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| str_of(x, "name")).collect())
            .unwrap_or_default();
        let track = Track {
            id: link.id.clone(),
            title: name.clone(),
            artists,
            release_date: date,
            duration_ms: e.get("duration").and_then(Value::as_u64).unwrap_or(0),
            cover_url: cover.clone(),
            preview_url: preview_of(e),
            ..Default::default()
        };
        return Ok(Collection {
            kind: Kind::Track,
            id: link.id.clone(),
            name,
            owner: Some(track.artist_line()),
            cover_url: cover,
            tracks: vec![track],
            truncated: false,
            source: Source::Embed,
            notice: None,
        });
    }

    let owner = str_of(e, "subtitle");
    let list = e.get("trackList").and_then(Value::as_array).cloned().unwrap_or_default();
    let total = list.len() as u32;
    let tracks: Vec<Track> = list
        .iter()
        .enumerate()
        .filter(|(_, t)| t.get("isPlayable").and_then(Value::as_bool).unwrap_or(true))
        .filter_map(|(i, t)| {
            let title = str_of(t, "title")?;
            let mut track = Track {
                id: t.get("uri").and_then(Value::as_str).map(id_from_uri).unwrap_or_default(),
                title,
                artists: str_of(t, "subtitle").map(|s| split_artists(&s)).unwrap_or_default(),
                duration_ms: t.get("duration").and_then(Value::as_u64).unwrap_or(0),
                preview_url: preview_of(t),
                ..Default::default()
            };
            if link.kind == Kind::Album {
                track.album = Some(name.clone());
                track.album_artist = owner.clone();
                track.track_number = Some(i as u32 + 1);
                track.track_total = Some(total);
                track.release_date = date.clone();
                track.cover_url = cover.clone();
            }
            Some(track)
        })
        .collect();

    Ok(Collection {
        kind: link.kind,
        id: link.id.clone(),
        name,
        owner,
        cover_url: cover,
        truncated: match link.kind {
            Kind::Playlist => total >= 100,
            Kind::Artist => true,
            _ => false,
        },
        tracks,
        source: Source::Embed,
        notice: None,
    })
}

// ───────────── Web API with the user's own key ─────────────

mod api {
    use super::*;

    const API: &str = "https://api.spotify.com/v1";

    fn token(c: &Credentials) -> Result<String> {
        let resp = web::client()?
            .post("https://accounts.spotify.com/api/token")
            .basic_auth(c.client_id.trim(), Some(c.client_secret.trim()))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials")
            .send()?;
        let status = resp.status();
        let v: Value = serde_json::from_slice(&resp.bytes()?).unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("{}", v["error_description"].as_str().unwrap_or(status.as_str()));
        }
        v["access_token"].as_str().map(str::to_string).ok_or_else(|| anyhow!("no access token"))
    }

    fn get(token: &str, url: &str) -> Result<Value> {
        let resp = web::client()?.get(url).bearer_auth(token).send()?;
        let status = resp.status();
        if status.as_u16() == 429 {
            bail!("rate-limited by Spotify");
        }
        if !status.is_success() {
            bail!("HTTP {status}");
        }
        Ok(serde_json::from_slice(&resp.bytes()?)?)
    }

    /// Follow a paging object's `next` links.
    fn all_items(token: &str, first: &Value) -> Result<Vec<Value>> {
        let mut items: Vec<Value> = first["items"].as_array().cloned().unwrap_or_default();
        let mut next = first["next"].as_str().map(str::to_string);
        while let Some(url) = next {
            let page = get(token, &url)?;
            items.extend(page["items"].as_array().cloned().unwrap_or_default());
            next = page["next"].as_str().map(str::to_string);
        }
        Ok(items)
    }

    fn largest_image(images: &Value) -> Option<String> {
        images
            .as_array()?
            .iter()
            .max_by_key(|i| i["width"].as_u64().unwrap_or(0))
            .and_then(|i| i["url"].as_str())
            .map(str::to_string)
    }

    /// Full track object (or simplified one + its album) -> Track.
    fn track(t: &Value, album: Option<&Value>) -> Option<Track> {
        let album = album.or_else(|| t.get("album"));
        Some(Track {
            id: t["id"].as_str()?.to_string(),
            title: t["name"].as_str()?.to_string(),
            artists: t["artists"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x["name"].as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            album: album.and_then(|a| a["name"].as_str()).map(str::to_string),
            album_artist: album.and_then(|a| a["artists"][0]["name"].as_str()).map(str::to_string),
            track_number: t["track_number"].as_u64().map(|n| n as u32),
            track_total: album.and_then(|a| a["total_tracks"].as_u64()).map(|n| n as u32),
            disc_number: t["disc_number"].as_u64().map(|n| n as u32),
            release_date: album.and_then(|a| a["release_date"].as_str()).map(str::to_string),
            duration_ms: t["duration_ms"].as_u64().unwrap_or(0),
            cover_url: album.and_then(|a| largest_image(&a["images"])),
            isrc: t["external_ids"]["isrc"].as_str().map(str::to_string),
            preview_url: t["preview_url"].as_str().map(str::to_string),
        })
    }

    pub fn fetch(link: &Link, c: &Credentials) -> Result<Collection> {
        let token = token(c)?;
        let id = &link.id;
        let (name, owner, cover, tracks) = match link.kind {
            Kind::Track => {
                let t = get(&token, &format!("{API}/tracks/{id}"))?;
                let tr = track(&t, None).ok_or_else(|| anyhow!("bad track data"))?;
                (tr.title.clone(), Some(tr.artist_line()), tr.cover_url.clone(), vec![tr])
            }
            Kind::Album => {
                let a = get(&token, &format!("{API}/albums/{id}"))?;
                let items = all_items(&token, &a["tracks"])?;
                let tracks = items.iter().filter_map(|t| track(t, Some(&a))).collect();
                (
                    a["name"].as_str().unwrap_or("Album").to_string(),
                    a["artists"][0]["name"].as_str().map(str::to_string),
                    largest_image(&a["images"]),
                    tracks,
                )
            }
            Kind::Playlist => {
                let p = get(&token, &format!("{API}/playlists/{id}"))?;
                let items = all_items(&token, &p["tracks"])?;
                let tracks = items
                    .iter()
                    .filter_map(|i| i.get("track").filter(|t| t["type"] == "track"))
                    .filter_map(|t| track(t, None))
                    .collect();
                (
                    p["name"].as_str().unwrap_or("Playlist").to_string(),
                    p["owner"]["display_name"].as_str().map(str::to_string),
                    largest_image(&p["images"]),
                    tracks,
                )
            }
            Kind::Artist => {
                let a = get(&token, &format!("{API}/artists/{id}"))?;
                let top = get(&token, &format!("{API}/artists/{id}/top-tracks?market=US"))?;
                let tracks = top["tracks"].as_array().into_iter().flatten().filter_map(|t| track(t, None)).collect();
                (a["name"].as_str().unwrap_or("Artist").to_string(), None, largest_image(&a["images"]), tracks)
            }
        };
        Ok(Collection {
            kind: link.kind,
            id: link.id.clone(),
            name,
            owner,
            cover_url: cover,
            tracks,
            truncated: link.kind == Kind::Artist,
            source: Source::Api,
            notice: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_links() {
        let want = Some(Link { kind: Kind::Track, id: "69kOkLUCkxIZYexIgSG8rq".into() });
        assert_eq!(parse_link("https://open.spotify.com/track/69kOkLUCkxIZYexIgSG8rq?si=abc123"), want);
        assert_eq!(parse_link("https://open.spotify.com/intl-de/track/69kOkLUCkxIZYexIgSG8rq"), want);
        assert_eq!(parse_link("spotify:track:69kOkLUCkxIZYexIgSG8rq"), want);
        assert_eq!(
            parse_link("https://open.spotify.com/embed/playlist/37i9dQZF1DXcBWIGoYBM5M").map(|l| l.kind),
            Some(Kind::Playlist)
        );
        assert_eq!(parse_link("https://www.youtube.com/watch?v=x"), None);
        assert_eq!(parse_link("https://open.spotify.com/show/69kOkLUCkxIZYexIgSG8rq"), None);
    }

    #[test]
    fn explains_unsupported_links() {
        let reason = |s| unsupported_reason(s).map(|r| r.0);
        assert_eq!(reason("https://open.spotify.com/track/69kOkLUCkxIZYexIgSG8rq"), None);
        assert_eq!(reason("https://www.youtube.com/watch?v=x"), None);
        assert_eq!(
            reason("https://open.spotify.com/episode/4rOoJ6Egrf8K2IrywzwOMk"),
            Some("Neria can't download Spotify podcasts")
        );
        assert_eq!(
            reason("https://open.spotify.com/intl-fr/show/4rOoJ6Egrf8K2IrywzwOMk?si=1"),
            Some("Neria can't download Spotify podcasts")
        );
        assert_eq!(reason("spotify:episode:4rOoJ6Egrf8K2IrywzwOMk"), Some("Neria can't download Spotify podcasts"));
        assert_eq!(
            reason("https://open.spotify.com/audiobook/4rOoJ6Egrf8K2IrywzwOMk"),
            Some("Neria can't download Spotify audiobooks")
        );
        assert_eq!(reason("https://spotify.link/ZCkfTK3Tdyb"), Some("Short Spotify links can't be opened here"));
        assert_eq!(reason("https://open.spotify.com/track/short"), Some("This Spotify link looks incomplete"));
        assert!(reason("https://open.spotify.com/").is_some());
    }

    #[test]
    fn parses_album_embed() {
        let e: Value = serde_json::json!({
            "name": "Random Access Memories",
            "subtitle": "Daft Punk",
            "releaseDate": { "isoString": "2013-05-17T00:00:00Z" },
            "visualIdentity": { "image": [
                { "url": "small", "maxWidth": 64 }, { "url": "big", "maxWidth": 640 }
            ]},
            "trackList": [
                { "uri": "spotify:track:0dEIca2nhcxDUV8C5QkPYb", "title": "Give Life Back to Music", "subtitle": "Daft Punk", "duration": 275386 },
                { "uri": "spotify:track:69kOkLUCkxIZYexIgSG8rq", "title": "Get Lucky (feat. Pharrell Williams and Nile Rodgers)",
                  "subtitle": "Daft Punk,\u{a0}Pharrell Williams,\u{a0}Nile Rodgers", "duration": 369626,
                  "audioPreview": { "url": "https://p.scdn.co/mp3-preview/abc" } }
            ]
        });
        let link = Link { kind: Kind::Album, id: "4m2880jivSbbyEGAKfITCa".into() };
        let c = parse_embed(&link, &e).unwrap();
        assert_eq!(c.tracks.len(), 2);
        let t = &c.tracks[1];
        assert_eq!(t.artists, vec!["Daft Punk", "Pharrell Williams", "Nile Rodgers"]);
        assert_eq!(t.album.as_deref(), Some("Random Access Memories"));
        assert_eq!(t.track_number, Some(2));
        assert_eq!(t.release_date.as_deref(), Some("2013-05-17"));
        assert_eq!(t.cover_url.as_deref(), Some("big"));
        assert_eq!(t.id, "69kOkLUCkxIZYexIgSG8rq");
        assert_eq!(t.preview_url.as_deref(), Some("https://p.scdn.co/mp3-preview/abc"));
        assert_eq!(c.tracks[0].preview_url, None);
        assert!(!c.truncated);
    }
}
