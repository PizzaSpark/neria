//! Present (Capture page → Phone → "Present in a window"): the phone's screen in a window of
//! Neria's own, "Phone screen · Pixel 7". Not scrcpy's window: that was a second session on
//! the phone, pulling its sound a second time (both stuttered, and closing it handed the sound
//! back to the phone's speaker) and its picture a second time over the Wi-Fi. This one draws
//! the picture Neria already gets, with its own mpv in its own GL context, and the sound plays
//! from Neria, so sharing the window (Discord, OBS, Meet) carries it.

use std::cell::RefCell;
use std::num::NonZeroU32;

use glow::HasContext;
use neria_core::paths;
use neria_player::{GlRenderer, Mpv};
use slint::{ComponentHandle, GraphicsAPI, RenderingState};

use crate::PresentWindow;
use crate::clipper::{Target, make_target};

#[derive(Default)]
struct State {
    win: Option<PresentWindow>,
    open: bool,
    gl: Option<glow::Context>,
    mpv: Option<Mpv>,
    renderer: Option<GlRenderer>,
    target: Option<Target>,
    /// The stream to load once the renderer exists.
    pending: Option<String>,
    shown_frame: bool,
}

thread_local! {
    static P: RefCell<State> = RefCell::new(State::default());
}

pub struct Open<'a> {
    /// "Phone screen · Pixel 7"
    pub title: String,
    /// The page's raw picture stream (`tcp://127.0.0.1:…`).
    pub url: &'a str,
    pub size: (u32, u32),
    pub on_top: bool,
    /// Where to put it: next to this window.
    pub beside: &'a slint::Window,
}

/// What the window does with your input, and when it's closed by hand.
pub struct Hooks {
    pub touch: Box<dyn Fn(i32, f32, f32)>,
    pub scroll: Box<dyn Fn(f32, f32, f32)>,
    pub typed: Box<dyn Fn(&str)>,
    pub press: Box<dyn Fn(i32)>,
    pub closed: Box<dyn Fn()>,
}

pub fn is_open() -> bool {
    P.with(|p| p.borrow().open)
}

/// Show the window drawing `o.url` (made the first time, kept hidden between uses).
pub fn open(o: Open, hooks: Hooks) -> anyhow::Result<()> {
    let made = P.with(|p| p.borrow().win.is_some());
    if !made {
        let win = PresentWindow::new()?;
        let hooks = std::rc::Rc::new(hooks);
        {
            let h = hooks.clone();
            win.on_touch(move |k, x, y| (h.touch)(k, x, y));
            let h = hooks.clone();
            win.on_scroll(move |x, y, d| (h.scroll)(x, y, d));
            let h = hooks.clone();
            win.on_typed(move |t| (h.typed)(&t));
            let h = hooks.clone();
            win.on_press(move |c| (h.press)(c));
            let h = hooks.clone();
            win.window().on_close_requested(move || {
                stop_picture();
                P.with(|p| p.borrow_mut().open = false);
                (h.closed)();
                slint::CloseRequestResponse::HideWindow
            });
        }
        let weak = win.as_weak();
        win.window().set_rendering_notifier(move |state, api| match state {
            RenderingState::RenderingSetup => {
                if let GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                    let gl = unsafe { glow::Context::from_loader_function_cstr(|s| get_proc_address(s)) };
                    P.with(|p| p.borrow_mut().gl = Some(gl));
                }
            }
            RenderingState::BeforeRendering => {
                if let Some(w) = weak.upgrade() {
                    render(&w);
                }
            }
            RenderingState::RenderingTeardown => teardown(),
            _ => {}
        })?;
        P.with(|p| p.borrow_mut().win = Some(win));
    }
    let win = P.with(|p| p.borrow().win.as_ref().unwrap().clone_strong());
    win.set_name(o.title.into());
    win.set_on_top(o.on_top);
    win.set_has_frame(false);
    if o.size.0 > 0 && o.size.1 > 0 {
        win.set_frame_w(o.size.0 as i32);
        win.set_frame_h(o.size.1 as i32);
    }
    // The phone's shape, about as tall as a laptop screen lets it be; next to Neria.
    let aspect = if o.size.1 > 0 { o.size.0 as f32 / o.size.1 as f32 } else { 0.45 };
    let (w, h) = if aspect < 1.0 { (760.0 * aspect, 760.0) } else { (900.0, 900.0 / aspect) };
    win.window().set_size(slint::LogicalSize::new(w, h));
    let (pos, size) = (o.beside.position(), o.beside.size());
    win.window().set_position(slint::PhysicalPosition::new(pos.x + size.width as i32 + 16, pos.y));
    load(o.url)?;
    win.show()?;
    P.with(|p| p.borrow_mut().open = true);
    Ok(())
}

/// Hide it (Stop presenting); the picture stops.
pub fn close() {
    stop_picture();
    P.with(|p| {
        let mut p = p.borrow_mut();
        p.open = false;
        if let Some(w) = &p.win {
            let _ = w.hide();
        }
    });
}

