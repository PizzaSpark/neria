//! "Report a problem": a short plain-text report (versions, settings that
//! matter, the link, the last lines of the tool output) to paste in a message.

use std::process::Stdio;
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use crate::settings::Settings;
use crate::{paths, proc};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How many lines of yt-dlp output each job keeps for its report.
pub const LOG_LINES: usize = 40;

/// Versions of Neria's tools, e.g. `[("yt-dlp", "2026.10.31"), ("FFmpeg", "N-121…")]`.
/// Runs each tool once (about a second in total), then cached.
/// Call after the tools are installed and yt-dlp's daily update ran.
pub fn tool_versions() -> &'static [(&'static str, String)] {
    static CACHE: OnceLock<Vec<(&'static str, String)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let missing = || "not installed".to_string();
        vec![
            ("yt-dlp", first_line("yt-dlp", "--version").unwrap_or_else(missing)),
            (
                "FFmpeg",
                // "ffmpeg version N-121234-gabc-20261101 Copyright …"
                first_line("ffmpeg", "-version")
                    .and_then(|l| l.split_whitespace().nth(2).map(str::to_string))
                    .unwrap_or_else(missing),
            ),
            (
                "Deno",
                // "deno 2.5.6 (stable, release, x86_64-pc-windows-msvc)"
                first_line("deno", "--version")
                    .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
                    .unwrap_or_else(missing),
            ),
            (
                "libmpv",
                file_date(&paths::bin_dir().join(LIBMPV)).map(|d| format!("downloaded {d}")).unwrap_or_else(missing),
            ),
        ]
    })
}

#[cfg(windows)]
const LIBMPV: &str = "libmpv-2.dll";
#[cfg(not(windows))]
const LIBMPV: &str = "libmpv.so";

