//! "Clip that" by your own voice: Test it keeps the three times you said the phrase, and a
//! saying that sounds like them counts too, even when the phrase model doesn't understand the
//! words (an accent, another language). The way old phones dialled by voice: each saying is
//! turned into a sound fingerprint (MFCCs, 10 ms steps) and compared with the samples by
//! dynamic time warping, which lets one be said a little faster or slower than the other.
//!
//! Strict on purpose: the match has to be about as close as your samples are to each other,
//! and about as long. Samples stay on this PC (`voice/samples/` next to the exe), as 16 kHz
//! WAV files.

use std::f32::consts::PI;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::paths;

const RATE: usize = 16_000;
const FRAME: usize = 400; // 25 ms
const HOP: usize = 160; // 10 ms
const NFFT: usize = 512;
const MELS: usize = 26;
const COEFFS: usize = 12;

/// Where the samples live (user data, next to the exe).
pub fn dir() -> PathBuf {
    paths::app_dir().join("voice").join("samples")
}

/// A saying's fingerprint: one row of coefficients per 10 ms.
#[derive(Clone, Debug)]
pub struct Print(Vec<[f32; COEFFS]>);

impl Print {
    pub fn secs(&self) -> f32 {
        self.0.len() as f32 * HOP as f32 / RATE as f32
    }
}

/// The fingerprint of 16 kHz mono sound (the speech in it: quiet ends are cut first).
pub fn print(samples: &[f32]) -> Option<Print> {
    let speech = trim(samples);
    if speech.len() < FRAME * 4 {
        return None;
    }
    let bank = mel_bank();
    let window: Vec<f32> = (0..FRAME).map(|i| 0.54 - 0.46 * (2.0 * PI * i as f32 / (FRAME - 1) as f32).cos()).collect();
    let mut rows: Vec<[f32; COEFFS]> = Vec::new();
    let mut prev = 0f32;
    let emph: Vec<f32> = speech
        .iter()
        .map(|&s| {
            let e = s - 0.97 * prev;
            prev = s;
            e
        })
        .collect();
    let mut start = 0;
    while start + FRAME <= emph.len() {
        let mut re = [0f32; NFFT];
        let mut im = [0f32; NFFT];
        for i in 0..FRAME {
            re[i] = emph[start + i] * window[i];
        }
        fft(&mut re, &mut im);
        let power: Vec<f32> = (0..=NFFT / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect();
        let logmel: Vec<f32> =
            bank.iter().map(|f| f.iter().map(|(k, w)| power[*k] * w).sum::<f32>().max(1e-10).ln()).collect();
        // DCT-II, leaving out the first coefficient (loudness).
        let mut row = [0f32; COEFFS];
        for (c, slot) in row.iter_mut().enumerate() {
            let n = (c + 1) as f32;
            *slot = logmel
                .iter()
                .enumerate()
                .map(|(m, v)| v * (PI * n * (m as f32 + 0.5) / MELS as f32).cos())
                .sum::<f32>();
        }
        rows.push(row);
        start += HOP;
    }
    // The room and the mic out: each coefficient minus its average over the saying.
    let n = rows.len() as f32;
    let mut mean = [0f32; COEFFS];
    for r in &rows {
        for c in 0..COEFFS {
            mean[c] += r[c] / n;
        }
    }
    for r in &mut rows {
        for c in 0..COEFFS {
            r[c] -= mean[c];
        }
    }
    Some(Print(rows))
}

/// The loud middle of a saying: from the first to the last 10 ms step above a share of its peak.
fn trim(samples: &[f32]) -> &[f32] {
    let steps: Vec<f32> =
        samples.chunks(HOP).map(|c| (c.iter().map(|x| x * x).sum::<f32>() / c.len().max(1) as f32).sqrt()).collect();
    let peak = steps.iter().cloned().fold(0f32, f32::max);
    if peak <= 0.0 {
        return &samples[..0];
    }
    let gate = (peak * 0.12).max(0.004);
    let first = steps.iter().position(|e| *e > gate).unwrap_or(0);
    let last = steps.iter().rposition(|e| *e > gate).unwrap_or(0);
    let a = first.saturating_sub(5) * HOP;
    let b = ((last + 6) * HOP).min(samples.len());
    &samples[a.min(b)..b]
}

/// 26 triangles between 20 Hz and 8 kHz on the mel scale: (FFT bin, weight) each.
fn mel_bank() -> Vec<Vec<(usize, f32)>> {
    let mel = |f: f32| 2595.0 * (1.0 + f / 700.0).log10();
    let hz = |m: f32| 700.0 * (10f32.powf(m / 2595.0) - 1.0);
    let (lo, hi) = (mel(20.0), mel(RATE as f32 / 2.0));
    let points: Vec<f32> = (0..MELS + 2).map(|i| hz(lo + (hi - lo) * i as f32 / (MELS + 1) as f32)).collect();
    let bin = |f: f32| f * NFFT as f32 / RATE as f32;
    (0..MELS)
        .map(|m| {
            let (a, b, c) = (bin(points[m]), bin(points[m + 1]), bin(points[m + 2]));
            (a.floor() as usize..=c.ceil() as usize)
                .filter(|k| *k <= NFFT / 2)
                .filter_map(|k| {
                    let x = k as f32;
                    let w = if x < b { (x - a) / (b - a) } else { (c - x) / (c - b) };
                    (w > 0.0).then_some((k, w))
                })
                .collect()
        })
        .collect()
}

/// In-place radix-2 FFT of `NFFT` points.
fn fft(re: &mut [f32; NFFT], im: &mut [f32; NFFT]) {
    let n = NFFT;
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((ang * k as f32).cos(), (ang * k as f32).sin());
                let (a, b) = (start + k, start + k + len / 2);
                let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
            }
        }
        len <<= 1;
    }
}

