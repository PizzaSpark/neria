//! Minimal libmpv binding.
//!
//! libmpv is loaded at runtime with `libloading`, so the app builds without mpv
//! headers or import libraries and still starts before the DLL is downloaded.
//! Only what Neria needs is bound: commands, a few properties, and the OpenGL
//! render API (mpv draws into an FBO we own, which the UI shows as a texture).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use libloading::Library;

pub mod gl_loader;

const FORMAT_STRING: c_int = 1;
const FORMAT_FLAG: c_int = 3;
const FORMAT_INT64: c_int = 4;
const FORMAT_DOUBLE: c_int = 5;

const RENDER_PARAM_INVALID: c_int = 0;
const RENDER_PARAM_API_TYPE: c_int = 1;
const RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const RENDER_PARAM_OPENGL_FBO: c_int = 3;
const RENDER_PARAM_FLIP_Y: c_int = 4;

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct OpenGlInitParams {
    get_proc_address: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
    get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
struct OpenGlFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

type UpdateFn = unsafe extern "C" fn(*mut c_void);

struct Api {
    _lib: Library,
    create: unsafe extern "C" fn() -> *mut c_void,
    initialize: unsafe extern "C" fn(*mut c_void) -> c_int,
    terminate_destroy: unsafe extern "C" fn(*mut c_void),
    set_option_string: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    command: unsafe extern "C" fn(*mut c_void, *mut *const c_char) -> c_int,
    get_property: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
    set_property_string: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    wait_event: unsafe extern "C" fn(*mut c_void, f64) -> *mut MpvEvent,
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    free: unsafe extern "C" fn(*mut c_void),
    render_create: unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *mut RenderParam) -> c_int,
    render_render: unsafe extern "C" fn(*mut c_void, *mut RenderParam) -> c_int,
    render_set_update_callback: unsafe extern "C" fn(*mut c_void, Option<UpdateFn>, *mut c_void),
    render_update: unsafe extern "C" fn(*mut c_void) -> u64,
    render_free: unsafe extern "C" fn(*mut c_void),
}

impl Api {
    fn load(path: &Path) -> Result<Self> {
        unsafe {
            let lib = Library::new(path).map_err(|e| anyhow!("can't load {}: {e}", path.display()))?;
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get(concat!($name, "\0").as_bytes()).map_err(|e| anyhow!("{}: {e}", $name))?
                };
            }
            Ok(Self {
                create: sym!("mpv_create"),
                initialize: sym!("mpv_initialize"),
                terminate_destroy: sym!("mpv_terminate_destroy"),
                set_option_string: sym!("mpv_set_option_string"),
                command: sym!("mpv_command"),
                get_property: sym!("mpv_get_property"),
                set_property_string: sym!("mpv_set_property_string"),
                wait_event: sym!("mpv_wait_event"),
                error_string: sym!("mpv_error_string"),
                free: sym!("mpv_free"),
                render_create: sym!("mpv_render_context_create"),
                render_render: sym!("mpv_render_context_render"),
                render_set_update_callback: sym!("mpv_render_context_set_update_callback"),
                render_update: sym!("mpv_render_context_update"),
                render_free: sym!("mpv_render_context_free"),
                _lib: lib,
            })
        }
    }

    fn check(&self, code: c_int, what: &str) -> Result<()> {
        if code >= 0 {
            return Ok(());
        }
        let msg = unsafe { CStr::from_ptr((self.error_string)(code)) }.to_string_lossy();
        bail!("{what}: {msg}")
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).expect("no interior NUL")
}

/// An mpv instance. Not thread-safe by design: use it from the UI thread.
pub struct Mpv {
    api: Arc<Api>,
    handle: *mut c_void,
}

