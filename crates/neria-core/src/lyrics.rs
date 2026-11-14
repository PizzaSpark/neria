//! Lyrics for songs, from these sources in turn:
//! 1. LRCLIB: synced, free and open, no account.
//! 2. Spotify's own synced lyrics (Musixmatch's): opt-in, needs a Spotify sign-in, see
//!    [`spotify_lyrics`]. Off by default: it's against Spotify's terms.
//! 3. Genius: plain text only.
//!
//! Everything is in this file: no keys, tokens or code fetched from third parties at
//! runtime. Genius is reached through the same search its website uses (no API token), as
//! the `syncedlyrics` project does (MIT, © 2022 Momo); the page reading follows spotDL's
//! `providers/lyrics/genius.py` (MIT). Musixmatch without an account and AZLyrics were
//! looked at and left out: see `docs/UPSTREAM.md` ("Lyrics sources").

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::spotify::Track;
use crate::web;
use crate::ytmusic::{normalize, normalize_title, title_similarity};

/// One lyrics lookup never holds a song up for long: each request gets this long.
const TIMEOUT: Duration = Duration::from_secs(10);

pub const LRCLIB: &str = "LRCLIB";
pub const GENIUS: &str = "Genius";
pub const SPOTIFY: &str = "Spotify";

/// Which sources to ask, and whether plain text is good enough (Settings → Spotify).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub lrclib: bool,
    /// Spotify's own synced lyrics, with the user's Spotify sign-in (opt-in).
    pub spotify: bool,
    pub genius: bool,
    /// Only synced lyrics count: plain text is never saved.
    pub synced_only: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { lrclib: true, spotify: false, genius: true, synced_only: false }
    }
}

