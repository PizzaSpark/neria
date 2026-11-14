//! Signed-in downloads: one Netscape `cookies.txt` per site in `cookies/` next to the exe.
//!
//! Cookies come from Neria's own sign-in window (the desktop app's `--sign-in` mode) or
//! from a `cookies.txt` the user imports. A site's file only ever holds that site's cookies,
//! and it is only handed to yt-dlp / gallery-dl for links on that same site, so a cookie
//! never goes anywhere it didn't come from. Nothing leaves the PC.

use std::path::{Path, PathBuf};

use crate::paths;
use crate::settings::Settings;

/// Folder with one `<site>.txt` per signed-in site.
pub fn dir() -> PathBuf {
    paths::app_dir().join("cookies")
}

/// Second-level labels under country domains (`bbc.co.uk`, `abc.net.au`).
const SECOND_LEVEL: [&str; 8] = ["co", "com", "net", "org", "ac", "gov", "edu", "ne"];

/// The site a host belongs to: its registrable domain, lower case (`www.youtube.com` and
/// `music.youtube.com` → `youtube.com`, `news.bbc.co.uk` → `bbc.co.uk`). Short links count as
/// the site they lead to (`youtu.be` → `youtube.com`).
pub fn site_of_host(host: &str) -> Option<String> {
    let host = host.trim().trim_start_matches('.').trim_end_matches('.').to_ascii_lowercase();
    let host = host.rsplit_once('@').map_or(host.as_str(), |h| h.1).split(':').next().unwrap_or("").to_string();
    if host.is_empty() || !host.contains('.') || host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    if !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        return None;
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() < 2 {
        return None;
    }
    let n = labels.len();
    let take = if n >= 3 && labels[n - 1].len() == 2 && SECOND_LEVEL.contains(&labels[n - 2]) { 3 } else { 2 };
    let site = labels[n.saturating_sub(take)..].join(".");
    Some(match site.as_str() {
        "youtu.be" | "youtube-nocookie.com" => "youtube.com".into(),
        "instagr.am" => "instagram.com".into(),
        "fb.watch" | "fb.com" => "facebook.com".into(),
        "redd.it" => "reddit.com".into(),
        _ => site,
    })
}

/// The site of a link (`https://www.youtube.com/watch?v=…` → `youtube.com`).
pub fn site_of(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    site_of_host(rest.split(['/', '?', '#']).next()?)
}

/// A friendly name for a site: "YouTube", or the domain itself.
pub fn site_name(site: &str) -> String {
    match site {
        "youtube.com" => "YouTube",
        "instagram.com" => "Instagram",
        "facebook.com" => "Facebook",
        "x.com" | "twitter.com" => "X",
        "tiktok.com" => "TikTok",
        "twitch.tv" => "Twitch",
        "vimeo.com" => "Vimeo",
        "reddit.com" => "Reddit",
        "patreon.com" => "Patreon",
        "soundcloud.com" => "SoundCloud",
        "bilibili.com" => "Bilibili",
        "dailymotion.com" => "Dailymotion",
        "nicovideo.jp" => "Niconico",
        "spotify.com" => "Spotify",
        other => other,
    }
    .to_string()
}

/// Where the sign-in window starts: the site's own sign-in page, or its home page.
pub fn sign_in_url(site: &str) -> String {
    match site {
        "youtube.com" => {
            "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fwww.youtube.com%2F".into()
        }
        "instagram.com" => "https://www.instagram.com/accounts/login/".into(),
        "facebook.com" => "https://www.facebook.com/login/".into(),
        "x.com" => "https://x.com/i/flow/login".into(),
        "twitter.com" => "https://twitter.com/i/flow/login".into(),
        "tiktok.com" => "https://www.tiktok.com/login".into(),
        "twitch.tv" => "https://www.twitch.tv/login".into(),
        "vimeo.com" => "https://vimeo.com/log_in".into(),
        "reddit.com" => "https://www.reddit.com/login/".into(),
        "patreon.com" => "https://www.patreon.com/login".into(),
        "soundcloud.com" => "https://soundcloud.com/signin".into(),
        "spotify.com" => "https://accounts.spotify.com/login?continue=https%3A%2F%2Fopen.spotify.com%2F".into(),
        other => format!("https://www.{other}/"),
    }
}

/// Cookies that only exist once you're signed in, so the window can say "Signed in".
/// Sites not listed here: the user says when they're done.
pub fn signed_in_markers(site: &str) -> &'static [&'static str] {
    match site {
        "youtube.com" => &["SAPISID", "__Secure-3PAPISID", "LOGIN_INFO"],
        "instagram.com" => &["sessionid"],
        "facebook.com" => &["c_user"],
        "x.com" | "twitter.com" => &["auth_token"],
        "tiktok.com" => &["sessionid", "sessionid_ss"],
        "twitch.tv" => &["auth-token"],
        "vimeo.com" => &["vimeo"],
        "reddit.com" => &["reddit_session", "token_v2"],
        "patreon.com" => &["session_id"],
        "soundcloud.com" => &["oauth_token"],
        "spotify.com" => &["sp_dc"],
        _ => &[],
    }
}

