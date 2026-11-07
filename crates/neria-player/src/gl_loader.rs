//! OpenGL function lookup that doesn't depend on the UI toolkit, so mpv can
//! resolve GL functions whenever it needs to (not just during a UI callback).

use std::ffi::{CStr, c_void};
use std::ptr::null;

#[cfg(windows)]
pub fn get_proc_address(name: &CStr) -> *const c_void {
    use std::ffi::c_char;
    use std::sync::OnceLock;

    use libloading::Library;

    type WglGetProcAddress = unsafe extern "system" fn(*const c_char) -> *const c_void;
    static GL: OnceLock<Option<(Library, WglGetProcAddress)>> = OnceLock::new();

    let Some((lib, wgl)) = GL.get_or_init(|| unsafe {
        let lib = Library::new("opengl32.dll").ok()?;
        let wgl: WglGetProcAddress = *lib.get(b"wglGetProcAddress\0").ok()?;
        Some((lib, wgl))
    }) else {
        return null();
    };

    // wglGetProcAddress only knows extension/1.2+ functions and signals
    // failure with a few magic values; core 1.1 functions live in opengl32.dll.
    let p = unsafe { wgl(name.as_ptr()) };
    if !matches!(p as isize, -1 | 0 | 1 | 2 | 3) {
        return p;
    }
    unsafe {
        lib.get::<*const c_void>(name.to_bytes_with_nul())
            .map(|s| *s)
            .unwrap_or(null())
    }
}

#[cfg(not(windows))]
pub fn get_proc_address(_name: &CStr) -> *const c_void {
    // TODO: EGL/GLX lookup for Linux and Android.
    null()
}
