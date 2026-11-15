//! Twitch live streams without Twitch's ads.
//!
//! Twitch stitches ads into a live stream's playlist for viewers who aren't signed in
//! ("Amazon|…" pieces where the stream's would be, at the same times). The stream address
//! depends on the player that asked for it, and only some players get ads: in a check on
//! 2026-11-15 a fresh `site` token (what yt-dlp asks for) opened on a pre-roll while `embed`,
//! `popout`, `mobile_web` and `frontpage` had the stream at full quality. The idea (backup
//! player types, ad pieces known by "stitched") is from TTV-AB by GosuDRM,
//! <https://github.com/GosuDRM/TTV-AB> (baseline `1825fa5`, see `docs/UPSTREAM.md`); the code
//! here is Neria's own.
//!
//! A small relay on 127.0.0.1 serves one playlist per quality: each time it's asked, it reads
//! the current source's playlist and, when that has ad pieces, the next source's; pieces are
//! matched by their date-time (every player's playlist lists the same 2 s pieces at the same
//! times), so a switch adds no seam. When every source has the ad, its pieces are left out (a
//! gap, kept in `gaps` for the recording's tape) rather than recorded. The recorder's yt-dlp
//! and the player read the relay's address instead of twitch.tv; anything that fails falls
//! back to twitch.tv as before.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// Twitch's own web client id and the persisted query its player sends for a token.
const CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const TOKEN_QUERY: &str = "ed230aa1e33e07eebb8928504583da78a5173989fadfb1ac94be06a04f3cdbe9";
/// The players asked, in order: full quality without ads first, `site` (what twitch.tv
/// itself gets) after them, and the two that stop at 360p last.
const PLAYERS: [&str; 7] = ["embed", "popout", "mobile_web", "frontpage", "site", "autoplay", "picture-by-picture"];
/// Pieces kept in a served playlist (2 s each).
const WINDOW: usize = 15;

/// The channel of a twitch.tv live link (`twitch.tv/<login>`), not a video or a clip.
pub fn login_of(url: &str) -> Option<String> {
    let rest = url.split("//").nth(1).unwrap_or(url);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut parts = rest.split('/');
    let host = parts.next()?.to_ascii_lowercase();
    if !(host == "twitch.tv" || host == "www.twitch.tv" || host == "m.twitch.tv") {
        return None;
    }
    let login = parts.next()?.to_ascii_lowercase();
    let reserved = ["videos", "directory", "settings", "downloads", "p", "search", "subscriptions", "inventory"];
    let ok = !login.is_empty()
        && !reserved.contains(&login.as_str())
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    // twitch.tv/<login>/clip/… and /videos are not the live stream.
    (ok && parts.next().is_none_or(|p| p.is_empty())).then_some(login)
}

/// The relay's address for this link's live stream (its master playlist), or None to use
/// the link as it is: not a Twitch channel, or the relay can't reach the stream.
pub fn relay_url(url: &str) -> Option<String> {
    let login = login_of(url)?;
    let port = server()?;
    // Know it works before handing it out: the sources and their qualities.
    channel_master(&login).ok()?;
    Some(format!("http://127.0.0.1:{port}/ttv/{login}/master.m3u8"))
}

/// The relay's playlist of the best quality up to `max_height` (the player's preview), with
/// the master playlist's address; None as for `relay_url`.
pub fn relay_for_player(url: &str, max_height: u32) -> Option<(String, String)> {
    let master = relay_url(url)?;
    let text = channel_master(&login_of(url)?).ok()?;
    let mut best: Option<(u32, String)> = None;
    let mut height = 0;
    for line in text.lines() {
        if line.starts_with("#EXT-X-STREAM-INF:") {
            height = attr(line, "RESOLUTION").and_then(|r| r.split('x').nth(1)?.parse().ok()).unwrap_or(0);
        } else if line.starts_with("http")
            && height > 0
            && height <= max_height
            && best.as_ref().is_none_or(|(h, _)| height > *h)
        {
            best = Some((height, line.to_string()));
        }
    }
    Some((master, best.map(|(_, u)| u)?))
}

