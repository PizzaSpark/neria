//! Keeping Neria and its tools current, portably.
//!
//! - Neria itself: GitHub releases of `REPO`. The new exe is downloaded next to
//!   the running one and swapped in (Windows lets a running exe be renamed), then
//!   Neria restarts. `bin/` and `settings.json` are never touched.
//! - FFmpeg, Deno and libmpv: their GitHub releases are compared with the files
//!   in `bin/`; a tool counts as outdated once a newer build exists and ours is
//!   over [`TOOL_MAX_AGE_DAYS`] old (FFmpeg and libmpv publish builds daily, so
//!   "newer exists" alone would nag every day). yt-dlp updates itself daily.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::{paths, report, tools};

/// Where Neria's releases are published.
pub const REPO: &str = "PizzaSpark/neria";

/// Tools older than this get offered an update when a newer build exists.
pub const TOOL_MAX_AGE_DAYS: u64 = 30;

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// "0.2.0"
    pub version: String,
    /// Release notes (Markdown), may be empty.
    pub notes: String,
    /// The Windows zip.
    pub zip_url: String,
    pub size: Option<u64>,
}

fn api_get(url: &str) -> Result<Value> {
    let resp = reqwest::blocking::Client::builder()
        .user_agent(concat!("Neria/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()?;
    let status = resp.status();
    if status.as_u16() == 404 {
        bail!("no releases published yet");
    }
    if status.as_u16() == 403 || status.as_u16() == 429 {
        bail!("GitHub is limiting update checks right now, try again in an hour");
    }
    if !status.is_success() {
        bail!("GitHub answered HTTP {status}");
    }
    Ok(serde_json::from_slice(&resp.bytes()?)?)
}

/// `NERIA_UPDATE_URL` (dev hook) points the check at another "latest release" JSON.
fn latest_url() -> String {
    std::env::var("NERIA_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"))
}

/// The newest Neria release, if it's newer than this build.
pub fn check_neria() -> Result<Option<Release>> {
    let v = api_get(&latest_url()).context("couldn't check for a new Neria")?;
    let rel = parse_release(&v)?;
    Ok(is_newer(&rel.version, report::VERSION).then_some(rel))
}

fn parse_release(v: &Value) -> Result<Release> {
    let tag = v["tag_name"].as_str().ok_or_else(|| anyhow!("release has no tag"))?;
    let asset = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.ends_with("-windows-x64.zip")))
        .ok_or_else(|| anyhow!("release {tag} has no Windows download"))?;
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        notes: v["body"].as_str().unwrap_or("").trim().to_string(),
        zip_url: asset["browser_download_url"].as_str().unwrap_or("").to_string(),
        size: asset["size"].as_u64(),
    })
}

fn parse_version(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
}

/// `a` is a newer version than `b` ("0.10.0" > "0.9.3").
pub fn is_newer(a: &str, b: &str) -> bool {
    let (mut a, mut b) = (parse_version(a), parse_version(b));
    a.resize(3, 0);
    b.resize(3, 0);
    a > b
}

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().context("can't find Neria's own file")
}

/// Where the replaced exe is parked until the next start.
fn old_exe(exe: &Path) -> PathBuf {
    exe.with_extension("old")
}

/// Download the release and swap it in for the running exe. Call
/// [`restart`] afterwards. `on(downloaded, total)` reports progress.
pub fn install_neria(rel: &Release, on: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let exe = current_exe()?;
    let dir = exe.parent().ok_or_else(|| anyhow!("Neria has no folder"))?.to_path_buf();
    let zip = paths::cache_dir().join(format!("Neria-{}.zip", rel.version));
    tools::download_to(&rel.zip_url, &zip, on).context("downloading the update")?;

    const NEW: &str = ".neria-update.exe";
    let res = tools::extract_zip(&zip, &dir, &[("Neria.exe", NEW)]);
    let _ = std::fs::remove_file(&zip);
    res.context("unpacking the update")?;
    let new = dir.join(NEW);

    let old = old_exe(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).context("Neria couldn't replace its own file (is the folder read-only?)")?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        let _ = std::fs::remove_file(&new);
        return Err(e).context("couldn't put the new version in place");
    }
    Ok(())
}

/// Start Neria again (the new exe after an update). The caller should exit right after.
pub fn restart() -> Result<()> {
    let exe = current_exe()?;
    std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn().context("couldn't restart Neria")?;
    Ok(())
}