/// In front of everything (its title is the phone's name).
pub fn bring_to_front() {
    let Some(name) = P.with(|p| p.borrow().win.as_ref().map(|w| w.get_name().to_string())) else { return };
    let title: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow};
    unsafe {
        if let Ok(h) = FindWindowW(None, windows::core::PCWSTR(title.as_ptr())) {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
    }
}

fn load(url: &str) -> anyhow::Result<()> {
    P.with(|p| {
        let mut p = p.borrow_mut();
        if p.mpv.is_none() {
            // As the page's player: a raw live stream, shown the moment it comes.
            let extra = [
                ("profile", "low-latency"),
                ("untimed", "yes"),
                ("cache", "no"),
                ("demuxer-lavf-format", "h264"),
                ("demuxer-lavf-o", "fflags=+nobuffer"),
                ("demuxer-lavf-probesize", "32"),
                ("demuxer-lavf-analyzeduration", "0.1"),
                ("video-latency-hacks", "yes"),
                ("aid", "no"),
                ("keep-open", "no"),
                ("pause", "no"),
            ];
            p.mpv = Some(Mpv::new(&paths::bin_dir().join("libmpv-2.dll"), &extra)?);
        }
        if p.renderer.is_none() {
            p.pending = Some(url.to_string());
        } else if let Some(m) = &p.mpv {
            let _ = m.command(&["loadfile", url, "replace"]);
        }
        Ok(())
    })
}

fn stop_picture() {
    P.with(|p| {
        let mut p = p.borrow_mut();
        if let Some(m) = &p.mpv {
            let _ = m.command(&["stop"]);
        }
        p.pending = None;
        p.shown_frame = false;
        if let Some(w) = &p.win {
            w.set_frame(slint::Image::default());
            w.set_has_frame(false);
        }
    });
}

/// Its rendering hook: the newest frame into the window's texture.
fn render(win: &PresentWindow) {
    P.with(|p| {
        let mut p = p.borrow_mut();
        let p = &mut *p;
        let (Some(gl), Some(mpv)) = (p.gl.as_ref(), p.mpv.as_ref()) else { return };
        if !p.open {
            return;
        }
        if p.renderer.is_none() {
            let weak = win.as_weak();
            let redraw = move || {
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_frame_tick(w.get_frame_tick().wrapping_add(1) & 0x3fff_ffff);
                        w.window().request_redraw();
                    }
                });
            };
            match mpv.create_gl_renderer(redraw) {
                Ok(r) => p.renderer = Some(r),
                Err(_) => return,
            }
            if let Some(url) = p.pending.take() {
                let _ = mpv.command(&["loadfile", &url, "replace"]);
            }
            return;
        }
        mpv.drain_events();
        let (dw, dh) = (mpv.get_i64("dwidth").unwrap_or(0), mpv.get_i64("dheight").unwrap_or(0));
        if dw <= 0 || dh <= 0 {
            return;
        }
        // At most 1600 on its long side: plenty for a shared window.
        let scale = (1600.0 / dw.max(dh) as f64).min(1.0);
        let (w, h) = (((dw as f64) * scale).round() as i32, ((dh as f64) * scale).round() as i32);
        unsafe {
            if p.target.as_ref().is_none_or(|t| t.w != w || t.h != h) {
                if let Some(old) = p.target.take() {
                    gl.delete_framebuffer(old.fbo);
                    gl.delete_texture(old.tex);
                }
                match make_target(gl, w, h) {
                    Ok(t) => {
                        p.target = Some(t);
                        p.shown_frame = false;
                    }
                    Err(_) => return,
                }
            }
            if !p.shown_frame {
                let t = p.target.as_ref().unwrap();
                let image =
                    slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(t.tex.0, [w as u32, h as u32].into())
                        .build();
                win.set_frame(image);
                win.set_has_frame(true);
                win.set_frame_w(dw as i32);
                win.set_frame_h(dh as i32);
                p.shown_frame = true;
                win.window().request_redraw();
            }
            let target = p.target.as_ref().unwrap();
            let prev_fbo = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
            let mut viewport = [0i32; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            if let Some(r) = &p.renderer {
                // Never wait for a frame's time (the raw stream's are guesses; UI thread).
                r.render_with(target.fbo.0.get() as i32, w, h, false, false);
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, NonZeroU32::new(prev_fbo as u32).map(glow::NativeFramebuffer));
            gl.viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        }
    });
}

/// Its GL context goes: the renderer and texture first, with the context still current.
fn teardown() {
    P.with(|p| {
        let mut p = p.borrow_mut();
        p.renderer = None;
        let target = p.target.take();
        if let (Some(gl), Some(t)) = (p.gl.as_ref(), target) {
            unsafe {
                gl.delete_framebuffer(t.fbo);
                gl.delete_texture(t.tex);
            }
        }
        p.gl = None;
    });
}
