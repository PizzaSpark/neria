//! "Clip that": a spoken phrase that presses save, listened for on the mic (prototype, no UI).
//!
//! sherpa-onnx's keyword spotting (k2-fsa, Apache 2.0): a tiny streaming speech recogniser that
//! only follows the phrases it's given, so the phrase is whatever the user types ("clip that",
//! "Neria, clip that", "save it"): no training, no recordings. The phrase is cut into the
//! model's word pieces here (`Pieces`, read from its `bpe.model`), the way the model saw words
//! while it learned. Everything stays on the PC; the mic is never recorded or sent anywhere.
//!
//! The model (`sherpa-onnx-kws-zipformer-gigaspeech-3.3M`, int8, ~5 MB on the drive) is
//! fetched into `bin/voice/` only when someone turns voice on, never at first run.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::capture_sound::{self, Sound};
use crate::{paths, tools};

const MODEL_NAME: &str = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";
const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/\
                         sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2";
const ENCODER: &str = "encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx";
const DECODER: &str = "decoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx";
const JOINER: &str = "joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx";
const FILES: [&str; 5] = [ENCODER, DECODER, JOINER, "tokens.txt", "bpe.model"];

/// The phrase when nobody picked one.
pub const DEFAULT_PHRASE: &str = "clip that";

pub fn dir() -> PathBuf {
    paths::bin_dir().join("voice")
}

/// The model is on the drive.
pub fn ready() -> bool {
    FILES.iter().all(|f| dir().join(f).is_file())
}

/// Fetch the model (one ~17 MB archive; only the five files used are kept). Blocking.
pub fn install(cancel: &AtomicBool, mut on: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let base = dir();
    fs::create_dir_all(&base).with_context(|| format!("cannot create {}", base.display()))?;
    let tmp = base.join("model.tar.bz2.download");
    tools::download_to_cancellable(MODEL_URL, &tmp, cancel, &mut on).context("downloading the voice model")?;
    let res = extract(&tmp, &base);
    let _ = fs::remove_file(&tmp);
    res.context("extracting the voice model")
}

fn extract(archive: &Path, to: &Path) -> Result<()> {
    let file = fs::File::open(archive)?;
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(file));
    let mut got = 0;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !path.starts_with(MODEL_NAME) || !FILES.contains(&name) {
            continue;
        }
        let part = to.join(format!("{name}.part"));
        let mut out = fs::File::create(&part)?;
        std::io::copy(&mut entry, &mut out)?;
        drop(out);
        fs::rename(&part, to.join(name))?;
        got += 1;
    }
    if got < FILES.len() {
        bail!("the archive is missing files ({got} of {})", FILES.len());
    }
    Ok(())
}

/// Take the model off the drive.
pub fn remove() -> Result<()> {
    let d = dir();
    if d.exists() {
        fs::remove_dir_all(&d).with_context(|| format!("removing {}", d.display()))?;
    }
    Ok(())
}

/// The model's word pieces and how sentencepiece joins letters into them.
pub struct Pieces {
    score: HashMap<String, f32>,
}

impl Pieces {
    pub fn load() -> Result<Pieces> {
        Self::from_model(&fs::read(dir().join("bpe.model")).context("reading the voice model's word pieces")?)
    }

    /// Read sentencepiece's `ModelProto`: field 1 is each piece (1 = text, 2 = score).
    pub fn from_model(bytes: &[u8]) -> Result<Pieces> {
        let mut score = HashMap::new();
        let mut r = Proto(bytes);
        while let Some((field, wire)) = r.key()? {
            if field == 1 && wire == 2 {
                let mut p = Proto(r.bytes()?);
                let (mut text, mut s) = (String::new(), 0.0f32);
                while let Some((f, w)) = p.key()? {
                    match (f, w) {
                        (1, 2) => text = String::from_utf8_lossy(p.bytes()?).into_owned(),
                        (2, 5) => s = f32::from_le_bytes(p.fixed::<4>()?),
                        _ => p.skip(w)?,
                    }
                }
                score.insert(text, s);
            } else {
                r.skip(wire)?;
            }
        }
        if score.len() < 50 {
            bail!("the voice model's word pieces look wrong ({} found)", score.len());
        }
        Ok(Pieces { score })
    }

