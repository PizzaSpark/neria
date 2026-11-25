//! Settings → General → "Video work on": processor or graphics card, one choice for the
//! video Neria makes outside instant replay (which has its own Encoder, Settings → Capture):
//! clips cut exactly, the Clip page's Sound and Picture on a video, GIFs saved as MP4, chat
//! and captions burned in, Compress's starting tick, workflows, and speech to text.
//!
//! Automatic and Graphics card both use the card's encoder when one works here (NVIDIA, then
//! AMD, then Intel); Processor never touches the card. A file the card's encoder refuses
//! (too big for it, an odd driver) is made again on the processor, so picking the card
//! never costs a clip.

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::compress::{Codec, Encoders, Gpu};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Processing {
    /// The graphics card when it has an encoder that works, else the processor.
    #[default]
    Auto,
    /// The graphics card (the processor only for what the card can't do).
    Gpu,
    /// The processor only.
    Cpu,
}

impl Processing {
    pub fn index(self) -> i32 {
        match self {
            Self::Auto => 0,
            Self::Gpu => 1,
            Self::Cpu => 2,
        }
    }

    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Gpu,
            2 => Self::Cpu,
            _ => Self::Auto,
        }
    }
}

static PICK: AtomicU8 = AtomicU8::new(0);

/// The choice, from settings (at start and whenever it changes).
pub fn set(p: Processing) {
    PICK.store(p.index() as u8, Ordering::SeqCst);
}

pub fn get() -> Processing {
    Processing::from_index(PICK.load(Ordering::SeqCst) as i32)
}

/// The graphics card may be used.
pub fn allowed() -> bool {
    get() != Processing::Cpu
}

/// The graphics card encoders that work here, tested once (about a second; `warm` does it
/// at start, off the UI thread).
pub fn encoders() -> &'static Encoders {
    static E: OnceLock<Encoders> = OnceLock::new();
    E.get_or_init(Encoders::detect)
}

/// Test the encoders in the background, so the first clip doesn't wait for it.
pub fn warm() {
    std::thread::spawn(|| {
        let _ = encoders();
    });
}

/// The card with a working H.264 encoder here, whatever the choice.
pub fn card() -> Option<Gpu> {
    [Gpu::Nvidia, Gpu::Amd, Gpu::Intel].into_iter().find(|g| encoders().gpu.contains(&(Codec::H264, *g)))
}

/// The card whose H.264 encoder a clip gets now, if any.
pub fn h264_card() -> Option<Gpu> {
    if !allowed() || CPU_ONLY.with(Cell::get) {
        return None;
    }
    card()
}

/// "NVIDIA" / "AMD" / "Intel": a card's maker, for the settings' hint.
pub fn maker(g: Gpu) -> &'static str {
    match g {
        Gpu::Nvidia => "NVIDIA",
        Gpu::Amd => "AMD",
        Gpu::Intel => "Intel",
    }
}

thread_local! {
    static CPU_ONLY: Cell<bool> = const { Cell::new(false) };
    static USED: Cell<bool> = const { Cell::new(false) };
}

/// H.264 video at about libx264's `crf` (18 looks the same as the source), as the choice
/// says: the card's encoder, or libx264 `veryfast`. 8-bit 4:2:0, which plays everywhere.
pub fn h264_args(crf: u8) -> Vec<String> {
    let q = crf.to_string();
    let v: Vec<&str> = match h264_card() {
        Some(Gpu::Nvidia) => vec![
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p5",
            "-tune",
            "hq",
            "-rc",
            "vbr",
            "-cq",
            &q,
            "-b:v",
            "0",
            "-pix_fmt",
            "yuv420p",
        ],
        Some(Gpu::Amd) => {
            vec!["-c:v", "h264_amf", "-quality", "quality", "-rc", "cqp", "-qp_i", &q, "-qp_p", &q, "-pix_fmt", "nv12"]
        }
        Some(Gpu::Intel) => vec!["-c:v", "h264_qsv", "-preset", "medium", "-global_quality", &q, "-pix_fmt", "nv12"],
        None => vec!["-c:v", "libx264", "-preset", "veryfast", "-crf", &q, "-pix_fmt", "yuv420p"],
    };
    if v[1] != "libx264" {
        USED.with(|u| u.set(true));
    }
    v.into_iter().map(String::from).collect()
}

/// Stands in an argument list for `h264_args`, filled in by `fill` when it runs (inside
/// `or_on_cpu`, so a retry gets the processor's).
pub const H264: &str = "\u{1}h264";

/// `args` with `H264` replaced by the encoder's arguments at `crf`.
pub fn fill(args: &[String], crf: u8) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len() + 12);
    for a in args {
        if a == H264 {
            out.extend(h264_args(crf));
        } else {
            out.push(a.clone());
        }
    }
    out
}

/// Run `f` (which builds its arguments with `h264_args`); if it failed with the card's
/// encoder, run it once more on the processor. Cancelling isn't retried.
pub fn or_on_cpu<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
    USED.with(|u| u.set(false));
    match f() {
        Err(e) if USED.with(Cell::get) && e.to_string() != "Cancelled" => {
            eprintln!("graphics card encoder failed ({e}); again on the processor");
            CPU_ONLY.with(|c| c.set(true));
            let r = f();
            CPU_ONLY.with(|c| c.set(false));
            r
        }
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processor_means_libx264() {
        set(Processing::Cpu);
        let a = h264_args(18);
        assert_eq!(a[1], "libx264");
        assert!(a.windows(2).any(|w| w[0] == "-crf" && w[1] == "18"));
    }

    #[test]
    fn retries_once_on_the_processor() {
        // Pretend the card was asked for and failed.
        let mut runs = 0;
        let r: Result<()> = or_on_cpu(|| {
            runs += 1;
            if runs == 1 {
                USED.with(|u| u.set(true));
                anyhow::bail!("nvenc refused");
            }
            assert!(CPU_ONLY.with(Cell::get));
            Ok(())
        });
        assert!(r.is_ok());
        assert_eq!(runs, 2);
        assert!(!CPU_ONLY.with(Cell::get));
    }

    #[test]
    fn cancelling_is_not_retried() {
        let mut runs = 0;
        let r: Result<()> = or_on_cpu(|| {
            runs += 1;
            USED.with(|u| u.set(true));
            anyhow::bail!("Cancelled");
        });
        assert!(r.is_err());
        assert_eq!(runs, 1);
    }

    #[test]
    fn index_round_trips() {
        for p in [Processing::Auto, Processing::Gpu, Processing::Cpu] {
            assert_eq!(Processing::from_index(p.index()), p);
        }
    }
}
