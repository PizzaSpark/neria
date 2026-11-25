//! Speech to text, optional: whisper.cpp's `whisper-cli` and a Whisper model, fetched only
//! when someone asks for it (Clip page → Transcribe, a dropped file, a download's Transcript
//! extra) into `bin/whisper/`. Nothing about it is part of first-run setup.
//!
//! ```text
//! bin/whisper/engine/   whisper-cli.exe + its DLLs (the CPU pack, or the NVIDIA pack)
//! bin/whisper/ggml-large-v3-turbo-q5_0.bin   Fast: the model (574 MB)
//! bin/whisper/ggml-large-v3-q5_0.bin         Best quality, only once picked (1.08 GB):
//!                                            better on mixed languages, accents, poor sound,
//!                                            and the one that translates
//! bin/whisper/ggml-silero-v5.1.2.bin         voice detection: skips silence (no made-up
//!                                            lines over music or quiet parts)
//! ```
//!
//! The sound is turned into 16 kHz mono WAV by our ffmpeg first (whisper-cli reads WAV
//! only), several files joined in order into one, then whisper-cli writes JSON that we turn
//! into a `.txt` (with or without times) and `.srt` captions.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{paths, proc, tools};

const REPO: &str = "ggml-org/whisper.cpp";
/// The last whisper.cpp build we checked, used when GitHub's list can't be read.
const KNOWN_BUILD: &str = "b5454";
const CPU_ASSET: &str = "whisper-blas-bin-x64.zip";
const GPU_ASSET: &str = "whisper-bin-win-cuda-12.4.0-x64.zip";

pub const MODEL_FILE: &str = "ggml-large-v3-turbo-q5_0.bin";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin";
const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";
const VAD_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";

/// Download sizes for the "Get speech to text" button (MB, rounded up).
pub const CPU_MB: u64 = 22;
pub const GPU_MB: u64 = 685;
pub const MODEL_MB: u64 = 575;

/// Which Whisper model writes it down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Model {
    /// large-v3-turbo: quick, good on clear speech. Can't translate.
    #[default]
    Fast,
    /// large-v3 (q5_0, as close to the full one as makes no difference): about 3 times slower,
    /// better on mixed languages, accents and poor sound, and it translates into English.
    Best,
}

impl Model {
    pub fn file(self) -> &'static str {
        match self {
            Model::Fast => MODEL_FILE,
            Model::Best => "ggml-large-v3-q5_0.bin",
        }
    }

    fn url(self) -> String {
        format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", self.file())
    }

    /// Its download, MB rounded up.
    pub fn mb(self) -> u64 {
        match self {
            Model::Fast => MODEL_MB,
            Model::Best => 1082,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Model::Fast => "Fast",
            Model::Best => "Best quality",
        }
    }

    pub fn from_index(i: i32) -> Model {
        if i == 1 { Model::Best } else { Model::Fast }
    }

    pub fn index(self) -> i32 {
        self as i32
    }
}

/// This model is on the drive (a cut-off download is smaller).
pub fn has(m: Model) -> bool {
    dir().join(m.file()).metadata().is_ok_and(|x| x.len() > 100_000_000)
}

/// A hint that gets Whisper to write sentences (capitals, full stops), which it often
/// drops on long talks without one. Only for English or "detect": it's English itself.
const PUNCTUATION_PROMPT: &str = "Hello, welcome. Okay, so let's begin.";

pub fn dir() -> PathBuf {
    paths::bin_dir().join("whisper")
}

fn engine_dir() -> PathBuf {
    dir().join("engine")
}

fn cli() -> PathBuf {
    engine_dir().join(format!("whisper-cli{}", std::env::consts::EXE_SUFFIX))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub engine: bool,
    /// The NVIDIA pack is the one installed.
    pub gpu: bool,
    /// Fast is there (and voice detection).
    pub model: bool,
    /// Best quality is there.
    pub best: bool,
}

impl Status {
    pub fn ready(self) -> bool {
        self.engine && (self.model || self.best)
    }
}

pub fn status() -> Status {
    let vad = dir().join(VAD_FILE).is_file();
    Status {
        engine: cli().is_file(),
        gpu: engine_dir().join("ggml-cuda.dll").is_file(),
        model: has(Model::Fast) && vad,
        best: has(Model::Best) && vad,
    }
}

pub fn ready() -> bool {
    status().ready()
}