    /// The phrase as the model's pieces (`▁CLIP ▁THAT`), or why it can't be.
    pub fn encode(&self, phrase: &str) -> Result<Vec<String>> {
        let words = words(phrase);
        if words.is_empty() {
            bail!("say it in words (letters only, numbers spelled out)");
        }
        let mut out = Vec::new();
        for w in words {
            let mut sym: Vec<String> = std::iter::once("▁".to_string()).chain(w.chars().map(String::from)).collect();
            // "▁" joins the first letter when the model has that piece; sentencepiece does
            // the same by merging the best-scored neighbours until none join.
            loop {
                let best = (0..sym.len().saturating_sub(1))
                    .filter_map(|i| {
                        let joined = format!("{}{}", sym[i], sym[i + 1]);
                        self.score.get(&joined).map(|s| (i, *s, joined))
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
                let Some((i, _, joined)) = best else { break };
                sym[i] = joined;
                sym.remove(i + 1);
            }
            if let Some(bad) = sym.iter().find(|s| !self.score.contains_key(*s)) {
                bail!("the model doesn't know the letter {bad:?}");
            }
            out.extend(sym);
        }
        Ok(out)
    }
}

/// `"Neria, clip that!"` → `["NERIA", "CLIP", "THAT"]` (the model knows A–Z and ').
pub fn words(phrase: &str) -> Vec<String> {
    phrase
        .split(|c: char| !(c.is_ascii_alphabetic() || c == '\''))
        .map(|w| w.trim_matches('\'').to_ascii_uppercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// A tiny protobuf reader, enough for sentencepiece's model file.
struct Proto<'a>(&'a [u8]);

impl<'a> Proto<'a> {
    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let Some((&b, rest)) = self.0.split_first() else { bail!("cut short") };
            self.0 = rest;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bail!("bad varint")
    }
    fn key(&mut self) -> Result<Option<(u64, u8)>> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let k = self.varint()?;
        Ok(Some((k >> 3, (k & 7) as u8)))
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            bail!("cut short");
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.varint()? as usize;
        self.take(n)
    }
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn skip(&mut self, wire: u8) -> Result<()> {
        match wire {
            0 => drop(self.varint()?),
            1 => drop(self.take(8)?),
            2 => drop(self.bytes()?),
            5 => drop(self.take(4)?),
            w => bail!("unknown wire type {w}"),
        }
        Ok(())
    }
}

/// How eager to be: higher hears more (and is fooled more).
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// 0–1: how sure the model must be before it fires (sherpa's trigger threshold).
    pub threshold: f32,
    /// How hard the phrase is pushed while listening (sherpa's boosting score).
    pub boost: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { threshold: 0.25, boost: 1.5 }
    }
}

/// sherpa's keyword lines: pieces, `:boost`, `#threshold`, `@phrase` (spaces as `_`).
pub fn keyword_lines(pieces: &Pieces, phrases: &[String], tune: Tuning) -> Result<String> {
    let mut lines = Vec::new();
    for p in phrases {
        let tokens = pieces.encode(p).with_context(|| format!("\"{p}\""))?;
        let name = words(p).join("_");
        lines.push(format!("{} :{} #{} @{}", tokens.join(" "), tune.boost, tune.threshold, name));
    }
    Ok(lines.join("\n"))
}

/// A phrase was heard.
#[derive(Clone, Debug)]
pub struct Heard {
    /// Which one, as typed words (`CLIP THAT`).
    pub phrase: String,
    /// Seconds since listening started (for files: into the file).
    pub at: f64,
}

/// The spotter with the phrases loaded, fed 16 kHz or 48 kHz mono samples.
pub struct Spotter {
    kws: sherpa_onnx::KeywordSpotter,
    stream: sherpa_onnx::OnlineStream,
    fed: f64,
    /// Time spent decoding, to say what listening costs.
    pub busy: Duration,
}

impl Spotter {
    pub fn new(phrases: &[String], tune: Tuning) -> Result<Spotter> {
        if !ready() {
            bail!("the voice model isn't downloaded yet");
        }
        let lines = keyword_lines(&Pieces::load()?, phrases, tune)?;
        let d = dir();
        let s = |f: &str| Some(d.join(f).to_string_lossy().into_owned());
        let mut cfg = sherpa_onnx::KeywordSpotterConfig::default();
        cfg.model_config.transducer.encoder = s(ENCODER);
        cfg.model_config.transducer.decoder = s(DECODER);
        cfg.model_config.transducer.joiner = s(JOINER);
        cfg.model_config.tokens = s("tokens.txt");
        cfg.model_config.num_threads = 1;
        cfg.keywords_buf = Some(lines);
        cfg.keywords_threshold = tune.threshold;
        cfg.keywords_score = tune.boost;
        let kws = sherpa_onnx::KeywordSpotter::create(&cfg).context("starting the voice listener")?;
        let stream = kws.create_stream();
        Ok(Spotter { kws, stream, fed: 0.0, busy: Duration::ZERO })
    }

