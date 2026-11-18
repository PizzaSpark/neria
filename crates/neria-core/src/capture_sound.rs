//! Instant replay's sound, from Windows (WASAPI): what the PC plays, one app's sound (Discord)
//! or everything but it, and a mic. Each is sent to the replay's ffmpeg as raw 48 kHz stereo
//! over a local socket, one track each.
//!
//! WASAPI sends nothing while nothing plays, so silence is filled in by the clock: every track
//! stays as long as the picture, and sound that starts later lands at the right moment.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
/// Bytes per frame (16-bit stereo).
const FRAME: usize = 4;

/// One track of sound.
#[derive(Clone, Debug, PartialEq)]
pub enum Sound {
    /// Everything the PC plays (the default output).
    Pc,
    /// Everything but one app (by its file name, `Discord.exe`) and what it starts.
    PcExcept(String),
    /// One app and what it starts.
    App(String),
    /// A mic: part of its name, or Windows' default.
    Mic(Option<String>),
}

impl Sound {
    /// The track's name in the saved file.
    pub fn title(&self) -> String {
        match self {
            Sound::Pc => "PC sound".into(),
            Sound::PcExcept(app) => format!("Game & PC (no {})", app_name(app)),
            Sound::App(app) => app_name(app),
            Sound::Mic(_) => "Mic".into(),
        }
    }
}

/// `discord.exe` → `Discord`.
fn app_name(exe: &str) -> String {
    let stem = exe.strip_suffix(".exe").or_else(|| exe.strip_suffix(".EXE")).unwrap_or(exe);
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// A track waiting for ffmpeg to connect, then sending until `stop` or ffmpeg goes.
pub struct Feed {
    pub port: u16,
    pub title: String,
    pub thread: std::thread::JoinHandle<()>,
}

/// Open `sound` now (so a missing app or mic fails before ffmpeg starts) and send it to the
/// first connection on the returned port.
pub fn feed(sound: &Sound, stop: Arc<AtomicBool>) -> Result<Feed> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let source = imp::Source::open(sound)?;
    let title = sound.title();
    let thread = std::thread::spawn(move || {
        let Some(stream) = accept(&listener, &stop) else { return };
        // An error here is ffmpeg going away (stop, restart): the run's own error says why.
        let _ = pump(source, stream, &stop);
    });
    Ok(Feed { port, title, thread })
}

/// The ffmpeg input for a feed.
pub fn input_args(port: u16) -> Vec<String> {
    [
        // The format is known: don't sit on the first seconds of sound working it out (that
        // stalls the picture at start, input after input).
        "-analyzeduration", "0", "-probesize", "32",
        "-f", "s16le", "-ar", &RATE.to_string(), "-ac", &CHANNELS.to_string(),
        "-i", &format!("tcp://127.0.0.1:{port}"),
    ]
    .map(String::from)
    .to_vec()
}

