//! Auto-fetching of yt-dlp, ffmpeg and Deno into the portable `bin/` folder.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::{paths, proc};

pub enum Source {
    Url(&'static str),
    /// Newest asset of a GitHub release whose name starts with `prefix`,
    /// ends with `suffix`, and doesn't contain `exclude`.
    GithubAsset {
        repo: &'static str,
        prefix: &'static str,
        suffix: &'static str,
        exclude: &'static str,
    },
}

pub struct ToolSpec {
    pub label: &'static str,
    /// What it's for, in a few plain words (first-run screen).
    pub purpose: &'static str,
    pub url: Source,
    /// For archives: (entry path suffix inside the archive, output file name).
    /// `None` means the URL is the executable itself.
    pub archive: Option<&'static [(&'static str, &'static str)]>,
    /// Files that must exist in `bin/` for this tool to count as installed.
    pub files: &'static [&'static str],
    /// GitHub repo (and tag, else the latest release) to check for newer builds.
    /// `None` for yt-dlp, which updates itself.
    pub release: Option<(&'static str, Option<&'static str>)>,
}

#[cfg(windows)]
pub static TOOLS: &[ToolSpec] = &[
    ToolSpec {
        label: "yt-dlp",
        purpose: "Downloads from YouTube and 1,000+ other sites",
        url: Source::Url("https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"),
        archive: None,
        files: &["yt-dlp.exe"],
        release: None,
    },
    ToolSpec {
        label: "FFmpeg",
        purpose: "Joins, converts, clips and compresses video",
        url: Source::Url(
            "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip",
        ),
        archive: Some(&[("bin/ffmpeg.exe", "ffmpeg.exe"), ("bin/ffprobe.exe", "ffprobe.exe")]),
        files: &["ffmpeg.exe", "ffprobe.exe"],
        release: Some(("yt-dlp/FFmpeg-Builds", Some("latest"))),
    },
    ToolSpec {
        label: "Deno (needed by yt-dlp for YouTube)",
        purpose: "Helps yt-dlp read YouTube",
        url: Source::Url("https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip"),
        archive: Some(&[("deno.exe", "deno.exe")]),
        files: &["deno.exe"],
        release: Some(("denoland/deno", None)),
    },
    ToolSpec {
        label: "libmpv (video player)",
        purpose: "Plays video on the Clip page",
        url: Source::GithubAsset {
            repo: "shinchiro/mpv-winbuild-cmake",
            prefix: "mpv-dev-x86_64-",
            suffix: ".7z",
            exclude: "-v3-",
        },
        archive: Some(&[("libmpv-2.dll", "libmpv-2.dll")]),
        files: &["libmpv-2.dll"],
        release: Some(("shinchiro/mpv-winbuild-cmake", None)),
    },
];

fn http() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("Neria/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .timeout(None::<Duration>)
        .build()?)
}

fn resolve(source: &Source) -> Result<String> {
    match source {
        Source::Url(u) => Ok(u.to_string()),
        Source::GithubAsset { repo, prefix, suffix, exclude } => {
            let api = format!("https://api.github.com/repos/{repo}/releases/latest");
            let body =
                http()?.get(api).header("Accept", "application/vnd.github+json").send()?.error_for_status()?.bytes()?;
            let release: serde_json::Value = serde_json::from_slice(&body)?;
            release["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|a| {
                    let n = a["name"].as_str().unwrap_or("");
                    n.starts_with(prefix) && n.ends_with(suffix) && !n.contains(exclude)
                })
                .and_then(|a| a["browser_download_url"].as_str())
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("no matching asset in {repo}'s latest release"))
        }
    }
}

#[cfg(not(windows))]
pub static TOOLS: &[ToolSpec] = &[];

#[derive(Clone, Debug)]
pub struct Progress {
    /// Index into `TOOLS`.
    pub tool: usize,
    pub step: usize,
    pub steps: usize,
    pub label: String,
    pub downloaded: u64,
    pub total: Option<u64>,
}

