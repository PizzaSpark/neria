//! Live chat (Livestreams track): read, saved and shown next to the video, and put into a
//! clip (`chat_draw.rs` draws it, `chat_out.rs` puts it in a file).
//!
//! Read-only, and never signed in:
//! - **Twitch live**: its chat server, anonymously (`justinfan…`, the way the site's own
//!   logged-out viewers read it), over plain IRC with Twitch's tags (colours, badges, emotes).
//! - **YouTube live**: the page's own chat feed (`live_chat` + `get_live_chat`), polled as the
//!   site's chat frame does. The "Live chat" view (all messages), not "Top chat".
//! - **Past streams**: YouTube's chat replay through yt-dlp (`--sub-langs live_chat`), Twitch's
//!   saved chat of a past broadcast (its comments, paged by `cursor`).
//!
//! Emotes the channel added on 7TV, BetterTTV and FrankerFaceZ are their public lists (data,
//! no code); they turn words in Twitch messages into pictures. Emoji become Noto's pictures
//! (the ones YouTube's own chat uses), so they draw in colour.
//!
//! A message's `t` is seconds on a timeline: Unix seconds while live, seconds into the video
//! in a saved `.chat.json` (whose `zero` says where its second 0 was on the clock).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::settings::Settings;

const TWITCH_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Site {
    #[default]
    YouTube,
    Twitch,
    Kick,
}

impl Site {
    pub fn of(url: &str) -> Option<Site> {
        let host = host_of(url);
        if host == "youtu.be" || host == "youtube.com" || host.ends_with(".youtube.com") {
            Some(Site::YouTube)
        } else if host == "twitch.tv" || host.ends_with(".twitch.tv") {
            Some(Site::Twitch)
        } else if host == "kick.com" {
            Some(Site::Kick)
        } else {
            None
        }
    }

    /// "YT" / "TW": the small mark on a message when a panel mixes both.
    pub fn mark(self) -> &'static str {
        match self {
            Site::YouTube => "YT",
            Site::Twitch => "TW",
            Site::Kick => "KI",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Site::YouTube => "YouTube",
            Site::Twitch => "Twitch",
            Site::Kick => "Kick",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Chat,
    /// Money: a Super Chat / Super Sticker, Twitch Bits.
    Paid,
    /// A new member / sub, a milestone, a gift.
    Member,
    /// A raid, an announcement.
    Notice,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "lowercase")]
pub enum Part {
    Text {
        s: String,
    },
    /// A picture in the text: an emote or an emoji (`name` is its text, for search and
    /// where the picture can't be had).
    Emote {
        name: String,
        url: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    pub t: f64,
    pub id: String,
    pub site: Site,
    pub name: String,
    /// "#RRGGBB" when the site gives one (Twitch); else drawn by role.
    pub color: Option<String>,
    /// "broadcaster", "owner", "moderator", "vip", "member", "subscriber", "verified".
    pub badges: Vec<String>,
    pub parts: Vec<Part>,
    pub kind: Kind,
    /// "$20.00", "500 bits".
    pub amount: Option<String>,
    /// The site's line above it: "Subscribed for 12 months", "New member".
    pub note: Option<String>,
}

impl Message {
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Text { s } => s.as_str(),
                Part::Emote { name, .. } => name.as_str(),
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn has(&self, badge: &str) -> bool {
        self.badges.iter().any(|b| b == badge)
    }

    /// Mods, the channel itself, VIPs: drawn as a role.
    pub fn is_staff(&self) -> bool {
        ["broadcaster", "owner", "moderator", "vip"].iter().any(|b| self.has(b))
    }
}

/// A saved chat: `<video>.chat.json` next to the video.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatFile {
    pub version: u32,
    /// The sites in it (two when YouTube and Twitch were read together).
    pub sites: Vec<Site>,
    /// Channels, as their pages call them.
    pub channels: Vec<String>,
    /// Where the video's second 0 was on the clock (Unix seconds), when known.
    pub zero: Option<f64>,
    /// Chat from here on only (watched or recorded from a point, not from the start).
    pub from: f64,
    pub messages: Vec<Message>,
}