/// How different two sayings sound (0 the same): dynamic time warping over the fingerprints,
/// per step of the path.
pub fn distance(a: &Print, b: &Print) -> f32 {
    let (n, m) = (a.0.len(), b.0.len());
    if n == 0 || m == 0 {
        return f32::MAX;
    }
    let cost =
        |i: usize, j: usize| -> f32 { a.0[i].iter().zip(&b.0[j]).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt() };
    let mut prev = vec![f32::MAX; m + 1];
    let mut cur = vec![f32::MAX; m + 1];
    prev[0] = 0.0;
    for i in 1..=n {
        cur[0] = f32::MAX;
        for j in 1..=m {
            let best = prev[j - 1].min(prev[j]).min(cur[j - 1]);
            cur[j] = if best == f32::MAX { f32::MAX } else { best + cost(i - 1, j - 1) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m] / (n + m) as f32
}

/// Your samples, ready to compare with.
#[derive(Clone, Debug)]
pub struct Samples {
    prints: Vec<Print>,
    /// A saying this close (or closer) to the nearest sample counts.
    pub limit: f32,
    /// How long they are, on average (a saying much shorter or longer isn't the phrase).
    secs: f32,
}

impl Samples {
    /// From the sound of each saying (16 kHz). Needs at least two that are speech.
    pub fn from_sound(sayings: &[Vec<f32>]) -> Option<Samples> {
        let prints: Vec<Print> = sayings.iter().filter_map(|s| print(s)).collect();
        Self::from_prints(prints)
    }

    fn from_prints(prints: Vec<Print>) -> Option<Samples> {
        if prints.len() < 2 {
            return None;
        }
        // How close your own sayings are to each other sets how close a match must be.
        let mut pair = Vec::new();
        for i in 0..prints.len() {
            for j in i + 1..prints.len() {
                pair.push(distance(&prints[i], &prints[j]));
            }
        }
        let typical = pair.iter().sum::<f32>() / pair.len() as f32;
        let secs = prints.iter().map(Print::secs).sum::<f32>() / prints.len() as f32;
        // Checked on recorded voices: the phrase came in at 0.8–7, every other line at 11 or more.
        let limit = (typical * LIMIT_OVER_TYPICAL).clamp(4.5, 8.0);
        Some(Samples { prints, limit, secs })
    }

    /// Does this saying (16 kHz) sound like the samples? With how close the nearest one is.
    pub fn matches(&self, sound: &[f32]) -> (bool, f32) {
        let Some(p) = print(sound) else { return (false, f32::MAX) };
        let ratio = p.secs() / self.secs.max(0.1);
        if !(0.6..=1.7).contains(&ratio) {
            return (false, f32::MAX);
        }
        let d = self.prints.iter().map(|s| distance(&p, s)).fold(f32::MAX, f32::min);
        (d <= self.limit, d)
    }

    /// Test it's score by voice: each sample compared with the others (would it be heard?).
    pub fn self_check(&self) -> Vec<bool> {
        (0..self.prints.len())
            .map(|i| {
                let others: Vec<Print> =
                    self.prints.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, p)| p.clone()).collect();
                let d = others.iter().map(|o| distance(&self.prints[i], o)).fold(f32::MAX, f32::min);
                d <= self.limit
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.prints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prints.is_empty()
    }
}

/// How much further than your samples are from each other a match may be.
pub const LIMIT_OVER_TYPICAL: f32 = 1.5;

/// At most this many sets are kept (the oldest goes): each Test it adds one.
pub const MAX_SETS: usize = 5;

/// All your sets: one per Test it (a person, or a way of saying it). A saying counts if it
/// matches any one set, judged by that set's own strictness, so another accent or another
/// person added doesn't loosen the match for the others.
#[derive(Clone, Debug, Default)]
pub struct Voices {
    pub sets: Vec<Samples>,
}

impl Voices {
    pub fn matches(&self, sound: &[f32]) -> bool {
        self.sets.iter().any(|s| s.matches(sound).0)
    }

    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }
}

/// Loose WAVs from before sets were a thing: moved into a set of their own.
fn tidy_loose() {
    let loose = wavs_in(&dir());
    if loose.is_empty() {
        return;
    }
    let old = dir().join("set-000000000000000");
    if std::fs::create_dir_all(&old).is_ok() {
        for f in loose {
            if let Some(name) = f.file_name() {
                let _ = std::fs::rename(&f, old.join(name));
            }
        }
    }
}

/// The folders of the sets, oldest first (`set-<unix ms>`; loose WAVs from before sets were
/// a thing are one set too).
fn set_dirs() -> Vec<PathBuf> {
    tidy_loose();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("set-")))
        .collect();
    dirs.sort();
    dirs
}