/// An NVIDIA card with its driver: the faster pack can use it.
pub fn has_nvidia() -> bool {
    if std::env::var_os("NERIA_FAKE_NVIDIA").is_some() {
        return true;
    }
    #[cfg(windows)]
    {
        let sys = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        sys.join("System32").join("nvcuda.dll").is_file()
    }
    #[cfg(not(windows))]
    false
}

/// What it all takes on the drive now.
pub fn disk_bytes() -> u64 {
    fn walk(p: &Path) -> u64 {
        fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let p = e.path();
                if p.is_dir() { walk(&p) } else { e.metadata().map(|m| m.len()).unwrap_or(0) }
            })
            .sum()
    }
    walk(&dir())
}

/// Download what's missing: the engine (the NVIDIA pack when `gpu`, replacing the CPU one,
/// or back), the model and voice detection. Blocking; call from a worker thread.
pub fn install(gpu: bool, cancel: &AtomicBool, mut on: impl FnMut(tools::Progress)) -> Result<()> {
    let st = status();
    let base = dir();
    fs::create_dir_all(&base).with_context(|| format!("cannot create {}", base.display()))?;
    let engine = !st.engine || st.gpu != gpu;
    let steps = engine as usize + !st.model as usize;
    let mut step = 0;
    let p = |step, label: &str, downloaded, total| tools::Progress {
        tool: 0,
        step,
        steps: steps.max(1),
        label: label.to_string(),
        downloaded,
        total,
    };

    if engine {
        let label = if gpu { "speech to text (NVIDIA)" } else { "speech to text" };
        on(p(step, label, 0, None));
        let asset = if gpu { GPU_ASSET } else { CPU_ASSET };
        let url = newest_asset(asset)
            .unwrap_or_else(|_| format!("https://github.com/{REPO}/releases/download/{KNOWN_BUILD}/{asset}"));
        let zip = base.join("engine.zip.download");
        tools::download_to_cancellable(&url, &zip, cancel, |d, t| on(p(step, label, d, t)))
            .with_context(|| format!("downloading {label}"))?;
        let part = base.join("engine.part");
        let _ = fs::remove_dir_all(&part);
        fs::create_dir_all(&part)?;
        let res = extract_engine(&zip, &part);
        let _ = fs::remove_file(&zip);
        res.context("extracting speech to text")?;
        let _ = fs::remove_dir_all(engine_dir());
        fs::rename(&part, engine_dir())?;
        step += 1;
    }

    if !st.model {
        for (file, url, label) in [(VAD_FILE, VAD_URL, "voice detection"), (MODEL_FILE, MODEL_URL, "the Whisper model")]
        {
            let dest = base.join(file);
            if dest.is_file() && (file != MODEL_FILE || dest.metadata().is_ok_and(|m| m.len() > 100_000_000)) {
                continue;
            }
            let tmp = base.join(format!("{file}.download"));
            tools::download_to_cancellable(url, &tmp, cancel, |d, t| on(p(step, label, d, t)))
                .with_context(|| format!("downloading {label}"))?;
            fs::rename(&tmp, &dest)?;
        }
    }
    Ok(())
}

/// Download one model (and voice detection, if it's missing). Blocking; call from a worker
/// thread.
pub fn install_model(model: Model, cancel: &AtomicBool, mut on: impl FnMut(tools::Progress)) -> Result<()> {
    let base = dir();
    fs::create_dir_all(&base).with_context(|| format!("cannot create {}", base.display()))?;
    let label = format!("the {} model", model.label());
    let vad = base.join(VAD_FILE);
    let mut jobs: Vec<(&str, String, String)> = Vec::new();
    if !vad.is_file() {
        jobs.push((VAD_FILE, VAD_URL.to_string(), "voice detection".into()));
    }
    if !has(model) {
        jobs.push((model.file(), model.url(), label));
    }
    let steps = jobs.len().max(1);
    for (step, (file, url, label)) in jobs.into_iter().enumerate() {
        let tmp = base.join(format!("{file}.download"));
        tools::download_to_cancellable(&url, &tmp, cancel, |d, t| {
            on(tools::Progress { tool: 0, step, steps, label: label.clone(), downloaded: d, total: t })
        })
        .with_context(|| format!("downloading {label}"))?;
        fs::rename(&tmp, base.join(file))?;
    }
    Ok(())
}

/// Take one model off the drive (Settings → Speech to text).
pub fn remove_model(model: Model) -> Result<()> {
    let f = dir().join(model.file());
    if f.exists() {
        fs::remove_file(&f).with_context(|| format!("removing {}", f.display()))?;
    }
    Ok(())
}