impl ChatFile {
    pub fn load(path: &Path) -> Result<ChatFile> {
        let text = std::fs::read_to_string(path).with_context(|| format!("can't read {}", path.display()))?;
        let mut f: ChatFile = serde_json::from_str(&text).context("not a chat file")?;
        f.messages.sort_by(|a, b| a.t.total_cmp(&b.t));
        Ok(f)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// `Video.mp4` → `Video.chat.json`.
pub fn sidecar_of(video: &Path) -> PathBuf {
    let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    video.with_file_name(format!("{stem}.chat.json"))
}

/// The chat saved next to `video`, if any.
pub fn load_for(video: &Path) -> Option<ChatFile> {
    let p = sidecar_of(video);
    p.is_file().then(|| ChatFile::load(&p).ok()).flatten()
}

fn host_of(url: &str) -> String {
    let rest = url.split("//").nth(1).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
    host.strip_prefix("www.").or_else(|| host.strip_prefix("m.")).unwrap_or(&host).to_string()
}

/// A YouTube video's id from its link (watch, youtu.be, /live/, /shorts/).
pub fn youtube_id(url: &str) -> Option<String> {
    let rest = url.split("//").nth(1).unwrap_or(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let ok = |s: &str| s.len() == 11 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if host.ends_with("youtu.be") {
        let id = path.split(['?', '#', '/']).next().unwrap_or("");
        return ok(id).then(|| id.to_string());
    }
    if let Some(q) = path.split_once('?').map(|(_, q)| q) {
        for kv in q.split(['&', '#']) {
            if let Some(v) = kv.strip_prefix("v=")
                && ok(v)
            {
                return Some(v.to_string());
            }
        }
    }
    let mut segs = path.split(['?', '#']).next().unwrap_or("").split('/');
    while let Some(s) = segs.next() {
        if matches!(s, "live" | "shorts" | "embed")
            && let Some(id) = segs.next()
            && ok(id)
        {
            return Some(id.to_string());
        }
    }
    None
}

/// A Twitch past broadcast's id (`twitch.tv/videos/123`).
pub fn twitch_video_id(url: &str) -> Option<String> {
    let path = url.split("//").nth(1).unwrap_or(url).split_once('/')?.1;
    let mut segs = path.split(['?', '#']).next()?.split('/');
    while let Some(s) = segs.next() {
        if s == "videos" || s == "v" {
            let id = segs.next()?;
            return id.chars().all(|c| c.is_ascii_digit()).then(|| id.to_string());
        }
    }
    None
}

/// Whether Neria can read this link's chat at all.
pub fn supported(url: &str) -> bool {
    match Site::of(url) {
        Some(Site::Twitch) => crate::ttv::login_of(url).is_some() || twitch_video_id(url).is_some(),
        Some(Site::YouTube) => true,
        // Kick's channel API turns Neria's own requests away (Cloudflare, HTTP 403) though
        // its chat itself is open: the reader is here (`kick_live`) for when the chat room can
        // be found another way. Until then a Kick link offers no chat.
        Some(Site::Kick) => false,
        None => false,
    }
}

/// A Kick channel's name from its link (`kick.com/xqc`; not a past stream's `/videos/`).
pub fn kick_slug(url: &str) -> Option<String> {
    if Site::of(url) != Some(Site::Kick) {
        return None;
    }
    let path = url.split("//").nth(1).unwrap_or(url).split_once('/')?.1;
    let mut segs = path.split(['?', '#']).next()?.split('/').filter(|s| !s.is_empty());
    let slug = segs.next()?;
    let reserved = ["video", "videos", "categories", "category", "browse", "following", "search", "clips", "api"];
    let ok = slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && !reserved.contains(&slug);
    (ok && segs.next().is_none()).then(|| slug.to_ascii_lowercase())
}

/// Whether a (non-live) link kept its chat: a Twitch past broadcast, or a YouTube video
/// whose page has the chat replay. A quick look at the page, off the UI thread.
pub fn has_replay(url: &str) -> bool {
    match Site::of(url) {
        Some(Site::Twitch) => twitch_video_id(url).is_some(),
        Some(Site::Kick) => false,
        Some(Site::YouTube) => match youtube_id(url) {
            Some(id) => {
                yt_get(&format!("https://www.youtube.com/watch?v={id}")).is_ok_and(|h| h.contains("liveChatRenderer"))
            }
            None => false,
        },
        None => false,
    }
}

// ───────────── what goes in (filters) ─────────────

/// Accounts that post for the channel, not people (left out with "Leave out bots").
pub const BOTS: &[&str] = &[
    "nightbot",
    "streamelements",
    "streamlabs",
    "moobot",
    "fossabot",
    "wizebot",
    "sery_bot",
    "botisimo",
    "soundalerts",
    "kofistreambot",
    "blerp",
    "pokemoncommunitygame",
    "streamlootsbot",
    "commanderroot",
    "deepbot",
    "phantombot",
    "coebot",
    "ankhbot",
    "own3d",
    "tangiabot",
];

pub fn is_bot(m: &Message) -> bool {
    let name = m.name.trim_start_matches('@').to_lowercase();
    BOTS.contains(&name.as_str()) || m.text().trim_start().starts_with('!')
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Which {
    #[default]
    All,
    /// Super Chats, subs and members, mods and the channel, and the words you gave.
    Highlights,
    /// The ones ticked in the panel.
    Picked,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    pub which: Which,
    pub no_bots: bool,
    /// Highlights also take messages with any of these (case doesn't matter).
    pub words: Vec<String>,
    pub picked: HashSet<String>,
}

impl Filter {
    pub fn keeps(&self, m: &Message) -> bool {
        if self.no_bots && is_bot(m) {
            return false;
        }
        match self.which {
            Which::All => true,
            Which::Picked => self.picked.contains(&m.id),
            Which::Highlights => is_highlight(m, &self.words),
        }
    }
}

pub fn is_highlight(m: &Message, words: &[String]) -> bool {
    if m.kind != Kind::Chat || ["broadcaster", "owner", "moderator"].iter().any(|b| m.has(b)) {
        return true;
    }
    let text = m.text().to_lowercase();
    words.iter().map(|w| w.trim().to_lowercase()).any(|w| !w.is_empty() && text.contains(&w))
}

/// The messages between `from` and `to` (seconds on the chat's timeline), filtered, moved so
/// `from` is 0: what a clip of that part gets.
pub fn for_clip(messages: &[Message], from: f64, to: f64, filter: &Filter) -> Vec<Message> {
    messages
        .iter()
        .filter(|m| m.t >= from && m.t <= to && filter.keeps(m))
        .map(|m| Message { t: m.t - from, ..m.clone() })
        .collect()
}

/// How busy chat is along `len` seconds: `buckets` counts (for the strip above the timeline).
pub fn activity(messages: &[Message], start: f64, len: f64, buckets: usize) -> Vec<u32> {
    let mut out = vec![0u32; buckets.max(1)];
    if len <= 0.0 {
        return out;
    }
    for m in messages {
        let i = ((m.t - start) / len * buckets as f64).floor();
        if i >= 0.0 && (i as usize) < out.len() {
            out[i as usize] += 1;
        }
    }
    out
}

// ───────────── emoji as pictures ─────────────

fn is_emoji_start(c: char) -> bool {
    let u = c as u32;
    (0x1F300..=0x1FAFF).contains(&u)
        || (0x1F000..=0x1F2FF).contains(&u)
        || (0x2600..=0x27BF).contains(&u)
        || (0x2B00..=0x2BFF).contains(&u)
        || u == 0x00A9
        || u == 0x00AE
        || u == 0x203C
        || u == 0x2049
        || u == 0x2122
        || (0x231A..=0x23FF).contains(&u)
}

fn is_emoji_joiner(c: char) -> bool {
    let u = c as u32;
    u == 0x200D || u == 0xFE0F || (0x1F3FB..=0x1F3FF).contains(&u) || (0xE0020..=0xE007F).contains(&u) || u == 0x20E3
}

/// Noto's picture of an emoji (what YouTube's chat shows): codepoints in hex, `_` between,
/// without the "show as emoji" mark.
pub fn emoji_url(seq: &str) -> String {
    let code: Vec<String> = seq.chars().filter(|c| *c as u32 != 0xFE0F).map(|c| format!("{:x}", c as u32)).collect();
    format!("https://fonts.gstatic.com/s/e/notoemoji/15.1/{}/72.png", code.join("_"))
}

/// Emoji in text become pictures (they draw in colour, and every font has them).
pub fn emojify(parts: Vec<Part>) -> Vec<Part> {
    let mut out = Vec::new();
    for p in parts {
        let Part::Text { s } = p else {
            out.push(p);
            continue;
        };
        let mut text = String::new();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            // Text-style symbols (©, ™, ♥ without the emoji mark) stay text.
            let wants =
                is_emoji_start(c) && ((c as u32) >= 0x1F000 || chars.get(i + 1).is_some_and(|n| *n as u32 == 0xFE0F));
            if !wants {
                text.push(c);
                i += 1;
                continue;
            }
            let mut seq = String::from(c);
            i += 1;
            while i < chars.len() {
                let n = chars[i];
                if is_emoji_joiner(n) {
                    seq.push(n);
                    i += 1;
                    // After a joiner the next emoji belongs to the same picture.
                    if n as u32 == 0x200D && i < chars.len() {
                        seq.push(chars[i]);
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            if !text.is_empty() {
                out.push(Part::Text { s: std::mem::take(&mut text) });
            }
            out.push(Part::Emote { url: emoji_url(&seq), name: seq });
        }
        if !text.is_empty() {
            out.push(Part::Text { s: text });
        }
    }
    out
}

// ───────────── channel emotes (7TV, BetterTTV, FrankerFaceZ) ─────────────

/// Words that are pictures in a Twitch channel's chat, and their addresses.
#[derive(Clone, Debug, Default)]
pub struct Emotes {
    pub map: HashMap<String, String>,
}

impl Emotes {
    /// The channel's (and everyone's) emotes from the three lists; whatever answers.
    pub fn for_twitch(room_id: &str) -> Emotes {
        let mut map = HashMap::new();
        let get = |url: &str| -> Option<Value> {
            serde_json::from_str(&crate::web::client().ok()?.get(url).send().ok()?.text().ok()?).ok()
        };
        // BetterTTV: everyone's, then the channel's own and shared.
        if let Some(v) = get("https://api.betterttv.net/3/cached/emotes/global") {
            bttv(&v, &mut map);
        }
        if let Some(v) = get(&format!("https://api.betterttv.net/3/cached/users/twitch/{room_id}")) {
            bttv(&v["channelEmotes"], &mut map);
            bttv(&v["sharedEmotes"], &mut map);
        }
        // FrankerFaceZ.
        for url in [
            "https://api.frankerfacez.com/v1/set/global".to_string(),
            format!("https://api.frankerfacez.com/v1/room/id/{room_id}"),
        ] {
            if let Some(v) = get(&url)
                && let Some(sets) = v["sets"].as_object()
            {
                for set in sets.values() {
                    for e in set["emoticons"].as_array().into_iter().flatten() {
                        let (Some(name), Some(url)) =
                            (e["name"].as_str(), e["urls"]["2"].as_str().or(e["urls"]["1"].as_str()))
                        else {
                            continue;
                        };
                        let url = if url.starts_with("//") { format!("https:{url}") } else { url.to_string() };
                        map.insert(name.to_string(), url);
                    }
                }
            }
        }
        // 7TV last: it's what most channels use now, so it wins a shared name.
        if let Some(v) = get("https://7tv.io/v3/emote-sets/global") {
            seventv(&v["emotes"], &mut map);
        }
        if let Some(v) = get(&format!("https://7tv.io/v3/users/twitch/{room_id}")) {
            seventv(&v["emote_set"]["emotes"], &mut map);
        }
        Emotes { map }
    }

    /// Words that are emotes become pictures.
    pub fn apply(&self, parts: Vec<Part>) -> Vec<Part> {
        if self.map.is_empty() {
            return parts;
        }
        let mut out = Vec::new();
        for p in parts {
            let Part::Text { s } = p else {
                out.push(p);
                continue;
            };
            let mut text = String::new();
            for (i, word) in s.split(' ').enumerate() {
                if i > 0 {
                    text.push(' ');
                }
                match self.map.get(word) {
                    Some(url) => {
                        if !text.is_empty() {
                            out.push(Part::Text { s: std::mem::take(&mut text) });
                        }
                        out.push(Part::Emote { name: word.to_string(), url: url.clone() });
                    }
                    None => text.push_str(word),
                }
            }
            if !text.is_empty() {
                out.push(Part::Text { s: text });
            }
        }
        out
    }
}

fn bttv(v: &Value, map: &mut HashMap<String, String>) {
    for e in v.as_array().into_iter().flatten() {
        if let (Some(code), Some(id)) = (e["code"].as_str(), e["id"].as_str()) {
            map.insert(code.to_string(), format!("https://cdn.betterttv.net/emote/{id}/2x"));
        }
    }
}

fn seventv(v: &Value, map: &mut HashMap<String, String>) {
    for e in v.as_array().into_iter().flatten() {
        let (Some(name), Some(id)) = (e["name"].as_str(), e["id"].as_str()) else { continue };
        map.insert(name.to_string(), format!("https://cdn.7tv.app/emote/{id}/2x.png"));
    }
}

/// An emote's picture, fetched once into Neria's cache (`cache/emotes`). `None` while it
/// can't be had (no network, gone).
pub fn emote_file(url: &str) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let dir = crate::paths::cache_dir().join("emotes");
    let path = dir.join(format!("{:016x}", h.finish()));
    if path.is_file() {
        return Some(path);
    }
    // A miss is remembered for a while, so a gone picture isn't asked for every frame.
    let miss = path.with_extension("miss");
    if miss
        .metadata()
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(3600))
    {
        return None;
    }
    let _ = std::fs::create_dir_all(&dir);
    let got = (|| -> Option<Vec<u8>> {
        // Asked for PNG/GIF: WebP and AVIF answers can't all be read.
        let resp =
            crate::web::client().ok()?.get(url).header("Accept", "image/png,image/gif,image/*;q=0.5").send().ok()?;
        resp.status().is_success().then_some(())?;
        let bytes = resp.bytes().ok()?;
        (bytes.len() > 16).then(|| bytes.to_vec())
    })();
    match got {
        Some(bytes) => {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, bytes).ok()?;
            std::fs::rename(&tmp, &path).ok()?;
            Some(path)
        }
        None => {
            let _ = std::fs::write(&miss, b"");
            None
        }
    }
}

// ───────────── Twitch: messages ─────────────

fn unescape_tag(v: &str) -> String {
    let mut out = String::new();
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some(':') => out.push(';'),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// Twitch's own colours for names that didn't pick one (its web chat's set).
const TWITCH_DEFAULT: [&str; 15] = [
    "#FF0000", "#0000FF", "#00FF00", "#B22222", "#FF7F50", "#9ACD32", "#FF4500", "#2E8B57", "#DAA520", "#D2691E",
    "#5F9EA0", "#1E90FF", "#FF69B4", "#8A2BE2", "#00FF7F",
];

fn default_color(name: &str) -> String {
    let n: u32 = name.bytes().map(|b| b as u32).sum();
    TWITCH_DEFAULT[(n as usize) % TWITCH_DEFAULT.len()].to_string()
}

fn twitch_badges(tag: &str) -> Vec<String> {
    tag.split(',')
        .filter_map(|b| b.split('/').next())
        .filter_map(|b| match b {
            "broadcaster" => Some("broadcaster"),
            "moderator" => Some("moderator"),
            "vip" => Some("vip"),
            "subscriber" | "founder" => Some("subscriber"),
            "partner" => Some("verified"),
            _ => None,
        })
        .map(String::from)
        .collect()
}

/// Twitch emotes in a message: `25:0-4,12-16/1902:6-10` (places in characters).
fn twitch_parts(text: &str, emotes: &str) -> Vec<Part> {
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    for e in emotes.split('/').filter(|e| !e.is_empty()) {
        let Some((id, places)) = e.split_once(':') else { continue };
        for p in places.split(',') {
            if let Some((a, b)) = p.split_once('-')
                && let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>())
            {
                spans.push((a, b, id.to_string()));
            }
        }
    }
    spans.sort();
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    for (a, b, id) in spans {
        if a < at || b >= chars.len() {
            continue;
        }
        if a > at {
            out.push(Part::Text { s: chars[at..a].iter().collect() });
        }
        out.push(Part::Emote {
            name: chars[a..=b].iter().collect(),
            url: format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/static/dark/2.0"),
        });
        at = b + 1;
    }
    if at < chars.len() {
        out.push(Part::Text { s: chars[at..].iter().collect() });
    }
    out
}

/// What a line from Twitch's chat server is for us.
#[derive(Debug, PartialEq)]
enum Irc {
    Message(Message),
    /// Messages to take out (a ban, a timeout, a deleted message): by id, or every one of a
    /// name.
    Remove {
        id: Option<String>,
        name: Option<String>,
    },
    Room(String),
    Ping(String),
    Reconnect,
    Other,
}

fn parse_irc(line: &str) -> Irc {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(rest) = line.strip_prefix("PING") {
        return Irc::Ping(rest.trim().to_string());
    }
    let (tags, rest) = match line.strip_prefix('@') {
        Some(r) => r.split_once(' ').unwrap_or((r, "")),
        None => ("", line),
    };
    let tag: HashMap<&str, &str> = tags.split(';').filter_map(|kv| kv.split_once('=')).collect();
    let rest = rest.strip_prefix(':').map(|r| r.split_once(' ').map_or("", |x| x.1)).unwrap_or(rest);
    let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));
    let trailing = args.split_once(" :").map(|x| x.1).unwrap_or("");
    let get = |k: &str| tag.get(k).map(|v| unescape_tag(v)).filter(|v| !v.is_empty());
    match cmd {
        "PRIVMSG" | "USERNOTICE" => {
            let mut text = trailing.to_string();
            // "/me does a thing" comes wrapped.
            if let Some(t) = text.strip_prefix("\u{1}ACTION ").and_then(|t| t.strip_suffix('\u{1}')) {
                text = t.to_string();
            }
            let name = get("display-name").or_else(|| get("login")).unwrap_or_else(|| "someone".into());
            let t = get("tmi-sent-ts")
                .and_then(|v| v.parse::<f64>().ok())
                .map(|ms| ms / 1000.0)
                .unwrap_or_else(crate::live::now);
            let mut m = Message {
                t,
                id: get("id").unwrap_or_default(),
                site: Site::Twitch,
                color: get("color").or_else(|| Some(default_color(&name))),
                badges: twitch_badges(&get("badges").unwrap_or_default()),
                parts: twitch_parts(&text, &get("emotes").unwrap_or_default()),
                name,
                ..Message::default()
            };
            if let Some(bits) = get("bits") {
                m.kind = Kind::Paid;
                m.amount = Some(format!("{bits} bits"));
            }
            if cmd == "USERNOTICE" {
                let id = get("msg-id").unwrap_or_default();
                m.kind = match id.as_str() {
                    "raid" | "announcement" | "unraid" => Kind::Notice,
                    _ => Kind::Member,
                };
                m.note = get("system-msg");
                if m.parts.is_empty() && m.note.is_none() {
                    return Irc::Other;
                }
            }
            m.parts = emojify(m.parts);
            Irc::Message(m)
        }
        "CLEARMSG" => Irc::Remove { id: get("target-msg-id"), name: None },
        "CLEARCHAT" => match trailing {
            "" => Irc::Other, // the whole chat cleared: keep what was said
            name => Irc::Remove { id: None, name: Some(name.to_string()) },
        },
        "ROOMSTATE" => get("room-id").map(Irc::Room).unwrap_or(Irc::Other),
        "RECONNECT" => Irc::Reconnect,
        _ => Irc::Other,
    }
}

// ───────────── YouTube: messages ─────────────

fn yt_text(v: &Value) -> String {
    if let Some(s) = v["simpleText"].as_str() {
        return s.to_string();
    }
    if let Some(s) = v["content"].as_str() {
        return s.to_string();
    }
    v["runs"].as_array().into_iter().flatten().filter_map(|r| r["text"].as_str()).collect()
}

fn yt_runs(v: &Value) -> Vec<Part> {
    let mut out = Vec::new();
    if let Some(s) = v["simpleText"].as_str() {
        out.push(Part::Text { s: s.to_string() });
    }
    for r in v["runs"].as_array().into_iter().flatten() {
        if let Some(s) = r["text"].as_str() {
            out.push(Part::Text { s: s.to_string() });
        } else if let Some(e) = r.get("emoji") {
            let url = e["image"]["thumbnails"].as_array().and_then(|t| t.last()).and_then(|t| t["url"].as_str());
            let name = if e["isCustomEmoji"].as_bool() == Some(true) {
                e["shortcuts"][0].as_str().unwrap_or("").to_string()
            } else {
                e["emojiId"].as_str().unwrap_or("").to_string()
            };
            match url {
                Some(u) => out
                    .push(Part::Emote { name, url: if u.starts_with("//") { format!("https:{u}") } else { u.into() } }),
                None => out.push(Part::Text { s: name }),
            }
        }
    }
    out
}

fn yt_badges(r: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for b in r["authorBadges"].as_array().into_iter().flatten() {
        let b = &b["liveChatAuthorBadgeRenderer"];
        match b["icon"]["iconType"].as_str() {
            Some("OWNER") => out.push("owner".to_string()),
            Some("MODERATOR") => out.push("moderator".to_string()),
            Some("VERIFIED") => out.push("verified".to_string()),
            _ if b.get("customThumbnail").is_some() => out.push("member".to_string()),
            _ => {}
        }
    }
    out
}

/// YouTube's colours for roles (its chat has no colours per name).
fn yt_color(badges: &[String]) -> Option<String> {
    if badges.iter().any(|b| b == "owner") {
        Some("#FFD23F".into())
    } else if badges.iter().any(|b| b == "moderator") {
        Some("#5E84F1".into())
    } else if badges.iter().any(|b| b == "member") {
        Some("#2BA640".into())
    } else {
        None
    }
}

/// One of YouTube's chat items (live or replay) as a message; `t` from its own time.
fn parse_yt_item(item: &Value) -> Option<Message> {
    let (kind_name, r) = item.as_object()?.iter().next()?;
    let usec = r["timestampUsec"].as_str().and_then(|s| s.parse::<f64>().ok());
    let mut m = Message {
        t: usec.map(|u| u / 1e6).unwrap_or_else(crate::live::now),
        id: r["id"].as_str().unwrap_or("").to_string(),
        site: Site::YouTube,
        name: yt_text(&r["authorName"]).trim().to_string(),
        badges: yt_badges(r),
        ..Message::default()
    };
    match kind_name.as_str() {
        "liveChatTextMessageRenderer" => m.parts = yt_runs(&r["message"]),
        "liveChatPaidMessageRenderer" => {
            m.kind = Kind::Paid;
            m.amount = Some(yt_text(&r["purchaseAmountText"]));
            m.parts = yt_runs(&r["message"]);
        }
        "liveChatPaidStickerRenderer" => {
            m.kind = Kind::Paid;
            m.amount = Some(yt_text(&r["purchaseAmountText"]));
            if let Some(u) =
                r["sticker"]["thumbnails"].as_array().and_then(|t| t.last()).and_then(|t| t["url"].as_str())
            {
                let url = if u.starts_with("//") { format!("https:{u}") } else { u.to_string() };
                m.parts = vec![Part::Emote { name: "sticker".into(), url }];
            }
        }
        "liveChatMembershipItemRenderer" => {
            m.kind = Kind::Member;
            let head = yt_text(&r["headerPrimaryText"]);
            let sub = yt_text(&r["headerSubtext"]);
            m.note = Some(if head.is_empty() { sub } else { format!("{head} · {sub}") });
            m.parts = yt_runs(&r["message"]);
        }
        "liveChatSponsorshipsGiftPurchaseAnnouncementRenderer" => {
            let h = &r["header"]["liveChatSponsorshipsHeaderRenderer"];
            m.kind = Kind::Member;
            m.name = yt_text(&h["authorName"]).trim().to_string();
            m.note = Some(yt_text(&h["primaryText"]));
            m.badges = yt_badges(h);
        }
        "giftMessageViewModel" => {
            m.kind = Kind::Paid;
            m.name = yt_text(&r["authorName"]).trim().to_string();
            m.note = Some(yt_text(&r["text"]));
        }
        _ => return None,
    }
    if m.name.is_empty() && m.parts.is_empty() {
        return None;
    }
    m.color = yt_color(&m.badges);
    Some(m)
}

/// The messages of a list of YouTube chat actions, and the ids of ones taken back.
fn yt_actions(actions: &Value, out: &mut Vec<Message>, removed: &mut Vec<String>) {
    for a in actions.as_array().into_iter().flatten() {
        if let Some(item) = a.get("addChatItemAction").map(|x| &x["item"])
            && let Some(m) = parse_yt_item(item)
        {
            out.push(m);
        } else if let Some(id) = a["removeChatItemAction"]["targetItemId"].as_str() {
            removed.push(id.to_string());
        } else if let Some(id) = a["markChatItemAsDeletedAction"]["targetItemId"].as_str() {
            removed.push(id.to_string());
        }
    }
}

/// A line of yt-dlp's `.live_chat.json` (a chat replay): its messages, `t` from the video's
/// start.
fn parse_yt_replay_line(line: &str) -> Vec<Message> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let r = &v["replayChatItemAction"];
    let offset = r["videoOffsetTimeMsec"].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0) / 1000.0;
    let mut out = Vec::new();
    let mut removed = Vec::new();
    yt_actions(&r["actions"], &mut out, &mut removed);
    for m in &mut out {
        m.t = offset;
    }
    out
}

