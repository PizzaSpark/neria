//! A key that works anywhere, even while a game has focus (instant replay's "save").
//!
//! Windows' own `RegisterHotKey`: no keyboard hook, nothing injected, so anti-cheat has
//! nothing to object to. The key is Neria's while registered (the game doesn't get it), so
//! the default is one games rarely use. Fails if another program has it (Medal, ShadowPlay).

use std::sync::mpsc;

use anyhow::{Result, bail};

/// A key and its modifiers, from text like `F8`, `Alt+F10`, `Ctrl+Shift+S`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Windows' virtual key code.
    pub vk: u32,
}

/// Instant replay's default: F8 (Medal's; games rarely use it).
pub const DEFAULT: &str = "F8";

impl Hotkey {
    pub fn parse(text: &str) -> Result<Hotkey> {
        let mut k = Hotkey { ctrl: false, alt: false, shift: false, win: false, vk: 0 };
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => k.ctrl = true,
                "alt" => k.alt = true,
                "shift" => k.shift = true,
                "win" | "windows" => k.win = true,
                key => {
                    if k.vk != 0 {
                        bail!("\"{text}\": one key besides Ctrl, Alt, Shift and Win");
                    }
                    k.vk = vk_of(key).ok_or_else(|| anyhow::anyhow!("Unknown key \"{part}\""))?;
                }
            }
        }
        if k.vk == 0 {
            bail!("\"{text}\" has no key");
        }
        Ok(k)
    }
}

fn vk_of(key: &str) -> Option<u32> {
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x6F + n);
    }
    if let Some(n) = key.strip_prefix("num").and_then(|n| n.parse::<u32>().ok()) {
        return (n <= 9).then_some(0x60 + n);
    }
    let mut c = key.chars();
    if let (Some(ch), None) = (c.next(), c.next()) {
        if ch.is_ascii_alphanumeric() {
            return Some(ch.to_ascii_uppercase() as u32);
        }
    }
    Some(match key {
        "insert" | "ins" => 0x2D,
        "delete" | "del" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "printscreen" | "prtsc" => 0x2C,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        _ => return None,
    })
}

/// Listening for a hotkey; dropping it stops.
pub struct Listener {
    #[cfg(windows)]
    thread_id: u32,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Call `on_press` (on the listener's thread) each time `key` is pressed, anywhere.
pub fn listen(key: Hotkey, on_press: impl Fn() + Send + 'static) -> Result<Listener> {
    imp::listen(key, Box::new(on_press))
}

/// A short sound saying "saved", for when the screen is a game (Windows' own sound).
pub fn confirm_sound() {
    imp::confirm_sound();
}

impl Drop for Listener {
    fn drop(&mut self) {
        imp::stop(self);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, PostThreadMessageW, WM_HOTKEY, WM_QUIT};

    pub fn listen(key: Hotkey, on_press: Box<dyn Fn() + Send>) -> Result<Listener> {
        // The key belongs to the thread that registers it: register there, say how it went.
        let (tx, rx) = mpsc::channel::<std::result::Result<u32, String>>();
        let thread = std::thread::spawn(move || unsafe {
            let mut mods = MOD_NOREPEAT;
            if key.ctrl {
                mods |= MOD_CONTROL;
            }
            if key.alt {
                mods |= MOD_ALT;
            }
            if key.shift {
                mods |= MOD_SHIFT;
            }
            if key.win {
                mods |= MOD_WIN;
            }
            if RegisterHotKey(None, 1, mods, key.vk).is_err() {
                let _ = tx.send(Err("Another program already uses that key".into()));
                return;
            }
            let _ = tx.send(Ok(GetCurrentThreadId()));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_HOTKEY {
                    on_press();
                }
            }
            let _ = UnregisterHotKey(None, 1);
        });
        match rx.recv() {
            Ok(Ok(thread_id)) => Ok(Listener { thread_id, thread: Some(thread) }),
            Ok(Err(e)) => bail!(e),
            Err(_) => bail!("The hotkey couldn't start"),
        }
    }

    pub fn stop(l: &Listener) {
        unsafe {
            let _ = PostThreadMessageW(l.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }

    pub fn confirm_sound() {
        let file = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()) + r"\Media\Speech On.wav";
        let wide: Vec<u16> = file.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let _ = PlaySoundW(windows_core::PCWSTR(wide.as_ptr()), None, SND_FILENAME | SND_ASYNC | SND_NODEFAULT);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn listen(_: Hotkey, _: Box<dyn Fn() + Send>) -> Result<Listener> {
        bail!("Hotkeys work on Windows only")
    }
    pub fn stop(_: &Listener) {}
    pub fn confirm_sound() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_keys() {
        assert_eq!(Hotkey::parse("F8").unwrap().vk, 0x77);
        let k = Hotkey::parse("Alt + F10").unwrap();
        assert!(k.alt && !k.ctrl && k.vk == 0x79);
        let k = Hotkey::parse("ctrl+shift+s").unwrap();
        assert!(k.ctrl && k.shift && k.vk == b'S' as u32);
        assert_eq!(Hotkey::parse("Num5").unwrap().vk, 0x65);
        assert!(Hotkey::parse("Ctrl").is_err());
        assert!(Hotkey::parse("F8+F9").is_err());
        assert!(Hotkey::parse("Banana").is_err());
    }
}
