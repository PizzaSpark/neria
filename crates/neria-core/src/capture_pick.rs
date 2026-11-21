//! What instant replay can record, by name: the monitors ("DELL U2720Q · Main screen ·
//! 2560 × 1440", in the order `ddagrab` numbers them) and the programs with a window open now,
//! games first (`capture::Source`).
//!
//! A game is a program whose window covers a whole screen (fullscreen or borderless) or that
//! lives in a games folder (Steam, Epic, Riot, Battle.net, Xbox, EA, Ubisoft, GOG).

/// One monitor, as `ddagrab` sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    /// Its place in the list (`capture::Source::Monitor`): every graphics chip's screens in
    /// turn (a laptop's own screen is often on the Intel one, a plugged-in one on NVIDIA).
    pub index: u32,
    /// The graphics chip it's on (`-init_hw_device d3d11va:N`) and `ddagrab`'s `output_idx`.
    pub adapter: u32,
    pub output: u32,
    /// The monitor's own name ("DELL U2720Q"), "Built-in screen", else "Screen 2".
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Windows' main screen (top-left at 0, 0).
    pub main: bool,
    /// Windows' handle for it (`HMONITOR`), to find the screen a window is on.
    pub handle: isize,
}

impl Monitor {
    /// "Main screen · 2560 × 1440".
    pub fn detail(&self) -> String {
        let place = if self.main { "Main screen" } else { "Another screen" };
        format!("{place} · {} × {}", self.width, self.height)
    }
}

/// Who made a graphics chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

/// A graphics chip (`-init_hw_device d3d11va:N`).
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub index: u32,
    pub vendor: Vendor,
    /// "NVIDIA GeForce RTX 3060 Laptop GPU".
    pub name: String,
}

/// The real graphics chips, in Windows' order (Windows' own software one left out).
pub fn chips() -> Vec<Chip> {
    imp::chips()
}

/// A program with a window open now.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    /// Its file name (`VALORANT-Win64-Shipping.exe`), what `gfxcapture` looks for.
    pub exe: String,
    /// Its window's title ("VALORANT"), else the file name without `.exe`.
    pub name: String,
    pub game: bool,
}

/// Folders games install into (lowercase, `\` separated).
const GAME_DIRS: [&str; 10] = [
    "\\steamapps\\",
    "\\epic games\\",
    "\\riot games\\",
    "\\battle.net\\",
    "\\xboxgames\\",
    "\\windowsapps\\microsoft.minecraft",
    "\\ea games\\",
    "\\ubisoft game launcher\\games\\",
    "\\gog galaxy\\games\\",
    "\\games\\",
];

/// Programs that have windows but are never what you'd record.
const SKIP: [&str; 9] = [
    "explorer.exe",
    "applicationframehost.exe",
    "textinputhost.exe",
    "systemsettings.exe",
    "shellexperiencehost.exe",
    "startmenuexperiencehost.exe",
    "searchhost.exe",
    "lockapp.exe",
    "neria.exe",
];

pub fn in_games_folder(path: &str) -> bool {
    let p = path.to_ascii_lowercase().replace('/', "\\");
    GAME_DIRS.iter().any(|d| p.contains(d))
}

/// A window title fit for a list: trimmed, and "VALORANT  " → "VALORANT".
fn tidy(title: &str, exe: &str) -> String {
    let t = title.trim();
    if t.is_empty() {
        exe.strip_suffix(".exe").or_else(|| exe.strip_suffix(".EXE")).unwrap_or(exe).to_string()
    } else {
        t.to_string()
    }
}