// ───────────── live ─────────────

#[derive(Clone, Debug, PartialEq)]
pub enum LiveState {
    Connecting,
    On,
    /// Lost; trying again (why).
    Retrying(String),
    /// Over: the stream ended, or chat is off / can't be read (why).
    Off(String),
}

struct Shared {
    msgs: Mutex<Vec<Message>>,
    state: Mutex<LiveState>,
    /// Bumped on every change (the panel redraws when it moves).
    generation: AtomicU64,
    channel: Mutex<String>,
}

/// A live stream's chat, read in the background until dropped.
pub struct LiveChat {
    pub site: Site,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
}

/// Kept in memory (the file has everything).
const KEEP: usize = 60_000;

impl LiveChat {
    /// Start reading `url`'s chat. `log`: every message is also added to this file (one JSON
    /// per line), so a recording keeps all of it even through a crash.
    pub fn start(url: &str, log: Option<PathBuf>) -> Option<LiveChat> {
        let site = Site::of(url)?;
        let shared = Arc::new(Shared {
            msgs: Mutex::new(Vec::new()),
            state: Mutex::new(LiveState::Connecting),
            generation: AtomicU64::new(0),
            channel: Mutex::new(String::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st, url) = (shared.clone(), stop.clone(), url.to_string());
        std::thread::spawn(move || {
            let mut sink = Sink {
                shared: s.clone(),
                log: log.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()),
            };
            let end = match site {
                Site::Twitch => twitch_live(&url, &st, &mut sink),
                Site::YouTube => youtube_live(&url, &st, &mut sink),
                Site::Kick => kick_live(&url, &st, &mut sink),
            };
            if !st.load(Ordering::SeqCst) {
                sink.state(LiveState::Off(match end {
                    Ok(()) => "The stream ended".into(),
                    Err(e) => e.to_string(),
                }));
            }
        });
        Some(LiveChat { site, shared, stop })
    }

    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::SeqCst)
    }

    pub fn state(&self) -> LiveState {
        self.shared.state.lock().unwrap().clone()
    }

    pub fn channel(&self) -> String {
        self.shared.channel.lock().unwrap().clone()
    }

    /// A copy of what's in memory, oldest first (`t` in Unix seconds).
    pub fn messages(&self) -> Vec<Message> {
        self.shared.msgs.lock().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.shared.msgs.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for LiveChat {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Sink {
    shared: Arc<Shared>,
    log: Option<std::fs::File>,
}

impl Sink {
    fn push(&mut self, mut new: Vec<Message>) {
        if new.is_empty() {
            return;
        }
        if let Some(f) = self.log.as_mut() {
            let mut buf = Vec::new();
            for m in &new {
                if serde_json::to_writer(&mut buf, m).is_ok() {
                    buf.push(b'\n');
                }
            }
            let _ = f.write_all(&buf);
        }
        let mut msgs = self.shared.msgs.lock().unwrap();
        new.sort_by(|a, b| a.t.total_cmp(&b.t));
        msgs.extend(new);
        if msgs.len() > KEEP + KEEP / 10 {
            let cut = msgs.len() - KEEP;
            msgs.drain(..cut);
        }
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn remove(&mut self, id: Option<&str>, name: Option<&str>) {
        let mut msgs = self.shared.msgs.lock().unwrap();
        let before = msgs.len();
        msgs.retain(|m| id.is_none_or(|i| m.id != i) && name.is_none_or(|n| !m.name.eq_ignore_ascii_case(n)));
        if msgs.len() != before {
            self.shared.generation.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(f) = self.log.as_mut() {
            let line = json!({ "removed": id, "name": name });
            let _ = writeln!(f, "{line}");
        }
    }

    fn state(&self, s: LiveState) {
        *self.shared.state.lock().unwrap() = s;
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
    }
}

fn sleep_unless(stop: &AtomicBool, d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn twitch_live(url: &str, stop: &AtomicBool, sink: &mut Sink) -> Result<()> {
    let login = crate::ttv::login_of(url).context("Not a Twitch channel")?;
    *sink.shared.channel.lock().unwrap() = login.clone();
    // The channel's 7TV / BetterTTV / FrankerFaceZ emotes, fetched beside the reading (a slow
    // list mustn't hold up the messages); words become pictures once they're in.
    let emotes: Arc<Mutex<Option<Emotes>>> = Arc::new(Mutex::new(None));
    let mut asked_emotes = false;
    let mut failures = 0u32;
    while !stop.load(Ordering::SeqCst) {
        let run = (|| -> Result<bool> {
            use std::net::TcpStream;
            let mut s = TcpStream::connect("irc.chat.twitch.tv:6667")?;
            s.set_read_timeout(Some(Duration::from_millis(500)))?;
            let nick = format!("justinfan{}", 10000 + (crate::live::now() as u64 % 80000));
            write!(
                s,
                "CAP REQ :twitch.tv/tags twitch.tv/commands\r\nPASS SCHMOOPIIE\r\nNICK {nick}\r\nJOIN #{login}\r\n"
            )?;
            let mut reader = BufReader::new(s.try_clone()?);
            let debug = std::env::var_os("NERIA_CHAT_DEBUG").is_some();
            let mut line = String::new();
            let mut batch = Vec::new();
            let mut last_flush = Instant::now();
            let mut last_data = Instant::now();
            loop {
                if stop.load(Ordering::SeqCst) {
                    return Ok(true);
                }
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => return Ok(false),
                    Ok(_) => {
                        last_data = Instant::now();
                        if debug {
                            eprintln!("chat< {}", line.trim_end().chars().take(160).collect::<String>());
                        }
                        match parse_irc(&line) {
                            Irc::Message(mut m) => {
                                if let Some(e) = emotes.lock().unwrap().as_ref() {
                                    m.parts = e.apply(m.parts);
                                }
                                batch.push(m);
                            }
                            Irc::Remove { id, name } => sink.remove(id.as_deref(), name.as_deref()),
                            Irc::Ping(p) => write!(s, "PONG {p}\r\n")?,
                            Irc::Room(id) => {
                                sink.state(LiveState::On);
                                if !asked_emotes {
                                    asked_emotes = true;
                                    let slot = emotes.clone();
                                    std::thread::spawn(move || *slot.lock().unwrap() = Some(Emotes::for_twitch(&id)));
                                }
                            }
                            Irc::Reconnect => return Ok(false),
                            Irc::Other => {}
                        }
                    }
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                        // Quiet for a long while: make sure the line is still there.
                        if last_data.elapsed() > Duration::from_secs(240) {
                            write!(s, "PING :neria\r\n")?;
                            last_data = Instant::now();
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
                if !batch.is_empty() && last_flush.elapsed() >= Duration::from_millis(200) {
                    sink.push(std::mem::take(&mut batch));
                    last_flush = Instant::now();
                }
            }
        })();
        match run {
            Ok(true) => return Ok(()),
            Ok(false) => failures = 0,
            Err(e) => {
                failures += 1;
                sink.state(LiveState::Retrying(format!("Chat dropped: {e}")));
            }
        }
        sleep_unless(stop, Duration::from_secs([1, 2, 5, 10, 30][failures.min(4) as usize]));
    }
    Ok(())
}

// ───────────── Kick ─────────────

/// Kick's own public Pusher app (the key its web page uses for chat; anyone can listen).
const KICK_PUSHER: &str =
    "wss://ws-us2.pusher.com/app/32cbd69e4b950bf97679?protocol=7&client=js&version=8.4.0&flash=false";

/// `[emote:37226:KEKW]` in a Kick message → the emote's picture.
fn kick_parts(content: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut rest = content;
    while let Some(at) = rest.find("[emote:") {
        let after = &rest[at + 7..];
        let Some(end) = after.find(']') else { break };
        let inner = &after[..end];
        let Some((id, name)) = inner.split_once(':') else { break };
        if !id.chars().all(|c| c.is_ascii_digit()) {
            break;
        }
        if at > 0 {
            parts.push(Part::Text { s: rest[..at].to_string() });
        }
        parts.push(Part::Emote { name: name.to_string(), url: format!("https://files.kick.com/emotes/{id}/fullsize") });
        rest = &after[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(Part::Text { s: rest.to_string() });
    }
    emojify(parts)
}

/// A Kick chat message (Pusher's `ChatMessageEvent` data) as Neria's.
fn kick_message(d: &Value, now: f64) -> Option<Message> {
    let sender = &d["sender"];
    let name = sender["username"].as_str()?.to_string();
    let content = d["content"].as_str()?;
    let badges: Vec<String> = sender["identity"]["badges"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| b["type"].as_str())
                .map(|t| match t {
                    "og" | "founder" | "sub_gifter" => "subscriber".to_string(),
                    "staff" => "verified".to_string(),
                    t => t.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Message {
        t: now,
        id: d["id"].as_str().map(String::from).unwrap_or_default(),
        site: Site::Kick,
        name,
        color: sender["identity"]["color"].as_str().filter(|c| c.starts_with('#')).map(String::from),
        badges,
        parts: kick_parts(content),
        ..Message::default()
    })
}

fn kick_live(url: &str, stop: &AtomicBool, sink: &mut Sink) -> Result<()> {
    let slug = kick_slug(url).context("Not a Kick channel")?;
    let resp = crate::web::client()?
        .get(format!("https://kick.com/api/v2/channels/{slug}"))
        .header("Accept", "application/json")
        .send()?;
    let status = resp.status();
    if !status.is_success() {
        bail!("Kick didn't say where this channel's chat is (HTTP {})", status.as_u16());
    }
    let info: Value = serde_json::from_str(&resp.text()?).unwrap_or(Value::Null);
    let room = info["chatroom"]["id"].as_i64().context("Kick didn't say where this channel's chat is")?;
    *sink.shared.channel.lock().unwrap() = info["user"]["username"].as_str().unwrap_or(&slug).to_string();
    let mut failures = 0u32;
    while !stop.load(Ordering::SeqCst) {
        let run = (|| -> Result<bool> {
            let (mut ws, _) = tungstenite::connect(KICK_PUSHER)?;
            if let tungstenite::stream::MaybeTlsStream::NativeTls(s) = ws.get_mut() {
                s.get_mut().set_read_timeout(Some(Duration::from_millis(500)))?;
            }
            let sub = json!({ "event": "pusher:subscribe", "data": { "auth": "", "channel": format!("chatrooms.{room}.v2") } });
            ws.send(tungstenite::Message::Text(sub.to_string().into()))?;
            let mut batch = Vec::new();
            let mut last_flush = Instant::now();
            loop {
                if stop.load(Ordering::SeqCst) {
                    let _ = ws.close(None);
                    return Ok(true);
                }
                match ws.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                        let event = v["event"].as_str().unwrap_or("");
                        // Pusher's data is JSON inside a string.
                        let data: Value =
                            v["data"].as_str().and_then(|d| serde_json::from_str(d).ok()).unwrap_or(Value::Null);
                        match event {
                            "pusher:ping" => ws.send(tungstenite::Message::Text(
                                json!({ "event": "pusher:pong", "data": {} }).to_string().into(),
                            ))?,
                            "pusher_internal:subscription_succeeded" => sink.state(LiveState::On),
                            "App\\Events\\ChatMessageEvent" => {
                                if let Some(m) = kick_message(&data, crate::live::now()) {
                                    batch.push(m);
                                }
                            }
                            "App\\Events\\MessageDeletedEvent" => sink.remove(data["message"]["id"].as_str(), None),
                            "App\\Events\\UserBannedEvent" => sink.remove(None, data["user"]["username"].as_str()),
                            "pusher:error" => bail!("{}", data["message"].as_str().unwrap_or("Kick's chat refused")),
                            _ => {}
                        }
                    }
                    Ok(tungstenite::Message::Close(_)) => return Ok(false),
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e))
                        if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                    Err(e) => return Err(e.into()),
                }
                if !batch.is_empty() && last_flush.elapsed() >= Duration::from_millis(200) {
                    sink.push(std::mem::take(&mut batch));
                    last_flush = Instant::now();
                }
            }
        })();
        match run {
            Ok(true) => return Ok(()),
            Ok(false) => failures = 0,
            Err(e) => {
                failures += 1;
                sink.state(LiveState::Retrying(format!("Chat dropped: {e}")));
            }
        }
        sleep_unless(stop, Duration::from_secs([1, 2, 5, 10, 30][failures.min(4) as usize]));
    }
    Ok(())
}

/// What YouTube's chat page gives to start polling.
struct YtStart {
    key: Option<String>,
    version: String,
    continuation: String,
    initial: Vec<Message>,
}

fn find_str(text: &str, key: &str) -> Option<String> {
    let at = text.find(key)? + key.len();
    let rest = &text[at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The JSON object that starts right after `marker` in `html`.
fn json_after(html: &str, marker: &str) -> Option<Value> {
    let start = html.find(marker)? + marker.len();
    let rest = html[start..].trim_start();
    let mut de = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
    de.next()?.ok()
}

/// The video id a channel's `/live` page is showing now.
fn yt_live_id(url: &str) -> Result<String> {
    if let Some(id) = youtube_id(url) {
        return Ok(id);
    }
    let html = yt_get(url)?;
    let id = find_str(&html, "<link rel=\"canonical\" href=\"https://www.youtube.com/watch?v=")
        .context("That channel isn't live now")?;
    Ok(id)
}

fn yt_get(url: &str) -> Result<String> {
    let resp = crate::web::client()?
        .get(url)
        .header("Cookie", "SOCS=CAI; CONSENT=YES+")
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()?;
    if !resp.status().is_success() {
        bail!("YouTube said {}", resp.status());
    }
    Ok(String::from_utf8_lossy(&resp.bytes()?).into_owned())
}

fn yt_start(id: &str) -> Result<YtStart> {
    let html = yt_get(&format!("https://www.youtube.com/live_chat?is_popout=1&v={id}"))?;
    let data = json_after(&html, "window[\"ytInitialData\"] =")
        .or_else(|| json_after(&html, "var ytInitialData ="))
        .or_else(|| json_after(&html, "ytInitialData ="))
        .context("This stream's chat is off")?;
    let r = &data["contents"]["liveChatRenderer"];
    if r.is_null() {
        let why = yt_text(&data["contents"]["messageRenderer"]["text"]);
        bail!("{}", if why.is_empty() { "This stream's chat is off".to_string() } else { why });
    }
    // The view menu's second item is "Live chat" (everything); the default is "Top chat".
    let items = &r["header"]["liveChatHeaderRenderer"]["viewSelector"]["sortFilterSubMenuRenderer"]["subMenuItems"];
    let all = items[1]["continuation"]["reloadContinuationData"]["continuation"].as_str();
    let first =
        r["continuations"][0].as_object().and_then(|o| o.values().next()).and_then(|c| c["continuation"].as_str());
    let continuation = all.or(first).context("This stream's chat is off")?.to_string();
    let mut initial = Vec::new();
    let mut removed = Vec::new();
    yt_actions(&r["actions"], &mut initial, &mut removed);
    Ok(YtStart {
        key: find_str(&html, "\"INNERTUBE_API_KEY\":\""),
        version: find_str(&html, "\"INNERTUBE_CLIENT_VERSION\":\"").unwrap_or_else(|| "2.20261116.00.00".into()),
        continuation,
        initial,
    })
}

fn youtube_live(url: &str, stop: &AtomicBool, sink: &mut Sink) -> Result<()> {
    let id = yt_live_id(url)?;
    *sink.shared.channel.lock().unwrap() = id.clone();
    let mut failures = 0u32;
    'outer: while !stop.load(Ordering::SeqCst) {
        let start = match yt_start(&id) {
            Ok(s) => s,
            Err(e) if failures < 3 && !e.to_string().contains("off") => {
                failures += 1;
                sink.state(LiveState::Retrying(e.to_string()));
                sleep_unless(stop, Duration::from_secs(5));
                continue;
            }
            Err(e) => return Err(e),
        };
        sink.state(LiveState::On);
        // The page's first messages are older than now: only those not already in.
        let have: HashSet<String> = sink.shared.msgs.lock().unwrap().iter().map(|m| m.id.clone()).collect();
        sink.push(start.initial.into_iter().filter(|m| !have.contains(&m.id)).collect());
        let client = crate::web::client()?;
        let endpoint = match &start.key {
            Some(k) => format!("https://www.youtube.com/youtubei/v1/live_chat/get_live_chat?key={k}&prettyPrint=false"),
            None => "https://www.youtube.com/youtubei/v1/live_chat/get_live_chat?prettyPrint=false".into(),
        };
        let mut continuation = start.continuation;
        let mut seen: HashSet<String> = have;
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let body = json!({
                "context": {"client": {"clientName": "WEB", "clientVersion": start.version, "hl": "en"}},
                "continuation": continuation,
            });
            let resp = client.post(&endpoint).header("Content-Type", "application/json").body(body.to_string()).send();
            let v: Value = match resp
                .and_then(|r| r.text())
                .map_err(anyhow::Error::from)
                .and_then(|t| Ok(serde_json::from_str(&t)?))
            {
                Ok(v) => v,
                Err(e) => {
                    failures += 1;
                    sink.state(LiveState::Retrying(format!("Chat dropped: {e}")));
                    if failures > 6 {
                        continue 'outer;
                    }
                    sleep_unless(stop, Duration::from_secs(3));
                    continue;
                }
            };
            failures = 0;
            let c = &v["continuationContents"]["liveChatContinuation"];
            let mut new = Vec::new();
            let mut removed = Vec::new();
            yt_actions(&c["actions"], &mut new, &mut removed);
            new.retain(|m| seen.insert(m.id.clone()));
            sink.push(new);
            for id in removed {
                sink.remove(Some(&id), None);
            }
            let next = c["continuations"][0].as_object().and_then(|o| o.values().next());
            let Some(next) = next.filter(|n| n["continuation"].is_string()) else {
                // No more: the stream ended (or chat was turned off).
                return Ok(());
            };
            if *sink.shared.state.lock().unwrap() != LiveState::On {
                sink.state(LiveState::On);
            }
            continuation = next["continuation"].as_str().unwrap_or_default().to_string();
            // How soon to ask again. An "invalidation" feed's 10 s is only the page's fallback:
            // YouTube's page is told the moment there's more (a push Neria doesn't have) and asks
            // at once, so chat there runs ~12 s ahead of a 10 s poll (measured). Ask every 2 s
            // instead; a "timed" feed's own wait, at most 5 s.
            let invalidation = c["continuations"][0].get("invalidationContinuationData").is_some();
            let wait = if invalidation { 2000 } else { next["timeoutMs"].as_u64().unwrap_or(3000).clamp(1000, 5000) };
            sleep_unless(stop, Duration::from_millis(wait));
        }
    }
    Ok(())
}

/// The lines a recording logged (`LiveChat::start`'s `log`), with removals applied.
pub fn read_log(path: &Path) -> Vec<Message> {
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    let mut out: Vec<Message> = Vec::new();
    for line in BufReader::new(f).lines().map_while(|l| l.ok()) {
        if line.starts_with("{\"removed\"") {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                let (id, name) = (v["removed"].as_str(), v["name"].as_str());
                out.retain(|m| id.is_none_or(|i| m.id != i) && name.is_none_or(|n| !m.name.eq_ignore_ascii_case(n)));
            }
            continue;
        }
        if let Ok(m) = serde_json::from_str::<Message>(&line) {
            out.push(m);
        }
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

/// A chat on the clock (Unix seconds) as the chat of a file whose second 0 was `zero`.
pub fn to_file(messages: Vec<Message>, zero: f64, sites: Vec<Site>, channels: Vec<String>) -> ChatFile {
    let messages: Vec<Message> = messages.into_iter().map(|m| Message { t: m.t - zero, ..m }).collect();
    let from = messages.first().map(|m| m.t.max(0.0)).unwrap_or(0.0);
    ChatFile { version: 1, sites, channels, zero: Some(zero), from, messages }
}

// ───────────── past streams ─────────────

/// How fetching a past stream's chat is going: messages so far, and how far into the video
/// (0–1) when that's known.
pub type Progress<'a> = &'a mut dyn FnMut(usize, Option<f64>);

/// A past stream's whole chat (`t`: seconds into the video). `duration` helps say how far.
pub fn fetch_replay(url: &str, duration: Option<f64>, cancel: &AtomicBool, on: Progress) -> Result<ChatFile> {
    match Site::of(url) {
        Some(Site::YouTube) => youtube_replay(url, duration, cancel, on),
        Some(Site::Twitch) => twitch_replay(url, duration, cancel, on),
        Some(Site::Kick) => bail!("Neria reads Kick's chat while it's live, not a past stream's"),
        None => bail!("Neria can read chat from YouTube, Twitch and Kick"),
    }
}

fn youtube_replay(url: &str, duration: Option<f64>, cancel: &AtomicBool, on: Progress) -> Result<ChatFile> {
    let settings = Settings::load();
    let dir = crate::paths::cache_dir().join(format!("chat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let mut args: Vec<String> = [
        "--encoding",
        "utf-8",
        "--skip-download",
        "--write-subs",
        "--sub-langs",
        "live_chat",
        "--no-playlist",
        "--no-progress",
    ]
    .map(String::from)
    .to_vec();
    args.extend(crate::cookies::args_for(url, &settings));
    args.extend(["-o".into(), dir.join("chat.%(ext)s").to_string_lossy().into_owned(), url.to_string()]);
    let mut child = crate::proc::command(crate::paths::tool("yt-dlp"))
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("couldn't start yt-dlp")?;
    let err = child.stderr.take();
    let err_text = Arc::new(Mutex::new(String::new()));
    let et = err_text.clone();
    let reader = std::thread::spawn(move || {
        if let Some(e) = err {
            for l in BufReader::new(e).lines().map_while(|l| l.ok()) {
                let mut t = et.lock().unwrap();
                t.push_str(&l);
                t.push('\n');
            }
        }
    });
    let file = dir.join("chat.live_chat.json");
    let file_part = dir.join("chat.live_chat.json.part");
    let mut messages = Vec::new();
    let mut read_to = 0u64;
    let read_part = |path: &Path, messages: &mut Vec<Message>, read_to: &mut u64| {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(path) else { return };
        if f.seek(SeekFrom::Start(*read_to)).is_err() {
            return;
        }
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_err() {
            return;
        }
        // Only whole lines: the last may still be being written.
        let upto = buf.rfind('\n').map(|i| i + 1).unwrap_or(0);
        for line in buf[..upto].lines() {
            messages.extend(parse_yt_replay_line(line));
        }
        *read_to += upto as u64;
    };
    loop {
        if cancel.load(Ordering::SeqCst) {
            crate::proc::kill_tree(&mut child);
            let _ = std::fs::remove_dir_all(&dir);
            bail!("Cancelled");
        }
        let done = child.try_wait()?.is_some();
        let path = if file_part.is_file() { &file_part } else { &file };
        read_part(path, &mut messages, &mut read_to);
        let frac = duration.filter(|d| *d > 0.0).and_then(|d| messages.last().map(|m| (m.t / d).clamp(0.0, 1.0)));
        on(messages.len(), frac);
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    let _ = reader.join();
    if file.is_file() && read_to == 0 {
        read_part(&file, &mut messages, &mut read_to);
    }
    let _ = std::fs::remove_dir_all(&dir);
    if messages.is_empty() {
        let e = err_text.lock().unwrap().clone();
        if e.contains("live_chat") || e.contains("subtitles") || e.is_empty() {
            bail!("This video has no saved chat");
        }
        bail!("{}", e.lines().rev().find(|l| l.contains("ERROR")).unwrap_or("Couldn't get its chat").trim());
    }
    messages.sort_by(|a, b| a.t.total_cmp(&b.t));
    Ok(ChatFile { version: 1, sites: vec![Site::YouTube], channels: Vec::new(), zero: None, from: 0.0, messages })
}

/// Twitch's saved chat, a page at a time from a second into the video. (Its page cursors
/// need a browser check; seconds don't.)
const TWITCH_COMMENTS: &str = "query($id: ID!, $offset: Int) { video(id: $id) { lengthSeconds creator { id login }     comments(contentOffsetSeconds: $offset) { edges { node { id contentOffsetSeconds     commenter { displayName login } message { userColor userBadges { setID version }     fragments { text emote { emoteID } } } } } pageInfo { hasNextPage } } } }";

fn twitch_gql(body: &Value) -> Result<Value> {
    let text = crate::web::client()?
        .post("https://gql.twitch.tv/gql")
        .header("Client-ID", TWITCH_CLIENT_ID)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()?
        .text()?;
    Ok(serde_json::from_str(&text)?)
}

// ───────────── the same stream elsewhere ─────────────

/// The same streamer live on the other site at the same time (a multi-stream), whose chat
/// can be read beside this one.
#[derive(Clone, Debug, PartialEq)]
pub struct AlsoLive {
    pub site: Site,
    pub link: String,
}

/// Words of a title, for "the same stream": lowercase, 3+ letters, no emoji or tags.
fn title_words(t: &str) -> HashSet<String> {
    t.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).map(|w| w.to_lowercase()).collect()
}

/// Two titles of the same stream: most of the shorter one's words are in the other.
pub fn same_title(a: &str, b: &str) -> bool {
    let (wa, wb) = (title_words(a), title_words(b));
    let small = wa.len().min(wb.len());
    if small == 0 {
        return false;
    }
    let shared = wa.intersection(&wb).count();
    shared * 10 >= small * 6
}

/// Logins in `twitch.tv/<login>` links in `text`.
fn twitch_links(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, _) in text.match_indices("twitch.tv/") {
        let login: String = text[i + 10..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<String>()
            .to_lowercase();
        if login.len() >= 3
            && !["videos", "directory", "p", "search"].contains(&login.as_str())
            && !out.contains(&login)
        {
            out.push(login);
        }
    }
    out
}

/// YouTube channel paths (`@name`, `channel/UC…`, `c/name`) in links in `text`.
fn youtube_channels(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, _) in text.match_indices("youtube.com/") {
        let rest = &text[i + 12..];
        let path: String = if let Some(r) = rest.strip_prefix('@') {
            let n: String = r.chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.')).collect();
            format!("@{n}")
        } else if let Some(r) = rest.strip_prefix("channel/") {
            format!(
                "channel/{}",
                r.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).collect::<String>()
            )
        } else if let Some(r) = rest.strip_prefix("c/") {
            format!("c/{}", r.chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-')).collect::<String>())
        } else {
            continue;
        };
        if path.len() > 3 && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// A Twitch login's stream title when it's live.
fn twitch_live_title(login: &str) -> Option<String> {
    let q = json!({ "query": format!("query{{user(login:\"{login}\"){{stream{{title}}}}}}") });
    let v = twitch_gql(&q).ok()?;
    let s = &v["data"]["user"]["stream"];
    (!s.is_null()).then(|| s["title"].as_str().unwrap_or("").to_string())
}

/// A YouTube channel's live stream now (its link and title), from `youtube.com/<path>/live`.
fn youtube_live_now(path: &str) -> Option<(String, String)> {
    let html = yt_get(&format!("https://www.youtube.com/{path}/live")).ok()?;
    if !html.contains("\"isLiveNow\":true") {
        return None;
    }
    let id = find_str(&html, "<link rel=\"canonical\" href=\"https://www.youtube.com/watch?v=")?;
    let title = find_str(&html, "<meta name=\"title\" content=\"").unwrap_or_default();
    Some((format!("https://www.youtube.com/watch?v={id}"), title))
}

/// The same stream live on the other site now: a link to it in the stream's description or
/// the channel's links (enough by itself), else a channel of the same name with a matching
/// title. A few quick looks; run it off the UI thread.
pub fn also_live(url: &str) -> Option<AlsoLive> {
    match Site::of(url)? {
        Site::YouTube => {
            let id = yt_live_id(url).ok()?;
            let html = yt_get(&format!("https://www.youtube.com/watch?v={id}")).ok()?;
            let title = find_str(&html, "<meta name=\"title\" content=\"").unwrap_or_default();
            let desc: String = find_str(&html, "\"shortDescription\":\"")
                .map(|d| serde_json::from_str::<String>(&format!("\"{d}\"")).unwrap_or(d))
                .unwrap_or_default();
            let handle = find_str(&html, "\"canonicalBaseUrl\":\"/@").unwrap_or_default().to_lowercase();
            let mut tries: Vec<(String, bool)> = twitch_links(&desc).into_iter().map(|l| (l, true)).collect();
            let name: String = handle.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            if name.len() >= 3 && !tries.iter().any(|(l, _)| *l == name) {
                tries.push((name, false));
            }
            for (login, linked) in tries.into_iter().take(4) {
                if let Some(t) = twitch_live_title(&login)
                    && (linked || same_title(&t, &title))
                {
                    return Some(AlsoLive { site: Site::Twitch, link: format!("https://www.twitch.tv/{login}") });
                }
            }
            None
        }
        Site::Twitch => {
            let login = crate::ttv::login_of(url)?;
            let q = json!({ "query": format!("query{{user(login:\"{login}\"){{description stream{{title}} channel{{socialMedias{{url}}}}}}}}") });
            let v = twitch_gql(&q).ok()?;
            let u = &v["data"]["user"];
            let title = u["stream"]["title"].as_str().unwrap_or("").to_string();
            let mut text = u["description"].as_str().unwrap_or("").to_string();
            for s in u["channel"]["socialMedias"].as_array().into_iter().flatten() {
                text.push(' ');
                text.push_str(s["url"].as_str().unwrap_or(""));
            }
            let mut tries: Vec<(String, bool)> = youtube_channels(&text).into_iter().map(|c| (c, true)).collect();
            let same = format!("@{login}");
            if !tries.iter().any(|(c, _)| c.eq_ignore_ascii_case(&same)) {
                tries.push((same, false));
            }
            for (path, linked) in tries.into_iter().take(4) {
                if let Some((link, t)) = youtube_live_now(&path)
                    && (linked || same_title(&t, &title))
                {
                    return Some(AlsoLive { site: Site::YouTube, link });
                }
            }
            None
        }
        Site::Kick => None,
    }
}

fn twitch_comment(node: &Value) -> Option<Message> {
    let name = node["commenter"]["displayName"]
        .as_str()
        .or(node["commenter"]["login"].as_str())
        .unwrap_or("someone")
        .to_string();
    let msg = &node["message"];
    let mut parts = Vec::new();
    for f in msg["fragments"].as_array().into_iter().flatten() {
        let text = f["text"].as_str().unwrap_or("").to_string();
        match f["emote"]["emoteID"].as_str() {
            Some(id) => parts.push(Part::Emote {
                name: text,
                url: format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/static/dark/2.0"),
            }),
            None => parts.push(Part::Text { s: text }),
        }
    }
    let sets: Vec<&str> =
        msg["userBadges"].as_array().into_iter().flatten().filter_map(|b| b["setID"].as_str()).collect();
    let badges = twitch_badges(&sets.join(","));
    Some(Message {
        t: node["contentOffsetSeconds"].as_f64()?,
        id: node["id"].as_str().unwrap_or("").to_string(),
        site: Site::Twitch,
        color: msg["userColor"].as_str().map(String::from).or_else(|| Some(default_color(&name))),
        name,
        badges,
        parts: emojify(parts),
        ..Message::default()
    })
}

/// One page of a past broadcast's chat from second `offset`: the video's facts and its
/// messages.
fn twitch_page(id: &str, offset: i64) -> Result<(Value, Vec<Message>, bool)> {
    let mut tries = 0;
    loop {
        let v = twitch_gql(&json!({"query": TWITCH_COMMENTS, "variables": {"id": id, "offset": offset}}));
        match v {
            Ok(v) if v["data"]["video"].is_null() && v["errors"].is_null() => {
                bail!("That past stream isn't there any more")
            }
            Ok(v) if v["data"]["video"]["comments"].is_object() => {
                let video = &v["data"]["video"];
                let msgs = video["comments"]["edges"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| twitch_comment(&e["node"]))
                    .collect();
                let more = video["comments"]["pageInfo"]["hasNextPage"].as_bool() == Some(true);
                return Ok((video.clone(), msgs, more));
            }
            Ok(_) | Err(_) if tries < 4 => {
                tries += 1;
                std::thread::sleep(Duration::from_secs(2));
            }
            Ok(v) => {
                bail!("Twitch wouldn't give its chat: {}", v["errors"][0]["message"].as_str().unwrap_or("no answer"))
            }
            Err(e) => return Err(e),
        }
    }
}

fn twitch_replay(url: &str, duration: Option<f64>, cancel: &AtomicBool, on: Progress) -> Result<ChatFile> {
    let id = twitch_video_id(url).context("That's a Twitch channel, not one of its past streams")?;
    let (video, first, _) = twitch_page(&id, 0)?;
    let creator = (
        video["creator"]["id"].as_str().unwrap_or("").to_string(),
        video["creator"]["login"].as_str().unwrap_or("").to_string(),
    );
    let length = video["lengthSeconds"].as_f64().or(duration).unwrap_or(0.0).max(1.0);
    // Pages come a few dozen messages at a time: the video is split into stretches read side
    // by side, each from its start to the next one's.
    const WORKERS: i64 = 8;
    let step = ((length as i64) / WORKERS).max(60);
    let starts: Vec<i64> = (0..WORKERS).map(|i| i * step).filter(|s| (*s as f64) < length).collect();
    let got: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(first));
    let reached: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(starts.clone()));
    let failed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();
    for (i, start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(i64::MAX);
        let (id, got, reached, failed, stop) = (id.clone(), got.clone(), reached.clone(), failed.clone(), stop.clone());
        let mut offset = *start;
        handles.push(std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) && offset < end {
                match twitch_page(&id, offset) {
                    Ok((_, msgs, more)) => {
                        let newest = msgs.iter().map(|m| m.t as i64).max().unwrap_or(offset);
                        let empty = msgs.is_empty();
                        got.lock().unwrap().extend(msgs.into_iter().filter(|m| (m.t as i64) < end));
                        reached.lock().unwrap()[i] = newest.min(end);
                        if empty || !more {
                            break;
                        }
                        // From the newest second reached (again: what's seen is dropped
                        // later); a page all in one second moves on.
                        offset = if newest > offset { newest } else { offset + 1 };
                    }
                    Err(e) => {
                        *failed.lock().unwrap() = Some(e.to_string());
                        stop.store(true, Ordering::SeqCst);
                    }
                }
            }
            reached.lock().unwrap()[i] = end;
        }));
    }
    while handles.iter().any(|h| !h.is_finished()) {
        if cancel.load(Ordering::SeqCst) {
            stop.store(true, Ordering::SeqCst);
        }
        let done: i64 =
            reached.lock().unwrap().iter().zip(&starts).map(|(r, s)| (r.min(&(length as i64)) - s).max(0)).sum();
        on(got.lock().unwrap().len(), Some((done as f64 / length).clamp(0.0, 1.0)));
        std::thread::sleep(Duration::from_millis(300));
    }
    for h in handles {
        let _ = h.join();
    }
    if cancel.load(Ordering::SeqCst) {
        bail!("Cancelled");
    }
    if let Some(e) = failed.lock().unwrap().take() {
        bail!(e);
    }
    let mut seen = HashSet::new();
    let mut messages: Vec<Message> =
        std::mem::take(&mut *got.lock().unwrap()).into_iter().filter(|m| seen.insert(m.id.clone())).collect();
    if messages.is_empty() {
        bail!("This past stream has no saved chat");
    }
    // The channel's 7TV / BetterTTV / FrankerFaceZ emotes.
    if !creator.0.is_empty() {
        let e = Emotes::for_twitch(&creator.0);
        for m in &mut messages {
            m.parts = e.apply(std::mem::take(&mut m.parts));
        }
    }
    messages.sort_by(|a, b| a.t.total_cmp(&b.t));
    Ok(ChatFile { version: 1, sites: vec![Site::Twitch], channels: vec![creator.1], zero: None, from: 0.0, messages })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_youtube_ids() {
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=4t4AQ15yj1M&t=3").as_deref(), Some("4t4AQ15yj1M"));
        assert_eq!(youtube_id("https://youtu.be/4t4AQ15yj1M?si=x").as_deref(), Some("4t4AQ15yj1M"));
        assert_eq!(youtube_id("https://www.youtube.com/live/4t4AQ15yj1M").as_deref(), Some("4t4AQ15yj1M"));
        assert_eq!(youtube_id("https://www.youtube.com/@LofiGirl/live"), None);
        assert_eq!(twitch_video_id("https://www.twitch.tv/videos/2931596568").as_deref(), Some("2931596568"));
        assert_eq!(twitch_video_id("https://www.twitch.tv/zackrawrr"), None);
        assert_eq!(Site::of("https://m.youtube.com/watch?v=x"), Some(Site::YouTube));
        assert_eq!(Site::of("https://www.twitch.tv/x"), Some(Site::Twitch));
        assert_eq!(Site::of("https://kick.com/x"), Some(Site::Kick));
        assert_eq!(kick_slug("https://kick.com/XQC"), Some("xqc".into()));
        // The same stream elsewhere: links in a description, titles that match.
        assert_eq!(twitch_links("Also on https://www.twitch.tv/Foo_Bar and twitch.tv/videos/1"), vec!["foo_bar"]);
        assert_eq!(
            youtube_channels("yt: https://www.youtube.com/@Some.Name x youtube.com/channel/UCabc-1_x"),
            vec!["@Some.Name", "channel/UCabc-1_x"]
        );
        assert!(same_title("🔴 GTA 6 FULL PLAYTHROUGH day 5 !drops", "GTA 6 full playthrough DAY 5"));
        assert!(!same_title("Chill Minecraft build", "GTA 6 full playthrough"));
        assert_eq!(kick_slug("https://kick.com/xqc/videos/abc"), None);
        assert_eq!(kick_slug("https://kick.com/categories/games"), None);
        let p = kick_parts("hi [emote:37226:KEKW] there");
        assert_eq!(
            p[1],
            Part::Emote { name: "KEKW".into(), url: "https://files.kick.com/emotes/37226/fullsize".into() }
        );
    }

    #[test]
    fn reads_twitch_lines() {
        let line = "@badge-info=subscriber/2;badges=moderator/1,subscriber/0;color=#9ACD32;display-name=Goycho;emotes=25:0-4/1902:10-14;id=46e3;tmi-sent-ts=1794957484662 :goycho!goycho@goycho.tmi.twitch.tv PRIVMSG #zack :Kappa lol Keepo";
        let Irc::Message(m) = parse_irc(line) else { panic!() };
        assert_eq!(m.name, "Goycho");
        assert_eq!(m.color.as_deref(), Some("#9ACD32"));
        assert_eq!(m.badges, vec!["moderator", "subscriber"]);
        assert!((m.t - 1794957484.662).abs() < 1e-6);
        assert_eq!(m.parts.len(), 3);
        assert!(matches!(&m.parts[0], Part::Emote { name, .. } if name == "Kappa"));
        assert_eq!(m.parts[1], Part::Text { s: " lol ".into() });
        assert!(matches!(&m.parts[2], Part::Emote { name, .. } if name == "Keepo"));

        let sub = "@badges=;color=;display-name=Ann;msg-id=resub;system-msg=Ann\\ssubscribed\\sfor\\s12\\smonths.;tmi-sent-ts=1 :tmi.twitch.tv USERNOTICE #zack :hype";
        let Irc::Message(m) = parse_irc(sub) else { panic!() };
        assert_eq!(m.kind, Kind::Member);
        assert_eq!(m.note.as_deref(), Some("Ann subscribed for 12 months."));
        assert!(m.color.is_some());

        assert_eq!(parse_irc("PING :tmi.twitch.tv"), Irc::Ping(":tmi.twitch.tv".into()));
        assert_eq!(
            parse_irc("@target-msg-id=abc :tmi.twitch.tv CLEARMSG #zack :bad"),
            Irc::Remove { id: Some("abc".into()), name: None }
        );
        assert_eq!(parse_irc("@room-id=55 :tmi.twitch.tv ROOMSTATE #zack"), Irc::Room("55".into()));
    }

    #[test]
    fn reads_youtube_items() {
        let item: Value = serde_json::from_str(r#"{"liveChatTextMessageRenderer": {"message": {"runs": [{"text": "hi "}, {"emoji": {"emojiId": "😊", "shortcuts": [":blush:"], "image": {"thumbnails": [{"url": "https://fonts.gstatic.com/s/e/notoemoji/15.1/1f60a/72.png"}]}}}]}, "authorName": {"simpleText": "@Ay"}, "id": "X1", "timestampUsec": "1794956358746012", "authorBadges": [{"liveChatAuthorBadgeRenderer": {"icon": {"iconType": "MODERATOR"}}}]}}"#).unwrap();
        let m = parse_yt_item(&item).unwrap();
        assert_eq!(m.name, "@Ay");
        assert_eq!(m.badges, vec!["moderator"]);
        assert_eq!(m.color.as_deref(), Some("#5E84F1"));
        assert_eq!(m.parts.len(), 2);
        assert!((m.t - 1794956358.746012).abs() < 1e-3);

        let paid: Value = serde_json::from_str(r#"{"liveChatPaidMessageRenderer": {"authorName": {"simpleText": "Rin"}, "purchaseAmountText": {"simpleText": "$20.00"}, "message": {"runs": [{"text": "best play"}]}, "id": "P", "timestampUsec": "1"}}"#).unwrap();
        let m = parse_yt_item(&paid).unwrap();
        assert_eq!((m.kind, m.amount.as_deref()), (Kind::Paid, Some("$20.00")));

        let line = r#"{"replayChatItemAction": {"actions": [{"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {"message": {"runs": [{"text": "gg"}]}, "authorName": {"simpleText": "a"}, "id": "R", "timestampUsec": "1"}}}}], "videoOffsetTimeMsec": "12345"}}"#;
        let ms = parse_yt_replay_line(line);
        assert_eq!(ms.len(), 1);
        assert!((ms[0].t - 12.345).abs() < 1e-9);
    }

    #[test]
    fn turns_emoji_into_pictures() {
        let parts = emojify(vec![Part::Text { s: "nice 👍🏽 ok ❤️ © x".into() }]);
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], Part::Text { s: "nice ".into() });
        assert!(matches!(&parts[1], Part::Emote { url, .. } if url.ends_with("/1f44d_1f3fd/72.png")));
        assert!(matches!(&parts[3], Part::Emote { url, .. } if url.ends_with("/2764/72.png")));
        assert_eq!(parts[4], Part::Text { s: " © x".into() });
        // A family: one picture.
        let fam = emojify(vec![Part::Text { s: "👨‍👩‍👧".into() }]);
        assert_eq!(fam.len(), 1);
    }

    #[test]
    fn filters_for_a_clip() {
        let m = |t: f64, name: &str, text: &str, kind: Kind| Message {
            t,
            id: format!("{t}"),
            name: name.into(),
            parts: vec![Part::Text { s: text.into() }],
            kind,
            ..Message::default()
        };
        let all = vec![
            m(1.0, "a", "early", Kind::Chat),
            m(10.0, "Nightbot", "follow me", Kind::Chat),
            m(11.0, "b", "!socials", Kind::Chat),
            m(12.0, "c", "CLIP IT", Kind::Chat),
            m(13.0, "d", "thanks", Kind::Paid),
            m(30.0, "e", "late", Kind::Chat),
        ];
        let f = Filter { no_bots: true, ..Filter::default() };
        let got = for_clip(&all, 9.0, 20.0, &f);
        assert_eq!(got.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["c", "d"]);
        assert!((got[0].t - 3.0).abs() < 1e-9);
        let f = Filter { which: Which::Highlights, words: vec!["clip".into()], ..Filter::default() };
        assert_eq!(for_clip(&all, 0.0, 40.0, &f).len(), 2);
        let f = Filter { which: Which::Picked, picked: ["30".to_string()].into(), ..Filter::default() };
        assert_eq!(for_clip(&all, 0.0, 40.0, &f).len(), 1);
        assert_eq!(activity(&all, 0.0, 40.0, 4), vec![1, 4, 0, 1]);
    }

    #[test]
    fn applies_channel_emotes() {
        let e = Emotes { map: [("OMEGALUL".to_string(), "u".to_string())].into() };
        let p = e.apply(vec![Part::Text { s: "lol OMEGALUL ok".into() }]);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], Part::Text { s: "lol ".into() });
        assert_eq!(p[2], Part::Text { s: " ok".into() });
    }
}
