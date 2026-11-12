//! Compress page: pick a file, choose a size or quality, encode (GPU when possible).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use neria_core::compress::{self, Codec, Encoders, Options, Resolution, Target};
use neria_core::fmt;
use neria_core::media::{self, FileInfo};
use slint::ComponentHandle;

use crate::AppWindow;

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "flv", "wmv"];
const SIZES_MB: [f64; 4] = [20.0, 25.0, 50.0, 100.0];

#[derive(Default)]
pub struct Compressor {
    file: Option<PathBuf>,
    info: Arc<Mutex<Option<FileInfo>>>,
    /// Detected once, in the background, the first time a file is opened.
    encoders: Arc<Mutex<Option<Encoders>>>,
    detecting: bool,
    generation: Arc<AtomicU64>,
    cancel: Option<Arc<AtomicBool>>,
    last_output: Arc<Mutex<Option<PathBuf>>>,
    autostarted: bool,
}

fn options_from_ui(ui: &AppWindow) -> Result<Options> {
    let target = if ui.get_cmp_mode() == 0 {
        let mb = match ui.get_cmp_size_index() {
            i @ 0..=3 => SIZES_MB[i as usize],
            _ => ui
                .get_cmp_custom_mb()
                .trim()
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .filter(|v| *v >= 0.5)
                .ok_or_else(|| anyhow!("Type a size in MB (at least 0.5)."))?,
        };
        Target::SizeMb(mb)
    } else {
        Target::Quality(ui.get_cmp_quality())
    };
    Ok(Options {
        target,
        codec: match ui.get_cmp_codec() {
            1 => Codec::Hevc,
            2 => Codec::Av1,
            _ => Codec::H264,
        },
        use_gpu: ui.get_cmp_use_gpu(),
        resolution: match ui.get_cmp_res_index() {
            1 => Resolution::Original,
            2 => Resolution::Max(1080),
            3 => Resolution::Max(720),
            4 => Resolution::Max(480),
            _ => Resolution::Auto,
        },
        max_fps: match ui.get_cmp_fps_index() {
            1 => Some(60.0),
            2 => Some(30.0),
            _ => None,
        },
        audio_kbps: match ui.get_cmp_audio_index() {
            1 => Some(96),
            2 => Some(64),
            3 => None,
            _ => Some(128),
        },
    })
}

