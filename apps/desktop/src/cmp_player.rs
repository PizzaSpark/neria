//! Compress page → the 10 s sample, played in the page: Now (the file as it is, from the same
//! moment) or After (the sample made with the page's choices), switching at the same moment
//! so the difference is easy to hear (and see). Its own mpv, drawn into a GL texture from the
//! window's rendering hook, which `clipper.rs` owns (it calls `render` / `teardown` here).

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use glow::HasContext;
use neria_core::{fmt, paths};
use neria_player::{GlRenderer, Mpv};
use slint::{ComponentHandle, Timer, TimerMode};

use crate::clipper::{Target, make_target};
use crate::{AppWindow, Compress};

/// A sample ready to play.
pub struct Sample {
    /// What it was made for (the page's preview key): other choices drop it.
    pub key: String,
    /// The original file, and where the sample starts in it.
    pub file: PathBuf,
    pub at: f64,
    pub len: f64,
    /// The sample encoded with the page's choices.
    pub after: PathBuf,
    pub has_video: bool,
}

#[derive(Default)]
struct Player {
    mpv: Option<Mpv>,
    renderer: Option<GlRenderer>,
    target: Option<Target>,
    sample: Option<Sample>,
    /// Loaded once the renderer exists (a video can't start without one): (file, start).
    pending: Option<(PathBuf, f64)>,
    /// Which one is loaded: true After, false Now.
    after: bool,
    timer: Option<Timer>,
}

thread_local! {
    static PLAYER: RefCell<Player> = RefCell::new(Player::default());
}

impl Player {
    fn ensure_mpv(&mut self) -> anyhow::Result<&Mpv> {
        if self.mpv.is_none() {
            self.mpv = Some(Mpv::new(&paths::bin_dir().join("libmpv-2.dll"), &[])?);
        }
        Ok(self.mpv.as_ref().unwrap())
    }

    /// Where the loaded one starts in its own file: the original from `at`, the sample from 0.
    fn base(&self) -> f64 {
        match &self.sample {
            Some(s) if !self.after => s.at,
            _ => 0.0,
        }
    }

    /// Load Now or After, `offset` seconds into the sample, looping the 10 s.
    fn load(&mut self, offset: f64) {
        let Some(s) = &self.sample else { return };
        let (file, base) = if self.after { (s.after.clone(), 0.0) } else { (s.file.clone(), s.at) };
        let (len, video) = (s.len, s.has_video);
        let Some(mpv) = &self.mpv else { return };
        let _ = mpv.set_property("ab-loop-a", &format!("{base:.3}"));
        let _ = mpv.set_property("ab-loop-b", &format!("{:.3}", base + len));
        if video && self.renderer.is_none() {
            self.pending = Some((file, base + offset));
            return;
        }
        let _ = mpv.set_property("start", &format!("{:.3}", base + offset));
        let _ = mpv.command(&["loadfile", &file.to_string_lossy(), "replace"]);
        let _ = mpv.set_property("pause", "no");
    }

    fn offset(&self) -> f64 {
        let pos = self.mpv.as_ref().and_then(|m| m.get_f64("time-pos")).unwrap_or(0.0);
        (pos - self.base()).max(0.0)
    }
}

/// The sample is made: play it (the side the page shows).
pub fn ready(ui: &AppWindow, sample: Sample) {
    let c = ui.global::<Compress>();
    let ok = PLAYER.with(|p| {
        let mut p = p.borrow_mut();
        if let Err(e) = p.ensure_mpv() {
            crate::show_toast(ui, &format!("Player unavailable: {e}"));
            return false;
        }
        p.after = c.get_side() == 1;
        p.sample = Some(sample);
        p.load(0.0);
        true
    });
    if !ok {
        return;
    }
    c.set_player_on(true);
    c.set_player_frame(slint::Image::default());
    start_timer(ui);
    ui.window().request_redraw();
}

/// Play / pause the sample made for `key`. False: there's none (make it first).
pub fn toggle(ui: &AppWindow, key: &str) -> bool {
    let done = PLAYER.with(|p| {
        let p = p.borrow();
        let (Some(mpv), Some(s)) = (&p.mpv, &p.sample) else { return false };
        if s.key != key {
            return false;
        }
        let paused = mpv.get_flag("pause").unwrap_or(true);
        let _ = mpv.set_property("pause", if paused { "no" } else { "yes" });
        true
    });
    if done {
        tick(ui);
    }
    done
}

/// Now ↔ After (the page's `side`), at the same moment of the sample.
pub fn switch(ui: &AppWindow) {
    let after = ui.global::<Compress>().get_side() == 1;
    PLAYER.with(|p| {
        let mut p = p.borrow_mut();
        if p.sample.is_none() || p.after == after {
            p.after = after;
            return;
        }
        let offset = p.offset();
        p.after = after;
        p.load(offset);
    });
    ui.window().request_redraw();
}

