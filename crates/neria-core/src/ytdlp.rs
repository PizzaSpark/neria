//! yt-dlp's onedir build, fetched and kept current by Neria itself.
//!
//! The onedir build (`yt-dlp_win.zip`: `yt-dlp.exe` + `_internal/`) starts about a second
//! faster than the one-file `yt-dlp.exe`, which unpacks Python into a temp folder on every
//! run. It can't update itself (`-U` refuses), so Neria does:
//!
//! ```text
//! bin/yt-dlp/2026.10.26/yt-dlp.exe   each release in a folder of its own; the newest runs
//! bin/yt-dlp/.new-<version>/         being unpacked and checked, renamed into place when it works
//! bin/yt-dlp/.new-<version>.zip      the download, checked against the release's SHA2-256SUMS
//! bin/yt-dlp/.old-<version>/         an older release on its way out
//! bin/yt-dlp/.lock                  held while one Neria updates, so two never do at once
//! bin/yt-dlp.exe                     the one-file build: older installs until they move over,
//!                                    and the fallback when the onedir build can't be had
//! ```
//!
//! A new release never touches the folder a running download uses: it lands next to it,
//! and older folders are removed only once Windows lets them be renamed (it refuses while
//! anything inside runs), so a download in progress carries on with the version it began.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::{paths, proc, tools};

const RELEASES: &str = "https://github.com/yt-dlp/yt-dlp/releases";
/// The Windows x64 onedir build's asset name.
pub const ASSET: &str = "yt-dlp_win.zip";
/// The one-file build, kept as the fallback.
pub const ONEFILE: &str = "yt-dlp.exe";

/// Where the onedir releases live.
pub fn dir() -> PathBuf {
    paths::bin_dir().join("yt-dlp")
}

/// The one-file build's place (`bin/yt-dlp.exe`).
pub fn onefile() -> PathBuf {
    paths::bin_dir().join(ONEFILE)
}

/// "2026.10.26" or "2026.10.26.1" → its numbers; `None` for anything else.
fn version_key(name: &str) -> Option<Vec<u64>> {
    let parts: Option<Vec<u64>> = name.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| p.len() >= 3)
}

/// `a` is a newer yt-dlp release than `b`.
pub fn is_newer(a: &str, b: &str) -> bool {
    match (version_key(a), version_key(b)) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Release folders in `bin/yt-dlp/` that have their exe, newest first.
fn versions() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(Vec<u64>, String, PathBuf)> = fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let key = version_key(&name)?;
            let exe = e.path().join(ONEFILE);
            exe.is_file().then_some((key, name, e.path()))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, n, p)| (n, p)).collect()
}

/// The newest onedir release Neria has: (version, its `yt-dlp.exe`).
pub fn current() -> Option<(String, PathBuf)> {
    versions().into_iter().next().map(|(v, p)| (v, p.join(ONEFILE)))
}

/// The yt-dlp Neria runs: the newest onedir release, else the one-file build.
pub fn exe() -> PathBuf {
    current().map(|(_, exe)| exe).unwrap_or_else(onefile)
}

