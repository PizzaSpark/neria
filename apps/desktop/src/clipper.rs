//! Clip page: libmpv playback rendered into a GL texture shown by Slint,
//! timeline data (filmstrip + waveform), and ffmpeg export.

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use glow::HasContext;
use neria_core::clip::{self, ClipMode, ClipRequest};
use neria_core::download::DownloadRequest;
use neria_core::settings::Quality;
use neria_core::{media, paths, probe};
use neria_player::{GlRenderer, Mpv};
use slint::{ComponentHandle, GraphicsAPI, Model, ModelRc, RenderingState, VecModel};

use crate::{AppWindow, Fmt};

const MEDIA_EXTENSIONS: &[&str] =
    &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "flv", "wmv", "mp3", "m4a", "opus", "ogg", "flac", "wav", "aac"];

struct Target {
    tex: glow::Texture,
    fbo: glow::Framebuffer,
    w: i32,
    h: i32,
}

#[derive(Clone, Debug)]
enum Source {
    File(PathBuf),
    /// A web page URL, streamed by mpv through yt-dlp.
    Link(String),
}

#[derive(Default)]
pub struct Clipper {
    // Field order matters for Drop: the renderer must go before mpv.
    renderer: Option<GlRenderer>,
    target: Option<Target>,
    gl: Option<glow::Context>,
    mpv: Option<Mpv>,
    source: Option<Source>,
    /// Link mode: replace the URL with the real title once mpv knows it.
    title_known: bool,
    pending_load: bool,
    duration_applied: bool,
    /// Playback position at the previous tick (to notice crossing the Out point).
    last_pos: f64,
    load_gen: Arc<AtomicU64>,
    export_cancel: Option<Arc<AtomicBool>>,
    last_output: Arc<Mutex<Option<PathBuf>>>,
}

impl Clipper {
    fn ensure_mpv(&mut self) -> Result<&Mpv> {
        if self.mpv.is_none() {
            let dll = paths::bin_dir().join("libmpv-2.dll");
            let ytdl = format!("ytdl_hook-ytdl_path={}", paths::tool("yt-dlp").display());
            let mut opts: Vec<(&str, String)> = vec![
                ("script-opts", ytdl),
                // Preview quality: quick to start and seek; the download uses the chosen quality.
                ("ytdl-format", "bv*[height<=720]+ba/b[height<=720]/b".into()),
                ("cache", "yes".into()),
                ("demuxer-readahead-secs", "10".into()),
            ];
            // NERIA_MPV_LOG=<file>: verbose mpv log for debugging playback.
            if let Some(log) = std::env::var_os("NERIA_MPV_LOG") {
                opts.push(("log-file", log.to_string_lossy().into_owned()));
                opts.push(("msg-level", "all=v".into()));
            }
            let opts: Vec<(&str, &str)> = opts.iter().map(|(k, v)| (*k, v.as_str())).collect();
            self.mpv = Some(Mpv::new(&dll, &opts)?);
        }
        Ok(self.mpv.as_ref().unwrap())
    }