    /// Feed samples (-1..1, mono); returns the phrases heard in them.
    pub fn feed(&mut self, rate: u32, samples: &[f32]) -> Vec<Heard> {
        let t0 = Instant::now();
        self.stream.accept_waveform(rate as i32, samples);
        self.fed += samples.len() as f64 / rate as f64;
        let mut out = Vec::new();
        while self.kws.is_ready(&self.stream) {
            self.kws.decode(&self.stream);
            if let Some(r) = self.kws.get_result(&self.stream) {
                if !r.keyword.is_empty() {
                    out.push(Heard { phrase: r.keyword.replace('_', " "), at: self.fed });
                    // Start fresh so one saying fires once.
                    self.kws.reset(&self.stream);
                }
            }
        }
        self.busy += t0.elapsed();
        out
    }

    /// Seconds of sound fed so far.
    pub fn fed(&self) -> f64 {
        self.fed
    }
}

/// 48 kHz stereo 16-bit → mono -1..1 (the spotter brings it to 16 kHz itself).
pub fn mono(pcm: &[u8]) -> Vec<f32> {
    pcm.chunks_exact(4)
        .map(|f| {
            let l = i16::from_le_bytes([f[0], f[1]]) as f32;
            let r = i16::from_le_bytes([f[2], f[3]]) as f32;
            (l + r) / 65536.0
        })
        .collect()
}

/// Listening on a mic until `stop`: `on` gets each phrase heard and, about every 100 ms, how
/// loud the mic is (0–1, for a meter).
pub fn listen(
    mic: Option<String>,
    phrases: &[String],
    tune: Tuning,
    stop: Arc<AtomicBool>,
    mut on: impl FnMut(Event),
) -> Result<Spotter> {
    let mut spotter = Spotter::new(phrases, tune)?;
    let mut reader = capture_sound::Reader::open(&Sound::Mic(mic))?;
    let mut buf = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        buf.clear();
        reader.read(&mut buf, Duration::from_millis(100))?;
        if buf.is_empty() {
            continue;
        }
        let samples = mono(&buf);
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        on(Event::Level(peak));
        for h in spotter.feed(capture_sound::RATE, &samples) {
            on(Event::Heard(h));
        }
    }
    Ok(spotter)
}

/// What `listen` reports.
#[derive(Clone, Debug)]
pub enum Event {
    Heard(Heard),
    Level(f32),
}

/// Read a sound file as 16 kHz mono through ffmpeg (for checks on recordings).
pub fn read_file(path: &Path) -> Result<Vec<f32>> {
    let mut child = crate::proc::command(paths::tool("ffmpeg"))
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-ac", "1", "-ar", "16000", "-"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("starting ffmpeg")?;
    let mut raw = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut raw)?;
    child.wait()?;
    Ok(raw.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_keep_letters_only() {
        assert_eq!(words("Neria, clip that!"), ["NERIA", "CLIP", "THAT"]);
        assert_eq!(words("  don't   stop "), ["DON'T", "STOP"]);
        assert!(words("123 !!").is_empty());
    }

    /// Against the model's own examples (`keywords_raw.txt` → `keywords.txt`), when it's here.
    #[test]
    fn pieces_match_the_models_examples() {
        let Ok(p) = Pieces::load() else { return };
        for (raw, want) in [
            ("HELLO WORLD", "▁HE LL O ▁WORLD"),
            ("HI GOOGLE", "▁HI ▁GO O G LE"),
            ("HEY SIRI", "▁HE Y ▁S I RI"),
            ("ALEXA", "▁A LE X A"),
            ("LOVE AND PEACE", "▁LOVE ▁AND ▁P E A CE"),
            ("HAPPY NEW YEAR", "▁HA PP Y ▁NEW ▁YEAR"),
            ("MERRY CHRISTMAS", "▁ME R RY ▁ CH R IST MA S"),
        ] {
            assert_eq!(p.encode(raw).unwrap().join(" "), want, "{raw}");
        }
    }
}