fn accept(listener: &TcpListener, stop: &AtomicBool) -> Option<TcpStream> {
    listener.set_nonblocking(true).ok()?;
    let until = Instant::now() + Duration::from_secs(15);
    while !stop.load(Ordering::SeqCst) && Instant::now() < until {
        match listener.accept() {
            Ok((s, _)) => {
                s.set_nonblocking(false).ok()?;
                let _ = s.set_nodelay(true);
                return Some(s);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    None
}

/// Read packets, fill silence by the clock, send.
fn pump(mut source: imp::Source, mut out: TcpStream, stop: &AtomicBool) -> Result<()> {
    source.start()?;
    let began = Instant::now();
    let mut sent: u64 = 0; // frames
    let mut buf = Vec::with_capacity(RATE as usize * FRAME);
    while !stop.load(Ordering::SeqCst) {
        buf.clear();
        source.read(&mut buf, Duration::from_millis(20))?;
        let due = (began.elapsed().as_secs_f64() * RATE as f64) as u64;
        let have = (buf.len() / FRAME) as u64;
        // Ahead of the clock by more than 0.2 s (a device running fast): drop the surplus.
        let keep = if sent + have > due + RATE as u64 / 5 { due.saturating_sub(sent) } else { have };
        out.write_all(&buf[..keep as usize * FRAME])?;
        sent += keep;
        // Behind by more than 0.1 s: nothing played; fill with silence up to now.
        if due > sent + RATE as u64 / 10 {
            let gap = (due - sent) as usize;
            out.write_all(&vec![0u8; gap * FRAME])?;
            sent += gap as u64;
        }
    }
    Ok(())
}

/// Sound inputs (mics), by name.
pub fn mics() -> Vec<String> {
    imp::mics().unwrap_or_default()
}

/// Is `exe` running (for "split Discord off" only when it's there).
pub fn app_running(exe: &str) -> bool {
    imp::root_pid(exe).is_some()
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::mpsc;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{
        BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, IAgileObject, STGM_READ,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::Win32::System::Variant::VT_BLOB;
    use windows_core::{Interface, implement};

    pub struct Source {
        client: IAudioClient,
        capture: IAudioCaptureClient,
        event: HANDLE,
    }

    // The COM objects are used from one thread at a time (opened here, read in the feed's).
    unsafe impl Send for Source {}

    fn com() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    fn format() -> WAVEFORMATEX {
        let align = CHANNELS * 2;
        WAVEFORMATEX {
            wFormatTag: 1, // PCM
            nChannels: CHANNELS,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * align as u32,
            nBlockAlign: align,
            wBitsPerSample: 16,
            cbSize: 0,
        }
    }

    impl Source {
        pub fn open(sound: &Sound) -> Result<Source> {
            com();
            let (client, loopback) = match sound {
                Sound::Pc => (default_device(eRender)?, true),
                Sound::Mic(name) => (mic_device(name.as_deref())?, false),
                Sound::App(exe) => (app_client(exe, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE)?, true),
                Sound::PcExcept(exe) => (app_client(exe, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE)?, true),
            };
            let process = matches!(sound, Sound::App(_) | Sound::PcExcept(_));
            let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
            if loopback {
                flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
            }
            if !process {
                // Devices have their own format; Windows converts to ours.
                flags |= AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            }
            let fmt = format();
            unsafe {
                client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &fmt, None)?;
                let event = CreateEventW(None, false, false, None)?;
                client.SetEventHandle(event)?;
                let capture: IAudioCaptureClient = client.GetService()?;
                Ok(Source { client, capture, event })
            }
        }

        pub fn start(&mut self) -> Result<()> {
            com();
            unsafe { self.client.Start()? };
            Ok(())
        }

        /// What came since last time, waiting up to `wait` for something.
        pub fn read(&mut self, out: &mut Vec<u8>, wait: Duration) -> Result<()> {
            unsafe {
                let _ = WaitForSingleObject(self.event, wait.as_millis() as u32) == WAIT_OBJECT_0;
                loop {
                    let next = self.capture.GetNextPacketSize()?;
                    if next == 0 {
                        break;
                    }
                    let mut data = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                    let bytes = frames as usize * FRAME;
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                        out.resize(out.len() + bytes, 0);
                    } else {
                        out.extend_from_slice(std::slice::from_raw_parts(data, bytes));
                    }
                    self.capture.ReleaseBuffer(frames)?;
                }
            }
            Ok(())
        }
    }

    impl Drop for Source {
        fn drop(&mut self) {
            unsafe {
                let _ = self.client.Stop();
                let _ = CloseHandle(self.event);
            }
        }
    }

    fn enumerator() -> Result<IMMDeviceEnumerator> {
        Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
    }

    fn default_device(flow: EDataFlow) -> Result<IAudioClient> {
        unsafe {
            let dev = enumerator()?.GetDefaultAudioEndpoint(flow, eConsole)?;
            Ok(dev.Activate(CLSCTX_ALL, None)?)
        }
    }

    fn name_of(dev: &IMMDevice) -> Result<String> {
        unsafe {
            let store = dev.OpenPropertyStore(STGM_READ)?;
            let v = store.GetValue(&PKEY_Device_FriendlyName)?;
            let s = v.Anonymous.Anonymous.Anonymous.pwszVal;
            Ok(if s.is_null() { String::new() } else { s.to_string()? })
        }
    }

    fn capture_devices() -> Result<Vec<(String, IMMDevice)>> {
        unsafe {
            let list = enumerator()?.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
            let mut out = Vec::new();
            for i in 0..list.GetCount()? {
                let d = list.Item(i)?;
                out.push((name_of(&d)?, d));
            }
            Ok(out)
        }
    }

    pub fn mics() -> Result<Vec<String>> {
        com();
        Ok(capture_devices()?.into_iter().map(|(n, _)| n).collect())
    }

    fn mic_device(name: Option<&str>) -> Result<IAudioClient> {
        let Some(name) = name else { return default_device(eCapture) };
        let want = name.to_lowercase();
        let all = capture_devices()?;
        let Some((_, dev)) = all.iter().find(|(n, _)| n.to_lowercase().contains(&want)) else {
            let names: Vec<_> = all.iter().map(|(n, _)| n.as_str()).collect();
            bail!("No mic named like \"{name}\" (there are: {})", names.join(", "));
        };
        Ok(unsafe { dev.Activate(CLSCTX_ALL, None)? })
    }

    /// The first process of `exe` whose parent isn't one too (Discord runs several).
    pub fn root_pid(exe: &str) -> Option<u32> {
        let mut procs = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
            let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            if Process32FirstW(snap, &mut e).is_ok() {
                loop {
                    let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                    procs.push((String::from_utf16_lossy(&e.szExeFile[..len]), e.th32ProcessID, e.th32ParentProcessID));
                    if Process32NextW(snap, &mut e).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
        }
        let mine: Vec<_> = procs.iter().filter(|(n, _, _)| n.eq_ignore_ascii_case(exe)).collect();
        mine.iter().find(|(_, _, parent)| !mine.iter().any(|(_, pid, _)| pid == parent)).map(|(_, pid, _)| *pid)
    }

    #[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
    struct Done(std::sync::Mutex<Option<mpsc::Sender<()>>>);

    // Windows calls it from its own thread: it must say it can be (it holds only a Sender).
    impl windows::Win32::System::Com::IAgileObject_Impl for Done_Impl {}

    impl IActivateAudioInterfaceCompletionHandler_Impl for Done_Impl {
        fn ActivateCompleted(&self, _: windows_core::Ref<IActivateAudioInterfaceAsyncOperation>) -> windows_core::Result<()> {
            if let Some(tx) = self.0.lock().unwrap().take() {
                let _ = tx.send(());
            }
            Ok(())
        }
    }

    /// A loopback client for one process tree (Windows 10 2004 and later).
    fn app_client(exe: &str, mode: PROCESS_LOOPBACK_MODE) -> Result<IAudioClient> {
        let Some(pid) = root_pid(exe) else { bail!("{} isn't running", app_name(exe)) };
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: mode,
                },
            },
        };
        unsafe {
            let mut prop = PROPVARIANT::default();
            let inner = &mut prop.Anonymous.Anonymous;
            inner.vt = VT_BLOB;
            inner.Anonymous.blob = BLOB {
                cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                pBlobData: &mut params as *mut _ as *mut u8,
            };
            let (tx, rx) = mpsc::channel();
            let handler: IActivateAudioInterfaceCompletionHandler = Done(std::sync::Mutex::new(Some(tx))).into();
            let op = ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(&prop),
                &handler,
            )?;
            if rx.recv_timeout(Duration::from_secs(5)).is_err() {
                bail!("Windows didn't open {}'s sound", app_name(exe));
            }
            let mut hr = windows_core::HRESULT(0);
            let mut unk = None;
            op.GetActivateResult(&mut hr, &mut unk)?;
            hr.ok()?;
            let Some(unk) = unk else { bail!("no audio client") };
            // `prop` only borrows `params`: nothing for PropVariantClear to free.
            std::mem::forget(prop);
            Ok(unk.cast()?)
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub struct Source;
    impl Source {
        pub fn open(_: &Sound) -> Result<Source> {
            bail!("Recording sound works on Windows only")
        }
        pub fn start(&mut self) -> Result<()> {
            Ok(())
        }
        pub fn read(&mut self, _: &mut Vec<u8>, _: Duration) -> Result<()> {
            Ok(())
        }
    }
    pub fn mics() -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    pub fn root_pid(_: &str) -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_names() {
        assert_eq!(Sound::App("Discord.exe".into()).title(), "Discord");
        assert_eq!(Sound::PcExcept("discord.exe".into()).title(), "Game & PC (no Discord)");
        assert_eq!(Sound::Mic(None).title(), "Mic");
    }
}