    fn open(&mut self, ui: &AppWindow, path: &Path) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.open_source(ui, Source::File(path.to_path_buf()), name, true);
    }

    fn open_link(&mut self, ui: &AppWindow, url: &str, title: &str) {
        let known = !title.is_empty();
        let name = if known { title } else { url };
        self.open_source(ui, Source::Link(url.to_string()), name.to_string(), known);
    }

    fn open_source(&mut self, ui: &AppWindow, source: Source, name: String, title_known: bool) {
        if let Err(e) = self.ensure_mpv() {
            ui.set_clip_status(format!("Player unavailable: {e}").into());
            return;
        }
        self.source = Some(source.clone());
        self.title_known = title_known;
        self.duration_applied = false;
        self.last_pos = 0.0;
        self.pending_load = true;
        *self.last_output.lock().unwrap() = None;

        ui.set_clip_loaded(true);
        ui.set_clip_is_link(matches!(source, Source::Link(_)));
        ui.set_clip_buffering(matches!(source, Source::Link(_)));
        ui.set_clip_name(name.into());
        ui.set_clip_status("".into());
        ui.set_clip_has_output(false);
        ui.set_clip_position(0.0);
        ui.set_clip_duration(0.0);
        ui.set_clip_in(0.0);
        ui.set_clip_out(0.0);
        ui.set_clip_playing(false);
        ui.set_clip_has_video(true);
        ui.set_clip_thumbs(ModelRc::from(Rc::new(VecModel::<slint::Image>::default())));
        ui.set_clip_keyframes(ModelRc::from(Rc::new(VecModel::<f32>::default())));
        ui.set_clip_keyframe_ticks(ModelRc::from(Rc::new(VecModel::<f32>::default())));
        ui.set_clip_wave(slint::Image::default());
        ui.set_clip_frame(slint::Image::default());
        ui.set_page(1);

        // The file is loaded on the next frame, once mpv's GL renderer exists
        // (vo=libmpv can't start video without one).
        if self.renderer.is_some() {
            self.load_pending();
        }
        ui.window().request_redraw();

        match source {
            Source::File(path) => self.spawn_timeline_jobs(ui, path),
            Source::Link(url) => self.spawn_link_timeline(ui, url),
        }
    }

    fn load_pending(&mut self) {
        if !self.pending_load {
            return;
        }
        if let (Some(mpv), Some(source)) = (&self.mpv, &self.source) {
            let target = match source {
                Source::File(p) => p.to_string_lossy().into_owned(),
                Source::Link(u) => u.clone(),
            };
            let _ = mpv.set_property("pause", "yes");
            let _ = mpv.command(&["loadfile", &target, "replace"]);
            self.pending_load = false;
        }
    }

    /// Filmstrip for a link: grab frames from a tiny preview stream.
    fn spawn_link_timeline(&self, ui: &AppWindow, url: String) {
        let generation = self.load_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let load_gen = self.load_gen.clone();
        let weak = ui.as_weak();
        let settings = neria_core::settings::Settings::load();
        let need_title = !self.title_known;
        std::thread::spawn(move || {
            // Storyboards first (YouTube blocks direct stream reads); stream frames as a fallback.
            let (title, thumbs) = match media::storyboard_strip(&url, &settings, 12, 120) {
                Ok((title, _, thumbs)) => (title, thumbs),
                Err(_) => {
                    let Ok(preview) = neria_core::probe::preview_stream(&url, &settings) else { return };
                    let thumbs = media::thumbnails_remote(&preview.stream_url, &url, preview.duration, 12, 120);
                    (preview.title, thumbs)
                }
            };
            if need_title && !title.is_empty() {
                let load_gen = load_gen.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    if load_gen.load(Ordering::SeqCst) == generation {
                        ui.set_clip_name(title.into());
                    }
                });
            }
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if load_gen.load(Ordering::SeqCst) != generation {
                    return;
                }
                let images: Vec<slint::Image> =
                    thumbs.iter().filter_map(|p| slint::Image::load_from_path(p).ok()).collect();
                ui.set_clip_thumbs(ModelRc::from(Rc::new(VecModel::from(images))));
            });
        });
    }

    /// Filmstrip and waveform are generated off the UI thread.
    fn spawn_timeline_jobs(&self, ui: &AppWindow, path: PathBuf) {
        let generation = self.load_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let load_gen = self.load_gen.clone();
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let info = match media::file_info(&path) {
                Ok(i) => i,
                Err(e) => {
                    let msg = e.to_string();
                    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_clip_status(msg.into()));
                    return;
                }
            };
            let has_video = info.has_video;
            let _ = weak.upgrade_in_event_loop(move |ui| ui.set_clip_has_video(has_video));

            let wave_job = {
                let path = path.clone();
                let audio = info.has_audio;
                std::thread::spawn(move || {
                    audio.then(|| media::waveform(&path, 1600, if has_video { 68 } else { 128 }).ok()).flatten()
                })
            };
            let keyframe_job = {
                let path = path.clone();
                std::thread::spawn(
                    move || if has_video { media::keyframes(&path).unwrap_or_default() } else { Vec::new() },
                )
            };
            let thumbs = if has_video { media::thumbnails(&path, info.duration, 12, 120) } else { Vec::new() };
            let wave = wave_job.join().ok().flatten();
            let keyframes = keyframe_job.join().unwrap_or_default();
            let ticks = media::decimate(&keyframes, info.duration, 300);

            let _ = weak.upgrade_in_event_loop(move |ui| {
                if load_gen.load(Ordering::SeqCst) != generation {
                    return;
                }
                let to_model =
                    |v: &[f64]| ModelRc::from(Rc::new(VecModel::from(v.iter().map(|k| *k as f32).collect::<Vec<_>>())));
                ui.set_clip_keyframes(to_model(&keyframes));
                ui.set_clip_keyframe_ticks(to_model(&ticks));
                let images: Vec<slint::Image> =
                    thumbs.iter().filter_map(|p| slint::Image::load_from_path(p).ok()).collect();
                ui.set_clip_thumbs(ModelRc::from(Rc::new(VecModel::from(images))));
                if let Some(img) = wave.and_then(|p| slint::Image::load_from_path(&p).ok()) {
                    ui.set_clip_wave(img);
                }
            });
        });
    }

    /// Poll playback state (cheap) and push it to the UI.
    fn tick(&mut self, ui: &AppWindow) {
        let Some(mpv) = &self.mpv else { return };
        mpv.drain_events();
        if let Some(d) = mpv.get_f64("duration").filter(|d| *d > 0.0) {
            ui.set_clip_duration(d as f32);
            if !self.duration_applied {
                self.duration_applied = true;
                // NERIA_CLIP_IN / NERIA_CLIP_OUT: dev hooks for scripted checks.
                let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
                ui.set_clip_in(env("NERIA_CLIP_IN").unwrap_or(0.0));
                ui.set_clip_out(env("NERIA_CLIP_OUT").unwrap_or(d as f32));
                if let Some(t) = env("NERIA_CLIP_IN") {
                    let _ = mpv.command(&["seek", &t.to_string(), "absolute+exact"]);
                    self.last_pos = t as f64;
                }
                if std::env::var_os("NERIA_CLIP_PLAY").is_some() {
                    let _ = mpv.set_property("pause", "no");
                }
                if std::env::var_os("NERIA_CLIP_LOOP").is_some() {
                    ui.set_clip_loop(true);
                }
            }
        }
        let paused = mpv.get_flag("pause").unwrap_or(true);
        let ended = mpv.get_flag("eof-reached").unwrap_or(false);
        if let Some(p) = mpv.get_f64("time-pos") {
            // Playing through the selection: stop at Out, or loop back to In.
            let (cin, cout) = (ui.get_clip_in() as f64, ui.get_clip_out() as f64);
            let crossed = !paused && cout > cin && self.last_pos < cout && p >= cout && self.last_pos >= cin - 0.25;
            if crossed {
                if ui.get_clip_loop() {
                    let _ = mpv.command(&["seek", &format!("{cin:.3}"), "absolute+exact"]);
                } else {
                    let _ = mpv.set_property("pause", "yes");
                    let _ = mpv.command(&["seek", &format!("{cout:.3}"), "absolute+exact"]);
                }
                self.last_pos = if ui.get_clip_loop() { cin } else { cout };
                ui.set_clip_position(self.last_pos as f32);
            } else {
                self.last_pos = p;
                ui.set_clip_position(p as f32);
            }
        }
        let paused = mpv.get_flag("pause").unwrap_or(true);
        ui.set_clip_playing(!paused && !ended);

        if matches!(self.source, Some(Source::Link(_))) {
            let waiting = !self.duration_applied || mpv.get_flag("paused-for-cache").unwrap_or(false);
            ui.set_clip_buffering(waiting);
        }
    }

    /// Play/pause. Pressing play at (or after) the Out point replays the
    /// selection from In, so "play" always means "play my clip".
    fn toggle_play(&mut self, clip_in: f64, clip_out: f64) {
        let Some(mpv) = &self.mpv else { return };
        let paused = mpv.get_flag("pause").unwrap_or(true);
        let ended = mpv.get_flag("eof-reached").unwrap_or(false);
        let pos = mpv.get_f64("time-pos").unwrap_or(0.0);
        let has_selection = clip_out > clip_in;
        if (paused || ended) && has_selection && pos >= clip_out - 0.05 {
            let _ = mpv.command(&["seek", &format!("{clip_in:.3}"), "absolute+exact"]);
            self.last_pos = clip_in;
            let _ = mpv.set_property("pause", "no");
        } else if ended {
            let _ = mpv.command(&["seek", "0", "absolute+exact"]);
            self.last_pos = 0.0;
            let _ = mpv.set_property("pause", "no");
        } else {
            let _ = mpv.command(&["cycle", "pause"]);
        }
    }

    fn seek(&mut self, t: f32) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv.command(&["seek", &format!("{:.3}", t.max(0.0)), "absolute+exact"]);
            // A manual jump isn't "playing across" the Out point.
            self.last_pos = t.max(0.0) as f64;
        }
    }

    fn frame_step(&self, dir: i32) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv.command(&[if dir < 0 { "frame-back-step" } else { "frame-step" }]);
        }
    }

    // ───────────── GL rendering ─────────────

    fn render(&mut self, ui: &AppWindow) {
        let (Some(mpv), Some(gl)) = (&self.mpv, &self.gl) else { return };

        if self.renderer.is_none() {
            let weak = ui.as_weak();
            let redraw = move || {
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.window().request_redraw();
                    }
                });
            };
            match mpv.create_gl_renderer(redraw) {
                Ok(r) => self.renderer = Some(r),
                Err(e) => {
                    ui.set_clip_status(format!("Video renderer failed: {e}").into());
                    return;
                }
            }
            self.load_pending();
            return;
        }

        let (dw, dh) = (mpv.get_i64("dwidth").unwrap_or(0), mpv.get_i64("dheight").unwrap_or(0));
        if dw <= 0 || dh <= 0 {
            return;
        }
        // Render at native size, capped to 1080p: the UI scales it to fit.
        let scale = (1920.0 / dw as f64).min(1080.0 / dh as f64).min(1.0);
        let (w, h) = (((dw as f64) * scale).round() as i32, ((dh as f64) * scale).round() as i32);

        unsafe {
            if self.target.as_ref().is_none_or(|t| t.w != w || t.h != h) {
                if let Some(old) = self.target.take() {
                    gl.delete_framebuffer(old.fbo);
                    gl.delete_texture(old.tex);
                }
                match make_target(gl, w, h) {
                    Ok(t) => {
                        // mpv (flip_y = false) + Slint's default top-left origin = upright.
                        let image = slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                            t.tex.0,
                            [w as u32, h as u32].into(),
                        )
                        .build();
                        ui.set_clip_frame(image);
                        self.target = Some(t);
                        ui.window().request_redraw();
                    }
                    Err(e) => {
                        ui.set_clip_status(format!("GL error: {e}").into());
                        return;
                    }
                }
            }

            let target = self.target.as_ref().unwrap();
            let prev_fbo = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
            let mut viewport = [0i32; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);

            if let Some(r) = &self.renderer {
                r.render(target.fbo.0.get() as i32, w, h, false);
            }

            gl.bind_framebuffer(glow::FRAMEBUFFER, NonZeroU32::new(prev_fbo as u32).map(glow::NativeFramebuffer));
            gl.viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        }
    }

    fn teardown(&mut self) {
        self.renderer = None;
        if let (Some(gl), Some(t)) = (&self.gl, self.target.take()) {
            unsafe {
                gl.delete_framebuffer(t.fbo);
                gl.delete_texture(t.tex);
            }
        }
        self.gl = None;
    }
}