/// Take speech to text off the drive (Settings → Speech to text → Remove).
pub fn remove() -> Result<()> {
    let d = dir();
    if d.exists() {
        fs::remove_dir_all(&d).with_context(|| format!("removing {}", d.display()))?;
    }
    Ok(())
}

/// The newest whisper.cpp build that carries `asset` (its "latest" release often has no
/// files; the builds are attached to `bNNNN` releases).
fn newest_asset(asset: &str) -> Result<String> {
    let api = format!("https://api.github.com/repos/{REPO}/releases?per_page=15");
    let body = reqwest::blocking::Client::builder()
        .user_agent(concat!("Neria/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20))
        .build()?
        .get(api)
        .header("Accept", "application/vnd.github+json")
        .send()?
        .error_for_status()?
        .bytes()?;
    let releases: serde_json::Value = serde_json::from_slice(&body)?;
    releases
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| !r["prerelease"].as_bool().unwrap_or(false) && !r["draft"].as_bool().unwrap_or(false))
        .flat_map(|r| r["assets"].as_array().cloned().unwrap_or_default())
        .find(|a| a["name"].as_str() == Some(asset))
        .and_then(|a| a["browser_download_url"].as_str().map(str::to_string))
        .ok_or_else(|| anyhow::anyhow!("no {asset} in {REPO}'s recent releases"))
}

/// Is this file in the zip one whisper-cli needs? (The zips also carry a dozen other
/// programs and test tools we don't keep.)
fn keep(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "whisper-cli.exe"
        || n == "whisper.dll"
        || n == "libopenblas.dll"
        || (n.starts_with("ggml") && n.ends_with(".dll"))
        || matches!(n.as_str(), "cublas64_12.dll" | "cublaslt64_12.dll" | "cudart64_12.dll")
}

fn extract_engine(archive: &Path, dest: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(fs::File::open(archive)?)?;
    let mut got_cli = false;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        let full = file.name()?.replace('\\', "/");
        let name = full.rsplit('/').next().unwrap_or("").to_string();
        if !keep(&name) {
            continue;
        }
        got_cli |= name.eq_ignore_ascii_case("whisper-cli.exe");
        let mut out = std::io::BufWriter::new(fs::File::create(dest.join(&name))?);
        std::io::copy(&mut file, &mut out)?;
    }
    if !got_cli {
        bail!("whisper-cli.exe not found in archive");
    }
    Ok(())
}

/// What to write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Output {
    /// A `.txt` to read.
    #[default]
    Text,
    /// `.srt` captions, which players pick up next to the video.
    Captions,
    Both,
}

impl Output {
    pub fn text(self) -> bool {
        self != Output::Captions
    }
    pub fn captions(self) -> bool {
        self != Output::Text
    }
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Output::Captions,
            2 => Output::Both,
            _ => Output::Text,
        }
    }
    pub fn index(self) -> i32 {
        self as i32
    }
}

/// The choices, as remembered in Settings and carried by a download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spec {
    /// Whisper's language code (`en`, `tl`…); `None` = detect it.
    pub language: Option<String>,
    pub output: Output,
    /// `[01:23]` before each line of the `.txt`.
    pub times: bool,
    /// Into English, whatever is spoken (Whisper's own translation; Best quality only: the
    /// Fast model can't).
    pub translate: bool,
    /// Which model writes it down (Best quality is downloaded the first time it's used).
    pub model: Model,
    /// Also a copy of the video with the captions burned into the picture (one video file:
    /// the part transcribed), for apps that don't show `.srt` captions.
    pub burn: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Spec { language: None, output: Output::Text, times: true, translate: false, burn: false, model: Model::Fast }
    }
}

/// Languages offered by name, as chips (Whisper knows ~100; any of them is detected, so
/// these are for when detecting guesses wrong). Code, name.
pub static LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("tl", "Filipino"),
    ("es", "Spanish"),
    ("pt", "Portuguese"),
    ("fr", "French"),
    ("de", "German"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("hi", "Hindi"),
];

/// "English" for `en`; a code we don't name stays as it is.
pub fn language_name(code: &str) -> String {
    LANGUAGES.iter().find(|(c, _)| *c == code).map(|(_, n)| n.to_string()).unwrap_or_else(|| code.to_string())
}