/// Stop and drop the sample (another file, the page's choices changed, compressing).
pub fn stop(ui: &AppWindow) {
    PLAYER.with(|p| {
        let mut p = p.borrow_mut();
        if let Some(mpv) = &p.mpv {
            let _ = mpv.command(&["stop"]);
        }
        p.sample = None;
        p.pending = None;
        p.timer = None;
    });
    let c = ui.global::<Compress>();
    c.set_player_on(false);
    c.set_playing(false);
    c.set_player_pos(0.0);
    c.set_player_time("".into());
}

/// The choices changed: a sample made for others goes.
pub fn stop_unless(ui: &AppWindow, key: &str) {
    let other = PLAYER.with(|p| p.borrow().sample.as_ref().is_some_and(|s| s.key != key));
    if other {
        stop(ui);
    }
}

fn start_timer(ui: &AppWindow) {
    let timer = Timer::default();
    let weak = ui.as_weak();
    timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        if let Some(ui) = weak.upgrade() {
            tick(&ui);
        }
    });
    PLAYER.with(|p| p.borrow_mut().timer = Some(timer));
}

/// Where it is in the 10 s, and whether it plays.
fn tick(ui: &AppWindow) {
    let c = ui.global::<Compress>();
    PLAYER.with(|p| {
        let p = p.borrow();
        let (Some(mpv), Some(s)) = (&p.mpv, &p.sample) else { return };
        mpv.drain_events();
        let off = p.offset().min(s.len);
        c.set_playing(!mpv.get_flag("pause").unwrap_or(true));
        c.set_player_pos((off / s.len.max(0.1)) as f32);
        c.set_player_time(format!("{} / {}", fmt::duration(off), fmt::duration(s.len)).into());
    });
}

/// From the window's rendering hook: draw the video's frame (sound files have none).
pub fn render(ui: &AppWindow, gl: &glow::Context) {
    PLAYER.with(|p| {
        let mut p = p.borrow_mut();
        if p.sample.as_ref().is_none_or(|s| !s.has_video) || p.mpv.is_none() {
            return;
        }
        if p.renderer.is_none() {
            let weak = ui.as_weak();
            let redraw = move || {
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.window().request_redraw();
                    }
                });
            };
            match p.mpv.as_ref().unwrap().create_gl_renderer(redraw) {
                Ok(r) => p.renderer = Some(r),
                Err(e) => {
                    crate::show_toast(ui, &format!("Video renderer failed: {e}"));
                    return;
                }
            }
            if let Some((file, start)) = p.pending.take() {
                let mpv = p.mpv.as_ref().unwrap();
                let _ = mpv.set_property("start", &format!("{start:.3}"));
                let _ = mpv.command(&["loadfile", &file.to_string_lossy(), "replace"]);
                let _ = mpv.set_property("pause", "no");
            }
            return;
        }
        let mpv = p.mpv.as_ref().unwrap();
        let (dw, dh) = (mpv.get_i64("dwidth").unwrap_or(0), mpv.get_i64("dheight").unwrap_or(0));
        if dw <= 0 || dh <= 0 {
            return;
        }
        // mpv turns phone footage while drawing, after `dwidth` / `dheight`.
        let turned = mpv.get_i64("video-params/rotate").unwrap_or(0).rem_euclid(180) == 90;
        let (dw, dh) = if turned { (dh, dw) } else { (dw, dh) };
        // The panel is small: 720p is plenty.
        let scale = (1280.0 / dw as f64).min(720.0 / dh as f64).min(1.0);
        let (w, h) = (((dw as f64) * scale).round() as i32, ((dh as f64) * scale).round() as i32);
        unsafe {
            if p.target.as_ref().is_none_or(|t| t.w != w || t.h != h) {
                if let Some(old) = p.target.take() {
                    gl.delete_framebuffer(old.fbo);
                    gl.delete_texture(old.tex);
                }
                match make_target(gl, w, h) {
                    Ok(t) => {
                        let image = slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                            t.tex.0,
                            [w as u32, h as u32].into(),
                        )
                        .build();
                        ui.global::<Compress>().set_player_frame(image);
                        p.target = Some(t);
                        ui.window().request_redraw();
                    }
                    Err(_) => return,
                }
            }
            let target = p.target.as_ref().unwrap();
            let prev_fbo = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
            let mut viewport = [0i32; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            if let Some(r) = &p.renderer {
                r.render(target.fbo.0.get() as i32, w, h, false);
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, NonZeroU32::new(prev_fbo as u32).map(glow::NativeFramebuffer));
            gl.viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        }
    });
}

/// The GL context goes: the renderer and the texture first (before the mpv they came from).
pub fn teardown(gl: &glow::Context) {
    PLAYER.with(|p| {
        let mut p = p.borrow_mut();
        p.renderer = None;
        if let Some(t) = p.target.take() {
            unsafe {
                gl.delete_framebuffer(t.fbo);
                gl.delete_texture(t.tex);
            }
        }
    });
}