impl Mpv {
    /// Load libmpv from `dll` and create a player suited to embedding.
    /// `extra` options are applied before initialization (e.g. script options).
    pub fn new(dll: &Path, extra: &[(&str, &str)]) -> Result<Self> {
        let api = Arc::new(Api::load(dll)?);
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            bail!("mpv_create failed");
        }
        let mpv = Self { api, handle };
        for (k, v) in [
            ("vo", "libmpv"),
            ("hwdec", "auto-safe"),
            ("keep-open", "yes"),
            ("idle", "yes"),
            ("pause", "yes"),
            ("hr-seek", "yes"),
            ("hr-seek-framedrop", "no"),
            ("osd-level", "0"),
            ("input-default-bindings", "no"),
            ("config", "no"),
            ("terminal", "no"),
            ("audio-display", "no"),
        ] {
            mpv.set_option(k, v)?;
        }
        for (k, v) in extra {
            mpv.set_option(k, v)?;
        }
        mpv.api.check(unsafe { (mpv.api.initialize)(mpv.handle) }, "mpv_initialize")?;
        Ok(mpv)
    }

    pub fn set_option(&self, name: &str, value: &str) -> Result<()> {
        let (n, v) = (cstr(name), cstr(value));
        self.api.check(unsafe { (self.api.set_option_string)(self.handle, n.as_ptr(), v.as_ptr()) }, name)
    }

    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(null());
        self.api.check(unsafe { (self.api.command)(self.handle, ptrs.as_mut_ptr()) }, args.first().unwrap_or(&""))
    }

    pub fn set_property(&self, name: &str, value: &str) -> Result<()> {
        let (n, v) = (cstr(name), cstr(value));
        self.api.check(unsafe { (self.api.set_property_string)(self.handle, n.as_ptr(), v.as_ptr()) }, name)
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        let n = cstr(name);
        let mut v = 0f64;
        let r = unsafe { (self.api.get_property)(self.handle, n.as_ptr(), FORMAT_DOUBLE, &mut v as *mut f64 as *mut c_void) };
        (r >= 0).then_some(v)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        let n = cstr(name);
        let mut v = 0i64;
        let r = unsafe { (self.api.get_property)(self.handle, n.as_ptr(), FORMAT_INT64, &mut v as *mut i64 as *mut c_void) };
        (r >= 0).then_some(v)
    }

    /// String property (e.g. `media-title`).
    pub fn get_string(&self, name: &str) -> Option<String> {
        let n = cstr(name);
        let mut p: *mut c_char = null_mut();
        let r = unsafe { (self.api.get_property)(self.handle, n.as_ptr(), FORMAT_STRING, &mut p as *mut _ as *mut c_void) };
        if r < 0 || p.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { (self.api.free)(p as *mut c_void) };
        Some(s)
    }

    pub fn get_flag(&self, name: &str) -> Option<bool> {
        let n = cstr(name);
        let mut v: c_int = 0;
        let r = unsafe { (self.api.get_property)(self.handle, n.as_ptr(), FORMAT_FLAG, &mut v as *mut c_int as *mut c_void) };
        (r >= 0).then_some(v != 0)
    }

    /// Discard pending events. Call regularly; we poll properties instead.
    pub fn drain_events(&self) {
        unsafe {
            loop {
                let ev = (self.api.wait_event)(self.handle, 0.0);
                if ev.is_null() || (*ev).event_id == 0 {
                    break;
                }
            }
        }
    }

    /// Create an OpenGL renderer. The GL context must be current.
    /// `on_update` is called from an mpv thread whenever a new frame is ready.
    pub fn create_gl_renderer(&self, on_update: impl Fn() + Send + Sync + 'static) -> Result<GlRenderer> {
        unsafe extern "C" fn get_proc(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
            unsafe { gl_loader::get_proc_address(CStr::from_ptr(name)) as *mut c_void }
        }
        let mut init = OpenGlInitParams { get_proc_address: Some(get_proc), get_proc_address_ctx: null_mut() };
        let api_type = c"opengl";
        let mut params = [
            RenderParam { kind: RENDER_PARAM_API_TYPE, data: api_type.as_ptr() as *mut c_void },
            RenderParam { kind: RENDER_PARAM_OPENGL_INIT_PARAMS, data: &mut init as *mut _ as *mut c_void },
            RenderParam { kind: RENDER_PARAM_INVALID, data: null_mut() },
        ];
        let mut ctx: *mut c_void = null_mut();
        self.api.check(
            unsafe { (self.api.render_create)(&mut ctx, self.handle, params.as_mut_ptr()) },
            "mpv_render_context_create",
        )?;

        unsafe extern "C" fn trampoline(data: *mut c_void) {
            let cb = unsafe { &*(data as *const Box<dyn Fn() + Send + Sync>) };
            cb();
        }
        let callback: Box<Box<dyn Fn() + Send + Sync>> = Box::new(Box::new(on_update));
        let callback = Box::into_raw(callback);
        unsafe { (self.api.render_set_update_callback)(ctx, Some(trampoline), callback as *mut c_void) };

        Ok(GlRenderer { api: self.api.clone(), ctx, callback })
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe { (self.api.terminate_destroy)(self.handle) };
    }
}

/// mpv's OpenGL renderer. Must be dropped (with the GL context current)
/// before the `Mpv` it came from.
pub struct GlRenderer {
    api: Arc<Api>,
    ctx: *mut c_void,
    callback: *mut Box<dyn Fn() + Send + Sync>,
}

impl GlRenderer {
    /// Draw the current video frame into framebuffer `fbo` of size `w`x`h`.
    /// With `flip_y == false` the image is stored bottom-up (GL convention).
    pub fn render(&self, fbo: i32, w: i32, h: i32, flip_y: bool) {
        let mut target = OpenGlFbo { fbo, w, h, internal_format: 0 };
        let mut flip: c_int = flip_y as c_int;
        let mut params = [
            RenderParam { kind: RENDER_PARAM_OPENGL_FBO, data: &mut target as *mut _ as *mut c_void },
            RenderParam { kind: RENDER_PARAM_FLIP_Y, data: &mut flip as *mut _ as *mut c_void },
            RenderParam { kind: RENDER_PARAM_INVALID, data: null_mut() },
        ];
        unsafe {
            (self.api.render_update)(self.ctx);
            (self.api.render_render)(self.ctx, params.as_mut_ptr());
        }
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        unsafe {
            (self.api.render_set_update_callback)(self.ctx, None, null_mut());
            (self.api.render_free)(self.ctx);
            drop(Box::from_raw(self.callback));
        }
    }
}