/// Files in the order people number them: `2.m4a` before `10.m4a` (a drag from Explorer
/// comes in the order they were clicked).
pub fn in_order(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    // Runs of digits compare as numbers, and before letters (like Explorer).
    fn key(p: &Path) -> Vec<(u8, u64, String)> {
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        let mut out = Vec::new();
        let (mut num, mut text) = (String::new(), String::new());
        for c in name.chars() {
            if c.is_ascii_digit() {
                if !text.is_empty() {
                    out.push((1, 0, std::mem::take(&mut text)));
                }
                num.push(c);
            } else {
                if !num.is_empty() {
                    out.push((0, num.parse().unwrap_or(u64::MAX), String::new()));
                    num.clear();
                }
                text.push(c);
            }
        }
        if !num.is_empty() {
            out.push((0, num.parse().unwrap_or(u64::MAX), String::new()));
        }
        if !text.is_empty() {
            out.push((1, 0, text));
        }
        out
    }
    paths.sort_by_cached_key(|p| key(p));
    paths
}

pub struct Request {
    /// One file, or several joined in this order into one transcript (parts of one talk).
    pub inputs: Vec<PathBuf>,
    /// Only this part of the (single) file, in seconds; the times written stay the file's.
    pub range: Option<(f64, f64)>,
    pub spec: Spec,
    /// Where the files go; next to the (first) input when `None`.
    pub out_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    /// The model picked isn't here yet: being downloaded ("the Best quality model · 340 /
    /// 1,082 MB").
    Getting(String),
    /// ffmpeg pulling the sound out.
    Preparing,
    /// Whisper at work; the last line it heard, when there is one.
    Listening(String),
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub fraction: f32,
    pub stage: Stage,
}