/// When the stream went live (Unix seconds), from Twitch's own playlist: its first piece's
/// date-time less how long the stream had run by then (`#EXT-X-TWITCH-ELAPSED-SECS`). yt-dlp
/// says it for twitch.tv, not for the relay's address.
pub fn started_at(url: &str) -> Option<f64> {
    let login = login_of(url)?;
    for p in players() {
        let Ok(v) = master_for(&login, &p) else { continue };
        let Ok(text) = crate::web::get_text(&v.last()?.url) else { continue };
        if let Some(s) = started_in(&text) {
            return Some(s);
        }
    }
    None
}

/// The stream's start from a media playlist (None for one that begins with an ad).
pub fn started_in(text: &str) -> Option<f64> {
    let elapsed: f64 = text.lines().find_map(|l| l.strip_prefix("#EXT-X-TWITCH-ELAPSED-SECS:"))?.trim().parse().ok()?;
    let first = pieces(text).into_iter().next()?;
    (!first.ad).then_some(first.at - elapsed)
}

/// Stretches left out because every source had the ad: (date-time it began, seconds).
pub fn gaps(url: &str) -> Vec<(f64, f64)> {
    let Some(login) = login_of(url) else { return Vec::new() };
    let st = state().lock().unwrap();
    st.get(&login).map(|c| c.gaps.clone()).unwrap_or_default()
}

// ── Playlists ──

#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    /// Its date-time (Unix seconds), the same in every player's playlist.
    pub at: f64,
    pub len: f64,
    pub uri: String,
    pub ad: bool,
}

/// The pieces of a Twitch media playlist. Ad pieces are titled other than "live" (Twitch:
/// "Amazon|…"), or sit inside a `twitch-stitched-ad` range.
pub fn pieces(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let (mut at, mut len, mut title) = (None, 0.0, String::new());
    let mut ad_ranges: Vec<(f64, f64)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("#EXT-X-PROGRAM-DATE-TIME:") {
            at = parse_time(v);
        } else if let Some(v) = line.strip_prefix("#EXTINF:") {
            let (d, t) = v.split_once(',').unwrap_or((v, ""));
            len = d.trim().parse().unwrap_or(2.0);
            title = t.trim().to_string();
        } else if line.starts_with("#EXT-X-DATERANGE:") && line.contains("stitched") {
            let start = attr(line, "START-DATE").and_then(|s| parse_time(&s));
            let dur = attr(line, "DURATION").and_then(|d| d.parse::<f64>().ok());
            if let (Some(s), Some(d)) = (start, dur) {
                ad_ranges.push((s, s + d));
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            if let Some(a) = at.take() {
                let in_ad = ad_ranges.iter().any(|(s, e)| a + 0.01 >= *s && a < *e - 0.01);
                out.push(Piece { at: a, len, uri: line.to_string(), ad: in_ad || !title.starts_with("live") });
            }
            title.clear();
        }
    }
    out
}

/// One attribute of a `#EXT-X-…:A="b",C=d` line.
fn attr(line: &str, name: &str) -> Option<String> {
    let i = line.find(&format!("{name}="))? + name.len() + 1;
    let rest = &line[i..];
    if let Some(q) = rest.strip_prefix('"') {
        q.split('"').next().map(String::from)
    } else {
        rest.split(',').next().map(String::from)
    }
}

/// "2026-11-15T15:08:26.312Z" as Unix seconds (UTC; Twitch writes Z).
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':');
    let (hh, mm) = (t.next()?.parse::<f64>().ok()?, t.next()?.parse::<f64>().ok()?);
    let ss = t.next()?.split(['+', '-']).next()?.parse::<f64>().ok()?;
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days as f64 * 86400.0 + hh * 3600.0 + mm * 60.0 + ss)
}

/// A quality of the master playlist: its name (`VIDEO="720p60"`, the same for every player),
/// the lines that describe it, and its media playlist.
#[derive(Clone, Debug)]
struct Variant {
    name: String,
    head: Vec<String>,
    url: String,
}