/// Remove what an update left behind (old exe, replaced tool files). Call at start-up.
pub fn cleanup() {
    if let Ok(exe) = current_exe() {
        let _ = std::fs::remove_file(exe.with_file_name(".neria-update.exe"));
        let _ = std::fs::remove_file(exe.with_file_name(".neria-update.exe.part"));
        let old = old_exe(&exe);
        if old.exists() {
            // Right after "Restart now" the previous Neria may still be closing
            // (its exe can't be deleted while it runs), so keep trying a while.
            std::thread::spawn(move || {
                for _ in 0..40 {
                    if std::fs::remove_file(&old).is_ok() || !old.exists() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
        }
    }
    if let Ok(entries) = std::fs::read_dir(paths::bin_dir()) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "old") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

// ───────────── Tools ─────────────

#[derive(Clone, Debug, PartialEq)]
pub struct ToolUpdate {
    pub label: &'static str,
    /// Days between our copy and the newest build.
    pub behind_days: u64,
}

/// Tools (other than yt-dlp) with a newer build than ours, ours being over a month old.
pub fn check_tools() -> Result<Vec<ToolUpdate>> {
    check_tools_older_than(TOOL_MAX_AGE_DAYS)
}

/// [`check_tools`] with another age limit (0 = any newer build counts).
pub fn check_tools_older_than(max_age_days: u64) -> Result<Vec<ToolUpdate>> {
    let mut out = Vec::new();
    for spec in tools::TOOLS {
        let Some((repo, tag)) = spec.release else { continue };
        let Some(local) = file_secs(&paths::bin_dir().join(spec.files[0])) else { continue };
        let url = match tag {
            Some(t) => format!("https://api.github.com/repos/{repo}/releases/tags/{t}"),
            None => format!("https://api.github.com/repos/{repo}/releases/latest"),
        };
        let release = api_get(&url).with_context(|| format!("checking {}", spec.label))?;
        let Some(remote) = remote_secs(spec, &release) else { continue };
        let age = report_now().saturating_sub(local) / 86_400;
        if remote > local + 86_400 && age >= max_age_days {
            out.push(ToolUpdate { label: spec.label, behind_days: (remote - local) / 86_400 });
        }
    }
    Ok(out)
}

/// Re-download the given tools (by label). Existing files are moved aside
/// first (renaming works even while a tool runs), and put back on failure.
pub fn update_tools(labels: &[&str], mut on: impl FnMut(tools::Progress)) -> Result<()> {
    let bin = paths::bin_dir();
    let todo: Vec<_> = tools::TOOLS.iter().filter(|t| labels.contains(&t.label)).collect();
    let steps = todo.len();
    for (step, spec) in todo.into_iter().enumerate() {
        let mut moved = Vec::new();
        for f in spec.files {
            let (cur, old) = (bin.join(f), bin.join(format!("{f}.old")));
            let _ = std::fs::remove_file(&old);
            if cur.is_file() && std::fs::rename(&cur, &old).is_ok() {
                moved.push((cur, old));
            }
        }
        if let Err(e) = tools::install(spec, step, steps, &mut on) {
            for (cur, old) in moved {
                let _ = std::fs::remove_file(&cur);
                let _ = std::fs::rename(&old, &cur);
            }
            return Err(e);
        }
    }
    Ok(())
}

/// Release notes as plain text for the About section: GitHub's generated
/// "What's Changed" list without author/PR links, and without the install
/// blurb every release repeats.
pub fn plain_notes(md: &str) -> String {
    let mut lines = Vec::new();
    for line in md.lines() {
        let l = line.trim();
        if l.is_empty()
            || l.starts_with("**Full Changelog**")
            || l.starts_with("Unzip anywhere")
            || l.starts_with("<!--")
        {
            continue;
        }
        let l = l.trim_start_matches('#').trim();
        let l = match l.strip_prefix("* ").or_else(|| l.strip_prefix("- ")) {
            Some(item) => {
                // "Fix thing by @someone in https://github.com/…/pull/12"
                let item = item.split(" by @").next().unwrap_or(item);
                format!("• {item}")
            }
            None => l.to_string(),
        };
        lines.push(l);
    }
    lines.join("\n")
}

fn report_now() -> u64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn file_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// When the build we'd download was published: the matching asset's date.
fn remote_secs(spec: &tools::ToolSpec, release: &Value) -> Option<u64> {
    let assets = release["assets"].as_array()?;
    let asset = assets.iter().find(|a| {
        let name = a["name"].as_str().unwrap_or("");
        match &spec.url {
            tools::Source::Url(u) => u.rsplit('/').next() == Some(name),
            tools::Source::GithubAsset { prefix, suffix, exclude, .. } => {
                name.starts_with(prefix) && name.ends_with(suffix) && !name.contains(exclude)
            }
        }
    })?;
    iso_secs(asset["updated_at"].as_str()?)
}

/// `2026-11-06T12:34:56Z` → Unix seconds.
fn iso_secs(s: &str) -> Option<u64> {
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // Howard Hinnant's days-from-civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.3"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.2.0-beta", "0.2.0"));
    }

    #[test]
    fn parses_dates() {
        assert_eq!(iso_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_secs("2025-11-10T00:00:00Z"), Some(1_762_732_800));
        assert_eq!(iso_secs("2000-02-29T01:02:03Z"), Some(951_782_400 + 3723));
        assert_eq!(iso_secs("garbage"), None);
    }

    #[test]
    fn cleans_notes() {
        let md = "Unzip anywhere and run Neria.exe. The first start…\n\n## What's Changed\n* Clear answers for dead links by @PizzaSpark in https://github.com/PizzaSpark/neria/pull/3\n* Report a problem\n\n**Full Changelog**: https://github.com/x/compare/v0.1.0...v0.2.0";
        assert_eq!(plain_notes(md), "What's Changed\n• Clear answers for dead links\n• Report a problem");
    }

    #[test]
    fn parses_release() {
        let v = serde_json::json!({
            "tag_name": "v0.2.0",
            "body": "What's new\n- stuff",
            "assets": [
                { "name": "notes.txt", "browser_download_url": "x" },
                { "name": "Neria-v0.2.0-windows-x64.zip", "browser_download_url": "https://example.com/n.zip", "size": 1234 }
            ]
        });
        let r = parse_release(&v).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.zip_url, "https://example.com/n.zip");
        assert_eq!(r.size, Some(1234));
        assert!(parse_release(&serde_json::json!({ "tag_name": "v1", "assets": [] })).is_err());
    }
}