#[derive(Clone, Debug)]
pub struct Done {
    pub files: Vec<PathBuf>,
    /// Detected or picked, e.g. `en`.
    pub language: String,
    pub segments: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Transcribe. Blocking; call from a worker thread.
pub fn run(req: &Request, cancel: &AtomicBool, mut on: impl FnMut(Progress)) -> Result<Done> {
    if !ready() {
        bail!("Speech to text isn't set up yet");
    }
    let Some(first) = req.inputs.first() else { bail!("nothing to transcribe") };
    // Translating needs Best quality (Fast gives the words back untranslated).
    let model = if req.spec.translate { Model::Best } else { req.spec.model };
    if !has(model) || !dir().join(VAD_FILE).is_file() {
        install_model(model, cancel, |p| {
            on(Progress { fraction: p.fraction() * 0.99, stage: Stage::Getting(p.text()) })
        })?;
    }
    on(Progress { fraction: 0.0, stage: Stage::Preparing });

    let work = paths::cache_dir().join(format!("transcribe-{}-{}", std::process::id(), stamp_now()));
    fs::create_dir_all(&work)?;
    let result = (|| {
        let wav = work.join("sound.wav");
        let offset = req.range.map(|r| r.0).unwrap_or(0.0);
        // Where each input starts in the joined sound, for "Part 2" lines in the text.
        let starts = prepare(&req.inputs, req.range, &wav, cancel)?;
        if cancel.load(Ordering::SeqCst) {
            bail!("Cancelled");
        }
        on(Progress { fraction: 0.02, stage: Stage::Listening(String::new()) });

        let lang = req.spec.language.clone().filter(|l| !l.is_empty());
        let json_base = work.join("out");
        let st = status();
        let tr = req.spec.translate;
        // Settings → General → Video work on: Processor keeps the NVIDIA pack off the card.
        let cpu = !crate::gpu::allowed();
        let mut out = whisper(&wav, &json_base, model, lang.as_deref(), tr, cpu, cancel, &mut on);
        // The NVIDIA pack on a card or driver it can't use: the same pack runs on the CPU.
        if out.is_err() && st.gpu && !cpu && !cancel.load(Ordering::SeqCst) {
            out = whisper(&wav, &json_base, model, lang.as_deref(), tr, true, cancel, &mut on);
        }
        out?;
        let raw = fs::read(json_base.with_extension("json")).context("Whisper wrote nothing")?;
        let (detected, mut segs) = parse_json(&String::from_utf8_lossy(&raw))?;
        for s in &mut segs {
            s.start += offset;
            s.end += offset;
        }
        // Translated: what's written is English, whatever was spoken.
        let language = if tr { "en".to_string() } else { lang.unwrap_or(detected) };

        let dir = req.out_dir.clone().unwrap_or_else(|| first.parent().map(Path::to_path_buf).unwrap_or_default());
        let stem = crate::music::sanitize(&first.file_stem().unwrap_or_default().to_string_lossy());
        let mut base = stem.clone();
        if req.inputs.len() > 1 {
            base = format!("{stem} (+{} joined)", req.inputs.len() - 1);
        }
        if let Some((a, b)) = req.range {
            base = format!("{base} ({}-{})", file_stamp(a), file_stamp(b));
        }
        let mut files = Vec::new();
        if req.spec.output.text() {
            let parts: Vec<(f64, String)> = if req.inputs.len() > 1 {
                req.inputs
                    .iter()
                    .zip(&starts)
                    .map(|(p, s)| (*s, p.file_name().unwrap_or_default().to_string_lossy().into_owned()))
                    .collect()
            } else {
                Vec::new()
            };
            let names: Vec<String> =
                req.inputs.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
            let body = to_text(&names.join(" + "), &language, &segs, req.spec.times, &parts);
            let path = crate::clip::unique_path(&dir, &format!("{base} - transcript"), "txt");
            fs::write(&path, body).with_context(|| format!("saving {}", path.display()))?;
            files.push(path);
        }
        if req.spec.output.captions() {
            let path = crate::clip::unique_path(&dir, &base, "srt");
            fs::write(&path, to_srt(&segs)).with_context(|| format!("saving {}", path.display()))?;
            files.push(path);
        }
        // The captions in the picture: the part transcribed, as its own video.
        if req.spec.burn
            && req.inputs.len() == 1
            && crate::media::file_info(first).is_ok_and(|i| i.width > 0)
            && !segs.is_empty()
        {
            on(Progress { fraction: 0.99, stage: Stage::Preparing });
            let out = crate::clip::unique_path(&dir, &format!("{base} (captions)"), "mp4");
            crate::gpu::or_on_cpu(|| burn(first, req.range, &segs, &work, &out, cancel))?;
            files.push(out);
        }
        on(Progress { fraction: 1.0, stage: Stage::Listening(String::new()) });
        Ok(Done { files, language, segments: segs.len() })
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

fn stamp_now() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// `01.23` / `1.02.03` for a file name (no colons on Windows).
fn file_stamp(t: f64) -> String {
    let t = t.max(0.0) as u64;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 { format!("{h}.{m:02}.{s:02}") } else { format!("{m:02}.{s:02}") }
}

/// The sound of `inputs` (joined in order) as 16 kHz mono WAV. Returns where each input
/// starts in it, in seconds.
fn prepare(inputs: &[PathBuf], range: Option<(f64, f64)>, wav: &Path, cancel: &AtomicBool) -> Result<Vec<f64>> {
    let mut a: Vec<String> = ["-hide_banner", "-v", "error", "-nostdin", "-y"].map(String::from).to_vec();
    let mut starts = Vec::new();
    let mut at = 0.0;
    for (i, input) in inputs.iter().enumerate() {
        if i == 0
            && let Some((s, e)) = range
        {
            a.extend(["-ss".into(), format!("{s:.3}"), "-t".into(), format!("{:.3}", (e - s).max(0.1))]);
        }
        a.extend(["-i".into(), input.to_string_lossy().into_owned()]);
        starts.push(at);
        if inputs.len() > 1 {
            at += crate::media::duration(input).unwrap_or(0.0);
        }
    }
    if inputs.len() > 1 {
        let pads: String = (0..inputs.len()).map(|i| format!("[{i}:a:0]")).collect();
        a.extend([
            "-filter_complex".into(),
            format!("{pads}concat=n={}:v=0:a=1[a]", inputs.len()),
            "-map".into(),
            "[a]".into(),
        ]);
    } else {
        a.extend(["-map".into(), "0:a:0".into()]);
    }
    a.extend(["-vn", "-sn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"].map(String::from));
    a.push(wav.to_string_lossy().into_owned());

    let mut child = proc::command(paths::tool("ffmpeg"))
        .args(&a)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting ffmpeg")?;
    let mut err = String::new();
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    loop {
        if cancel.load(Ordering::SeqCst) {
            proc::kill_tree(&mut child);
            bail!("Cancelled");
        }
        if let Some(status) = child.try_wait()? {
            err.push_str(&reader.join().unwrap_or_default());
            if !status.success() {
                if err.contains("matches no streams") || err.contains("Stream specifier") {
                    bail!("This file has no sound to transcribe");
                }
                bail!("couldn't read the sound: {}", err.lines().last().unwrap_or("ffmpeg failed"));
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(starts)
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_sub(2).clamp(2, 12)
}

/// Run whisper-cli on `wav`, writing `<json_base>.json`.
/// `input` (or its `range`) with `segs` burned into the picture, into `out` (H.264, the
/// sound copied as AAC). The captions are written into `work` and read from there (ffmpeg's
/// subtitles filter can't take a Windows path with its drive colon unescaped).
fn burn(
    input: &Path,
    range: Option<(f64, f64)>,
    segs: &[Segment],
    work: &Path,
    out: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let from = range.map(|r| r.0).unwrap_or(0.0);
    let shifted: Vec<Segment> = segs
        .iter()
        .map(|s| Segment { start: (s.start - from).max(0.0), end: (s.end - from).max(0.0), text: s.text.clone() })
        .collect();
    fs::write(work.join("captions.srt"), to_srt(&shifted))?;
    let input = std::path::absolute(input).unwrap_or_else(|_| input.to_path_buf());
    let out_abs = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
    let mut cmd = proc::command(paths::tool("ffmpeg"));
    cmd.current_dir(work).args(["-hide_banner", "-v", "error", "-nostdin", "-y"]);
    if let Some((a, b)) = range {
        cmd.args(["-ss", &format!("{a:.3}"), "-to", &format!("{b:.3}")]);
    }
    cmd.arg("-i").arg(&input);
    // White words on a soft dark outline, near the bottom: readable on any picture.
    cmd.args([
        "-vf",
        "subtitles=captions.srt:force_style='FontName=Arial,FontSize=18,Bold=1,Outline=2,Shadow=0,MarginV=24'",
        "-map",
        "0:v:0",
        "-map",
        "0:a:0?",
    ]);
    // The processor or the graphics card (Settings → General → Video work on).
    cmd.args(crate::gpu::h264_args(20));
    cmd.args(["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"]);
    cmd.arg(&out_abs);
    let mut child = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().context("starting ffmpeg")?;
    proc::end_with_neria(&child);
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = fs::remove_file(&out_abs);
            bail!("Cancelled");
        }
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                use std::io::Read as _;
                let _ = e.read_to_string(&mut err);
            }
            let _ = fs::remove_file(&out_abs);
            bail!("Couldn't burn the captions in: {}", err.lines().last().unwrap_or("ffmpeg failed").trim());
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

#[allow(clippy::too_many_arguments)]
fn whisper(
    wav: &Path,
    json_base: &Path,
    model: Model,
    lang: Option<&str>,
    translate: bool,
    no_gpu: bool,
    cancel: &AtomicBool,
    on: &mut impl FnMut(Progress),
) -> Result<()> {
    // Absolute: whisper-cli runs in its own folder (a relative NERIA_HOME would point
    // elsewhere from there).
    let d = std::path::absolute(dir()).unwrap_or_else(|_| dir());
    let wav = std::path::absolute(wav).unwrap_or_else(|_| wav.to_path_buf());
    let json_base = std::path::absolute(json_base).unwrap_or_else(|_| json_base.to_path_buf());
    let (wav, json_base) = (wav.as_path(), json_base.as_path());
    let mut cmd = proc::command(std::path::absolute(cli()).unwrap_or_else(|_| cli()));
    cmd.current_dir(d.join("engine"))
        .arg("-m")
        .arg(d.join(model.file()))
        .arg("-f")
        .arg(wav)
        .args(["-l", lang.unwrap_or("auto"), "-t", &threads().to_string(), "-oj", "-pp"])
        .arg("-of")
        .arg(json_base);
    if d.join(VAD_FILE).is_file() {
        cmd.arg("--vad").arg("-vm").arg(d.join(VAD_FILE));
    }
    if matches!(lang, None | Some("en")) {
        cmd.args(["--prompt", PUNCTUATION_PROMPT]);
    }
    if no_gpu {
        cmd.arg("-ng");
    }
    if translate {
        cmd.arg("-tr");
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("starting Whisper")?;
    proc::end_with_neria(&child);

    // stdout: the lines as they're heard; stderr: "progress = 42%" and errors.
    let (tx, rx) = std::sync::mpsc::channel::<(bool, String)>();
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
                let _ = tx.send((true, String::from_utf8_lossy(&line).into_owned()));
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(err).split(b'\n').map_while(Result::ok) {
                let _ = tx.send((false, String::from_utf8_lossy(&line).into_owned()));
            }
        }));
    }
    drop(tx);

    let mut fraction = 0.02f32;
    let mut last_err = String::new();
    loop {
        if cancel.load(Ordering::SeqCst) {
            proc::kill_tree(&mut child);
            bail!("Cancelled");
        }
        match rx.recv_timeout(std::time::Duration::from_millis(150)) {
            Ok((true, line)) => {
                if let Some(text) = heard(&line) {
                    on(Progress { fraction, stage: Stage::Listening(text) });
                }
            }
            Ok((false, line)) => {
                if let Some(p) = percent(&line) {
                    fraction = (0.02 + p * 0.97).max(fraction);
                    on(Progress { fraction, stage: Stage::Listening(String::new()) });
                } else if line.contains("error") || line.contains("failed") {
                    last_err = line.trim().to_string();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait()?;
    for r in readers {
        let _ = r.join();
    }
    if !status.success() || !json_base.with_extension("json").is_file() {
        bail!("Whisper stopped: {}", if last_err.is_empty() { format!("{status}") } else { last_err });
    }
    Ok(())
}

/// `whisper_print_progress_callback: progress =  42%` → 0.42.
fn percent(line: &str) -> Option<f32> {
    let rest = line.split("progress =").nth(1)?;
    rest.trim().trim_end_matches('%').trim().parse::<f32>().ok().map(|p| (p / 100.0).clamp(0.0, 1.0))
}

/// `[00:00:06.640 --> 00:00:13.280]   okay so…` → `okay so…`.
fn heard(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix('[')?;
    let (_, text) = rest.split_once(']')?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// whisper-cli's `-oj` output → (language, segments).
pub fn parse_json(json: &str) -> Result<(String, Vec<Segment>)> {
    let v: serde_json::Value = serde_json::from_str(json).context("reading Whisper's output")?;
    let lang = v["result"]["language"].as_str().unwrap_or("").to_string();
    let segs = v["transcription"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let text = s["text"].as_str()?.trim().to_string();
            let from = s["offsets"]["from"].as_f64()? / 1000.0;
            let to = s["offsets"]["to"].as_f64()? / 1000.0;
            (!text.is_empty() && !is_noise(&text)).then_some(Segment { start: from, end: to, text })
        })
        .collect();
    Ok((lang, segs))
}

/// `[Music]`, `(applause)`, `[BLANK_AUDIO]`: not words anyone said.
fn is_noise(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')')) || t == "*"
}

/// `mm:ss`, or `h:mm:ss` past an hour.
pub fn clock(t: f64) -> String {
    let t = t.max(0.0) as u64;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

/// The `.txt`: a short header, then one line per segment (`[01:23] …` with `times`).
/// `parts` (start, name) adds a "Part 2 · name" line where each joined file begins.
pub fn to_text(source: &str, language: &str, segs: &[Segment], times: bool, parts: &[(f64, String)]) -> String {
    let mut out = format!("Transcript: {source}\nLanguage: {}\n\n", language_name(language));
    let mut next_part = 0;
    for s in segs {
        while next_part < parts.len() && s.start + 0.5 >= parts[next_part].0 {
            if next_part > 0 {
                out.push('\n');
            }
            out.push_str(&format!("Part {} · {}\n", next_part + 1, parts[next_part].1));
            next_part += 1;
        }
        if times {
            out.push_str(&format!("[{}] {}\n", clock(s.start), s.text));
        } else {
            out.push_str(&s.text);
            out.push('\n');
        }
    }
    out
}

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// Longest caption, in characters: two lines of 42, what fits on a phone.
const CAPTION_CHARS: usize = 84;

/// A long segment as captions that fit: cut at sentence ends or commas when it can, else
/// between words, each piece given its share of the time by length.
fn caption_pieces(s: &Segment) -> Vec<Segment> {
    let words: Vec<&str> = s.text.split_whitespace().collect();
    let mut pieces: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, w) in words.iter().enumerate() {
        if !cur.is_empty() && cur.len() + 1 + w.len() > CAPTION_CHARS {
            pieces.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
        // A natural break past half a caption: end the piece here.
        let ends = cur.ends_with(['.', '?', '!']) || (cur.ends_with(',') && cur.len() > CAPTION_CHARS * 2 / 3);
        if ends && cur.len() > CAPTION_CHARS / 2 && i + 1 < words.len() {
            pieces.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    let total: usize = pieces.iter().map(|p| p.len()).sum::<usize>().max(1);
    let span = (s.end - s.start).max(0.2);
    let mut at = s.start;
    pieces
        .into_iter()
        .map(|text| {
            let len = span * text.len() as f64 / total as f64;
            let seg = Segment { start: at, end: at + len, text };
            at += len;
            seg
        })
        .collect()
}

pub fn to_srt(segs: &[Segment]) -> String {
    let mut out = String::new();
    for (i, s) in segs.iter().flat_map(caption_pieces).enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            srt_time(s.start),
            srt_time(s.end.max(s.start + 0.2)),
            s.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"result":{"language":"tl"},"transcription":[
        {"timestamps":{"from":"00:00:00,000","to":"00:00:06,640"},"offsets":{"from":0,"to":6640},"text":" The difference pathway."},
        {"offsets":{"from":6640,"to":7000},"text":" [BLANK_AUDIO]"},
        {"offsets":{"from":65000,"to":3725500},"text":" Okay, so let us proceed."}]}"#;

    #[test]
    fn reads_whisper_json() {
        let (lang, segs) = parse_json(JSON).unwrap();
        assert_eq!(lang, "tl");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "The difference pathway.");
        assert_eq!(segs[1].start, 65.0);
    }

    #[test]
    fn writes_text_and_captions() {
        let (_, segs) = parse_json(JSON).unwrap();
        let txt = to_text("1.m4a", "tl", &segs, true, &[]);
        assert!(
            txt.starts_with("Transcript: 1.m4a\nLanguage: Filipino\n\n[00:00] The difference pathway.\n[01:05] Okay")
        );
        assert!(to_text("a", "en", &segs, false, &[]).contains("\nThe difference pathway.\nOkay, so"));
        let srt = to_srt(&segs);
        assert!(srt.starts_with(
            "1\n00:00:00,000 --> 00:00:06,640\nThe difference pathway.\n\n2\n00:01:05,000 --> 01:02:05,500\n"
        ));
    }

    #[test]
    fn splits_long_captions() {
        let s = Segment {
            start: 0.0,
            end: 14.0,
            text: "The difference pathway. Now let us, all right, specifically use glucose metabolism pathway. Okay, so let us proceed now with clinical significance, okay, involved now in your glucose pathway.".into(),
        };
        let p = caption_pieces(&s);
        assert!(p.len() >= 3, "{p:?}");
        assert!(p.iter().all(|x| x.text.len() <= CAPTION_CHARS), "{p:?}");
        assert_eq!(p[0].text, "The difference pathway. Now let us, all right, specifically use glucose metabolism");
        assert!((p.last().unwrap().end - 14.0).abs() < 1e-6);
        let short = Segment { start: 1.0, end: 2.0, text: "Okay.".into() };
        assert_eq!(caption_pieces(&short), vec![short.clone()]);
    }

    #[test]
    fn marks_joined_parts() {
        let segs = vec![
            Segment { start: 1.0, end: 2.0, text: "a".into() },
            Segment { start: 763.0, end: 765.0, text: "b".into() },
        ];
        let txt = to_text("1 + 2", "en", &segs, true, &[(0.0, "1.m4a".into()), (762.7, "2.m4a".into())]);
        assert!(txt.contains("\n\nPart 1 · 1.m4a\n[00:01] a\n\nPart 2 · 2.m4a\n[12:43] b\n"), "{txt}");
    }

    #[test]
    fn reads_progress_and_lines() {
        assert_eq!(percent("whisper_print_progress_callback: progress =  42%"), Some(0.42));
        assert_eq!(percent("whisper_init: loading"), None);
        assert_eq!(heard("[00:00:06.640 --> 00:00:13.280]   okay so"), Some("okay so".into()));
        assert_eq!(heard("main: processing"), None);
    }

    #[test]
    fn keeps_only_what_whisper_cli_needs() {
        for yes in [
            "whisper-cli.exe",
            "whisper.dll",
            "ggml-cuda.dll",
            "ggml-cpu-haswell.dll",
            "cublasLt64_12.dll",
            "libopenblas.dll",
        ] {
            assert!(keep(yes), "{yes}");
        }
        for no in ["SDL2.dll", "llama.dll", "parakeet.dll", "whisper-server.exe", "nvrtc64_120_0.dll", "main.exe"] {
            assert!(!keep(no), "{no}");
        }
    }

    #[test]
    fn orders_files_like_people_number_them() {
        let v: Vec<PathBuf> =
            ["10.m4a", "2 (compressed).m4a", "1.m4a", "b.mp4", "a.mp4"].iter().map(PathBuf::from).collect();
        let names: Vec<String> = in_order(v).iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["1.m4a", "2 (compressed).m4a", "10.m4a", "a.mp4", "b.mp4"]);
    }

    #[test]
    fn names_files_without_colons() {
        assert_eq!(file_stamp(83.4), "01.23");
        assert_eq!(file_stamp(3723.0), "1.02.03");
        assert_eq!(clock(763.0), "12:43");
    }
}