unsafe fn make_target(gl: &glow::Context, w: i32, h: i32) -> Result<Target> {
    unsafe {
        let prev_tex = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
        let prev_fbo = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);

        let tex = gl.create_texture().map_err(|e| anyhow!(e))?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);

        let fbo = gl.create_framebuffer().map_err(|e| anyhow!(e))?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);

        gl.bind_framebuffer(glow::FRAMEBUFFER, NonZeroU32::new(prev_fbo as u32).map(glow::NativeFramebuffer));
        gl.bind_texture(glow::TEXTURE_2D, NonZeroU32::new(prev_tex as u32).map(glow::NativeTexture));

        if status != glow::FRAMEBUFFER_COMPLETE {
            gl.delete_framebuffer(fbo);
            gl.delete_texture(tex);
            return Err(anyhow!("framebuffer incomplete ({status:#x})"));
        }
        Ok(Target { tex, fbo, w, h })
    }
}

fn format_time(t: f32) -> String {
    let t = t.max(0.0) as f64;
    let whole = t.floor() as u64;
    let cs = ((t - t.floor()) * 100.0).floor() as u64;
    let (h, m, s) = (whole / 3600, (whole % 3600) / 60, whole % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}.{cs:02}") } else { format!("{m}:{s:02}.{cs:02}") }
}