impl ToolSpec {
    /// Short name without the explanation in brackets ("Deno").
    pub fn name(&self) -> &'static str {
        self.label.split(" (").next().unwrap_or(self.label)
    }

    pub fn installed(&self) -> bool {
        let bin = paths::bin_dir();
        self.files.iter().all(|f| bin.join(f).is_file())
    }
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        let inner = match self.total {
            Some(t) if t > 0 => self.downloaded as f32 / t as f32,
            _ => 0.0,
        };
        ((self.step as f32 + inner.min(1.0)) / self.steps.max(1) as f32).min(1.0)
    }

    pub fn text(&self) -> String {
        let amount = match self.total {
            Some(t) => format!("{} / {}", crate::fmt::bytes(self.downloaded as f64), crate::fmt::bytes(t as f64)),
            None => crate::fmt::bytes(self.downloaded as f64),
        };
        format!("Downloading {} ({}/{})  ·  {}", self.label, self.step + 1, self.steps, amount)
    }
}

pub fn missing() -> Vec<&'static ToolSpec> {
    TOOLS.iter().filter(|t| !t.installed()).collect()
}

/// Download every missing tool. Blocking; call from a worker thread.
pub fn ensure_all(mut on: impl FnMut(Progress)) -> Result<()> {
    let todo = missing();
    let steps = todo.len();
    for (step, tool) in todo.into_iter().enumerate() {
        install(tool, step, steps, &mut on)?;
    }
    Ok(())
}

/// Download one tool into `bin/` (replacing what's there). `step`/`steps` are for progress.
pub fn install(tool: &ToolSpec, step: usize, steps: usize, on: &mut impl FnMut(Progress)) -> Result<()> {
    let bin = paths::bin_dir();
    fs::create_dir_all(&bin).with_context(|| format!("cannot create {}", bin.display()))?;
    let tmp = bin.join(format!("{}.download", tool.files[0]));
    let label = tool.label.to_string();
    let idx = TOOLS.iter().position(|t| std::ptr::eq(t, tool)).unwrap_or(0);
    let p = |label: String, downloaded, total| Progress { tool: idx, step, steps, label, downloaded, total };
    on(p(label.clone(), 0, None));
    let url = resolve(&tool.url).with_context(|| format!("finding {}", tool.label))?;
    download_to(&url, &tmp, |downloaded, total| on(p(label.clone(), downloaded, total)))
        .with_context(|| format!("downloading {}", tool.label))?;

    match tool.archive {
        None => fs::rename(&tmp, bin.join(tool.files[0]))?,
        Some(entries) => {
            on(p(format!("{} (extracting)", tool.label), 1, Some(1)));
            let res =
                if url.ends_with(".7z") { extract_7z(&tmp, &bin, entries) } else { extract_zip(&tmp, &bin, entries) };
            let _ = fs::remove_file(&tmp);
            res.with_context(|| format!("extracting {}", tool.label))?;
        }
    }
    Ok(())
}

/// Turn a failed tool download into one or two plain sentences for the setup screen.
/// `tool` is the short name of the tool that failed.
pub fn explain(err: &anyhow::Error, tool: &str) -> String {
    for cause in err.chain() {
        let net = cause.downcast_ref::<reqwest::Error>().or_else(|| {
            cause
                .downcast_ref::<std::io::Error>()
                .and_then(|e| e.get_ref())
                .and_then(|e| e.downcast_ref::<reqwest::Error>())
        });
        if let Some(e) = net {
            if let Some(status) = e.status() {
                return match status.as_u16() {
                    403 | 429 => "GitHub is limiting downloads from your connection right now. Wait a few minutes, then try again.".into(),
                    s if s >= 500 => format!("GitHub had a hiccup while sending {tool} (error {s}). Try again in a minute."),
                    s => format!("GitHub didn't have {tool} where Neria expected it (error {s}). Try again later; if it keeps happening, copy the details and report it."),
                };
            }
            if e.is_timeout() {
                return format!("The download of {tool} stalled. Your connection may be slow or dropping; try again.");
            }
            if e.is_connect() || e.is_request() && !e.is_body() {
                return "Neria couldn't reach the internet. Check that you're online, then try again. If you are, a firewall or network filter may be blocking github.com.".into();
            }
            return format!("The download of {tool} was cut off. Check your connection, then try again.");
        }
        if let Some(e) = cause.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind::*;
            match e.kind() {
                StorageFull | QuotaExceeded => {
                    return format!(
                        "Your drive is full, so Neria couldn't save {tool}. Free up about 500 MB, then try again."
                    );
                }
                PermissionDenied => {
                    return format!(
                        "Windows stopped Neria from saving {tool} in its bin folder. If your antivirus quarantined it, allow Neria there; otherwise move Neria to Documents or Desktop."
                    );
                }
                TimedOut | ConnectionReset | ConnectionAborted | UnexpectedEof => {
                    return format!("The download of {tool} was cut off. Check your connection, then try again.");
                }
                _ if e.raw_os_error() == Some(112) => {
                    return format!(
                        "Your drive is full, so Neria couldn't save {tool}. Free up about 500 MB, then try again."
                    );
                }
                _ => {}
            }
        }
    }
    format!(
        "Something went wrong while getting {tool}. Try again; if it keeps happening, copy the details and report it."
    )
}