fn variants(master: &str) -> Vec<Variant> {
    let mut out = Vec::new();
    let mut head: Vec<String> = Vec::new();
    for line in master.lines().map(str::trim) {
        if line.starts_with("#EXT-X-MEDIA:") || line.starts_with("#EXT-X-STREAM-INF:") {
            head.push(line.to_string());
        } else if line.starts_with("http") {
            let inf = head.iter().find(|h| h.starts_with("#EXT-X-STREAM-INF:")).cloned().unwrap_or_default();
            let name = attr(&inf, "VIDEO").unwrap_or_else(|| format!("v{}", out.len()));
            out.push(Variant { name, head: std::mem::take(&mut head), url: line.to_string() });
        }
    }
    out
}

/// What a served playlist holds: the pieces handed out, numbered from `first`.
#[derive(Default, Debug)]
pub struct Out {
    pub served: VecDeque<(Piece, bool)>,
    pub first: u64,
    pub source: usize,
    /// The next piece starts after a switch or a gap.
    pub break_next: bool,
}

impl Out {
    /// Add what `fresh` (the playlist of `source`) has after the last piece handed out,
    /// leaving out ad pieces (returned as gaps: date-time, seconds).
    pub fn merge(&mut self, fresh: &[Piece], source: usize) -> Vec<(f64, f64)> {
        let mut gaps: Vec<(f64, f64)> = Vec::new();
        if source != self.source && !self.served.is_empty() {
            self.break_next = true;
        }
        self.source = source;
        let last = self.served.back().map(|(p, _)| p.at);
        // Joining: start near the live edge, as players do.
        let skip = if last.is_none() { fresh.iter().filter(|p| !p.ad).count().saturating_sub(3) } else { 0 };
        let mut kept = 0;
        for p in fresh {
            if last.is_some_and(|l| p.at <= l + 0.5) {
                continue;
            }
            if p.ad {
                if last.is_some() || !self.served.is_empty() {
                    match gaps.last_mut() {
                        Some((at, len)) if (*at + *len - p.at).abs() < 0.5 => *len += p.len,
                        _ => gaps.push((p.at, p.len)),
                    }
                }
                self.break_next = true;
                continue;
            }
            if kept < skip {
                kept += 1;
                continue;
            }
            let brk = std::mem::take(&mut self.break_next);
            self.served.push_back((p.clone(), brk));
        }
        while self.served.len() > WINDOW {
            self.served.pop_front();
            self.first += 1;
        }
        gaps
    }

    pub fn render(&self) -> String {
        let target = self.served.iter().map(|(p, _)| p.len.ceil() as u64).max().unwrap_or(2).max(2);
        let mut s = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            self.first
        );
        for (p, brk) in &self.served {
            if *brk {
                s.push_str("#EXT-X-DISCONTINUITY\n");
            }
            s.push_str(&format!(
                "#EXT-X-PROGRAM-DATE-TIME:{}\n#EXTINF:{:.3},live\n{}\n",
                time_text(p.at),
                p.len,
                p.uri
            ));
        }
        s
    }
}