fn first_line(tool: &str, arg: &str) -> Option<String> {
    let out = proc::command(paths::tool(tool)).arg(arg).stdout(Stdio::piped()).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

fn file_date(path: &std::path::Path) -> Option<String> {
    let secs = std::fs::metadata(path).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(ymd(secs))
}

/// Unix seconds → `YYYY-MM-DD` (UTC).
fn ymd(secs: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

fn windows_version() -> Option<String> {
    #[cfg(windows)]
    {
        // "Microsoft Windows [Version 10.0.26300.1234]"
        let out =
            proc::command("cmd").args(["/c", "ver"]).stdout(Stdio::piped()).stderr(Stdio::null()).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let v = text.split("Version ").nth(1)?.trim().trim_end_matches(']').to_string();
        Some(format!("Windows {v}"))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Hide the user's name: their profile folder becomes `%USERPROFILE%`.
pub fn redact(text: &str) -> String {
    let Some(home) = dirs::home_dir() else { return text.to_string() };
    let home = home.to_string_lossy().into_owned();
    if home.len() < 4 {
        return text.to_string();
    }
    let mut out = text.replace(&home, "%USERPROFILE%");
    // yt-dlp sometimes prints paths with forward slashes, or JSON-escaped.
    out = out.replace(&home.replace('\\', "/"), "%USERPROFILE%");
    out.replace(&home.replace('\\', "\\\\"), "%USERPROFILE%")
}

/// Neria/tool/OS versions and the settings that change how downloads behave.
pub fn header(settings: &Settings) -> String {
    let mut s = String::new();
    let os = windows_version().unwrap_or_else(|| std::env::consts::OS.to_string());
    s.push_str(&format!("Neria {VERSION} · {os} ({})\n", std::env::consts::ARCH));
    let tools: Vec<String> = tool_versions().iter().map(|(n, v)| format!("{n} {v}")).collect();
    s.push_str(&tools.join(" · "));
    s.push('\n');

    let quality = format!("{:?}", settings.quality).replace('P', "");
    let mut opts = vec![
        format!("preset {:?}", settings.preset).to_lowercase(),
        format!("quality {quality}"),
        settings.container.to_string(),
        format!("audio {}", settings.audio_format),
    ];
    if settings.prefer_h264 {
        opts.push("H.264".into());
    }
    if settings.allow_playlists {
        opts.push("playlists on".into());
    }
    if settings.sponsorblock {
        opts.push("SponsorBlock".into());
    }
    opts.push(match settings.cookies_browser.as_str() {
        "" => "no browser cookies".into(),
        b => format!("cookies from {b}"),
    });
    if !settings.rate_limit.trim().is_empty() {
        opts.push(format!("speed limit {}", settings.rate_limit.trim()));
    }
    if settings.filename_template != Settings::default().filename_template {
        opts.push(format!("name template {}", settings.filename_template));
    }
    if !settings.extra_args.trim().is_empty() {
        opts.push(format!("extra args {}", settings.extra_args.trim()));
    }
    // Never the key itself, only whether one is set.
    if settings.spotify_credentials().is_some() {
        opts.push("own Spotify key".into());
    }
    s.push_str(&format!("Settings: {}\n", opts.join(", ")));
    s
}

/// What a report about one queue row needs.
pub struct JobDetails<'a> {
    pub url: &'a str,
    pub title: &'a str,
    pub error: Option<&'a str>,
    pub note: Option<&'a str>,
    /// "Artist - Title (Spotify id)" for Spotify songs.
    pub spotify: Option<String>,
    pub log: &'a [String],
}

/// A report about a failed (or odd) download, ready to paste.
pub fn job_report(settings: &Settings, job: &JobDetails) -> String {
    let mut s = String::from("Neria problem report\n");
    s.push_str(&header(settings));
    s.push('\n');
    if let Some(sp) = &job.spotify {
        s.push_str(&format!("Spotify song: {sp}\n"));
    }
    if !job.url.is_empty() {
        s.push_str(&format!("Link: {}\n", job.url));
        // A Neria sign-in beats the browser setting for this site (the header can't know).
        if let Some(site) = crate::cookies::site_of(job.url).filter(|site| crate::cookies::signed_in(site)) {
            s.push_str(&format!("Signed in to {site} in Neria (its cookies were used, not the browser's)\n"));
        }
    }
    if !job.title.is_empty() && job.title != job.url {
        s.push_str(&format!("Title: {}\n", job.title));
    }
    if let Some(e) = job.error {
        s.push_str(&format!("What happened: {e}\n"));
    }
    if let Some(n) = job.note {
        s.push_str(&format!("Note: {n}\n"));
    }
    if !job.log.is_empty() {
        s.push_str("\nLast lines from yt-dlp:\n```\n");
        for line in job.log {
            s.push_str(line);
            s.push('\n');
        }
        s.push_str("```\n");
    }
    redact(&s)
}

/// A general report (Settings → About): versions and settings, plus a prompt.
pub fn general_report(settings: &Settings) -> String {
    let mut s = String::from("Neria problem report\n");
    s.push_str(&header(settings));
    s.push_str("\nWhat went wrong (and the link, if there is one):\n");
    redact(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(1_762_732_800), "2025-11-10");
        assert_eq!(ymd(951_782_400), "2000-02-29");
    }

    #[test]
    fn redacts_home() {
        let Some(home) = dirs::home_dir() else { return };
        let home = home.to_string_lossy().into_owned();
        let text = format!("Destination: {home}\\Videos\\Neria\\a.mp4 and {}", home.replace('\\', "/"));
        let out = redact(&text);
        assert!(!out.contains(&home), "{out}");
        assert!(out.contains("%USERPROFILE%\\Videos"));
    }

    #[test]
    fn job_report_has_the_basics() {
        let log = vec!["[youtube] x: Downloading webpage".to_string(), "ERROR: boom".to_string()];
        let r = job_report(
            &Settings::default(),
            &JobDetails { url: "https://y/x", title: "X", error: Some("boom"), note: None, spotify: None, log: &log },
        );
        assert!(r.contains("Link: https://y/x"));
        assert!(r.contains("What happened: boom"));
        assert!(r.contains("ERROR: boom"));
        assert!(r.contains(&format!("Neria {VERSION}")));
    }
}