impl Compressor {
    fn open(&mut self, ui: &AppWindow, path: &Path) {
        self.file = Some(path.to_path_buf());
        *self.info.lock().unwrap() = None;
        *self.last_output.lock().unwrap() = None;
        ui.set_cmp_loaded(true);
        ui.set_cmp_name(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into());
        ui.set_cmp_info("Reading file…".into());
        ui.set_cmp_thumb(slint::Image::default());
        ui.set_cmp_status("".into());
        ui.set_cmp_has_output(false);
        ui.set_page(2);
        self.refresh(ui);

        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (gen_ref, info_slot, weak, path) =
            (self.generation.clone(), self.info.clone(), ui.as_weak(), path.to_path_buf());
        std::thread::spawn(move || {
            let result = media::file_info(&path);
            let thumb = result
                .as_ref()
                .ok()
                .filter(|i| i.has_video)
                .and_then(|i| media::thumbnails(&path, i.duration * 0.6, 1, 180).into_iter().next());
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if gen_ref.load(Ordering::SeqCst) != generation {
                    return;
                }
                match result {
                    Ok(info) => {
                        let mut parts = Vec::new();
                        if info.has_video {
                            parts.push(format!("{}×{}", info.width, info.height));
                            if info.fps > 0.0 {
                                parts.push(format!("{:.0} fps", info.fps));
                            }
                        }
                        parts.push(fmt::duration(info.duration));
                        parts.push(fmt::bytes(info.size_bytes as f64));
                        if !info.video_codec.is_empty() {
                            parts.push(info.video_codec.clone());
                        }
                        ui.set_cmp_info(parts.join("  ·  ").into());
                        *info_slot.lock().unwrap() = Some(info);
                    }
                    Err(e) => ui.set_cmp_info(e.to_string().into()),
                }
                if let Some(img) = thumb.and_then(|p| slint::Image::load_from_path(&p).ok()) {
                    ui.set_cmp_thumb(img);
                }
                ui.invoke_cmp_options_changed();
            });
        });

        if !self.detecting && self.encoders.lock().unwrap().is_none() {
            self.detecting = true;
            let (slot, weak) = (self.encoders.clone(), ui.as_weak());
            std::thread::spawn(move || {
                let found = Encoders::detect();
                *slot.lock().unwrap() = Some(found);
                let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_cmp_options_changed());
            });
        }
    }

    /// Recompute the GPU label and the size/resolution estimate.
    fn refresh(&self, ui: &AppWindow) {
        let encoders = self.encoders.lock().unwrap().clone();
        let opts = options_from_ui(ui);

        // GPU checkbox reflects what this machine can do for the chosen codec.
        let codec = opts.as_ref().map(|o| o.codec).unwrap_or(Codec::H264);
        match &encoders {
            None => {
                ui.set_cmp_gpu_available(false);
                ui.set_cmp_gpu_label("Checking GPU…".into());
            }
            Some(e) => {
                let gpu = e.pick(codec, true);
                let available = e.has_gpu_for(codec);
                ui.set_cmp_gpu_available(available);
                ui.set_cmp_gpu_label(match gpu {
                    compress::Encoder::Gpu(_, g) => format!("Use GPU ({})", g.label()).into(),
                    _ => format!("No GPU encoder for {}", codec.label()).into(),
                });
            }
        }

        let info = self.info.lock().unwrap().clone();
        let running = self.cancel.is_some();
        let (text, warning, can_start) = match (info, opts) {
            (None, _) => ("Reading file…".to_string(), false, false),
            (_, Err(e)) => (e.to_string(), true, false),
            (Some(info), Ok(opts)) => {
                match compress::plan(&info, &opts, encoders.as_ref().unwrap_or(&Encoders::default())) {
                    Err(e) => (e.to_string(), true, false),
                    Ok(plan) => {
                        let mut text = plan.summary();
                        let mut warning = false;
                        match plan.target_bytes {
                            Some(t) if t >= info.size_bytes => {
                                text = format!(
                                    "Already smaller than {}: compressing would only lose quality.  {text}",
                                    fmt::bytes(t as f64)
                                );
                                warning = true;
                            }
                            Some(t) => {
                                let pct = 100.0 - t as f64 / info.size_bytes.max(1) as f64 * 100.0;
                                text = format!(
                                    "Will fit in {} MB (at least {pct:.0}% smaller)  ·  {text}",
                                    t as f64 / 1e6
                                );
                            }
                            None => text = format!("Size depends on the video  ·  {text}"),
                        }
                        if encoders.is_none() {
                            text.push_str("  ·  checking GPU…");
                        }
                        (text, warning, true)
                    }
                }
            }
        };
        ui.set_cmp_estimate(text.into());
        ui.set_cmp_warning(warning);
        // Wait for GPU detection, or an early click would silently encode on the CPU.
        ui.set_cmp_can_start(can_start && !running && encoders.is_some());
    }

    fn start(&mut self, ui: &AppWindow) {
        if self.encoders.lock().unwrap().is_none() {
            return;
        }
        let (Some(input), Some(info)) = (self.file.clone(), self.info.lock().unwrap().clone()) else { return };
        let opts = match options_from_ui(ui) {
            Ok(o) => o,
            Err(e) => {
                ui.set_cmp_status(e.to_string().into());
                return;
            }
        };
        let encoders = self.encoders.lock().unwrap().clone().unwrap_or_default();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        let last_output = self.last_output.clone();

        ui.set_cmp_running(true);
        ui.set_cmp_progress(0.0);
        ui.set_cmp_status("Starting…".into());
        ui.set_cmp_has_output(false);
        ui.set_cmp_can_start(false);

        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let started = Instant::now();
            let mut last = Instant::now() - Duration::from_secs(1);
            let w = weak.clone();
            let result = compress::run(&input, &info, &opts, &encoders, &cancel, |p| {
                if last.elapsed() >= Duration::from_millis(100) || p.fraction >= 1.0 {
                    last = Instant::now();
                    let _ = w.upgrade_in_event_loop(move |ui| {
                        ui.set_cmp_progress(p.fraction);
                        ui.set_cmp_status(p.stage.into());
                    });
                }
            });
            let secs = started.elapsed().as_secs_f32();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_cmp_running(false);
                match result {
                    Ok(out) => {
                        let saved = 100.0 - out.bytes as f64 / info.size_bytes.max(1) as f64 * 100.0;
                        ui.set_cmp_status(
                            format!(
                                "Saved · {} ({}, {saved:.0}% smaller) in {secs:.0}s",
                                out.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                                fmt::bytes(out.bytes as f64)
                            )
                            .into(),
                        );
                        ui.set_cmp_has_output(true);
                        crate::background_ui::notify(
                            &ui,
                            crate::background_ui::Note {
                                title: "Compressed".into(),
                                body: format!(
                                    "{} · {}",
                                    out.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                                    fmt::bytes(out.bytes as f64)
                                ),
                                image: None,
                                file: Some(out.path.clone()),
                                failed: false,
                            },
                        );
                        *last_output.lock().unwrap() = Some(out.path);
                    }
                    Err(e) => {
                        crate::background_ui::notify(
                            &ui,
                            crate::background_ui::Note {
                                title: "Couldn't compress it".into(),
                                body: e.to_string(),
                                image: None,
                                file: None,
                                failed: true,
                            },
                        );
                        ui.set_cmp_status(e.to_string().into())
                    }
                }
                ui.invoke_cmp_options_changed();
            });
        });
    }
}

