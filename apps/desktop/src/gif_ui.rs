//! Clip page → Export ▾ → Make a GIF (`ClipGif` global, `dialogs/gif.slint`): the dialog's
//! choices as a `gif::GifSpec`, and the size estimate, worked out in the background.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use neria_core::gif::{self, GifSpec, Kind};
use neria_core::fmt;
use slint::ComponentHandle;

use crate::{AppWindow, ClipGif};

const WIDTHS: [u32; 4] = [320, 480, 720, 0];
const FPS: [f32; 4] = [10.0, 15.0, 24.0, 0.0];

/// Over this it's "big": a GIF people can't easily send (Discord's free limit is 10 MB).
const BIG_GIF: u64 = 10 * 1024 * 1024;
const BIG_OTHER: u64 = 8 * 1024 * 1024;

/// Only the newest estimate is shown; older ones still running are told to stop.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn spec(ui: &AppWindow) -> GifSpec {
    let g = ui.global::<ClipGif>();
    GifSpec {
        kind: match g.get_kind() {
            1 => Kind::Webp,
            2 => Kind::Mp4,
            _ => Kind::Gif,
        },
        width: WIDTHS.get(g.get_size().max(0) as usize).copied().unwrap_or(480),
        fps: FPS.get(g.get_fps().max(0) as usize).copied().unwrap_or(15.0),
        loop_forever: g.get_loop() == 0,
        picture: crate::picture_ui::picture(ui),
    }
}

/// The dialog's length line, and (for files) a fresh size estimate.
pub fn refresh(ui: &AppWindow, file: Option<PathBuf>) {
    let g = ui.global::<ClipGif>();
    let (start, end) = (ui.get_clip_in() as f64, ui.get_clip_out() as f64);
    g.set_length_text(format!("From the selection, {}.", fmt::duration(end - start)).into());
    g.set_big(false);
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(file) = file.filter(|_| ui.get_clip_loaded() && end > start) else {
        g.set_estimate("".into());
        return;
    };
    g.set_estimate("Working out the size…".into());
    let spec = spec(ui);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        // Stops as soon as a newer estimate starts.
        let cancel = AtomicBool::new(false);
        let watcher_stop = std::sync::Arc::new(AtomicBool::new(false));
        let result = std::thread::scope(|s| {
            s.spawn(|| {
                while !watcher_stop.load(Ordering::SeqCst) {
                    if GENERATION.load(Ordering::SeqCst) != generation {
                        cancel.store(true, Ordering::SeqCst);
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            });
            let r = gif::estimate(&file, start, end, &spec, &cancel);
            watcher_stop.store(true, Ordering::SeqCst);
            r
        });
        if GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let g = ui.global::<ClipGif>();
            match result {
                Ok(bytes) => {
                    g.set_estimate(format!("About {}", fmt::bytes(bytes as f64)).into());
                    g.set_big(bytes > if spec.kind == Kind::Gif { BIG_GIF } else { BIG_OTHER });
                }
                Err(_) => g.set_estimate("".into()),
            }
        });
    });
}