fn time_text(t: f64) -> String {
    let secs = t.floor() as i64;
    let ms = ((t - secs as f64) * 1000.0).round() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // civil_from_days
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

// ── Twitch ──

fn token(login: &str, player: &str) -> Result<(String, String)> {
    let body = json!({
        "operationName": "PlaybackAccessToken",
        "extensions": {"persistedQuery": {"version": 1, "sha256Hash": TOKEN_QUERY}},
        "variables": {"isLive": true, "login": login, "isVod": false, "vodID": "",
                      "playerType": player, "platform": if player == "autoplay" { "android" } else { "web" }},
    });
    let text = crate::web::client()?
        .post("https://gql.twitch.tv/gql")
        .header("Client-ID", CLIENT_ID)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()?
        .text()?;
    let v: Value = serde_json::from_str(&text)?;
    let t = &v["data"]["streamPlaybackAccessToken"];
    let (sig, value) = (t["signature"].as_str(), t["value"].as_str());
    match (sig, value) {
        (Some(s), Some(v)) => Ok((s.to_string(), v.to_string())),
        _ => bail!("Twitch gave no stream for {player}"),
    }
}

fn master_for(login: &str, player: &str) -> Result<Vec<Variant>> {
    let (sig, value) = token(login, player)?;
    let p: u32 = (crate::live::now() * 1000.0) as u32 % 9_999_999;
    let url = format!(
        "https://usher.ttvnw.net/api/channel/hls/{login}.m3u8?sig={}&token={}&allow_source=true&allow_audio_only=true&p={p}&player_backend=mediaplayer&playlist_include_framerate=true&supported_codecs=avc1",
        crate::web::encode(&sig),
        crate::web::encode(&value)
    );
    let v = variants(&crate::web::get_text(&url)?);
    if v.is_empty() {
        bail!("no qualities");
    }
    Ok(v)
}

#[derive(Default)]
struct Channel {
    /// Each player's qualities (by name), looked up when first needed and again every 10 min.
    sources: Vec<Option<(Instant, HashMap<String, String>)>>,
    /// The master playlist served (the first source's qualities, pointing here).
    master: Option<(Instant, String)>,
    outs: HashMap<String, (Instant, Out)>,
    gaps: Vec<(f64, f64)>,
    used: Option<Instant>,
}

fn state() -> &'static Mutex<HashMap<String, Channel>> {
    static S: OnceLock<Mutex<HashMap<String, Channel>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The players to ask, in order (`NERIA_TTV_ORDER=site,embed` for checks: `site` first meets
/// the pre-roll, so a switch can be seen).
fn players() -> Vec<String> {
    match std::env::var("NERIA_TTV_ORDER") {
        Ok(v) if !v.trim().is_empty() => v.split(',').map(|s| s.trim().to_string()).collect(),
        _ => PLAYERS.iter().map(|s| s.to_string()).collect(),
    }
}

/// The master playlist the relay serves for `login` (its qualities pointing at the relay).
fn channel_master(login: &str) -> Result<String> {
    {
        let st = state().lock().unwrap();
        if let Some((at, text)) = st.get(login).and_then(|c| c.master.clone())
            && at.elapsed() < Duration::from_secs(600)
        {
            return Ok(text);
        }
    }
    let port = server().context("no relay")?;
    // The first player that answers names the qualities (every player has the same names).
    let mut found = None;
    for p in players() {
        if let Ok(v) = master_for(login, &p) {
            found = Some(v);
            break;
        }
    }
    let variants = found.context("Twitch gave no stream")?;
    let mut text = String::from("#EXTM3U\n");
    for v in &variants {
        for h in &v.head {
            text.push_str(h);
            text.push('\n');
        }
        text.push_str(&format!("http://127.0.0.1:{port}/ttv/{login}/{}.m3u8\n", crate::web::encode(&v.name)));
    }
    let mut st = state().lock().unwrap();
    let c = st.entry(login.to_string()).or_default();
    c.master = Some((Instant::now(), text.clone()));
    c.used = Some(Instant::now());
    Ok(text)
}

/// The media playlist URL of `quality` from player number `i`, looked up when needed.
fn source_url(login: &str, i: usize, quality: &str) -> Option<String> {
    let names = players();
    {
        let st = state().lock().unwrap();
        if let Some(Some((at, map))) = st.get(login).and_then(|c| c.sources.get(i))
            && at.elapsed() < Duration::from_secs(600)
        {
            return map.get(quality).cloned();
        }
    }
    let map: HashMap<String, String> =
        master_for(login, names.get(i)?).ok()?.into_iter().map(|v| (v.name, v.url)).collect();
    let url = map.get(quality).cloned();
    let mut st = state().lock().unwrap();
    let c = st.entry(login.to_string()).or_default();
    if c.sources.len() < names.len() {
        c.sources.resize(names.len(), None);
    }
    c.sources[i] = Some((Instant::now(), map));
    url
}

/// The relay's playlist of `quality`: the newest pieces without ads, from the first source
/// that has none (starting with the one it used last).
fn channel_media(login: &str, quality: &str) -> Result<String> {
    {
        let st = state().lock().unwrap();
        if let Some((at, out)) = st.get(login).and_then(|c| c.outs.get(quality))
            && at.elapsed() < Duration::from_millis(900)
        {
            return Ok(out.render());
        }
    }
    let n = players().len();
    let start = state().lock().unwrap().get(login).and_then(|c| c.outs.get(quality)).map_or(0, |(_, o)| o.source);
    let mut chosen: Option<(usize, Vec<Piece>)> = None;
    let mut fallback: Option<(usize, Vec<Piece>)> = None;
    for k in 0..n {
        let i = (start + k) % n;
        let Some(url) = source_url(login, i, quality) else { continue };
        let Ok(text) = crate::web::get_text(&url) else {
            // An old address (its token ran out): look it up again next time.
            if let Some(c) = state().lock().unwrap().get_mut(login)
                && let Some(s) = c.sources.get_mut(i)
            {
                *s = None;
            }
            continue;
        };
        let list = pieces(&text);
        if list.is_empty() {
            continue;
        }
        if list.iter().any(|p| p.ad) {
            fallback.get_or_insert((i, list));
            continue;
        }
        chosen = Some((i, list));
        break;
    }
    let (i, list) = chosen.or(fallback).context("Twitch gave no pieces")?;
    let mut st = state().lock().unwrap();
    let c = st.entry(login.to_string()).or_default();
    c.used = Some(Instant::now());
    let (at, out) =
        c.outs.entry(quality.to_string()).or_insert_with(|| (Instant::now(), Out { source: i, ..Out::default() }));
    let gaps = out.merge(&list, i);
    *at = Instant::now();
    let text = out.render();
    c.gaps.extend(gaps);
    Ok(text)
}

// ── The server ──

/// The relay's port, starting it the first time (loopback only).
fn server() -> Option<u16> {
    static PORT: OnceLock<Option<u16>> = OnceLock::new();
    *PORT.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        let port = listener.local_addr().ok()?.port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let _ = serve(stream);
                });
            }
        });
        // Channels nobody has asked about for a while are forgotten.
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(60));
                state().lock().unwrap().retain(|_, c| c.used.is_some_and(|u| u.elapsed() < Duration::from_secs(300)));
            }
        });
        Some(port)
    })
}