/// Does a cookie for `domain` belong to `site`? (`.youtube.com`, `www.youtube.com` → yes;
/// `.google.com` → no.)
pub fn belongs_to(domain: &str, site: &str) -> bool {
    let d = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    d == site || d.ends_with(&format!(".{site}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cookie {
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    /// Unix seconds; 0 for a session cookie.
    pub expires: i64,
    pub name: String,
    pub value: String,
}

/// Netscape `cookies.txt`, the format yt-dlp and gallery-dl read.
pub fn to_netscape(cookies: &[Cookie]) -> String {
    let mut out =
        String::from("# Netscape HTTP Cookie File\n# Kept by Neria for one site. Do not share this file.\n\n");
    for c in cookies {
        let domain = c.domain.trim();
        // A domain cookie (sent to subdomains) is written with a leading dot.
        let sub = domain.starts_with('.');
        out.push_str(&format!(
            "{}{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            if c.http_only { "#HttpOnly_" } else { "" },
            domain,
            if sub { "TRUE" } else { "FALSE" },
            if c.path.is_empty() { "/" } else { &c.path },
            if c.secure { "TRUE" } else { "FALSE" },
            c.expires.max(0),
            c.name,
            c.value
        ));
    }
    out
}

/// Read a Netscape `cookies.txt` (as browsers' "Get cookies.txt" extensions export it).
pub fn parse_netscape(text: &str) -> Vec<Cookie> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            let (http_only, line) = match line.strip_prefix("#HttpOnly_") {
                Some(rest) => (true, rest),
                None if line.starts_with('#') || line.trim().is_empty() => return None,
                None => (false, line),
            };
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 7 {
                return None;
            }
            Some(Cookie {
                domain: f[0].trim().to_string(),
                path: f[2].trim().to_string(),
                secure: f[3].trim().eq_ignore_ascii_case("true"),
                http_only,
                expires: f[4].trim().parse::<f64>().map(|v| v as i64).unwrap_or(0),
                name: f[5].to_string(),
                value: f[6..].join("\t"),
            })
        })
        .filter(|c| !c.domain.is_empty() && !c.name.is_empty())
        .collect()
}

fn file_of(site: &str) -> PathBuf {
    dir().join(format!("{site}.txt"))
}

/// The cookie file for this link's site, if the user signed in there.
pub fn file_for(url: &str) -> Option<PathBuf> {
    let site = site_of(url)?;
    let f = file_of(&site);
    f.is_file().then_some(f)
}

/// One cookie's value from a site's sign-in (e.g. Spotify's `sp_dc` for lyrics).
pub fn value(site: &str, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(file_of(site)).ok()?;
    parse_netscape(&text).into_iter().find(|c| c.name == name && !c.value.is_empty()).map(|c| c.value)
}

/// Is there a sign-in for this site?
pub fn signed_in(site: &str) -> bool {
    file_of(site).is_file()
}

/// Keep `cookies` as `site`'s sign-in: only the ones that belong to the site are written.
/// Returns how many were kept.
pub fn save(site: &str, cookies: &[Cookie]) -> anyhow::Result<usize> {
    save_in(&dir(), site, cookies)
}

fn save_in(dir: &Path, site: &str, cookies: &[Cookie]) -> anyhow::Result<usize> {
    let mine: Vec<Cookie> = cookies.iter().filter(|c| belongs_to(&c.domain, site)).cloned().collect();
    if mine.is_empty() {
        anyhow::bail!("no cookies for {site}");
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{site}.txt"));
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, to_netscape(&mine))?;
    std::fs::rename(&tmp, &path)?;
    Ok(mine.len())
}

/// Import a `cookies.txt`: its cookies are split by site, one file each. Returns the sites.
pub fn import(file: &Path) -> anyhow::Result<Vec<String>> {
    import_in(&dir(), file)
}

fn import_in(dir: &Path, file: &Path) -> anyhow::Result<Vec<String>> {
    let text = std::fs::read_to_string(file)?;
    let cookies = parse_netscape(&text);
    let mut sites: Vec<String> = cookies.iter().filter_map(|c| site_of_host(&c.domain)).collect();
    sites.sort();
    sites.dedup();
    if sites.is_empty() {
        anyhow::bail!("That file has no cookies in it (it should be a Netscape cookies.txt).");
    }
    for site in &sites {
        save_in(dir, site, &cookies)?;
    }
    Ok(sites)
}

