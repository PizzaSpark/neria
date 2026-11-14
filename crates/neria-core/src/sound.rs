//! Clip audio: what the Clip page's "Sound" dialog does to a clip's sound when it's saved.
//! Keep / mute / louder or quieter / even out (loudness), plus fades. One filter chain is
//! built here for the export, the player's live preview and downloaded link clips.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{paths, proc};

/// How loud the clip is. One choice: these don't combine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Level {
    #[default]
    AsIs,
    /// No sound at all (the audio stream is dropped, so a lossless cut stays lossless).
    Mute,
    /// Louder or quieter by this many dB; above 0 a limiter keeps peaks from clipping.
    Gain(f32),
    /// Even out to this loudness (LUFS): -14 for YouTube/TikTok/Discord, -16 for podcasts.
    Normalize(f32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioEdit {
    pub level: Level,
    /// Seconds (0 = none).
    pub fade_in: f64,
    pub fade_out: f64,
}

/// Peaks stay under -0.5 dBFS after a boost. `level=disabled` keeps it from turning the
/// whole clip up on its own.
const LIMITER: &str = "alimiter=limit=0.944:level=disabled";

/// What the first loudness pass measured (ffmpeg `loudnorm` with `print_format=json`).
#[derive(Clone, Debug, PartialEq)]
pub struct Loudness {
    pub i: f64,
    pub tp: f64,
    pub lra: f64,
    pub thresh: f64,
    pub offset: f64,
}

impl AudioEdit {
    /// Nothing to do: the sound is saved as it is.
    pub fn is_none(&self) -> bool {
        let level_as_is = match self.level {
            Level::AsIs => true,
            Level::Gain(db) => db.abs() < 0.05,
            _ => false,
        };
        level_as_is && self.fade_in <= 0.0 && self.fade_out <= 0.0
    }

    pub fn mutes(&self) -> bool {
        self.level == Level::Mute
    }

    /// The sound has to be re-encoded (anything but as-is or muted).
    pub fn reencodes(&self) -> bool {
        !self.is_none() && !self.mutes()
    }

    pub fn normalize_target(&self) -> Option<f32> {
        match self.level {
            Level::Normalize(t) => Some(t),
            _ => None,
        }
    }

    /// The ffmpeg `-af` chain for a clip `dur` seconds long whose timestamps start at 0.
    /// `measured` comes from [`measure`] when evening out (two passes: one pass would
    /// pump the volume up and down). `None` when the sound isn't touched.
    pub fn chain(&self, dur: f64, measured: Option<&Loudness>) -> Option<String> {
        if !self.reencodes() {
            return None;
        }
        let mut f: Vec<String> = Vec::new();
        match self.level {
            Level::Gain(db) if db.abs() >= 0.05 => {
                f.push(format!("volume={db:.1}dB"));
                if db > 0.0 {
                    f.push(LIMITER.into());
                }
            }
            Level::Normalize(target) => f.push(match measured {
                Some(m) => format!(
                    "loudnorm=I={target}:TP=-1.5:LRA=11:measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}:measured_thresh={:.2}:offset={:.2}:linear=true",
                    m.i, m.tp, m.lra, m.thresh, m.offset
                ),
                None => format!("loudnorm=I={target}:TP=-1.5:LRA=11"),
            }),
            _ => {}
        }
        // A fade never takes more than half the clip.
        let half = (dur / 2.0).max(0.0);
        let (fi, fo) = (self.fade_in.min(half), self.fade_out.min(half));
        if fi > 0.0 {
            f.push(format!("afade=t=in:st=0:d={fi:.3}"));
        }
        if fo > 0.0 {
            f.push(format!("afade=t=out:st={:.3}:d={fo:.3}", (dur - fo).max(0.0)));
        }
        (!f.is_empty()).then(|| f.join(","))
    }

    /// For the player while previewing: the level only (fades are added when it's saved).
    /// One-pass loudnorm is fine for listening. "" = no filter.
    pub fn preview(&self) -> String {
        match self.level {
            Level::AsIs => String::new(),
            Level::Mute => "volume=0".into(),
            Level::Gain(db) if db.abs() < 0.05 => String::new(),
            Level::Gain(db) if db > 0.0 => format!("volume={db:.1}dB,{LIMITER}"),
            Level::Gain(db) => format!("volume={db:.1}dB"),
            Level::Normalize(t) => format!("loudnorm=I={t}:TP=-1.5:LRA=11"),
        }
    }

    /// In a few words, for the button that opens the dialog ("as is", "+3 dB · fades").
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        match self.level {
            Level::AsIs => {}
            Level::Mute => return "muted".into(),
            Level::Gain(db) if db.abs() < 0.05 => {}
            Level::Gain(db) => parts.push(format!("{}{db:.0} dB", if db > 0.0 { "+" } else { "" })),
            Level::Normalize(t) => parts.push(format!("evened out ({t:.0} LUFS)")),
        }
        match (self.fade_in > 0.0, self.fade_out > 0.0) {
            (true, true) => parts.push("fades".into()),
            (true, false) => parts.push("fade in".into()),
            (false, true) => parts.push("fade out".into()),
            _ => {}
        }
        if parts.is_empty() { "as is".into() } else { parts.join(" · ") }
    }
}