pub fn install(ui: &AppWindow, file_for_job: impl Fn(u64) -> Option<PathBuf> + 'static) -> Rc<RefCell<Compressor>> {
    let cmp = Rc::new(RefCell::new(Compressor::default()));

    ui.on_cmp_open({
        let (cmp, weak) = (cmp.clone(), ui.as_weak());
        move || {
            if let Some(path) =
                rfd::FileDialog::new().add_filter("Video", VIDEO_EXTENSIONS).add_filter("All files", &["*"]).pick_file()
            {
                cmp.borrow_mut().open(&weak.unwrap(), &path);
            }
        }
    });

    ui.on_cmp_job({
        let (cmp, weak) = (cmp.clone(), ui.as_weak());
        move |id| {
            if let Some(path) = file_for_job(id as u64).filter(|p| p.is_file()) {
                cmp.borrow_mut().open(&weak.unwrap(), &path);
            }
        }
    });

    ui.on_cmp_options_changed({
        let (cmp, weak) = (cmp.clone(), ui.as_weak());
        move || {
            // `cancel` doubles as the "running" flag; clear it once the job ended.
            let ui = weak.unwrap();
            let mut c = cmp.borrow_mut();
            if !ui.get_cmp_running() {
                c.cancel = None;
            }
            c.refresh(&ui);
            // Dev hook for scripted checks: start once everything is ready.
            if !c.autostarted && ui.get_cmp_can_start() && std::env::var_os("NERIA_COMPRESS_START").is_some() {
                c.autostarted = true;
                c.start(&ui);
            }
        }
    });

    ui.on_cmp_start({
        let (cmp, weak) = (cmp.clone(), ui.as_weak());
        move || cmp.borrow_mut().start(&weak.unwrap())
    });

    ui.on_cmp_cancel({
        let cmp = cmp.clone();
        move || {
            if let Some(c) = &cmp.borrow().cancel {
                c.store(true, Ordering::SeqCst);
            }
        }
    });

    ui.on_cmp_show_output({
        let cmp = cmp.clone();
        move || {
            if let Some(path) = cmp.borrow().last_output.lock().unwrap().clone() {
                crate::reveal(&path);
            }
        }
    });

    ui.on_cmp_open_path({
        let (cmp, weak) = (cmp.clone(), ui.as_weak());
        move |path| cmp.borrow_mut().open(&weak.unwrap(), Path::new(path.as_str()))
    });

    cmp
}