fn wavs_in(d: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "wav"))
        .collect();
    files.sort();
    files
}

/// One set as Settings lists it: its folder name (the id), its label, how many samples.
#[derive(Clone, Debug, PartialEq)]
pub struct SetInfo {
    pub id: String,
    pub name: String,
    pub samples: usize,
}

/// The sets, oldest first, with their labels (`name.txt` in each; "Voice 1"… when unnamed).
pub fn sets() -> Vec<SetInfo> {
    set_dirs()
        .iter()
        .enumerate()
        .map(|(i, d)| SetInfo {
            id: d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            name: std::fs::read_to_string(d.join("name.txt"))
                .ok()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("Voice {}", i + 1)),
            samples: wavs_in(d).len(),
        })
        .collect()
}

/// Give a set a label ("Me", "Jake").
pub fn rename_set(id: &str, name: &str) -> Result<()> {
    let d = dir().join(id);
    if !id.starts_with("set-") || !d.is_dir() {
        bail!("that set is gone");
    }
    std::fs::write(d.join("name.txt"), name.trim())?;
    Ok(())
}

/// Forget one set.
pub fn forget_set(id: &str) -> Result<()> {
    let d = dir().join(id);
    if id.starts_with("set-") && d.is_dir() {
        std::fs::remove_dir_all(d)?;
    }
    Ok(())
}

/// Add these sayings as a new set (the oldest set goes past `MAX_SETS`); its id.
pub fn add_set(sayings: &[Vec<f32>]) -> Result<String> {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let d = dir().join(format!("set-{ms:015}"));
    std::fs::create_dir_all(&d).with_context(|| format!("cannot create {}", d.display()))?;
    for (i, s) in sayings.iter().enumerate() {
        write_wav(&d.join(format!("{}.wav", i + 1)), s)?;
    }
    let sets = set_dirs();
    for extra in sets.iter().take(sets.len().saturating_sub(MAX_SETS)) {
        let _ = std::fs::remove_dir_all(extra);
    }
    Ok(d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

/// Every saved set.
pub fn load() -> Voices {
    let groups: Vec<Vec<PathBuf>> = set_dirs().iter().map(|d| wavs_in(d)).collect();
    let sets = groups
        .iter()
        .filter_map(|files| {
            let sound: Vec<Vec<f32>> = files.iter().filter_map(|f| read_wav(f).ok()).collect();
            Samples::from_sound(&sound)
        })
        .collect();
    Voices { sets }
}

/// Forget every set.
pub fn forget() -> Result<()> {
    let d = dir();
    if d.exists() {
        std::fs::remove_dir_all(&d)?;
    }
    Ok(())
}

/// How many sets there are, and samples in all.
pub fn count() -> (usize, usize) {
    let dirs = set_dirs();
    let samples = dirs.iter().map(|d| wavs_in(d).len()).sum();
    (dirs.len(), samples)
}

/// 16-bit mono WAV at 16 kHz.
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let data: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(RATE as u32).to_le_bytes());
    out.extend_from_slice(&(RATE as u32 * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out)?;
    Ok(())
}

/// Reads what `write_wav` writes (16-bit mono, 16 kHz).
pub fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let b = std::fs::read(path)?;
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        bail!("not a WAV file");
    }
    // Find the data chunk.
    let mut i = 12;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        if id == b"data" {
            let end = (i + 8 + len).min(b.len());
            return Ok(b[i + 8..end]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect());
        }
        i += 8 + len;
    }
    bail!("no sound in the WAV file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f32, secs: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize).map(|i| (2.0 * PI * f * i as f32 / RATE as f32).sin() * 0.3).collect()
    }

    #[test]
    fn same_sound_is_closer_than_another() {
        let a = print(&tone(440.0, 0.8)).unwrap();
        let b = print(&tone(450.0, 0.9)).unwrap();
        let c = print(&tone(1800.0, 0.8)).unwrap();
        assert!(distance(&a, &b) < distance(&a, &c));
        assert!(distance(&a, &a) < 1e-3);
    }

    #[test]
    fn wav_round_trip() {
        let p = std::env::temp_dir().join("neria-voice-match-test.wav");
        let s = tone(300.0, 0.2);
        write_wav(&p, &s).unwrap();
        let back = read_wav(&p).unwrap();
        assert_eq!(back.len(), s.len());
        assert!((back[100] - s[100]).abs() < 1e-3);
        let _ = std::fs::remove_file(p);
    }
}