/// The audio encoder (and its quality) that fits a container, by file extension. Empty for
/// containers not listed: ffmpeg then picks the one that container expects.
pub fn encoder_for(ext: &str) -> Vec<String> {
    let a: &[&str] = match ext.to_ascii_lowercase().as_str() {
        "webm" | "opus" => &["-c:a", "libopus", "-b:a", "160k"],
        "ogg" | "oga" => &["-c:a", "libvorbis", "-q:a", "6"],
        "mp3" => &["-c:a", "libmp3lame", "-q:a", "2"],
        "flac" => &["-c:a", "flac"],
        "wav" => &["-c:a", "pcm_s16le"],
        "mp4" | "m4a" | "m4v" | "mov" | "mkv" | "mka" | "aac" | "3gp" | "ts" => &["-c:a", "aac", "-b:a", "192k"],
        _ => &[],
    };
    a.iter().map(|s| s.to_string()).collect()
}

/// Run ffmpeg with `-progress pipe:1` already in `args`, reporting 0..1 of `total` seconds.
/// Returns its error output. Stops (and says "Cancelled") once `cancel` is set.
pub(crate) fn run_ffmpeg(
    args: &[String],
    total: f64,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f32),
) -> Result<String> {
    let mut child = proc::command(paths::tool("ffmpeg"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg")?;
    let mut stderr = child.stderr.take().expect("piped");
    let err_reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let total = total.max(0.01);
    let stdout = child.stdout.take().expect("piped");
    for line in BufReader::new(stdout).lines() {
        if cancel.load(Ordering::SeqCst) {
            proc::kill_tree(&mut child);
            break;
        }
        let Ok(line) = line else { break };
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
            on_progress(((us / 1_000_000.0) / total).clamp(0.0, 1.0) as f32);
        }
    }
    let status = child.wait()?;
    let errors = err_reader.join().unwrap_or_default();
    if cancel.load(Ordering::SeqCst) {
        bail!("Cancelled");
    }
    if !status.success() {
        let msg = errors.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("ffmpeg failed").to_string();
        bail!("{msg}");
    }
    Ok(errors)
}

/// First pass of evening out: how loud `input` is from `start` for `dur` seconds.
/// `None` when there's nothing to measure (silence).
pub fn measure(
    input: &Path,
    start: f64,
    dur: f64,
    target: f32,
    cancel: &AtomicBool,
    on_progress: impl FnMut(f32),
) -> Result<Option<Loudness>> {
    let mut a: Vec<String> =
        ["-hide_banner", "-nostats", "-progress", "pipe:1", "-y"].map(String::from).to_vec();
    if start > 0.0 {
        a.extend(["-ss".into(), format!("{start:.3}")]);
    }
    a.extend(["-i".into(), input.to_string_lossy().into_owned()]);
    if dur > 0.0 {
        a.extend(["-t".into(), format!("{dur:.3}")]);
    }
    a.extend(["-map".into(), "0:a:0".into(), "-vn".into(), "-sn".into(), "-dn".into()]);
    a.extend(["-af".into(), format!("loudnorm=I={target}:TP=-1.5:LRA=11:print_format=json")]);
    a.extend(["-f", "null", "-"].map(String::from));
    let total = if dur > 0.0 { dur } else { crate::media::file_info(input).map(|i| i.duration).unwrap_or(0.0) };
    let log = run_ffmpeg(&a, total, cancel, on_progress).map_err(|e| anyhow::anyhow!("Couldn't measure the loudness: {e}"))?;
    Ok(parse_loudness(&log))
}

fn parse_loudness(log: &str) -> Option<Loudness> {
    let start = log.rfind('{')?;
    let end = start + log[start..].find('}')? + 1;
    let v: serde_json::Value = serde_json::from_str(&log[start..end]).ok()?;
    let num = |k: &str| v[k].as_str().and_then(|s| s.trim().parse::<f64>().ok()).filter(|x| x.is_finite());
    Some(Loudness {
        i: num("input_i")?,
        tp: num("input_tp")?,
        lra: num("input_lra")?,
        thresh: num("input_thresh")?,
        offset: num("target_offset")?,
    })
}

/// Apply `edit` to a whole downloaded file, in place (a link clip after its download).
/// The video is copied; only the sound is re-encoded (or dropped when muted).
pub fn apply_in_place(file: &Path, edit: &AudioEdit, cancel: &AtomicBool, mut on_progress: impl FnMut(f32)) -> Result<()> {
    if edit.is_none() {
        return Ok(());
    }
    let info = crate::media::file_info(file)?;
    if !info.has_audio {
        return Ok(());
    }
    if edit.mutes() && !info.has_video {
        bail!("This is a sound-only file, so muting it would leave nothing");
    }
    let measured = match edit.normalize_target() {
        Some(t) => {
            let m = measure(file, 0.0, info.duration, t, cancel, |p| on_progress(p * 0.4))?;
            if m.is_none() {
                // Nothing but silence: nothing to even out.
                return Ok(());
            }
            m
        }
        None => None,
    };
    let ext = file.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
    let tmp: PathBuf = file.with_extension(format!("neria-sound.{ext}"));
    let mut a: Vec<String> =
        ["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y", "-i"].map(String::from).to_vec();
    a.push(file.to_string_lossy().into_owned());
    a.extend(["-map", "0", "-c", "copy"].map(String::from));
    if edit.mutes() {
        a.push("-an".into());
    } else {
        a.extend(encoder_for(&ext));
        if let Some(chain) = edit.chain(info.duration, measured.as_ref()) {
            a.extend(["-af".into(), chain]);
        }
        if measured.is_some() {
            a.extend(["-ar".into(), "48000".into()]);
        }
    }
    a.push(tmp.to_string_lossy().into_owned());
    let base = if measured.is_some() { 0.4 } else { 0.0 };
    let result = run_ffmpeg(&a, info.duration, cancel, |p| on_progress(base + p * (1.0 - base)));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        bail!("Couldn't change the sound: {e}");
    }
    std::fs::rename(&tmp, file).or_else(|_| {
        std::fs::copy(&tmp, file)?;
        std::fs::remove_file(&tmp)
    })?;
    on_progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_is_and_mute_touch_nothing() {
        assert!(AudioEdit::default().is_none());
        assert_eq!(AudioEdit::default().chain(10.0, None), None);
        let m = AudioEdit { level: Level::Mute, fade_in: 1.0, ..Default::default() };
        assert!(m.mutes() && !m.reencodes());
        assert_eq!(m.summary(), "muted");
        assert!(AudioEdit { level: Level::Gain(0.0), ..Default::default() }.is_none());
    }

    #[test]
    fn gain_gets_a_limiter_only_when_louder() {
        let up = AudioEdit { level: Level::Gain(6.0), ..Default::default() };
        assert_eq!(up.chain(5.0, None).unwrap(), format!("volume=6.0dB,{LIMITER}"));
        let down = AudioEdit { level: Level::Gain(-3.0), ..Default::default() };
        assert_eq!(down.chain(5.0, None).unwrap(), "volume=-3.0dB");
        assert_eq!(up.summary(), "+6 dB");
        assert_eq!(down.summary(), "-3 dB");
    }

    #[test]
    fn fades_fit_the_clip() {
        let e = AudioEdit { level: Level::AsIs, fade_in: 1.0, fade_out: 5.0 };
        // 4 s clip: the fade out is cut to half of it.
        assert_eq!(e.chain(4.0, None).unwrap(), "afade=t=in:st=0:d=1.000,afade=t=out:st=2.000:d=2.000");
        assert_eq!(e.summary(), "fades");
        assert_eq!(e.preview(), "");
    }

    #[test]
    fn second_pass_uses_the_measurement() {
        let e = AudioEdit { level: Level::Normalize(-14.0), ..Default::default() };
        let m = Loudness { i: -23.1, tp: -4.0, lra: 6.2, thresh: -33.5, offset: 0.3 };
        let c = e.chain(30.0, Some(&m)).unwrap();
        assert!(c.starts_with("loudnorm=I=-14:"));
        assert!(c.contains("measured_I=-23.10") && c.contains("linear=true"));
        assert_eq!(e.summary(), "evened out (-14 LUFS)");
    }

    #[test]
    fn reads_loudnorm_json() {
        let log = "[Parsed_loudnorm_0 @ 0x1] \n{\n\t\"input_i\" : \"-20.53\",\n\t\"input_tp\" : \"-3.10\",\n\t\"input_lra\" : \"5.40\",\n\t\"input_thresh\" : \"-30.80\",\n\t\"output_i\" : \"-14.2\",\n\t\"target_offset\" : \"0.20\"\n}\n";
        let m = parse_loudness(log).unwrap();
        assert_eq!(m.i, -20.53);
        assert_eq!(m.offset, 0.2);
        let silent = log.replace("\"-20.53\"", "\"-inf\"");
        assert_eq!(parse_loudness(&silent), None);
    }

    #[test]
    fn encoder_follows_the_container() {
        assert_eq!(encoder_for("webm")[1], "libopus");
        assert_eq!(encoder_for("MP4")[1], "aac");
        assert_eq!(encoder_for("mp3")[1], "libmp3lame");
        assert!(encoder_for("aiff").is_empty());
    }
}