fn serve(mut stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    // The rest of the request's head.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/");
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let result = match parts.as_slice() {
        ["ttv", login, "master.m3u8"] => channel_master(login),
        ["ttv", login, file] if file.ends_with(".m3u8") => {
            let quality = decode(file.trim_end_matches(".m3u8"));
            channel_media(login, &quality)
        }
        _ => Err(anyhow::anyhow!("not here")),
    };
    let (status, body) = match result {
        Ok(b) => ("200 OK", b),
        Err(e) => ("502 Bad Gateway", e.to_string()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    Ok(())
}

/// %XX back to bytes (quality names are plain, but `encode` may have touched them).
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured on 2026-11-15 (osulive, 480p), addresses shortened.
    const SITE: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:0\n\
#EXT-X-DATERANGE:ID=\"stitched-ad-1794755306-30235000000\",CLASS=\"twitch-stitched-ad\",START-DATE=\"2026-11-15T15:08:26.312Z\",DURATION=30.235\n\
#EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME:2026-11-15T15:08:26.312Z\n#EXTINF:2.000,Amazon|2474283100495.0\nhttps://a/1.ts\n\
#EXT-X-PROGRAM-DATE-TIME:2026-11-15T15:08:28.312Z\n#EXTINF:2.000,Amazon|2474283100495.0\nhttps://a/2.ts\n\
#EXT-X-PROGRAM-DATE-TIME:2026-11-15T15:08:30.312Z\n#EXTINF:2.000,Amazon|2474283100495.0\nhttps://a/3.ts\n";

    fn embed(from: u32, to: u32) -> String {
        let mut s = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:3990\n");
        for k in from..=to {
            let sec = 2 + 2 * k; // 15:08:02.312 + 2k
            s.push_str(&format!(
                "#EXT-X-PROGRAM-DATE-TIME:2026-11-15T15:08:{sec:02}.312Z\n#EXTINF:2.000,live\nhttps://e/{k}.ts\n"
            ));
        }
        s
    }

    #[test]
    fn reads_dates() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0.0));
        let t = parse_time("2026-11-15T15:08:26.312Z").unwrap();
        assert!((t - 1794755306.312).abs() < 0.001, "{t}");
        assert_eq!(time_text(t), "2026-11-15T15:08:26.312Z");
    }

    #[test]
    fn knows_ad_pieces() {
        let p = pieces(SITE);
        assert_eq!(p.len(), 3);
        assert!(p.iter().all(|x| x.ad));
        let e = pieces(&embed(0, 3));
        assert_eq!(e.len(), 4);
        assert!(e.iter().all(|x| !x.ad));
        assert!((e[1].at - e[0].at - 2.0).abs() < 0.001);
    }

    #[test]
    fn joins_at_the_live_edge_and_adds_only_new_pieces() {
        let mut out = Out::default();
        out.merge(&pieces(&embed(0, 13)), 0);
        assert_eq!(out.served.len(), 3, "starts 3 pieces from the edge");
        assert_eq!(out.served.back().unwrap().0.uri, "https://e/13.ts");
        out.merge(&pieces(&embed(2, 15)), 0);
        let uris: Vec<_> = out.served.iter().map(|(p, _)| p.uri.as_str()).collect();
        assert_eq!(
            uris,
            ["https://e/11.ts", "https://e/12.ts", "https://e/13.ts", "https://e/14.ts", "https://e/15.ts"]
        );
        assert!(out.served.iter().all(|(_, b)| !b));
    }

    #[test]
    fn a_switch_continues_by_time_and_ads_become_a_gap() {
        let mut out = Out::default();
        out.merge(&pieces(&embed(9, 11)), 0); // up to 15:08:24.312
        // Another player, same times, a piece further: no repeat, marked as a switch.
        out.merge(&pieces(&embed(10, 12)), 1);
        let last = out.served.back().unwrap();
        assert_eq!(last.0.uri, "https://e/12.ts");
        assert!(last.1, "a discontinuity where the source changed");
        // Only ads to be had: nothing added, the ad time is a gap.
        let gaps = out.merge(&pieces(SITE), 2);
        assert_eq!(out.served.back().unwrap().0.uri, "https://e/12.ts");
        assert_eq!(gaps.len(), 1);
        // 15:08:26 was handed out already; 28 and 30 were ads.
        assert!((gaps[0].1 - 4.0).abs() < 0.01, "{gaps:?}");
        let text = out.render();
        assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:0") && !text.contains("Amazon"));
    }

    #[test]
    fn knows_when_the_stream_began() {
        let e = embed(0, 3).replace(
            "#EXT-X-MEDIA-SEQUENCE:3990\n",
            "#EXT-X-MEDIA-SEQUENCE:3990\n#EXT-X-TWITCH-ELAPSED-SECS:7980.000\n",
        );
        let s = started_in(&e).unwrap();
        assert!((s - (parse_time("2026-11-15T15:08:02.312Z").unwrap() - 7980.0)).abs() < 0.001);
        // A playlist that opens on the ad says nothing about the stream.
        assert_eq!(
            started_in(&SITE.replace(
                "#EXT-X-MEDIA-SEQUENCE:0\n",
                "#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-TWITCH-ELAPSED-SECS:8006.000\n"
            )),
            None
        );
    }

    #[test]
    fn knows_channel_links() {
        assert_eq!(login_of("https://www.twitch.tv/osulive").as_deref(), Some("osulive"));
        assert_eq!(login_of("https://twitch.tv/OsuLive?x=1").as_deref(), Some("osulive"));
        assert_eq!(login_of("https://www.twitch.tv/videos/2928685004"), None);
        assert_eq!(login_of("https://www.twitch.tv/osulive/clip/Abc"), None);
        assert_eq!(login_of("https://www.youtube.com/watch?v=x"), None);
    }

    #[test]
    fn reads_the_qualities() {
        let m = "#EXTM3U\n#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID=\"chunked\",NAME=\"1080p60 (source)\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=8000000,RESOLUTION=1920x1080,VIDEO=\"chunked\",FRAME-RATE=60.000\nhttps://v/chunked.m3u8\n\
#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID=\"480p30\",NAME=\"480p\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=1400000,RESOLUTION=852x480,VIDEO=\"480p30\"\nhttps://v/480.m3u8\n";
        let v = variants(m);
        assert_eq!(v.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["chunked", "480p30"]);
        assert_eq!(v[1].head.len(), 2);
    }
}