/// Wire the clip page into the window. Returns the polling timer (keep it alive).
///
/// `file_for_job` finds a finished download's file; `enqueue` sends a
/// section download (link mode) to the download queue.
pub fn install(
    ui: &AppWindow,
    file_for_job: impl Fn(u64) -> Option<PathBuf> + 'static,
    enqueue: impl Fn(DownloadRequest) + 'static,
) -> Result<(Rc<RefCell<Clipper>>, slint::Timer)> {
    let clipper = Rc::new(RefCell::new(Clipper::default()));

    ui.global::<Fmt>().on_time(|t| format_time(t).into());
    ui.global::<Fmt>().on_keyframe_before(|t, keyframes| {
        let kf: Vec<f64> = keyframes.iter().map(|k| k as f64).collect();
        media::keyframe_before(&kf, t as f64) as f32
    });

    {
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        ui.window()
            .set_rendering_notifier(move |state, api| match state {
                RenderingState::RenderingSetup => {
                    if let GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                        let gl = unsafe { glow::Context::from_loader_function_cstr(|s| get_proc_address(s)) };
                        clipper.borrow_mut().gl = Some(gl);
                    }
                }
                RenderingState::BeforeRendering => {
                    if let Some(ui) = weak.upgrade() {
                        clipper.borrow_mut().render(&ui);
                    }
                }
                RenderingState::RenderingTeardown => clipper.borrow_mut().teardown(),
                _ => {}
            })
            .map_err(|e| anyhow!("video preview needs the OpenGL renderer: {e:?}"))?;
    }

    ui.on_clip_open({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Video and audio", MEDIA_EXTENSIONS)
                .add_filter("All files", &["*"])
                .pick_file()
            {
                clipper.borrow_mut().open(&ui, &path);
            }
        }
    });

    ui.on_clip_open_path({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move |target| {
            let ui = weak.unwrap();
            if probe::looks_like_url(&target) {
                clipper.borrow_mut().open_link(&ui, target.trim(), "");
            } else {
                clipper.borrow_mut().open(&ui, Path::new(target.as_str()));
            }
        }
    });

    ui.on_clip_open_link({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move |url, title| clipper.borrow_mut().open_link(&weak.unwrap(), url.trim(), &title)
    });

    ui.on_clip_paste_link({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
            let text = text.trim();
            if probe::looks_like_url(text) {
                clipper.borrow_mut().open_link(&ui, text, "");
            } else {
                ui.set_clip_status("No link on the clipboard. Copy a video URL first.".into());
            }
        }
    });

    ui.on_clip_job({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move |id| {
            let ui = weak.unwrap();
            match file_for_job(id as u64).filter(|p| p.is_file()) {
                Some(path) => clipper.borrow_mut().open(&ui, &path),
                None => ui.set_clip_status("Couldn't find that download's file.".into()),
            }
        }
    });

    ui.on_clip_toggle_play({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            clipper.borrow_mut().toggle_play(ui.get_clip_in() as f64, ui.get_clip_out() as f64)
        }
    });
    ui.on_clip_seek({
        let clipper = clipper.clone();
        move |t| clipper.borrow_mut().seek(t)
    });
    ui.on_clip_frame_step({
        let clipper = clipper.clone();
        move |d| clipper.borrow().frame_step(d)
    });

    ui.on_clip_export({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            let mut c = clipper.borrow_mut();
            if let Some(mpv) = &c.mpv {
                let _ = mpv.set_property("pause", "yes");
            }
            let input = match c.source.clone() {
                Some(Source::File(path)) => path,
                Some(Source::Link(url)) => {
                    // Link mode: download only the selected part, in the chosen quality.
                    let mut req = DownloadRequest::new(
                        url,
                        Quality::from_index(ui.get_quality()),
                        ui.get_clip_name().to_string(),
                    );
                    req.section = Some((ui.get_clip_in() as f64, ui.get_clip_out() as f64));
                    req.exact_cuts = ui.get_clip_mode() == 1;
                    enqueue(req);
                    ui.set_clip_status("Added to the download queue".into());
                    return;
                }
                None => return,
            };
            let req = ClipRequest {
                input,
                start: ui.get_clip_in() as f64,
                end: ui.get_clip_out() as f64,
                mode: if ui.get_clip_mode() == 0 { ClipMode::Fast } else { ClipMode::Exact },
                has_video: ui.get_clip_has_video(),
            };
            let cancel = Arc::new(AtomicBool::new(false));
            c.export_cancel = Some(cancel.clone());
            let last_output = c.last_output.clone();

            ui.set_clip_exporting(true);
            ui.set_clip_export_progress(0.0);
            ui.set_clip_status("Exporting…".into());

            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let mut last = Instant::now() - Duration::from_secs(1);
                let w = weak.clone();
                let result = clip::export(&req, &cancel, |p| {
                    if last.elapsed() >= Duration::from_millis(80) {
                        last = Instant::now();
                        let _ = w.upgrade_in_event_loop(move |ui| ui.set_clip_export_progress(p));
                    }
                });
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_clip_exporting(false);
                    match result {
                        Ok(path) => {
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            ui.set_clip_status(format!("Saved · {name}").into());
                            ui.set_clip_has_output(true);
                            *last_output.lock().unwrap() = Some(path);
                        }
                        Err(e) => ui.set_clip_status(e.to_string().into()),
                    }
                });
            });
        }
    });

    ui.on_clip_cancel_export({
        let clipper = clipper.clone();
        move || {
            if let Some(c) = &clipper.borrow().export_cancel {
                c.store(true, Ordering::SeqCst);
            }
        }
    });

    ui.on_clip_show_output({
        let clipper = clipper.clone();
        move || {
            if let Some(path) = clipper.borrow().last_output.lock().unwrap().clone() {
                crate::reveal(&path);
            }
        }
    });

    let timer = slint::Timer::default();
    {
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(33), move || {
            if let Some(ui) = weak.upgrade() {
                clipper.borrow_mut().tick(&ui);
            }
        });
    }

    Ok((clipper, timer))
}
