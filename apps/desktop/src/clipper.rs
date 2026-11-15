//! Clip page: libmpv playback rendered into a GL texture shown by Slint,
//! timeline data (filmstrip + waveform), and ffmpeg export.

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use glow::HasContext;
use neria_core::clip::{self, ClipMode, ClipRequest};
use neria_core::download::DownloadRequest;
use neria_core::settings::Quality;
use neria_core::{gif, media, paths, probe};
use neria_player::{GlRenderer, Mpv};
use slint::{ComponentHandle, GraphicsAPI, Model, ModelRc, RenderingState, VecModel};

use crate::{AppWindow, ClipGif, ClipList, ClipLive, ClipPicture, ClipSound, Fmt, Watch};

const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "flv", "wmv", "gif", "mp3", "m4a", "opus", "ogg", "flac", "wav",
    "aac",
];

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

/// What the clipper had open before "Watch" borrowed the player, to put it back.
#[derive(Clone, Debug)]
struct Previous {
    source: Source,
    name: String,
    title_known: bool,
    clip_in: f32,
    clip_out: f32,
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
    /// Watching a link: what was open before (comes back on Back).
    previous: Option<Previous>,
    /// In/Out to put back once the reopened file's length is known.
    restore: Option<(f32, f32)>,
    /// Start playing as soon as the length is known (Watch).
    autoplay: bool,
    /// Watching the link that was already open for clipping: Back leaves it open.
    keep_open: bool,
    /// The open file's own rotation (phone footage), clockwise degrees, from ffprobe: mpv's
    /// `video-params/rotate` reads 0 once its turning is switched off.
    rotation: Arc<AtomicI64>,
    /// Our filters do the turning (mpv's own is off): the frame size already includes it.
    filters_turn: std::cell::Cell<bool>,
    /// Stops the open link's waveform fetch when another source opens.
    link_cancel: Arc<AtomicBool>,
    /// A live stream's replay buffer ("Keep the last 10 min"); stops when dropped.
    buffer: Option<neria_core::live::ReplayBuffer>,
    /// Ticks since the buffer's line was last refreshed.
    buffer_ticks: u32,
    /// A Twitch channel: looking up Neria's relay (no ads) before loading it.
    relay_wait: Option<std::sync::mpsc::Receiver<Option<(String, String)>>>,
    relay_tried: bool,
    /// Playing through the relay: its master playlist (for the replay buffer). The stream is
    /// live (mpv's yt-dlp info, which says so for twitch.tv, isn't there).
    relay: Option<String>,
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
        // Only the Download page's preview knows a link is an X GIF.
        ui.set_clip_is_gif(ui.get_probe_is_gif() && ui.get_url().trim() == url);
    }

    fn open_source(&mut self, ui: &AppWindow, source: Source, name: String, title_known: bool) {
        if let Err(e) = self.ensure_mpv() {
            ui.set_clip_status(format!("Player unavailable: {e}").into());
            return;
        }
        ui.global::<Watch>().set_on(false);
        self.apply_volume(ui);
        self.source = Some(source.clone());
        self.title_known = title_known;
        self.duration_applied = false;
        // Known once ffprobe has read the file (links: none).
        self.rotation.store(0, Ordering::SeqCst);
        // The last link's waveform fetch isn't wanted any more.
        self.link_cancel.store(true, Ordering::SeqCst);
        self.link_cancel = Arc::new(AtomicBool::new(false));
        // Live: known once it plays; a replay buffer belongs to the stream it keeps.
        LIVE_ANCHOR.with(|a| a.set(None));
        self.buffer = None;
        let live = ui.global::<ClipLive>();
        live.set_on(false);
        live.set_can_rewind(false);
        live.set_buffering(false);
        live.set_buffer_text("".into());
        self.last_pos = 0.0;
        self.pending_load = true;
        self.relay_wait = None;
        self.relay_tried = false;
        self.relay = None;
        *self.last_output.lock().unwrap() = None;

        ui.set_clip_loaded(true);
        ui.set_clip_is_link(matches!(source, Source::Link(_)));
        ui.set_clip_is_gif(false);
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
        ui.set_clip_has_audio(true);
        ui.set_clip_gif_file(matches!(&source, Source::File(p) if is_gif_file(p)));
        crate::sound_ui::fit_to_clip(ui);
        // A crop or a clip list belongs to the video it was made on.
        ui.global::<ClipPicture>().set_editing(false);
        crate::picture_ui::reset(ui);
        crate::clips_ui::clear(ui);
        self.apply_sound(ui);
        ui.set_clip_thumbs(ModelRc::from(Rc::new(VecModel::<slint::Image>::default())));
        ui.set_clip_keyframes(ModelRc::from(Rc::new(VecModel::<f32>::default())));
        crate::timeline_ui::reset(
            ui,
            match &source {
                Source::File(p) => crate::timeline_ui::TileSource::File(p.clone()),
                Source::Link(u) => crate::timeline_ui::TileSource::Link(u.clone()),
            },
        );
        let chapters = match &source {
            Source::Link(url) => crate::extras_ui::chapters_for(url),
            _ => Vec::new(),
        };
        set_chapters(ui, &chapters);
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

    /// The listening volume (Watch global): the player's own, never the saved file's.
    fn apply_volume(&self, ui: &AppWindow) {
        let Some(mpv) = &self.mpv else { return };
        let w = ui.global::<Watch>();
        let _ = mpv.set_property("volume", &format!("{:.0}", w.get_volume().clamp(0.0, 100.0)));
        let _ = mpv.set_property("mute", if w.get_muted() { "yes" } else { "no" });
    }

    /// The clip's sound level (Sound dialog), played so you hear it before saving. Fades
    /// are left for the export. Watching plays the link as it is.
    fn apply_sound(&self, ui: &AppWindow) {
        let Some(mpv) = &self.mpv else { return };
        let chain = if ui.global::<Watch>().get_on() { String::new() } else { crate::sound_ui::edit(ui).preview() };
        let af = if chain.is_empty() { String::new() } else { format!("lavfi=[{chain}]") };
        let _ = mpv.set_property("af", &af);
        // The picture too: watching plays the link as it is. A phone video's own rotation
        // moves into the filters (see `player_filter`), so the frame is the saved file's.
        let rotation = self.rotation.load(Ordering::SeqCst);
        let chain =
            if ui.global::<Watch>().get_on() { String::new() } else { crate::picture_ui::player_filter(ui, rotation) };
        let _ = mpv.set_property("video-rotate", if chain.is_empty() { "0" } else { "no" });
        self.filters_turn.set(!chain.is_empty());
        let vf = if chain.is_empty() { String::new() } else { format!("lavfi=[{chain}]") };
        let _ = mpv.set_property("vf", &vf);
        // Paused: show the new picture now, not at the next frame.
        if mpv.get_flag("pause") == Some(true) {
            let _ = mpv.command(&["seek", "0", "relative+exact"]);
        }
    }

    /// Keep the last `minutes` of the open live link (the replay buffer), if there's room.
    fn start_buffer(&mut self, ui: &AppWindow, minutes: u32) {
        let Some(Source::Link(url)) = self.source.clone() else { return };
        // Through the relay too: the minutes kept have no ads either.
        let url = self.relay.clone().unwrap_or(url);
        let g = ui.global::<ClipLive>();
        // ~50 MB a minute at 1080p; leave room for the rest.
        let need = minutes as u64 * 60 * 1_000_000 + 1_000_000_000;
        let dir = neria_core::paths::app_dir();
        if neria_core::disk::free_space(&dir).is_some_and(|free| free < need) {
            g.set_buffer_text(format!("Not enough space to keep the last {minutes} min").into());
            return;
        }
        match neria_core::live::ReplayBuffer::start(&url, &neria_core::settings::Settings::load(), minutes) {
            Ok(b) => {
                self.buffer = Some(b);
                self.buffer_ticks = 0;
                g.set_buffering(true);
                g.set_buffer_text(format!("Keeping the last {minutes} min  ·  starting…").into());
            }
            Err(e) => ui.set_clip_status(format!("Couldn't keep it: {e}").into()),
        }
    }

    /// Stop a running replay buffer (on quit: its ffmpeg mustn't outlive Neria).
    pub fn stop_buffer(&mut self) {
        self.buffer = None;
    }

    /// The replay buffer's line ("Keeping the last 10 min · 3:12 kept · 48 MB"), or why it
    /// stopped.
    fn refresh_buffer(&mut self, ui: &AppWindow) {
        let g = ui.global::<ClipLive>();
        let Some(b) = &self.buffer else {
            g.set_buffering(false);
            return;
        };
        if !b.running() {
            let why = b.problem.lock().unwrap().clone();
            g.set_buffer_text(format!("Stopped keeping it: {why}").into());
            g.set_buffering(false);
            self.buffer = None;
            return;
        }
        let (secs, bytes) = b.kept();
        g.set_buffer_text(
            format!(
                "Keeping the last {} min  ·  {} kept  ·  {}",
                b.minutes,
                neria_core::fmt::duration(secs),
                neria_core::fmt::bytes(bytes as f64)
            )
            .into(),
        );
    }

    /// The picture before turning (decoded size, after the file's own rotation), for the
    /// shape chips.
    fn source_size(&self) -> (f64, f64) {
        let Some(mpv) = &self.mpv else { return (16.0, 9.0) };
        let (w, h) = (mpv.get_i64("video-params/dw").unwrap_or(0), mpv.get_i64("video-params/dh").unwrap_or(0));
        let rotate = self.rotation.load(Ordering::SeqCst);
        if w <= 0 || h <= 0 {
            return (16.0, 9.0);
        }
        if rotate % 180 == 90 { (h as f64, w as f64) } else { (w as f64, h as f64) }
    }

    /// Watch the Download page's previewed link: the player without In/Out.
    fn watch(&mut self, ui: &AppWindow, url: &str, title: &str) {
        let same = matches!(&self.source, Some(Source::Link(u)) if u == url);
        self.keep_open = same;
        if !same && let Some(source) = self.source.clone() {
            self.previous = Some(Previous {
                source,
                name: ui.get_clip_name().to_string(),
                title_known: self.title_known,
                clip_in: ui.get_clip_in(),
                clip_out: ui.get_clip_out(),
            });
        }
        if same {
            ui.set_page(1);
            if let Some(mpv) = &self.mpv {
                let _ = mpv.set_property("pause", "no");
            }
        } else {
            self.open_link(ui, url, title);
            self.autoplay = true;
        }
        ui.global::<Watch>().set_on(true);
        self.apply_sound(ui);
    }

    /// Leave watching: stop, and put back what was open before (or nothing).
    fn unwatch(&mut self, ui: &AppWindow) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv.set_property("pause", "yes");
        }
        self.autoplay = false;
        if std::mem::take(&mut self.keep_open) {
            ui.global::<Watch>().set_on(false);
            self.apply_sound(ui);
            return;
        }
        match self.previous.take() {
            Some(p) => {
                self.open_source(ui, p.source, p.name, p.title_known);
                self.restore = Some((p.clip_in, p.clip_out));
            }
            None => {
                if let Some(mpv) = &self.mpv {
                    let _ = mpv.command(&["stop"]);
                }
                self.source = None;
                self.load_gen.fetch_add(1, Ordering::SeqCst);
                ui.set_clip_loaded(false);
                ui.set_clip_is_link(false);
                ui.set_clip_frame(slint::Image::default());
            }
        }
        ui.global::<Watch>().set_on(false);
    }

    fn load_pending(&mut self) {
        if !self.pending_load {
            return;
        }
        // A Twitch channel plays through the relay that leaves its ads out (looked up off
        // this thread); signed in, or if the relay can't reach it, the link itself.
        let mut via_relay = None;
        if let Some(Source::Link(u)) = &self.source
            && neria_core::ttv::login_of(u).is_some()
            && std::env::var_os("NERIA_TTV_OFF").is_none()
            && neria_core::cookies::args_for(u, &neria_core::settings::Settings::load()).is_empty()
        {
            if !self.relay_tried {
                self.relay_tried = true;
                let (tx, rx) = std::sync::mpsc::channel();
                let url = u.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(neria_core::ttv::relay_for_player(&url, 720));
                });
                self.relay_wait = Some(rx);
            }
            if let Some(rx) = &self.relay_wait {
                match rx.try_recv() {
                    Ok(found) => {
                        self.relay_wait = None;
                        via_relay = found;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => return,
                    Err(_) => self.relay_wait = None,
                }
            }
        }
        if let Some((master, _)) = &via_relay {
            self.relay = Some(master.clone());
        }
        if let (Some(mpv), Some(source)) = (&self.mpv, &self.source) {
            let target = match (source, &via_relay) {
                (Source::File(p), _) => p.to_string_lossy().into_owned(),
                (Source::Link(_), Some((_, media))) => media.clone(),
                (Source::Link(u), None) => u.clone(),
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
        let cancel = self.link_cancel.clone();
        std::thread::spawn(move || {
            // Storyboards first (YouTube blocks direct stream reads); stream frames as a fallback.
            let (title, duration, thumbs) = match media::storyboard_strip(&url, &settings, 12, 120) {
                Ok((title, duration, thumbs)) => (title, duration, thumbs),
                Err(_) => {
                    let Ok(preview) = neria_core::probe::preview_stream(&url, &settings) else { return };
                    let thumbs = media::thumbnails_remote(&preview.stream_url, &url, preview.duration, 12, 120);
                    (preview.title, preview.duration, thumbs)
                }
            };
            // Then the waveform: it fetches the whole sound (smallest format), so only for
            // links of a known, not too long length (never a live stream: it has none).
            if duration > 0.0 && duration <= media::LINK_WAVEFORM_MAX_SECS {
                let (weak, load_gen, url, settings, cancel) =
                    (weak.clone(), load_gen.clone(), url.clone(), settings.clone(), cancel.clone());
                std::thread::spawn(move || {
                    let Ok(peaks) = media::peaks_link(&url, &settings, &cancel) else { return };
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        if load_gen.load(Ordering::SeqCst) == generation {
                            crate::timeline_ui::peaks_ready(&ui, peaks);
                        }
                    });
                });
            }
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
        let rotation = self.rotation.clone();
        std::thread::spawn(move || {
            let info = match media::file_info(&path) {
                Ok(i) => i,
                Err(e) => {
                    let msg = e.to_string();
                    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_clip_status(msg.into()));
                    return;
                }
            };
            if load_gen.load(Ordering::SeqCst) == generation {
                rotation.store(info.rotation as i64, Ordering::SeqCst);
            }
            let has_video = info.has_video;
            let has_audio = info.has_audio;
            let chapters = info.chapters.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_clip_has_video(has_video);
                ui.set_clip_has_audio(has_audio);
                // A sound-only file can't be muted or saved as sound only: back to as is.
                crate::sound_ui::fit_to_clip(&ui);
                ui.global::<ClipSound>().invoke_changed();
                set_chapters(&ui, &chapters);
            });

            // The waveform shows as soon as it's measured, not after the filmstrip.
            if info.has_audio {
                let (path, weak, load_gen) = (path.clone(), weak.clone(), load_gen.clone());
                std::thread::spawn(move || {
                    let Ok(peaks) = media::peaks(&path) else { return };
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        if load_gen.load(Ordering::SeqCst) == generation {
                            crate::timeline_ui::peaks_ready(&ui, peaks);
                        }
                    });
                });
            }
            let keyframe_job = {
                let path = path.clone();
                std::thread::spawn(
                    move || if has_video { media::keyframes(&path).unwrap_or_default() } else { Vec::new() },
                )
            };
            let thumbs = if has_video { media::thumbnails(&path, info.duration, 12, 120) } else { Vec::new() };
            let keyframes = keyframe_job.join().unwrap_or_default();

            let _ = weak.upgrade_in_event_loop(move |ui| {
                if load_gen.load(Ordering::SeqCst) != generation {
                    return;
                }
                let to_model =
                    |v: &[f64]| ModelRc::from(Rc::new(VecModel::from(v.iter().map(|k| *k as f32).collect::<Vec<_>>())));
                ui.set_clip_keyframes(to_model(&keyframes));
                crate::timeline_ui::keyframes_ready(&ui, keyframes);
                let images: Vec<slint::Image> =
                    thumbs.iter().filter_map(|p| slint::Image::load_from_path(p).ok()).collect();
                ui.set_clip_thumbs(ModelRc::from(Rc::new(VecModel::from(images))));
            });
        });
    }

    /// Poll playback state (cheap) and push it to the UI.
    fn tick(&mut self, ui: &AppWindow) {
        // The replay buffer's line, about once a second.
        self.buffer_ticks += 1;
        if self.buffer.is_some() && self.buffer_ticks >= 30 {
            self.buffer_ticks = 0;
            self.refresh_buffer(ui);
        }
        // Waiting for the relay's address: load once it's there.
        if self.pending_load && self.relay_wait.is_some() && self.renderer.is_some() {
            self.load_pending();
        }
        let mut start_buffer = false;
        self.tick_player(ui, &mut start_buffer);
        if start_buffer {
            self.start_buffer(ui, 10);
        }
    }

    fn tick_player(&mut self, ui: &AppWindow, start_buffer: &mut bool) {
        let Some(mpv) = &self.mpv else { return };
        mpv.drain_events();
        if let Some(d) = mpv.get_f64("duration").filter(|d| *d > 0.0) {
            ui.set_clip_duration(d as f32);
            if !self.duration_applied {
                self.duration_applied = true;
                // A live stream's edge on mpv's timeline: where its cache ends (the playlist's
                // end; `duration` lags it by a few pieces at first, 3585 vs 3600). Read before
                // any seek: seeking back empties the cache.
                let live_edge = mpv.get_f64("demuxer-cache-time").filter(|t| *t > d).unwrap_or(d);
                // NERIA_CLIP_IN / NERIA_CLIP_OUT: dev hooks for scripted checks.
                let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
                ui.set_clip_in(env("NERIA_CLIP_IN").unwrap_or(0.0));
                ui.set_clip_out(env("NERIA_CLIP_OUT").unwrap_or(d as f32));
                if let Some(t) = env("NERIA_CLIP_IN") {
                    let _ = mpv.command(&["seek", &t.to_string(), "absolute+exact"]);
                    self.last_pos = t as f64;
                }
                if let Some((a, b)) = self.restore.take() {
                    ui.set_clip_in(a);
                    ui.set_clip_out(if b > a { b } else { d as f32 });
                }
                if std::env::var_os("NERIA_CLIP_PLAY").is_some() || std::mem::take(&mut self.autoplay) {
                    let _ = mpv.set_property("pause", "no");
                }
                if std::env::var_os("NERIA_CLIP_LOOP").is_some() {
                    ui.set_clip_loop(true);
                }
                // A live stream: its timeline is the part the site keeps (it grows), pinned
                // to the clock from here; the selection starts as the last half minute.
                let live = self.relay.is_some()
                    || mpv
                        .get_string("user-data/mpv/ytdl/json-subprocess-result")
                        .is_some_and(|j| j.contains("\"is_live\": true") || j.contains("\\\"is_live\\\": true"));
                // NERIA_LIVE_DEBUG=<file>: mpv's timing properties when a live stream starts.
                if live && let Some(f) = std::env::var_os("NERIA_LIVE_DEBUG") {
                    let names = mpv.get_string("property-list").unwrap_or_default();
                    let mut out = format!("now={} duration={d}\n", neria_core::live::now());
                    for n in names.split(',').filter(|n| {
                        n.contains("start") || n.contains("time") || n.contains("cache") || *n == "duration"
                    }) {
                        out.push_str(&format!("{n}={}\n", mpv.get_string(n).unwrap_or_default()));
                    }
                    let _ = std::fs::write(f, out);
                }
                if live {
                    LIVE_ANCHOR.with(|a| a.set(Some(neria_core::live::now() - live_edge)));
                    // No filmstrip look-up for a live stream, so no title from there: mpv's.
                    if !self.title_known
                        && self.relay.is_none()
                        && let Some(t) =
                            mpv.get_string("media-title").filter(|t| !t.trim().is_empty() && !t.contains("://"))
                    {
                        ui.set_clip_name(t.into());
                        self.title_known = true;
                    }
                    let g = ui.global::<ClipLive>();
                    g.set_on(true);
                    g.set_can_rewind(d > 90.0);
                    // Can't rewind (Twitch): keep the last 10 minutes from the start, so a
                    // moment you want to clip is already there. Not while it's recorded.
                    if d <= 90.0 && self.buffer.is_none() && ui.global::<Watch>().get_recording_id() == 0 {
                        *start_buffer = true;
                    }
                    if std::env::var_os("NERIA_CLIP_IN").is_none() {
                        ui.set_clip_in((d - 30.0).max(0.0) as f32);
                        ui.set_clip_out(d as f32);
                    }
                }
                crate::timeline_ui::length_known(ui, live);
            }
        }
        let paused = mpv.get_flag("pause").unwrap_or(true);
        if !paused && ui.get_page() != 1 {
            let _ = mpv.set_property("pause", "yes");
        }
        let ended = mpv.get_flag("eof-reached").unwrap_or(false);
        if let Some(p) = mpv.get_f64("time-pos") {
            // Playing through the selection: stop at Out, or loop back to In. Watching has no
            // selection (an In/Out or loop left from clipping doesn't count).
            let watching = ui.global::<Watch>().get_on();
            let (cin, cout) = (ui.get_clip_in() as f64, ui.get_clip_out() as f64);
            let crossed =
                !watching && !paused && cout > cin && self.last_pos < cout && p >= cout && self.last_pos >= cin - 0.25;
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
        crate::timeline_ui::tick(ui, ui.get_clip_position() as f64);

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
        // mpv turns phone footage while drawing, after `dwidth` / `dheight`: the target needs
        // the turned shape (unless our filters do the turning, then it's already in there).
        let turned = !self.filters_turn.get() && self.rotation.load(Ordering::SeqCst).rem_euclid(180) == 90;
        let (dw, dh) = if turned { (dh, dw) } else { (dw, dh) };
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

/// The clipper's chapter strip.
fn set_chapters(ui: &AppWindow, chapters: &[neria_core::tracks::Chapter]) {
    let marks: Vec<crate::ChapterMark> = chapters
        .iter()
        .map(|c| crate::ChapterMark { start: c.start as f32, end: c.end as f32, title: c.title.as_str().into() })
        .collect();
    ui.set_clip_chapters(ModelRc::from(Rc::new(VecModel::from(marks))));
}

fn format_time(t: f32) -> String {
    let t = t.max(0.0) as f64;
    let whole = t.floor() as u64;
    let cs = ((t - t.floor()) * 100.0).floor() as u64;
    let (h, m, s) = (whole / 3600, (whole % 3600) / 60, whole % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}.{cs:02}") } else { format!("{m}:{s:02}.{cs:02}") }
}

thread_local! {
    /// The open live stream's timeline start on the clock (Unix seconds): a mark at `t` is
    /// `anchor + t`. `None` when what's open isn't live.
    static LIVE_ANCHOR: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

fn is_gif_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"))
}

/// Link mode: download only the selected part, in the chosen quality. Same rules as the
/// Download page: GIFs come as they are; container, H.264 and audio format come from
/// Settings when the job starts. `None` (with a status) when it can't be done.
fn link_request(ui: &AppWindow, url: String) -> Option<DownloadRequest> {
    let quality = if ui.get_clip_is_gif() { Quality::Best } else { crate::chosen_quality(ui).0 };
    let mut req = DownloadRequest::new(url, quality, ui.get_clip_name().to_string());
    req.preset = crate::chosen_quality(ui).1;
    req.section = Some((ui.get_clip_in() as f64, ui.get_clip_out() as f64));
    req.exact_cuts = ui.get_clip_mode() == 1;
    let sound = crate::sound_ui::edit(ui);
    if sound.mutes() && quality == Quality::Audio {
        ui.set_clip_status("Muted with Audio only would leave nothing to save. Change the Sound.".into());
        return None;
    }
    req.audio_edit = (!sound.is_none()).then_some(sound);
    let picture = crate::picture_ui::picture(ui);
    req.picture = (!picture.is_none() && quality != Quality::Audio).then_some(picture);
    // A live stream: the part is pinned to the clock and found in the site's window when the
    // download starts (it keeps sliding meanwhile).
    if let Some(anchor) = LIVE_ANCHOR.with(|a| a.get()) {
        let (a, b) = req.section.take().unwrap_or_default();
        req.live_window = Some((anchor + a, anchor + b));
        req.exact_cuts = false;
        // Named by the clock time it starts at (window seconds would mean nothing, and two
        // parts of one length would share a name): "Title (live 20.35.10, 0m30s)".
        let start = chrono::DateTime::from_timestamp((anchor + a) as i64, 0)
            .map(|t| t.with_timezone(&chrono::Local).format("%H.%M.%S").to_string())
            .unwrap_or_default();
        req.name_tag = Some(format!(" (live {start}, {})", clip::stamp(b - a)));
    }
    Some(req)
}

/// Link mode with a clip list: one download per clip (one queue card), or one download of
/// all the ranges joined (in time order: that's how the pieces come). `None` with a status.
fn link_requests(ui: &AppWindow, url: String, clips: &[crate::clips_ui::Clip]) -> Option<Vec<DownloadRequest>> {
    let base = link_request(ui, url)?;
    if ui.global::<ClipList>().get_join() && clips.len() > 1 {
        let mut ranges: Vec<(f64, f64)> = clips.iter().map(|c| (c.start, c.end)).collect();
        ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Overlapping or touching ranges become one.
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (a, b) in ranges {
            match merged.last_mut() {
                Some(last) if a <= last.1 + 0.05 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        let mut req = base;
        if merged.len() == 1 {
            req.section = Some(merged[0]);
        } else {
            req.section = None;
            req.sections = merged;
            req.name_tag = Some(format!(" ({} clips)", clips.len()));
        }
        return Some(vec![req]);
    }
    Some(
        clips
            .iter()
            .map(|c| {
                let mut req = base.clone();
                req.section = Some((c.start, c.end));
                req.name_tag = Some(format!(" - {}", neria_core::music::sanitize(&c.name)));
                req
            })
            .collect(),
    )
}

/// Run an export of the open file off the UI thread: progress on the Export button, Cancel,
/// "Saved · name" with Show file, and a notification titled `title`.
fn start_export(
    ui: &AppWindow,
    c: &mut Clipper,
    title: &str,
    job: impl FnOnce(&AtomicBool, &mut dyn FnMut(f32)) -> Result<PathBuf> + Send + 'static,
) {
    let cancel = Arc::new(AtomicBool::new(false));
    c.export_cancel = Some(cancel.clone());
    let last_output = c.last_output.clone();
    ui.set_clip_exporting(true);
    ui.set_clip_export_progress(0.0);
    ui.set_clip_status("Exporting…".into());
    let (weak, title) = (ui.as_weak(), title.to_string());
    std::thread::spawn(move || {
        let mut last = Instant::now() - Duration::from_secs(1);
        let w = weak.clone();
        let mut progress = |p: f32| {
            if last.elapsed() >= Duration::from_millis(80) {
                last = Instant::now();
                let _ = w.upgrade_in_event_loop(move |ui| ui.set_clip_export_progress(p));
            }
        };
        let result = job(&cancel, &mut progress);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_clip_exporting(false);
            match result {
                Ok(path) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    // Several clips: say how many ("3 clips saved · video - Clip 3.mp4").
                    let several = title.contains("clips");
                    ui.set_clip_status(
                        if several { format!("{title} · {name}") } else { format!("Saved · {name}") }.into(),
                    );
                    ui.set_clip_has_output(true);
                    crate::background_ui::notify(
                        &ui,
                        crate::background_ui::Note {
                            title,
                            body: name,
                            image: None,
                            file: Some(path.clone()),
                            failed: false,
                        },
                    );
                    *last_output.lock().unwrap() = Some(path);
                }
                Err(e) => ui.set_clip_status(e.to_string().into()),
            }
        });
    });
}

/// Wire the clip page into the window. Returns the polling timer (keep it alive).
///
/// `file_for_job` finds a finished download's file; `enqueue` sends a
/// section download (link mode) to the download queue.
pub fn install(
    ui: &AppWindow,
    file_for_job: impl Fn(u64) -> Option<PathBuf> + 'static,
    // One download, or several as one queue card titled by the string.
    enqueue: impl Fn(Vec<DownloadRequest>, String) + 'static,
) -> Result<(Rc<RefCell<Clipper>>, slint::Timer)> {
    let clipper = Rc::new(RefCell::new(Clipper::default()));

    ui.global::<Fmt>().on_time(|t| format_time(t).into());
    crate::timeline_ui::install(ui);
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
            // Watching: play the whole thing, whatever In/Out say.
            let (a, b) = if ui.global::<Watch>().get_on() {
                (0.0, 0.0)
            } else {
                (ui.get_clip_in() as f64, ui.get_clip_out() as f64)
            };
            clipper.borrow_mut().toggle_play(a, b)
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

    let enqueue: Rc<dyn Fn(Vec<DownloadRequest>, String)> = Rc::new(enqueue);

    ui.on_clip_export({
        let (clipper, weak, enqueue) = (clipper.clone(), ui.as_weak(), enqueue.clone());
        move || {
            let ui = weak.unwrap();
            let mut c = clipper.borrow_mut();
            if let Some(mpv) = &c.mpv {
                let _ = mpv.set_property("pause", "yes");
            }
            let clips = crate::clips_ui::clips();
            let input = match c.source.clone() {
                Some(Source::File(path)) => path,
                Some(Source::Link(url)) => {
                    let reqs = if clips.is_empty() {
                        link_request(&ui, url).map(|r| vec![r])
                    } else {
                        link_requests(&ui, url, &clips)
                    };
                    let Some(reqs) = reqs else { return };
                    let n = reqs.len();
                    enqueue(reqs, format!("{} · {n} clips", ui.get_clip_name()));
                    ui.set_clip_status(
                        if n > 1 {
                            format!("{n} clips added to the download queue")
                        } else {
                            "Added to the download queue".into()
                        }
                        .into(),
                    );
                    return;
                }
                None => return,
            };
            let (start, end) = (ui.get_clip_in() as f64, ui.get_clip_out() as f64);
            let picture = crate::picture_ui::picture(&ui);
            // A .gif comes out as a GIF again, as it was (size, speed, loop).
            if is_gif_file(&input) {
                let spec = gif::GifSpec { kind: gif::Kind::Gif, width: 0, fps: 0.0, loop_forever: true, picture };
                let ranges: Vec<(f64, f64)> = if clips.is_empty() {
                    vec![(start, end)]
                } else {
                    clips.iter().map(|c| (c.start, c.end)).collect()
                };
                start_export(&ui, &mut c, "GIF saved", move |cancel, progress| {
                    let mut last = Err(anyhow!("nothing to save"));
                    let n = ranges.len() as f32;
                    for (i, (a, b)) in ranges.iter().enumerate() {
                        last = gif::make(&input, *a, *b, &spec, cancel, |p| progress((i as f32 + p) / n));
                        last.as_ref().map_err(|e| anyhow!("{e}"))?;
                    }
                    last
                });
                return;
            }
            let make = |start: f64, end: f64, name: Option<String>| {
                let mut req = ClipRequest::new(
                    input.clone(),
                    start,
                    end,
                    if ui.get_clip_mode() == 0 { ClipMode::Fast } else { ClipMode::Exact },
                    ui.get_clip_has_video(),
                );
                req.audio = crate::sound_ui::edit(&ui);
                req.audio_only = crate::sound_ui::audio_only(&ui);
                req.picture = picture;
                req.name = name;
                req
            };
            if clips.is_empty() {
                let req = make(start, end, None);
                start_export(&ui, &mut c, "Clip saved", move |cancel, progress| clip::export(&req, cancel, progress));
                return;
            }
            let reqs: Vec<ClipRequest> = clips.iter().map(|k| make(k.start, k.end, Some(k.name.clone()))).collect();
            let join = ui.global::<ClipList>().get_join() && reqs.len() > 1;
            let title = if join { "Clips joined".to_string() } else { format!("{} clips saved", reqs.len()) };
            start_export(&ui, &mut c, &title, move |cancel, progress| {
                let made = clip::export_many(&reqs, join, cancel, progress)?;
                // The last one is the one "Show file" opens (they're side by side).
                made.last().cloned().ok_or_else(|| anyhow!("nothing was saved"))
            });
        }
    });

    // Clip page → Picture: the player shows each change (turned while the frame is up, the
    // finished picture after Done).
    let picture = ui.global::<ClipPicture>();
    picture.on_changed({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            crate::picture_ui::refresh(&ui);
            crate::clips_ui::show(&ui);
            clipper.borrow().apply_sound(&ui);
        }
    });
    picture.on_aspect_picked({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            let (w, h) = clipper.borrow().source_size();
            crate::picture_ui::aspect_picked(&ui, w, h);
            clipper.borrow().apply_sound(&ui);
        }
    });
    picture.on_reset({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            crate::picture_ui::reset(&ui);
            clipper.borrow().apply_sound(&ui);
        }
    });
    crate::clips_ui::install(ui);

    // A live stream: save the last bit, go back to live, keep the last few minutes.
    let live = ui.global::<ClipLive>();
    live.on_last({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move |secs| {
            let ui = weak.unwrap();
            let secs = secs as f64;
            let mut c = clipper.borrow_mut();
            if let Some(b) = &c.buffer {
                // From the replay buffer, into the Videos folder (off the UI thread).
                let pieces = b.dir.clone();
                let title = ui.get_clip_name().to_string();
                ui.set_clip_status("Saving…".into());
                start_export(&ui, &mut c, "Live clip saved", move |_cancel, progress| {
                    let dir = neria_core::folders::for_kind(
                        &neria_core::settings::Settings::load(),
                        neria_core::folders::Kind::Video,
                    );
                    let out = neria_core::live::clip_last_in(&pieces, secs, &dir, &title);
                    progress(1.0);
                    out
                });
                return;
            }
            // Rewound from the site's window: the last `secs` up to the live edge, taken from
            // the clock (mpv's length can stall while paused; the anchor can't).
            let edge = LIVE_ANCHOR
                .with(|a| a.get())
                .map_or(ui.get_clip_duration() as f64, |anchor| neria_core::live::now() - anchor);
            ui.set_clip_in((edge - secs).max(0.0) as f32);
            ui.set_clip_out(edge as f32);
            drop(c);
            ui.invoke_clip_export();
        }
    });
    live.on_go_live({
        let clipper = clipper.clone();
        move || {
            if let Some(mpv) = &clipper.borrow().mpv {
                let _ = mpv.command(&["seek", "100", "absolute-percent"]);
                let _ = mpv.set_property("pause", "no");
            }
        }
    });
    live.on_buffer_start({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            let minutes = [5, 10, 30, 60][ui.global::<ClipLive>().get_keep().clamp(0, 3) as usize];
            clipper.borrow_mut().start_buffer(&ui, minutes);
        }
    });
    live.on_buffer_stop({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            clipper.borrow_mut().buffer = None;
            ui.global::<ClipLive>().set_buffering(false);
            ui.global::<ClipLive>().set_buffer_text("".into());
        }
    });

    // Export ▾ → Make a GIF → Make: from the file, or the link's selection after downloading it.
    let clip_gif = ui.global::<ClipGif>();
    clip_gif.on_make({
        let (clipper, weak, enqueue) = (clipper.clone(), ui.as_weak(), enqueue.clone());
        move || {
            let ui = weak.unwrap();
            let spec = crate::gif_ui::spec(&ui);
            ui.global::<ClipGif>().set_open(false);
            let mut c = clipper.borrow_mut();
            if let Some(mpv) = &c.mpv {
                let _ = mpv.set_property("pause", "yes");
            }
            match c.source.clone() {
                Some(Source::File(input)) => {
                    let (start, end) = (ui.get_clip_in() as f64, ui.get_clip_out() as f64);
                    let title = format!("{} saved", gif::what(spec.kind));
                    start_export(&ui, &mut c, &title, move |cancel, progress| {
                        gif::make(&input, start, end, &spec, cancel, progress)
                    });
                }
                Some(Source::Link(url)) => {
                    let Some(mut req) = link_request(&ui, url) else { return };
                    // The picture is what counts: best video, no sound changes.
                    req.quality = Quality::Best;
                    req.preset = neria_core::settings::Preset::Best;
                    req.audio_edit = None;
                    // The GIF gets the picture changes while it's made.
                    req.picture = None;
                    req.gif_spec = Some(spec);
                    enqueue(vec![req], String::new());
                    ui.set_clip_status(
                        format!("Added to the download queue, then made into a {}", gif::what(spec.kind)).into(),
                    );
                }
                None => {}
            }
        }
    });
    clip_gif.on_changed({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            let file = match &clipper.borrow().source {
                Some(Source::File(p)) => Some(p.clone()),
                _ => None,
            };
            crate::gif_ui::refresh(&ui, file);
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

    let watch = ui.global::<Watch>();
    watch.on_start({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            let Some(url) = crate::link_of(&ui.get_url()) else { return };
            let title = if ui.get_probe_state() == 2 { ui.get_probe_title().to_string() } else { String::new() };
            ui.global::<Watch>().set_recording_id(0);
            clipper.borrow_mut().watch(&ui, &url, &title);
        }
    });
    // A recording's row: watch its stream (live) while it records.
    ui.global::<crate::QueueActions>().on_watch({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move |id| {
            let ui = weak.unwrap();
            let Some((url, title)) = crate::live_ui::link_of(id as u64) else { return };
            ui.global::<Watch>().set_recording_id(id);
            clipper.borrow_mut().watch(&ui, &url, &title);
        }
    });
    watch.on_back({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            clipper.borrow_mut().unwatch(&ui);
            ui.global::<Watch>().set_recording_id(0);
            ui.set_page(0);
        }
    });
    // Watching a live stream: record it from now, right here (the bar follows the recording).
    watch.on_record({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            let mut c = clipper.borrow_mut();
            let Some(Source::Link(url)) = c.source.clone() else { return };
            let title = ui.get_clip_name().to_string();
            let Some(id) = crate::live_ui::record_now(&ui, &url, &title) else { return };
            // The recording takes over from the replay buffer (it has the same, and more).
            c.buffer = None;
            ui.global::<ClipLive>().set_buffering(false);
            ui.global::<ClipLive>().set_buffer_text("".into());
            let w = ui.global::<Watch>();
            w.set_rec_done(false);
            w.set_rec_text("Starting the recording…".into());
            w.set_recording_id(id as i32);
        }
    });
    watch.on_show_file({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let id = ui.global::<Watch>().get_recording_id() as u64;
            if let Some(path) = crate::QUEUE.get().and_then(|q| q.file_of(id)) {
                crate::reveal(&path);
            }
        }
    });
    // The saved recording into the clipper, to cut it finer.
    watch.on_trim({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            let id = ui.global::<Watch>().get_recording_id() as u64;
            let Some(path) = crate::QUEUE.get().and_then(|q| q.file_of(id)) else { return };
            let mut c = clipper.borrow_mut();
            c.previous = None;
            c.keep_open = false;
            ui.global::<Watch>().set_recording_id(0);
            c.open(&ui, &path);
        }
    });
    watch.on_download({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            clipper.borrow_mut().unwatch(&ui);
            ui.set_page(0);
            // The preview it came from is still there: download it as if from there.
            ui.invoke_start_download();
        }
    });
    watch.on_clip_this({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            // Clipping the link now: what was open before stays closed.
            clipper.borrow_mut().previous = None;
            ui.global::<Watch>().set_on(false);
            ui.global::<Watch>().set_recording_id(0);
            clipper.borrow().apply_sound(&ui);
        }
    });
    let sound = ui.global::<ClipSound>();
    sound.on_changed({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            crate::sound_ui::refresh(&ui);
            clipper.borrow().apply_sound(&ui);
        }
    });
    sound.on_reset({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || {
            let ui = weak.unwrap();
            crate::sound_ui::reset(&ui);
            clipper.borrow().apply_sound(&ui);
        }
    });
    watch.on_volume_changed({
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        move || clipper.borrow().apply_volume(&weak.unwrap())
    });
    // Dev hook for design checks: watch this file or link as if from a preview.
    if let Ok(target) = std::env::var("NERIA_WATCH") {
        let (clipper, weak) = (clipper.clone(), ui.as_weak());
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut c = clipper.borrow_mut();
            if probe::looks_like_url(&target) {
                c.open_link(&ui, &target, "");
            } else {
                c.open(&ui, Path::new(&target));
            }
            ui.global::<Watch>().set_on(true);
        });
    }

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
