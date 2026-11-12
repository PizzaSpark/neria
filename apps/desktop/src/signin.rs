//! `neria --sign-in <site> [dark|light]`: a small window with the site's own sign-in page.
//!
//! Runs as its own process (Edge WebView2 through wry, in a plain winit window), so it never
//! touches the main window's event loop. A calm Neria bar sits on top: what's happening and
//! "Done". When the site's signed-in cookies show up the bar says so. On Done (or closing
//! the window once signed in) the site's cookies, and only those, go to `cookies/<site>.txt`
//! next to the exe. Exit code 0 = saved, 2 = nothing saved.
//!
//! The browser profile (`webview/` next to the exe) stays too, so signing in again later is
//! quick; "Sign out" in Settings removes both.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use neria_core::cookies;
use slint::winit_030::winit;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebContext, WebView, WebViewBuilder};

const BAR: f64 = 52.0;

/// WebView2's profile: kept next to the exe (portable), one for every site.
pub fn profile_dir() -> std::path::PathBuf {
    neria_core::paths::app_dir().join("webview")
}

fn bar_html(name: &str, dark: bool) -> String {
    let (bg, ink, muted, line) =
        if dark { ("#141416", "#f1efe9", "#8e8c88", "#34343a") } else { ("#f4f0e6", "#151412", "#6b665c", "#cfc7b4") };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{{margin:0;height:100%;background:{bg};color:{ink};font:13px "Segoe UI",sans-serif;overflow:hidden}}
.b{{display:flex;align-items:center;gap:12px;height:100%;padding:0 12px 0 16px;box-sizing:border-box;border-bottom:1px solid {line}}}
.t{{flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
.t small{{color:{muted};font-size:12px;margin-left:6px}}
button{{font:700 13px "Segoe UI",sans-serif;height:32px;padding:0 14px;border-radius:9px;border:2px solid {line};background:transparent;color:{ink};cursor:pointer}}
button.go{{background:#ffd23f;color:#1a1405;border-color:#000;box-shadow:3px 3px 0 {sh}}}
button:focus-visible{{outline:2px solid {ink};outline-offset:3px}}
</style></head><body><div class="b">
<span id="t" class="t">Sign in to {name}.<small>Neria keeps only {name}'s cookies, on this PC.</small></span>
<button id="d" onclick="window.ipc.postMessage('done')">Done</button>
</div><script>
function signedIn(){{document.getElementById('t').innerHTML='Signed in to {name}.<small>Press Done to go back to Neria.</small>';document.getElementById('d').className='go';}}
</script></body></html>"#,
        sh = if dark { "#9a6f00" } else { "#151412" },
    )
}

struct App {
    site: String,
    name: String,
    dark: bool,
    window: Option<Window>,
    bar: Option<WebView>,
    page: Option<WebView>,
    context: Option<WebContext>,
    done: Rc<Cell<bool>>,
    signed_in: bool,
    next_check: Instant,
    saved: Rc<RefCell<bool>>,
}

impl App {
    fn layout(&self) {
        let (Some(w), Some(bar), Some(page)) = (&self.window, &self.bar, &self.page) else { return };
        let size = w.inner_size().to_logical::<f64>(w.scale_factor());
        let _ = bar.set_bounds(Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(size.width, BAR).into(),
        });
        let _ = page.set_bounds(Rect {
            position: LogicalPosition::new(0.0, BAR).into(),
            size: LogicalSize::new(size.width, (size.height - BAR).max(0.0)).into(),
        });
    }

    /// The site's cookies as Neria keeps them.
    fn site_cookies(&self) -> Vec<cookies::Cookie> {
        let Some(page) = &self.page else { return Vec::new() };
        page.cookies()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| {
                let domain = c.domain()?.to_string();
                cookies::belongs_to(&domain, &self.site).then(|| cookies::Cookie {
                    domain,
                    path: c.path().unwrap_or("/").to_string(),
                    secure: c.secure().unwrap_or(false),
                    http_only: c.http_only().unwrap_or(false),
                    expires: c.expires_datetime().map_or(0, |t| t.unix_timestamp()),
                    name: c.name().to_string(),
                    value: c.value().to_string(),
                })
            })
            .collect()
    }

    fn check_signed_in(&mut self) {
        let markers = cookies::signed_in_markers(&self.site);
        if self.signed_in || markers.is_empty() {
            return;
        }
        if self.site_cookies().iter().any(|c| markers.contains(&c.name.as_str()) && !c.value.is_empty()) {
            self.signed_in = true;
            if let Some(bar) = &self.bar {
                let _ = bar.evaluate_script("signedIn()");
            }
        }
    }

    /// Keep the cookies (signed in, or the user says they're done) and close.
    fn finish(&mut self, event_loop: &ActiveEventLoop, pressed_done: bool) {
        self.check_signed_in();
        if self.signed_in || pressed_done {
            let jar = self.site_cookies();
            *self.saved.borrow_mut() = cookies::save(&self.site, &jar).is_ok();
        }
        // Drop the webviews before the window.
        self.page = None;
        self.bar = None;
        self.window = None;
        event_loop.exit();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(format!("Sign in to {} · Neria", self.name))
            .with_inner_size(winit::dpi::LogicalSize::new(520.0, 760.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(380.0, 480.0));
        let Ok(window) = event_loop.create_window(attrs) else {
            event_loop.exit();
            return;
        };
        let context = self.context.get_or_insert_with(|| WebContext::new(Some(profile_dir().join(&self.site))));
        let done = self.done.clone();
        let bar = WebViewBuilder::new_with_web_context(context)
            .with_html(bar_html(&self.name, self.dark))
            .with_ipc_handler(move |msg| {
                if msg.body() == "done" {
                    done.set(true);
                }
            })
            .with_bounds(Rect::default())
            .build_as_child(&window);
        let page = WebViewBuilder::new_with_web_context(context)
            .with_url(cookies::sign_in_url(&self.site))
            .with_bounds(Rect::default())
            .build_as_child(&window);
        match (bar, page) {
            (Ok(bar), Ok(page)) => {
                self.bar = Some(bar);
                self.page = Some(page);
            }
            _ => {
                // No WebView2 runtime (very old Windows 10): nothing we can show.
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
        self.layout();
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(300)));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.finish(event_loop, false),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => self.layout(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            return;
        }
        if self.done.get() {
            self.finish(event_loop, true);
            return;
        }
        if Instant::now() >= self.next_check {
            self.next_check = Instant::now() + Duration::from_millis(1500);
            self.check_signed_in();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(300)));
    }
}

/// The `--sign-in` process. Returns whether cookies were saved.
pub fn run(site: &str, dark: bool) -> bool {
    let Some(site) = cookies::site_of_host(site) else { return false };
    let Ok(event_loop) = EventLoop::new() else { return false };
    let saved = Rc::new(RefCell::new(false));
    let mut app = App {
        name: cookies::site_name(&site),
        site,
        dark,
        window: None,
        bar: None,
        page: None,
        context: None,
        done: Rc::new(Cell::new(false)),
        signed_in: false,
        next_check: Instant::now() + Duration::from_secs(2),
        saved: saved.clone(),
    };
    let _ = event_loop.run_app(&mut app);
    saved.take()
}