/// Programs with a visible window now: games first, then by name; one entry per program (its
/// biggest window's title).
pub fn programs() -> Vec<Program> {
    let mut list: Vec<(Program, i64)> = Vec::new();
    for w in imp::windows() {
        let lower = w.exe.to_ascii_lowercase();
        if SKIP.contains(&lower.as_str()) || w.pid == std::process::id() {
            continue;
        }
        let game = w.covers_screen || in_games_folder(&w.path);
        let p = Program { exe: w.exe.clone(), name: tidy(&w.title, &w.exe), game };
        match list.iter_mut().find(|(q, _)| q.exe.eq_ignore_ascii_case(&p.exe)) {
            Some((q, area)) => {
                q.game |= p.game;
                if w.area > *area {
                    q.name = p.name;
                    *area = w.area;
                }
            }
            None => list.push((p, w.area)),
        }
    }
    let mut out: Vec<Program> = list.into_iter().map(|(p, _)| p).collect();
    out.sort_by(|a, b| b.game.cmp(&a.game).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// What's in front now, for Automatic: the game (if the window in front is one) and the
/// monitor that window is on (`Monitor::index`).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Front {
    pub game: Option<Program>,
    pub monitor: Option<u32>,
}

pub fn front() -> Front {
    imp::front()
}

/// Whether `exe` has a visible window now (a game waited for has opened).
pub fn window_open(exe: &str) -> bool {
    imp::windows().iter().any(|w| w.exe.eq_ignore_ascii_case(exe))
}

/// The monitors `ddagrab` can record, in its order.
pub fn monitors() -> Vec<Monitor> {
    imp::monitors()
}

struct Win {
    exe: String,
    path: String,
    title: String,
    pid: u32,
    area: i64,
    covers_screen: bool,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::HashMap;
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
        DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
        GetWindowThreadProcessId, IsWindowVisible, WS_EX_TOOLWINDOW,
    };
    use windows_core::{BOOL, PWSTR};

    fn wide(s: &[u16]) -> String {
        let len = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        String::from_utf16_lossy(&s[..len])
    }

    fn exe_path(pid: u32) -> Option<String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = CloseHandle(h);
            ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        let out = unsafe { &mut *(lp.0 as *mut Vec<HWND>) };
        out.push(hwnd);
        BOOL(1)
    }

    pub fn front() -> Front {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        let _real = RealPixels::new();
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            return Front::default();
        }
        let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) };
        let monitor = monitors().into_iter().find(|m| m.handle == mon.0 as isize).map(|m| m.index);
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let game = (pid != std::process::id())
            .then(|| windows().into_iter().find(|w| w.pid == pid && w.covers_screen || w.pid == pid && in_games_folder(&w.path)))
            .flatten()
            .filter(|w| !SKIP.contains(&w.exe.to_ascii_lowercase().as_str()))
            .map(|w| Program { name: tidy(&w.title, &w.exe), exe: w.exe, game: true });
        Front { game, monitor }
    }

    pub fn windows() -> Vec<Win> {
        let _real = RealPixels::new();
        let mut hwnds: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut hwnds as *mut _ as isize));
        }
        let mut paths: HashMap<u32, Option<String>> = HashMap::new();
        let mut out = Vec::new();
        for hwnd in hwnds {
            unsafe {
                if !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
                    continue;
                }
                if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
                    continue;
                }
                let mut cloaked = 0u32;
                let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
                if cloaked != 0 {
                    continue;
                }
                let mut title = [0u16; 256];
                let n = GetWindowTextW(hwnd, &mut title);
                if n <= 0 {
                    continue;
                }
                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_err() {
                    continue;
                }
                let (w, h) = ((rect.right - rect.left) as i64, (rect.bottom - rect.top) as i64);
                if w < 120 || h < 80 {
                    continue;
                }
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                let Some(path) = paths.entry(pid).or_insert_with(|| exe_path(pid)).clone() else { continue };
                let exe = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
                let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
                let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                let covers_screen = !mon.is_invalid()
                    && GetMonitorInfoW(mon, &mut info).as_bool()
                    && rect.left <= info.rcMonitor.left
                    && rect.top <= info.rcMonitor.top
                    && rect.right >= info.rcMonitor.right
                    && rect.bottom >= info.rcMonitor.bottom;
                out.push(Win { exe, path, title: wide(&title[..n as usize]), pid, area: w * h, covers_screen });
            }
        }
        out
    }

    /// `\\.\DISPLAY1` → "DELL U2720Q", from Windows' display paths.
    fn friendly_names() -> HashMap<String, String> {
        use windows::Win32::Devices::Display::{
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
        };
        let mut names = HashMap::new();
        unsafe {
            let (mut np, mut nm) = (0u32, 0u32);
            if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).is_err() {
                return names;
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None)
                .is_err()
            {
                return names;
            }
            for p in paths.iter().take(np as usize) {
                let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
                src.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
                src.header.adapterId = p.sourceInfo.adapterId;
                src.header.id = p.sourceInfo.id;
                if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                    continue;
                }
                let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
                tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
                tgt.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
                tgt.header.adapterId = p.targetInfo.adapterId;
                tgt.header.id = p.targetInfo.id;
                if DisplayConfigGetDeviceInfo(&mut tgt.header) != 0 {
                    continue;
                }
                let name = wide(&tgt.monitorFriendlyDeviceName);
                let built_in = [
                    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
                    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
                    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
                    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
                ]
                .contains(&tgt.outputTechnology);
                let name = if !name.trim().is_empty() {
                    name.trim().to_string()
                } else if built_in {
                    "Built-in screen".into()
                } else {
                    continue;
                };
                names.insert(wide(&src.viewGdiDeviceName), name);
            }
        }
        names
    }

    /// Coordinates in real pixels whatever the process says about its DPI; back after.
    struct RealPixels(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);
    impl RealPixels {
        fn new() -> RealPixels {
            use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
            RealPixels(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
        }
    }
    impl Drop for RealPixels {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(self.0);
                }
            }
        }
    }

    pub fn chips() -> Vec<Chip> {
        let mut out = Vec::new();
        unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
            let mut a = 0u32;
            while let Ok(adapter) = factory.EnumAdapters1(a) {
                if let Ok(d) = adapter.GetDesc1() {
                    let vendor = match d.VendorId {
                        0x10DE => Vendor::Nvidia,
                        0x1002 | 0x1022 => Vendor::Amd,
                        0x8086 => Vendor::Intel,
                        _ => Vendor::Other,
                    };
                    // 0x1414: Microsoft's Basic Render Driver (no chip).
                    if d.VendorId != 0x1414 {
                        out.push(Chip { index: a, vendor, name: wide(&d.Description) });
                    }
                }
                a += 1;
            }
        }
        out
    }

    pub fn monitors() -> Vec<Monitor> {
        let _real = RealPixels::new();
        let names = friendly_names();
        let mut out: Vec<Monitor> = Vec::new();
        unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
            // Each graphics chip's screens, numbered as ddagrab numbers them on that chip.
            let mut a = 0u32;
            while let Ok(adapter) = factory.EnumAdapters1(a) {
                let mut i = 0u32;
                while let Ok(output) = adapter.EnumOutputs(i) {
                    if let Ok(d) = output.GetDesc() {
                        let r = d.DesktopCoordinates;
                        let gdi = wide(&d.DeviceName);
                        let n = out.len() as u32;
                        let name = names.get(&gdi).cloned().unwrap_or_else(|| format!("Screen {}", n + 1));
                        out.push(Monitor {
                            handle: d.Monitor.0 as isize,
                            index: n,
                            adapter: a,
                            output: i,
                            name,
                            width: (r.right - r.left).max(0) as u32,
                            height: (r.bottom - r.top).max(0) as u32,
                            main: r.left == 0 && r.top == 0,
                        });
                    }
                    i += 1;
                }
                a += 1;
            }
        }
        // Two of the same model: tell them apart.
        let dupes: Vec<String> = out
            .iter()
            .filter(|m| out.iter().filter(|o| o.name == m.name).count() > 1)
            .map(|m| m.name.clone())
            .collect();
        let mut seen: HashMap<String, u32> = HashMap::new();
        for m in out.iter_mut() {
            if dupes.contains(&m.name) {
                let n = seen.entry(m.name.clone()).or_insert(0);
                *n += 1;
                m.name = format!("{} ({})", m.name, n);
            }
        }
        out
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn windows() -> Vec<Win> {
        Vec::new()
    }
    pub fn monitors() -> Vec<Monitor> {
        Vec::new()
    }
    pub fn front() -> Front {
        Front::default()
    }
    pub fn chips() -> Vec<Chip> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_by_folder() {
        assert!(in_games_folder(r"C:\Riot Games\VALORANT\live\ShooterGame\Binaries\Win64\VALORANT-Win64-Shipping.exe"));
        assert!(in_games_folder(r"D:\SteamLibrary\steamapps\common\Apex Legends\r5apex.exe"));
        assert!(!in_games_folder(r"C:\Program Files\Google\Chrome\Application\chrome.exe"));
    }

    #[test]
    fn names_from_titles() {
        assert_eq!(tidy("  VALORANT  ", "VALORANT-Win64-Shipping.exe"), "VALORANT");
        assert_eq!(tidy("", "r5apex.exe"), "r5apex");
    }

    #[test]
    fn lists_without_crashing() {
        // On a desktop with a screen: at least one monitor; and never Neria itself.
        let _ = monitors();
        assert!(programs().iter().all(|p| !p.exe.eq_ignore_ascii_case("neria.exe")));
    }
}