pub(crate) fn download_to(url: &str, dest: &Path, mut on: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let mut resp = http()?.get(url).send()?.error_for_status()?;
    let total = resp.content_length();

    let mut out = BufWriter::new(File::create(dest)?);
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let mut last = Instant::now();
    on(0, total);
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            on(done, total);
            last = Instant::now();
        }
    }
    out.flush()?;
    on(done, total);
    Ok(())
}

pub(crate) fn extract_zip(archive: &Path, dest: &Path, entries: &[(&str, &str)]) -> Result<()> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
    let mut found = vec![false; entries.len()];

    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        let name = file.name()?.replace('\\', "/");
        for (idx, (suffix, out_name)) in entries.iter().enumerate() {
            if found[idx] || !(name == *suffix || name.ends_with(&format!("/{suffix}"))) {
                continue;
            }
            let tmp = dest.join(format!("{out_name}.part"));
            {
                let mut out = BufWriter::new(File::create(&tmp)?);
                std::io::copy(&mut file, &mut out)?;
                out.flush()?;
            }
            fs::rename(&tmp, dest.join(out_name))?;
            found[idx] = true;
            break;
        }
    }

    if let Some(idx) = found.iter().position(|f| !f) {
        bail!("{} not found in archive", entries[idx].0);
    }
    Ok(())
}

fn extract_7z(archive: &Path, dest: &Path, entries: &[(&str, &str)]) -> Result<()> {
    // Unpack into a scratch folder, keep only what we need.
    let scratch = dest.join(".unpack");
    let _ = fs::remove_dir_all(&scratch);
    sevenz_rust2::decompress_file(archive, &scratch).map_err(|e| anyhow::anyhow!("{e}"))?;

    let result = (|| {
        for (suffix, out_name) in entries {
            let found = find_file(&scratch, suffix).with_context(|| format!("{suffix} not found in archive"))?;
            fs::rename(found, dest.join(out_name))?;
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&scratch);
    result
}

fn find_file(dir: &Path, suffix: &str) -> Option<std::path::PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, suffix) {
                return Some(found);
            }
        } else if path.to_string_lossy().replace('\\', "/").ends_with(suffix) {
            return Some(path);
        }
    }
    None
}

pub fn ytdlp_version() -> Option<String> {
    let out = proc::command(paths::tool("yt-dlp"))
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Run `yt-dlp -U`. Must not run while downloads are active (it replaces itself).
pub fn update_ytdlp() -> Result<()> {
    let status = proc::command(paths::tool("yt-dlp")).arg("-U").stdout(Stdio::null()).stderr(Stdio::null()).status()?;
    if !status.success() {
        bail!("yt-dlp update failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn explains_disk_and_permission_problems() {
        let full = anyhow::Error::new(Error::from_raw_os_error(112)).context("downloading FFmpeg");
        assert!(explain(&full, "FFmpeg").starts_with("Your drive is full"));
        let denied = anyhow::Error::new(Error::from(ErrorKind::PermissionDenied)).context("extracting Deno");
        assert!(explain(&denied, "Deno").contains("antivirus"));
        let odd = anyhow::anyhow!("libmpv-2.dll not found in archive");
        assert!(explain(&odd, "libmpv").starts_with("Something went wrong while getting libmpv"));
    }

    #[test]
    fn explains_offline() {
        // Nothing listens on port 9 locally, so this fails like being offline.
        let err = http().unwrap().get("http://127.0.0.1:9/").send().unwrap_err();
        assert!(explain(&anyhow::Error::new(err), "yt-dlp").starts_with("Neria couldn't reach the internet"));
    }
}