impl Prefs {
    pub fn from_settings(s: &crate::settings::Settings) -> Self {
        Self {
            lrclib: s.lyrics_lrclib,
            spotify: s.lyrics_spotify,
            genius: s.lyrics_genius,
            synced_only: s.lyrics_synced_only,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Lyrics {
    /// LRC text (`[mm:ss.xx]line`).
    pub synced: Option<String>,
    pub plain: Option<String>,
    /// Where they came from (`LRCLIB`, `Genius`, `Musixmatch`…).
    pub source: String,
    /// The source says the song has no words.
    pub instrumental: bool,
}

impl Lyrics {
    /// Plain text: the source's own, or the synced lines without their times.
    pub fn text(&self) -> Option<String> {
        self.plain.clone().or_else(|| self.synced.as_deref().map(plain_from_synced))
    }
}

/// What a lookup came back with.
#[derive(Clone, Debug, Default)]
pub struct Found {
    pub lyrics: Option<Lyrics>,
    /// Only plain text existed and "synced only" is on.
    pub plain_skipped: bool,
    /// A source that couldn't be asked (e.g. the Spotify sign-in ran out).
    pub problem: Option<String>,
}

impl Found {
    /// What the song's row says about its lyrics.
    pub fn status(&self) -> String {
        let main = match &self.lyrics {
            Some(l) if l.instrumental => "instrumental, no lyrics".into(),
            Some(l) if l.synced.is_some() => format!("synced lyrics from {}", l.source),
            Some(l) => format!("lyrics from {}, plain text only", l.source),
            None if self.plain_skipped => "no synced lyrics found".into(),
            None => "no lyrics found".into(),
        };
        match &self.problem {
            Some(p) if self.lyrics.as_ref().is_none_or(|l| l.synced.is_none()) => format!("{main} ({p})"),
            _ => main,
        }
    }
}

/// Ask each source in turn; synced lyrics end the search. Plain text found on the way is
/// kept rather than asking Genius for the same thing.
pub fn fetch(track: &Track, album: Option<&str>, prefs: Prefs) -> Found {
    let mut found = Found::default();
    let mut plain: Option<Lyrics> = None;
    if prefs.lrclib
        && let Some(l) = lrclib(track, album)
    {
        if l.synced.is_some() || l.instrumental {
            found.lyrics = Some(l);
            return found;
        }
        plain = Some(l);
    }
    if prefs.spotify && !track.id.is_empty() {
        match spotify_lyrics::fetch(&track.id) {
            Ok(Some(l)) if l.synced.is_some() => {
                found.lyrics = Some(l);
                return found;
            }
            Ok(Some(l)) => plain = plain.or(Some(l)),
            Ok(None) => {}
            Err(e) => found.problem = Some(e),
        }
    }
    if prefs.synced_only {
        found.plain_skipped = plain.is_some();
        return found;
    }
    if plain.is_none() && prefs.genius {
        plain = genius(track);
    }
    found.lyrics = plain;
    found
}

fn get(url: &str) -> Option<String> {
    let resp = web::client().ok()?.get(url).timeout(TIMEOUT).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    Some(String::from_utf8_lossy(&resp.bytes().ok()?).into_owned())
}

// ───────────── LRCLIB ─────────────

fn lrclib_entry(v: &Value) -> Option<Lyrics> {
    if v["instrumental"].as_bool() == Some(true) {
        return Some(Lyrics { source: LRCLIB.into(), instrumental: true, ..Default::default() });
    }
    let text = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let l =
        Lyrics { synced: text("syncedLyrics"), plain: text("plainLyrics"), source: LRCLIB.into(), instrumental: false };
    (l.synced.is_some() || l.plain.is_some()).then_some(l)
}

/// lrclib.net (free, open, no key): the exact lookup first (album + duration), then a search
/// within 3 s of the song's length, synced preferred.
pub fn lrclib(track: &Track, album: Option<&str>) -> Option<Lyrics> {
    let (artist, title) = (web::encode(track.main_artist()), web::encode(&normalize_title(&track.title)));
    let secs = track.duration_secs().round() as u64;
    if let Some(album) = album {
        let url = format!(
            "https://lrclib.net/api/get?artist_name={artist}&track_name={title}&album_name={}&duration={secs}",
            web::encode(album)
        );
        if let Some(l) = get(&url).and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| lrclib_entry(&v)) {
            return Some(l);
        }
    }
    let url = format!("https://lrclib.net/api/search?artist_name={artist}&track_name={title}");
    let results: Value = serde_json::from_str(&get(&url)?).ok()?;
    let found: Vec<Lyrics> = results
        .as_array()?
        .iter()
        .filter(|r| (r["duration"].as_f64().unwrap_or(0.0) - track.duration_secs()).abs() <= 3.0)
        .filter_map(lrclib_entry)
        .collect();
    // Synced beats plain; a lone "instrumental" only counts when nothing has words.
    let rank = |l: &Lyrics| {
        if l.synced.is_some() {
            2
        } else if l.plain.is_some() {
            1
        } else {
            0
        }
    };
    found.into_iter().max_by_key(rank)
}

// ───────────── Genius (plain text) ─────────────

/// Is this Genius hit the song we want? Genius also lists translations ("Genius English
/// Translations"), remixes and covers, so both the artist and the title have to agree.
fn genius_hit_matches(track: &Track, artist_names: &str, title: &str) -> bool {
    let (want, got) = (normalize(track.main_artist()), normalize(artist_names));
    let artist_ok = !want.is_empty() && (got.contains(&want) || (!got.is_empty() && want.contains(&got)));
    artist_ok && title_similarity(&normalize_title(&track.title), &normalize_title(title)) >= 0.7
}

/// genius.com, through the search its own website uses (no token), then the song page.
pub fn genius(track: &Track) -> Option<Lyrics> {
    let q = format!("{} {}", normalize_title(&track.title), track.main_artist());
    let url = format!("https://genius.com/api/search/multi?per_page=5&q={}", web::encode(&q));
    let v: Value = serde_json::from_str(&get(&url)?).ok()?;
    let page = v["response"]["sections"]
        .as_array()?
        .iter()
        .flat_map(|s| s["hits"].as_array().into_iter().flatten())
        .filter(|h| h["type"] == "song")
        .map(|h| &h["result"])
        .find(|r| {
            r["lyrics_state"].as_str() != Some("unreleased")
                && genius_hit_matches(
                    track,
                    r["artist_names"].as_str().or(r["primary_artist"]["name"].as_str()).unwrap_or(""),
                    r["title"].as_str().unwrap_or(""),
                )
        })?["url"]
        .as_str()?
        .to_string();
    if !page.starts_with("https://genius.com/") {
        return None;
    }
    let text = genius_page_lyrics(&get(&page)?)?;
    Some(Lyrics { plain: Some(text), source: GENIUS.into(), ..Default::default() })
}

/// The lyrics on a Genius song page: every `data-lyrics-container` block, without the
/// header Genius puts inside the first one (contributors, translations).
pub fn genius_page_lyrics(html: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("data-lyrics-container=\"true\"") {
        let after = &rest[i..];
        let Some(open_end) = after.find('>') else { break };
        let inner = &after[open_end + 1..];
        let (text, used) = html_text(inner);
        parts.push(text);
        rest = &inner[used..];
    }
    let joined = parts.join("\n");
    // Blank line before each "[Verse]" header, none at the edges, at most one in a row.
    let mut out: Vec<&str> = Vec::new();
    for line in joined.lines().map(str::trim) {
        if line.starts_with('[') && out.last().is_some_and(|l| !l.is_empty()) {
            out.push("");
        }
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    let text = out.join("\n");
    (!text.trim().is_empty()).then_some(text)
}

/// Text of the element whose content starts at `html`, up to its closing tag: `<br>` becomes
/// a line break, tags go, entities are decoded and elements marked
/// `data-exclude-from-selection` are skipped. Returns the text and the bytes read.
fn html_text(html: &str) -> (String, usize) {
    const VOID: &[&str] = &["br", "img", "hr", "input", "meta", "link", "wbr", "source"];
    let mut out = String::new();
    let mut depth = 0i32;
    let mut skip_from: Option<i32> = None;
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < html.len() {
        if bytes[i] == b'<' {
            let Some(len) = html[i..].find('>') else { break };
            let tag = &html[i + 1..i + len];
            i += len + 1;
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if tag.starts_with('!') {
                continue;
            }
            if tag.starts_with('/') {
                if depth == 0 {
                    return (decode_entities(&out), i);
                }
                depth -= 1;
                if skip_from.is_some_and(|d| depth < d) {
                    skip_from = None;
                }
                continue;
            }
            if name == "br" && skip_from.is_none() {
                out.push('\n');
            }
            if VOID.contains(&name.as_str()) || tag.ends_with('/') {
                continue;
            }
            depth += 1;
            if skip_from.is_none() && tag.contains("data-exclude-from-selection=\"true\"") {
                skip_from = Some(depth);
            }
        } else {
            let next = html[i..].find('<').map(|n| i + n).unwrap_or(html.len());
            if skip_from.is_none() {
                out.push_str(&html[i..next]);
            }
            i = next;
        }
    }
    (decode_entities(&out), html.len())
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let end = after.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let ent = &after[1..e];
            let c = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                    u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
                }
                _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            c.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &after[used..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// LRC lines without their `[mm:ss.xx]` times (and without `[ar:…]`-style tags).
pub fn plain_from_synced(lrc: &str) -> String {
    lrc.lines()
        .map(|line| {
            let mut l = line.trim();
            while l.starts_with('[') {
                match l.find(']') {
                    Some(e) => l = l[e + 1..].trim_start(),
                    None => break,
                }
            }
            l
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

// ───────────── Spotify's own synced lyrics (opt-in) ─────────────

/// Spotify's synced lyrics (licensed from Musixmatch), read the way its web player does,
/// with the user's own sign-in (the `sp_dc` cookie from Settings → Spotify → Sign in).
///
/// Opt-in only and off by default: it breaks Spotify's terms and could get the account
/// restricted, so Neria tells people to use a spare account. Used for lyrics only, never for
/// audio or metadata. Idea from akashrchandran/spotify-lyrics-api (GPL, ideas only, no code);
/// unlike it, the TOTP secret is read from Spotify's own web-player script, not from a
/// third-party repo. The cookie and the token stay in memory and are never logged.
pub mod spotify_lyrics {
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use hmac::{Hmac, Mac};
    use serde_json::Value;
    use sha1::Sha1;

    use super::{Lyrics, SPOTIFY, TIMEOUT};
    use crate::web;

    pub const SITE: &str = "spotify.com";
    const RAN_OUT: &str = "Spotify sign-in ran out: sign in again in Settings → Spotify";
    const CHANGED: &str = "Spotify changed its web player; Spotify lyrics need a Neria update";

    struct Token {
        value: String,
        expires_ms: u64,
    }

    #[derive(Default)]
    struct State {
        /// (HMAC key, version) from the web-player script.
        secret: Option<(Vec<u8>, u32)>,
        token: Option<Token>,
        /// Why the source is off until Neria restarts (or the user signs in again).
        stopped: Option<String>,
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);

    /// Drop the cached token (signed out, or signed in again).
    pub fn forget() {
        *STATE.lock().unwrap() = None;
    }

    pub fn signed_in() -> bool {
        crate::cookies::value(SITE, "sp_dc").is_some()
    }

    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
    }

    /// RFC 6238 TOTP: HMAC-SHA1, 30 s steps, 6 digits.
    pub fn totp(key: &[u8], unix_secs: u64) -> String {
        let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("any key length");
        mac.update(&(unix_secs / 30).to_be_bytes());
        let h = mac.finalize().into_bytes();
        let o = (h[19] & 0x0f) as usize;
        let bin =
            (u32::from(h[o]) & 0x7f) << 24 | u32::from(h[o + 1]) << 16 | u32::from(h[o + 2]) << 8 | u32::from(h[o + 3]);
        format!("{:06}", bin % 1_000_000)
    }

    /// The web player's key from a `secret` string: each character's code XOR (i % 33 + 9),
    /// the numbers written out one after another, and that text's bytes are the key.
    pub fn key_from_secret(secret: &str) -> Vec<u8> {
        secret
            .chars()
            .enumerate()
            .map(|(i, c)| (c as u32 ^ ((i as u32 % 33) + 9)).to_string())
            .collect::<String>()
            .into_bytes()
    }

    /// The first `{secret:'…',version:N}` in the web-player script (the one it uses; the
    /// script has other `{secret:` objects, from its TOTP library, without a string).
    pub fn secret_in_script(js: &str) -> Option<(String, u32)> {
        js.match_indices("{secret:").find_map(|(i, m)| secret_at(&js[i + m.len()..]))
    }

    fn secret_at(rest: &str) -> Option<(String, u32)> {
        let quote = rest.chars().next().filter(|q| *q == '\'' || *q == '"')?;
        let mut secret = String::new();
        let mut chars = rest[1..].char_indices();
        let mut end = None;
        while let Some((j, c)) = chars.next() {
            match c {
                '\\' => {
                    let (_, e) = chars.next()?;
                    secret.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                }
                c if c == quote => {
                    end = Some(1 + j + c.len_utf8());
                    break;
                }
                c => secret.push(c),
            }
        }
        let after = rest[end?..].strip_prefix(",version:")?;
        let version = after.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()?;
        Some((secret, version))
    }

    fn get(url: &str, headers: &[(&str, String)]) -> Result<(u16, String), String> {
        let client = web::client().map_err(|_| "no network".to_string())?;
        let mut req = client.get(url).timeout(TIMEOUT);
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let resp = req.send().map_err(|_| "couldn't reach Spotify".to_string())?;
        let status = resp.status().as_u16();
        let body = resp.bytes().map_err(|_| "couldn't reach Spotify".to_string())?;
        Ok((status, String::from_utf8_lossy(&body).into_owned()))
    }

    /// The TOTP key and its version, read from Spotify's own web-player script.
    pub fn load_secret() -> Result<(Vec<u8>, u32), String> {
        let (_, home) = get("https://open.spotify.com/", &[])?;
        let script = home
            .split('"')
            .find(|s| {
                s.starts_with("https://open.spotifycdn.com/cdn/build/web-player/web-player.") && s.ends_with(".js")
            })
            .ok_or(CHANGED)?
            .to_string();
        let (_, js) = get(&script, &[])?;
        let (secret, version) = secret_in_script(&js).ok_or(CHANGED)?;
        Ok((key_from_secret(&secret), version))
    }

    /// A web-player access token: (token, expiry in Unix ms, anonymous). `sp_dc` None asks
    /// for an anonymous one (the `lyrics` example uses that to check the TOTP without an
    /// account).
    pub fn token(key: &[u8], version: u32, sp_dc: Option<&str>) -> Result<(String, u64, bool), String> {
        let server = get("https://open.spotify.com/api/server-time", &[])
            .ok()
            .and_then(|(_, t)| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["serverTime"].as_u64());
        let url = format!(
            "https://open.spotify.com/api/token?reason=init&productType=web-player&totp={}&totpServer={}&totpVer={version}",
            totp(key, now_ms() / 1000),
            server.map_or_else(|| "unavailable".to_string(), |s| totp(key, s)),
        );
        let headers: Vec<(&str, String)> = sp_dc.map(|c| ("cookie", format!("sp_dc={c}"))).into_iter().collect();
        let (status, body) = get(&url, &headers)?;
        let v: Value = serde_json::from_str(&body).map_err(|_| format!("Spotify said HTTP {status}"))?;
        let token = v["accessToken"].as_str().filter(|t| !t.is_empty()).ok_or_else(|| match status {
            429 => "Spotify asked Neria to slow down".to_string(),
            _ if v["totpVerExpired"].is_string() => CHANGED.to_string(),
            _ => format!("Spotify said HTTP {status}"),
        })?;
        let expires = v["accessTokenExpirationTimestampMs"].as_u64().unwrap_or(now_ms() + 30 * 60 * 1000);
        Ok((token.to_string(), expires, v["isAnonymous"].as_bool().unwrap_or(true)))
    }

    /// Lyrics from Spotify's `color-lyrics` response.
    pub fn parse(body: &str) -> Option<Lyrics> {
        let v: Value = serde_json::from_str(body).ok()?;
        let l = &v["lyrics"];
        let lines = l["lines"].as_array()?;
        let source = l["providerDisplayName"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|p| format!("{p} via {SPOTIFY}"))
            .unwrap_or_else(|| SPOTIFY.to_string());
        let words = |line: &Value| line["words"].as_str().unwrap_or("").replace('♪', "").trim().to_string();
        let plain: Vec<String> = lines.iter().map(words).collect();
        if plain.iter().all(String::is_empty) {
            return None;
        }
        let synced = (l["syncType"].as_str() == Some("LINE_SYNCED")).then(|| {
            lines
                .iter()
                .map(|line| {
                    let ms = line["startTimeMs"]
                        .as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .or(line["startTimeMs"].as_u64())
                        .unwrap_or(0);
                    format!("[{:02}:{:02}.{:02}]{}", ms / 60_000, ms / 1000 % 60, ms % 1000 / 10, words(line))
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        Some(Lyrics { synced, plain: Some(plain.join("\n").trim().to_string()), source, instrumental: false })
    }

    fn stop(st: &mut State, why: &str) -> String {
        st.stopped = Some(why.to_string());
        why.to_string()
    }

    /// Spotify's lyrics for a track id. `Err` says why the source can't be used; after a
    /// refusal it stays off for this session, so a playlist doesn't keep knocking.
    pub fn fetch(track_id: &str) -> Result<Option<Lyrics>, String> {
        let mut guard = STATE.lock().unwrap();
        let st = guard.get_or_insert_with(State::default);
        if let Some(why) = &st.stopped {
            return Err(why.clone());
        }
        let Some(sp_dc) = crate::cookies::value(SITE, "sp_dc") else {
            return Err("Spotify lyrics are on but you're not signed in to Spotify".into());
        };
        if !st.token.as_ref().is_some_and(|t| t.expires_ms > now_ms() + 60_000) {
            if st.secret.is_none() {
                st.secret = Some(load_secret()?);
            }
            let (key, version) = st.secret.clone().unwrap_or_default();
            match token(&key, version, Some(&sp_dc)) {
                Ok((_, _, true)) => return Err(stop(st, RAN_OUT)),
                Ok((value, expires_ms, false)) => st.token = Some(Token { value, expires_ms }),
                Err(e) => return Err(stop(st, &e)),
            }
        }
        let token = st.token.as_ref().map(|t| t.value.clone()).unwrap_or_default();
        let url =
            format!("https://spclient.wg.spotify.com/color-lyrics/v2/track/{track_id}?format=json&market=from_token");
        let (status, body) =
            get(&url, &[("app-platform", "WebPlayer".into()), ("authorization", format!("Bearer {token}"))])?;
        match status {
            200 => Ok(parse(&body)),
            404 => Ok(None),
            401 | 403 => Err(stop(st, RAN_OUT)),
            429 => Err(stop(st, "Spotify asked Neria to slow down; Spotify lyrics paused until Neria restarts")),
            s => Err(format!("Spotify lyrics: HTTP {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_matches_rfc_6238() {
        // RFC 6238 appendix B (SHA-1 key), last 6 digits.
        let key = b"12345678901234567890";
        assert_eq!(spotify_lyrics::totp(key, 59), "287082");
        assert_eq!(spotify_lyrics::totp(key, 1111111109), "081804");
        assert_eq!(spotify_lyrics::totp(key, 20000000000), "353130");
    }

    #[test]
    fn reads_web_player_secret() {
        let js = r#"{secret:e,algorithm:t}x;let ej=[{secret:'OmE{ZA.J^":0FG\\Uz?[@WW',version:60},{secret:"abc",version:59}].map(e=>"#;
        assert_eq!(spotify_lyrics::secret_in_script(js), Some((r#"OmE{ZA.J^":0FG\Uz?[@WW"#.to_string(), 60)));
        // 'a' (97) ^ 9 = 104, 'b' (98) ^ 10 = 104, 'c' (99) ^ 11 = 104.
        assert_eq!(spotify_lyrics::key_from_secret("abc"), b"104104104".to_vec());
    }

    #[test]
    fn reads_spotify_lyrics() {
        let body = r#"{"lyrics":{"syncType":"LINE_SYNCED","lines":[{"startTimeMs":"960","words":"Like the legend","endTimeMs":"0"},{"startTimeMs":"65230","words":"♪","endTimeMs":"0"}],"provider":"MusixMatch","providerDisplayName":"Musixmatch"}}"#;
        let l = spotify_lyrics::parse(body).unwrap();
        assert_eq!(l.synced.as_deref(), Some("[00:00.96]Like the legend\n[01:05.23]"));
        assert_eq!(l.source, "Musixmatch via Spotify");
        let unsynced = r#"{"lyrics":{"syncType":"UNSYNCED","lines":[{"startTimeMs":"0","words":"Hello"}]}}"#;
        let l = spotify_lyrics::parse(unsynced).unwrap();
        assert!(l.synced.is_none());
        assert_eq!(l.plain.as_deref(), Some("Hello"));
    }

    #[test]
    fn reads_genius_page() {
        let html = r#"<div id="x"><div data-lyrics-container="true" class="Lyrics__Container">
<div data-exclude-from-selection="true" class="LyricsHeader"><span>249 Contributors</span><div>Translations</div></div>
[Verse 1: Pharrell]<br/>Like the legend of the <i>phoenix</i><br>All ends with beginnings</div>
<div class="ad">Advert</div><div data-lyrics-container="true" class="Lyrics__Container">[Chorus]<br/>We&#x27;re up all night &amp; more<br/></div></div>"#;
        assert_eq!(
            genius_page_lyrics(html).unwrap(),
            "[Verse 1: Pharrell]\nLike the legend of the phoenix\nAll ends with beginnings\n\n[Chorus]\nWe're up all night & more"
        );
        assert!(genius_page_lyrics("<div>no lyrics here</div>").is_none());
    }

    #[test]
    fn genius_hits_must_be_the_song() {
        let t = Track {
            title: "Get Lucky (feat. Pharrell Williams)".into(),
            artists: vec!["Daft Punk".into()],
            ..Default::default()
        };
        assert!(genius_hit_matches(&t, "Daft Punk (Ft. Nile Rodgers & Pharrell Williams)", "Get Lucky"));
        assert!(!genius_hit_matches(&t, "Genius English Translations", "Daft Punk - Get Lucky (English Translation)"));
        assert!(!genius_hit_matches(&t, "Daft Punk", "Around the World"));
    }

    #[test]
    fn strips_lrc_times() {
        assert_eq!(plain_from_synced("[ar:X]\n[00:12.00]Hello\n[00:14.50][01:10.00]Again\n"), "Hello\nAgain");
    }

    #[test]
    fn status_lines() {
        assert_eq!(Found::default().status(), "no lyrics found");
        assert_eq!(Found { plain_skipped: true, ..Default::default() }.status(), "no synced lyrics found");
        let l = Lyrics { plain: Some("x".into()), source: GENIUS.into(), ..Default::default() };
        assert_eq!(Found { lyrics: Some(l), ..Default::default() }.status(), "lyrics from Genius, plain text only");
        let l = Lyrics { synced: Some("[00:01.00]x".into()), source: LRCLIB.into(), ..Default::default() };
        let f = Found { lyrics: Some(l), problem: Some("sign in again".into()), ..Default::default() };
        assert_eq!(f.status(), "synced lyrics from LRCLIB");
        let f = Found { problem: Some("Spotify sign-in ran out".into()), ..Default::default() };
        assert_eq!(f.status(), "no lyrics found (Spotify sign-in ran out)");
    }
}