/// Does this yt-dlp start? Its `--version` output if so.
pub fn works(exe: &Path) -> Option<String> {
    let out = proc::command(exe).arg("--version").stdout(Stdio::piped()).stderr(Stdio::null()).output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

fn http() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("Neria/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?)
}

/// The newest stable release, from its SHA2-256SUMS (1.6 KB): the version is in the
/// address GitHub redirects "latest" to, so no API call (and no API rate limit) is needed.
#[derive(Clone, Debug)]
pub struct Latest {
    pub version: String,
    /// The release's SHA2-256SUMS text.
    pub sums: String,
}

impl Latest {
    /// The zip's expected SHA-256 (lowercase hex).
    pub fn sum(&self) -> Option<String> {
        sum_for(&self.sums, ASSET)
    }

    pub fn zip_url(&self) -> String {
        format!("{RELEASES}/download/{}/{ASSET}", self.version)
    }

    /// Days since this release came out (its version is its date).
    pub fn days_old(&self) -> Option<u64> {
        let k = version_key(&self.version)?;
        let date = format!("{:04}-{:02}-{:02}T00:00:00Z", k[0], k[1], k[2]);
        let secs = crate::update::iso_secs(&date)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(now.saturating_sub(secs) / 86_400)
    }
}

/// The newest stable release and its checksums. GitHub redirects `releases/latest` to
/// `releases/tag/<version>`, so no API call (and no API rate limit) is needed.
/// `NERIA_YTDLP_TAG` (dev hook): pretend this release is the newest, to try updating.
pub fn latest() -> Result<Latest> {
    let version = match std::env::var("NERIA_YTDLP_TAG") {
        Ok(tag) => tag,
        Err(_) => {
            let client = reqwest::blocking::Client::builder()
                .user_agent(concat!("Neria/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?;
            let resp = client.get(format!("{RELEASES}/latest")).send()?;
            let to = resp.headers().get(reqwest::header::LOCATION).and_then(|l| l.to_str().ok()).unwrap_or("");
            // https://github.com/yt-dlp/yt-dlp/releases/tag/2026.10.26
            to.rsplit_once("/tag/").map(|(_, v)| v.to_string()).unwrap_or_default()
        }
    };
    if version_key(&version).is_none() {
        bail!("couldn't tell which yt-dlp is the newest");
    }
    let sums = http()?.get(format!("{RELEASES}/download/{version}/SHA2-256SUMS")).send()?.error_for_status()?.text()?;
    Ok(Latest { version, sums })
}

/// One file's hash in a `SHA2-256SUMS` list ("<hex>  <name>").
fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let (hash, file) = (it.next()?, it.next()?);
        (file.trim_start_matches('*') == name && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256(path: &Path) -> Result<String> {
    let mut f = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// One update at a time, in this process (the daily check and a broken download's retry
/// may meet)…
static BUSY: Mutex<()> = Mutex::new(());

/// …and across processes (several Neria windows, an example): `bin/yt-dlp/.lock`, open
/// with no sharing while held, so a second opener is refused until the first lets go.
struct Lock(#[allow(dead_code)] File);

impl Lock {
    fn take(wait: Duration) -> Option<Lock> {
        let path = dir().join(".lock");
        let _ = fs::create_dir_all(dir());
        let start = std::time::Instant::now();
        loop {
            let mut opts = fs::OpenOptions::new();
            opts.read(true).write(true).create(true).truncate(false);
            #[cfg(windows)]
            std::os::windows::fs::OpenOptionsExt::share_mode(&mut opts, 0);
            if let Ok(f) = opts.open(&path) {
                return Some(Lock(f));
            }
            if start.elapsed() >= wait {
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// How long an update waits for another process's to finish (it then finds the newest
/// release in place and has nothing to do).
const LOCK_WAIT: Duration = Duration::from_secs(300);

/// Fetch `latest`'s onedir build, check it and put it in place. Returns its exe.
/// `on(downloaded, total)` reports the download.
pub fn install(latest: &Latest, on: impl FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    let root = dir();
    fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;
    let v = &latest.version;
    let (zip, new, dest) = (root.join(format!(".new-{v}.zip")), root.join(format!(".new-{v}")), root.join(v));
    let _ = fs::remove_file(&zip);
    let _ = fs::remove_dir_all(&new);
    let res = (|| {
        let want = latest.sum().ok_or_else(|| anyhow!("yt-dlp {v} lists no checksum for {ASSET}"))?;
        tools::download_to(&latest.zip_url(), &zip, on).with_context(|| format!("downloading yt-dlp {v}"))?;
        let got = sha256(&zip)?;
        if got != want {
            bail!("yt-dlp {v} didn't match its checksum (the download was damaged)");
        }
        zip::ZipArchive::new(File::open(&zip)?)?.extract(&new).with_context(|| format!("unpacking yt-dlp {v}"))?;
        let _ = fs::remove_file(&zip);
        let exe = new.join(ONEFILE);
        let said = works(&exe).ok_or_else(|| anyhow!("yt-dlp {v} didn't start (an antivirus may have held it)"))?;
        if version_key(&said).is_none() {
            bail!("yt-dlp {v} answered \"{said}\" when asked its version");
        }
        // A folder of the same version that's there already didn't work (else we'd not be
        // here): move it aside first.
        if dest.exists() {
            let aside = root.join(format!(".old-{v}-{}", std::process::id()));
            fs::rename(&dest, &aside).with_context(|| format!("yt-dlp {v} is in use"))?;
            let _ = fs::remove_dir_all(&aside);
        }
        rename_soon(&new, &dest).with_context(|| format!("putting yt-dlp {v} in place"))?;
        Ok(dest.join(ONEFILE))
    })();
    if res.is_err() {
        let _ = fs::remove_file(&zip);
        let _ = fs::remove_dir_all(&new);
    }
    res
}

/// Rename a folder Neria just wrote. An antivirus scanning the new files holds them open
/// for a moment, and Windows won't rename a folder with open files, so try for a few seconds.
fn rename_soon(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Err(e) if tries < 20 && e.kind() != std::io::ErrorKind::NotFound => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            r => return r,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The newest release is already here and starts.
    UpToDate(String),
    /// This release was installed (the first onedir one, or a newer one).
    Updated(String),
}

/// Get the newest onedir release if ours is older, missing or doesn't start, then tidy
/// up: older releases nothing runs, and the one-file build once the onedir one works.
pub fn update(on: impl FnMut(u64, Option<u64>)) -> Result<Outcome> {
    let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    let latest = latest().context("couldn't check for a newer yt-dlp")?;
    let _lock = Lock::take(LOCK_WAIT).ok_or_else(|| anyhow!("another Neria is updating yt-dlp"))?;
    let have = current().filter(|(_, exe)| works(exe).is_some());
    let outcome = match have {
        Some((v, _)) if !is_newer(&latest.version, &v) => Outcome::UpToDate(v),
        _ => {
            install(&latest, on)?;
            Outcome::Updated(latest.version.clone())
        }
    };
    tidy_locked();
    retire_onefile();
    Ok(outcome)
}

/// First run: the onedir build, or the one-file build when the onedir one can't be had
/// (so Neria still has a yt-dlp; the next daily check tries the onedir build again).
pub fn first_install(mut on: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    let _lock = Lock::take(LOCK_WAIT);
    if current().is_some_and(|(_, exe)| works(&exe).is_some()) {
        return Ok(()); // another Neria got it meanwhile
    }
    let onedir = latest().and_then(|l| install(&l, &mut on));
    if onedir.is_ok() {
        return Ok(());
    }
    let tmp = paths::bin_dir().join(format!("{ONEFILE}.download"));
    let url = format!("{RELEASES}/latest/download/{ONEFILE}");
    match tools::download_to(&url, &tmp, on).and_then(|()| Ok(fs::rename(&tmp, onefile())?)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            // The onedir build's problem is the one worth telling.
            Err(onedir.err().unwrap_or(e))
        }
    }
}

/// Remove older releases that nothing runs, and leftovers of an interrupted update.
/// Windows won't rename a folder while a program inside it runs, so a folder is
/// renamed first and only deleted once that worked. Skipped while another Neria updates.
pub fn tidy() {
    let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(_lock) = Lock::take(Duration::ZERO) {
        tidy_locked();
    }
}

fn tidy_locked() {
    let keep = current().map(|(v, _)| v);
    for e in fs::read_dir(dir()).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let path = e.path();
        if name.starts_with(".new-") || name.starts_with(".old-") {
            if path.is_dir() {
                let _ = fs::remove_dir_all(&path);
            } else {
                let _ = fs::remove_file(&path);
            }
        } else if version_key(&name).is_some() && keep.as_deref() != Some(name.as_str()) {
            let aside = dir().join(format!(".old-{name}"));
            if fs::rename(&path, &aside).is_ok() {
                let _ = fs::remove_dir_all(&aside);
            }
        }
    }
}

/// Once a onedir release works, the one-file build goes (renamed aside first: that works
/// even while it runs; `update::cleanup` deletes the `.old` at the next start).
fn retire_onefile() {
    let one = onefile();
    if !one.is_file() || current().is_none_or(|(_, exe)| works(&exe).is_none()) {
        return;
    }
    let old = paths::bin_dir().join(format!("{ONEFILE}.old"));
    let _ = fs::remove_file(&old);
    if fs::rename(&one, &old).is_ok() {
        let _ = fs::remove_file(&old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("2026.11.02", "2026.10.26"));
        assert!(is_newer("2026.10.26.1", "2026.10.26"));
        assert!(is_newer("2027.01.01", "2026.12.31"));
        assert!(!is_newer("2026.10.26", "2026.10.26"));
        assert!(!is_newer("2026.09.14", "2026.10.26"));
        assert!(!is_newer("latest", "2026.10.26"));
        assert!(version_key(".new-2026.10.26").is_none());
    }

    #[test]
    fn finds_sums() {
        let sums = "e51e…  yt-dlp\nfd51b212cf8380c7a52ee08c0d6be7f3c59b2f2c832eaf7b45acfae6eab57419  yt-dlp_win.zip\n\
                    5eaa4c61f3c09189f3a551bf1855c13aad61fb865cc33f06012a6c2dbe287a45  yt-dlp_win_arm64.zip\n";
        assert_eq!(
            sum_for(sums, ASSET).as_deref(),
            Some("fd51b212cf8380c7a52ee08c0d6be7f3c59b2f2c832eaf7b45acfae6eab57419")
        );
        assert_eq!(sum_for(sums, "yt-dlp"), None, "too short to be a hash");
        assert_eq!(sum_for("", ASSET), None);
    }

    #[test]
    fn hashes_files() {
        let p = std::env::temp_dir().join(format!("neria-sha-{}", std::process::id()));
        fs::write(&p, b"abc").unwrap();
        assert_eq!(sha256(&p).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn dates_releases() {
        let l = Latest { version: "2000.01.01".into(), sums: String::new() };
        assert!(l.days_old().unwrap() > 9000);
        assert_eq!(Latest { version: "x".into(), sums: String::new() }.days_old(), None);
    }
}