/// Forget a site's sign-in, copies made for runs included.
pub fn sign_out(site: &str) -> anyhow::Result<()> {
    let path = file_of(site);
    if path.is_file() {
        std::fs::remove_file(path)?;
    }
    let prefix = format!("{site}-");
    for e in std::fs::read_dir(runs_dir()).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(())
}

/// Every signed-in site and when it was saved (Unix seconds), newest first.
pub fn list() -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let site = name.strip_suffix(".txt")?.to_string();
            let at =
                e.metadata().ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            Some((site, at))
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.1));
    out
}

/// What to tell yt-dlp / gallery-dl about cookies for `url`: the site's own file when the
/// user signed in there, else the browser from Settings → Network, else nothing.
///
/// yt-dlp writes its cookies back to the `--cookies` file when it ends, and several
/// downloads run at once, so each run gets its own copy: the site's file is only ever read
/// (and its date keeps saying when you signed in).
pub fn args_for(url: &str, settings: &Settings) -> Vec<String> {
    if let Some(f) = file_for(url) {
        let site = site_of(url).unwrap_or_default();
        let file = private_copy(&site, &f).unwrap_or(f);
        return vec!["--cookies".into(), file.to_string_lossy().into_owned()];
    }
    if !settings.cookies_browser.is_empty() {
        return vec!["--cookies-from-browser".into(), settings.cookies_browser.clone()];
    }
    Vec::new()
}

fn runs_dir() -> PathBuf {
    paths::cache_dir().join("cookie-runs")
}

/// Remove every run copy left from earlier sessions (and this one's): for when Neria starts
/// and quits, so copies of a sign-in don't outlive the runs that needed them.
pub fn clear_run_copies() {
    let _ = std::fs::remove_dir_all(runs_dir());
}

/// A throwaway copy of a site's cookie file for one yt-dlp / gallery-dl run, in the cache
/// (`<site>-<pid>-<n>.txt`). Copies older than an hour are removed on the way (lookups and
/// most downloads are long over by then; a longer run already read its cookies); the rest
/// go when Neria quits or starts, or when you sign out of the site.
fn private_copy(site: &str, file: &Path) -> Option<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = runs_dir();
    std::fs::create_dir_all(&dir).ok()?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            if old.is_some_and(|age| age.as_secs() > 3_600) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let copy = dir.join(format!("{site}-{}-{n}.txt", std::process::id()));
    std::fs::copy(file, &copy).ok()?;
    Some(copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites() {
        assert_eq!(site_of("https://www.youtube.com/watch?v=x").as_deref(), Some("youtube.com"));
        assert_eq!(site_of("https://music.youtube.com/watch?v=x").as_deref(), Some("youtube.com"));
        assert_eq!(site_of("https://youtu.be/x").as_deref(), Some("youtube.com"));
        assert_eq!(site_of("https://news.bbc.co.uk/a").as_deref(), Some("bbc.co.uk"));
        assert_eq!(site_of("https://www.instagram.com:443/p/1").as_deref(), Some("instagram.com"));
        assert_eq!(site_of("https://127.0.0.1/x"), None);
        assert_eq!(site_of("not a link"), None);
        assert!(belongs_to(".youtube.com", "youtube.com"));
        assert!(belongs_to("www.youtube.com", "youtube.com"));
        assert!(!belongs_to(".google.com", "youtube.com"));
        assert!(!belongs_to("notyoutube.com", "youtube.com"));
    }

    #[test]
    fn netscape_round_trip() {
        let c = vec![
            Cookie {
                domain: ".youtube.com".into(),
                path: "/".into(),
                secure: true,
                http_only: true,
                expires: 1_900_000_000,
                name: "SID".into(),
                value: "a\tb".into(),
            },
            Cookie {
                domain: "www.youtube.com".into(),
                path: "/".into(),
                secure: false,
                http_only: false,
                expires: 0,
                name: "PREF".into(),
                value: "x=1".into(),
            },
        ];
        let text = to_netscape(&c);
        assert!(text.contains("#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1900000000\tSID"));
        assert_eq!(parse_netscape(&text), c);
    }

    #[test]
    fn import_splits_by_site() {
        let home = std::env::temp_dir().join(format!("neria-cookies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let file = home.join("in.txt");
        std::fs::write(
            &file,
            "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\tSID\t1\n.google.com\tTRUE\t/\tTRUE\t0\tSID\t2\n",
        )
        .unwrap();
        let jar = home.join("cookies");
        let sites = import_in(&jar, &file).unwrap();
        assert_eq!(sites, vec!["google.com".to_string(), "youtube.com".to_string()]);
        let yt = std::fs::read_to_string(jar.join("youtube.com.txt")).unwrap();
        assert!(yt.contains("SID\t1") && !yt.contains("SID\t2"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
