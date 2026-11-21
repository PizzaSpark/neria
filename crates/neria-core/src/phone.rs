//! Your Android phone as a capture source: connect it (a QR code, no IPs or ports), mirror it,
//! keep its last minute, record it, hear it, present it.
//!
//! No phone code of our own runs on the phone: scrcpy's server (Apache 2.0, one pinned
//! release fetched on demand into `bin/phone/`, see `docs/UPSTREAM.md` → scrcpy) is pushed
//! with its own adb and streams H.264 and the phone's sound over an adb tunnel. Neria reads
//! scrcpy's wire itself (`Session`) and hands the stream on through local sockets (`Fanout`):
//! to the page's player (mpv), to ffmpeg for the replay buffer and recordings (copied, not
//! encoded again: the same 2 s pieces as `capture.rs`, saved by `capture::save_from`), and to
//! the speaker (`Speaker`, WASAPI). Taps, keys and scrolling go back over scrcpy's control
//! socket. The quick mirror and Present are scrcpy's own window (`open_window`).
//!
//! Connecting is adb's: pairing by QR is Android Studio's way (`Pairing`), a paired phone is
//! reconnected by the adb server by itself. Neria's adb runs on its own port
//! (`ADB_PORT`), so Android Studio's adb (5037) is never killed by ours, nor ours by it.
//! adb keeps its key and the paired phones in `<profile>\.android` (no way to move them).

use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{capture, paths, proc, tools};

/// The scrcpy release used; the server and the client must be the very same version.
pub const VERSION: &str = "5.0.1";
const ZIP_URL: &str = "https://github.com/Genymobile/scrcpy/releases/download/v5.0.1/scrcpy-win64-v5.0.1.zip";
/// Neria's own adb server port (Android Studio's is 5037).
pub const ADB_PORT: u16 = 5038;
/// What the server is called on the phone.
const DEVICE_SERVER: &str = "/data/local/tmp/scrcpy-server.jar";
/// What has to be in `bin/phone/`.
const FILES: [&str; 5] = ["adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll", "scrcpy.exe", "scrcpy-server"];

// ---------------------------------------------------------------------------------------------
// The tools

pub fn dir() -> PathBuf {
    paths::bin_dir().join("phone")
}

fn adb_exe() -> PathBuf {
    dir().join("adb.exe")
}

/// The pinned release is on the drive.
pub fn ready() -> bool {
    FILES.iter().all(|f| dir().join(f).is_file())
        && std::fs::read_to_string(dir().join("version.txt")).is_ok_and(|v| v.trim() == VERSION)
}

/// Fetch scrcpy's release (11 MB: adb, scrcpy and its server) into `bin/phone/`. Blocking.
pub fn install(cancel: &AtomicBool, mut on: impl FnMut(u64, Option<u64>)) -> Result<()> {
    stop_server();
    let base = dir();
    std::fs::create_dir_all(&base).with_context(|| format!("cannot create {}", base.display()))?;
    let tmp = base.join("scrcpy.zip.download");
    tools::download_to_cancellable(ZIP_URL, &tmp, cancel, &mut on).context("downloading scrcpy")?;
    let res = extract(&tmp, &base);
    let _ = std::fs::remove_file(&tmp);
    res.context("extracting scrcpy")?;
    std::fs::write(base.join("version.txt"), VERSION)?;
    Ok(())
}

/// Every file of the release's folder, flat (scrcpy.exe needs its DLLs next to it).
fn extract(archive: &Path, to: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        if file.is_dir() {
            continue;
        }
        let name = file.name()?.replace('\\', "/");
        let Some(base) = name.rsplit('/').next().filter(|b| !b.is_empty() && !b.contains("..")) else { continue };
        let part = to.join(format!("{base}.part"));
        {
            let mut out = std::fs::File::create(&part)?;
            std::io::copy(&mut file, &mut out)?;
        }
        std::fs::rename(&part, to.join(base))?;
    }
    if let Some(f) = FILES.iter().find(|f| !to.join(f).is_file()) {
        bail!("{f} not found in the archive");
    }
    Ok(())
}

/// Take the tools off the drive (stops Neria's adb first).
pub fn remove() -> Result<()> {
    stop_server();
    let d = dir();
    if d.exists() {
        std::fs::remove_dir_all(&d).with_context(|| format!("removing {}", d.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// adb

/// Neria's adb, talking to Neria's own adb server.
pub fn adb() -> Command {
    let mut cmd = proc::command(adb_exe());
    cmd.env("ANDROID_ADB_SERVER_PORT", ADB_PORT.to_string());
    cmd
}

/// Run adb and return what it printed (stdout, then stderr), failing on a non-zero exit or
/// after `timeout`.
fn adb_out(args: &[&str], timeout: Duration) -> Result<String> {
    let mut child = adb()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("couldn't start adb ({})", adb_exe().display()))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let o = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let e = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let began = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if began.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("adb {} took too long", args.first().unwrap_or(&""));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = format!("{}{}", o.join().unwrap_or_default(), e.join().unwrap_or_default());
    if !status.success() {
        bail!("{}", text.trim());
    }
    Ok(text)
}

/// Start Neria's adb server (a no-op when it runs). Paired phones on this Wi-Fi reconnect by
/// themselves a moment later.
pub fn start_server() -> Result<()> {
    adb_out(&["start-server"], Duration::from_secs(20))?;
    Ok(())
}

/// Stop Neria's adb server (on quit and before replacing the tools). Never touches another
/// adb's server.
pub fn stop_server() {
    if adb_exe().is_file() && TcpStream::connect_timeout(&([127, 0, 0, 1], ADB_PORT).into(), Duration::from_millis(200)).is_ok()
    {
        let _ = adb_out(&["kill-server"], Duration::from_secs(5));
    }
}

/// Another adb server (Android Studio's, usually) runs on the usual port: a USB phone it has
/// taken can't be seen by ours.
pub fn other_adb_running() -> bool {
    TcpStream::connect_timeout(&([127, 0, 0, 1], 5037).into(), Duration::from_millis(200)).is_ok()
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Connected and allowed.
    Ready,
    /// "Allow USB debugging?" is waiting on the phone.
    Unauthorized,
    Offline,
    Other(String),
}

/// A phone adb sees now.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// adb's name for it (`-s`): a USB serial, `ip:port` or an mDNS name.
    pub serial: String,
    pub state: State,
    /// adb's `model:` ("Pixel_7"), with spaces.
    pub model: String,
    pub wifi: bool,
}

/// `adb devices -l`.
pub fn devices() -> Result<Vec<Device>> {
    Ok(parse_devices(&adb_out(&["devices", "-l"], Duration::from_secs(10))?))
}

pub fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .filter(|l| !l.starts_with("List of") && !l.starts_with('*') && !l.trim().is_empty())
        .filter_map(|l| {
            // A serial can hold a space ("adb-…-shbQqu (2)._adb-tls-connect._tcp" when the
            // phone announced itself twice): it runs up to the state word.
            const STATES: [&str; 10] =
                ["device", "unauthorized", "offline", "authorizing", "connecting", "recovery", "sideload", "bootloader", "host", "unknown"];
            let words: Vec<&str> = l.split_whitespace().collect();
            let at = words.iter().skip(1).position(|w| STATES.contains(w) || *w == "no").map(|i| i + 1).unwrap_or(1);
            let serial = words[..at].join(" ");
            let state = match *words.get(at)? {
                "device" => State::Ready,
                "unauthorized" => State::Unauthorized,
                "offline" => State::Offline,
                s => State::Other(s.to_string()),
            };
            let model = words[at..].iter().find_map(|p| p.strip_prefix("model:")).unwrap_or("").replace('_', " ");
            let wifi = serial.contains("._adb-tls-connect.") || serial.contains(':');
            Some(Device { serial, state, model, wifi })
        })
        .collect()
}

/// What the phone says about itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Info {
    /// The name people know it by ("Pixel 7", "Galaxy S23").
    pub name: String,
    /// "15"
    pub android: String,
    pub sdk: u32,
    /// The same phone over USB and Wi-Fi.
    pub id: String,
}

pub fn info(serial: &str) -> Result<Info> {
    let script = "getprop ro.product.marketname; getprop ro.config.marketing_name; getprop ro.product.model; \
                  getprop ro.build.version.release; getprop ro.build.version.sdk; getprop ro.serialno; \
                  getprop ro.product.manufacturer";
    let out = adb_out(&["-s", serial, "shell", script], Duration::from_secs(10))?;
    let l: Vec<&str> = out.lines().map(str::trim).collect();
    let get = |i: usize| l.get(i).copied().unwrap_or("").to_string();
    Ok(Info::from_props(&get(0), &get(1), &get(2), &get(3), &get(4), &get(5), &get(6)))
}

impl Info {
    fn from_props(market: &str, marketing: &str, model: &str, android: &str, sdk: &str, id: &str, maker: &str) -> Info {
        let mut name = [market, marketing, model].into_iter().find(|s| !s.is_empty()).unwrap_or("Android phone").to_string();
        // "SM-S911B" says nothing; "Samsung SM-S911B" a little more.
        let codey = name.chars().any(|c| c.is_ascii_digit()) && !name.contains(' ');
        if codey && !maker.is_empty() && !name.to_lowercase().starts_with(&maker.to_lowercase()) {
            let mut m = maker.to_string();
            if let Some(f) = m.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            name = format!("{m} {name}");
        }
        Info { name, android: android.to_string(), sdk: sdk.parse().unwrap_or(0), id: id.to_string() }
    }

    /// Sound forwarding needs Android 11.
    pub fn has_sound(&self) -> bool {
        self.sdk == 0 || self.sdk >= 30
    }
}

// ---------------------------------------------------------------------------------------------
// Choices (in `settings.json`)

/// The Phone tab's choices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// While mirroring: the phone's sound here, its screen off, kept awake.
    pub sound: bool,
    pub screen_off: bool,
    pub awake: bool,
    /// Sound only: keep playing on the phone too; the volume here.
    pub keep_on_phone: bool,
    pub volume: f32,
    /// Present: on top, taps shown.
    pub on_top: bool,
    pub show_taps: bool,
    /// Seconds "Clip the last" saves, and how much is kept.
    pub clip: u32,
    pub keep: u32,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            sound: true,
            screen_off: false,
            awake: true,
            keep_on_phone: true,
            volume: 1.0,
            on_top: true,
            show_taps: false,
            clip: 30,
            keep: 60,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Phones you've connected (`phones.json`)

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Known {
    pub id: String,
    pub name: String,
    pub android: String,
    /// Last connected over Wi-Fi.
    pub wifi: bool,
}

fn known_file() -> PathBuf {
    paths::app_dir().join("phones.json")
}

pub fn known() -> Vec<Known> {
    std::fs::read(known_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Remember (or update) a phone, newest first.
pub fn remember(info: &Info, wifi: bool) {
    if info.id.is_empty() {
        return;
    }
    let mut all = known();
    all.retain(|k| k.id != info.id);
    all.insert(0, Known { id: info.id.clone(), name: info.name.clone(), android: info.android.clone(), wifi });
    if let Ok(json) = serde_json::to_vec_pretty(&all) {
        let _ = std::fs::write(known_file(), json);
    }
}

/// Forget a phone here. (adb's own pairing stays until the phone's "Forget" or "Revoke".)
pub fn forget(id: &str) {
    let mut all = known();
    all.retain(|k| k.id != id);
    if let Ok(json) = serde_json::to_vec_pretty(&all) {
        let _ = std::fs::write(known_file(), json);
    }
}

// ---------------------------------------------------------------------------------------------
// Finding phones on the Wi-Fi, pairing

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// "Pair device with QR code / pairing code" is open on the phone.
    Pairing,
    /// Wireless debugging is on.
    Connect,
}

/// A phone announcing itself on this network (adb's mDNS).
#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    pub instance: String,
    pub kind: Kind,
    /// `ip:port`
    pub addr: String,
}

impl Service {
    pub fn ip(&self) -> &str {
        self.addr.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(&self.addr)
    }
}

/// `adb mdns services`.
pub fn services() -> Result<Vec<Service>> {
    Ok(parse_services(&adb_out(&["mdns", "services"], Duration::from_secs(10))?))
}

pub fn parse_services(text: &str) -> Vec<Service> {
    text.lines()
        .filter_map(|l| {
            // Tab-separated: a name can have spaces ("adb-…-shbQqu (2)" when announced twice).
            let mut p = l.split(['\t']).map(str::trim).filter(|x| !x.is_empty());
            let instance = p.next()?.to_string();
            let kind = match p.next()? {
                t if t.starts_with("_adb-tls-pairing.") => Kind::Pairing,
                t if t.starts_with("_adb-tls-connect.") => Kind::Connect,
                _ => return None,
            };
            let addr = p.next()?.to_string();
            addr.contains(':').then_some(Service { instance, kind, addr })
        })
        .collect()
}

fn random() -> u64 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(Instant::now().elapsed().as_nanos() ^ std::process::id() as u128);
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    h.finish()
}

/// One QR pairing: the name the phone announces once it has scanned the code, and the
/// password inside it (Android Studio's format).
#[derive(Clone, Debug)]
pub struct Pairing {
    pub name: String,
    pub password: String,
}

impl Default for Pairing {
    fn default() -> Self {
        Self::new()
    }
}

impl Pairing {
    pub fn new() -> Pairing {
        const ABC: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
        let (a, b) = (random(), random());
        let name = format!("neria-{:06x}", a & 0xff_ffff);
        let password: String = (0..10).map(|i| ABC[((b >> (i * 5)) & 31) as usize] as char).collect();
        Pairing { name, password }
    }

    /// What the QR code holds.
    pub fn qr_text(&self) -> String {
        format!("WIFI:T:ADB;S:{};P:{};;", self.name, self.password)
    }

    /// The phone has scanned the code: where to pair.
    pub fn found<'a>(&self, services: &'a [Service]) -> Option<&'a Service> {
        services.iter().find(|s| s.kind == Kind::Pairing && s.instance == self.name)
    }
}

/// The QR code's modules, row by row (true = dark), with no quiet zone.
pub fn qr(text: &str) -> Result<(usize, Vec<bool>)> {
    let code = qrcode::QrCode::new(text.as_bytes())?;
    let w = code.width();
    Ok((w, code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect()))
}

/// Pair with a phone (`adb pair`): the QR's password, or the 6 digits it shows.
pub fn pair(addr: &str, code: &str) -> Result<()> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let out = adb_out(&["pair", addr, &code], Duration::from_secs(30)).map_err(|e| anyhow::anyhow!(explain_pair(&e.to_string())))?;
    if !out.contains("Successfully paired") {
        bail!("{}", explain_pair(&out));
    }
    Ok(())
}

fn explain_pair(out: &str) -> String {
    let o = out.to_lowercase();
    if o.contains("wrong password") || o.contains("password") {
        "That code didn't match. Check the 6 digits on the phone and try again.".into()
    } else if o.contains("unable to start pairing") || o.contains("connection refused") || o.contains("10061") {
        "The phone stopped listening for pairing. Open \"Pair device\" on the phone again.".into()
    } else {
        format!("Couldn't pair: {}", out.trim().lines().last().unwrap_or("adb failed"))
    }
}

/// Connect to a paired phone's Wireless debugging (`adb connect`), when adb hasn't by itself.
pub fn connect(addr: &str) -> Result<()> {
    let out = adb_out(&["connect", addr], Duration::from_secs(15))?;
    if out.contains("connected to") {
        Ok(())
    } else {
        bail!("{}", out.trim())
    }
}

/// After pairing, the phone that just paired, connected (by adb by itself, or by us).
pub fn wait_connected(ip: &str, timeout: Duration) -> Result<Device> {
    let began = Instant::now();
    let mut tried = false;
    while began.elapsed() < timeout {
        let devs = devices().unwrap_or_default();
        let svcs = services().unwrap_or_default();
        let connect = svcs.iter().find(|s| s.kind == Kind::Connect && s.ip() == ip);
        if let Some(d) = devs.iter().find(|d| {
            d.state == State::Ready
                && (d.serial.starts_with(&format!("{ip}:")) || connect.is_some_and(|c| d.serial.starts_with(&c.instance)))
        }) {
            return Ok(d.clone());
        }
        if let (Some(c), false) = (connect, tried) {
            if began.elapsed() > Duration::from_secs(3) {
                tried = true;
                let _ = self::connect(&c.addr);
            }
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    bail!("Paired, but the phone didn't connect. Check that Wireless debugging is still on.")
}

/// What the connect screen can tell about the phone's setup without it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    /// A phone with Wireless debugging on is on this network (so Developer options are on).
    pub wireless_on: bool,
    /// The phone scanned the code.
    pub scanned: Option<Service>,
}

pub fn look(pairing: &Pairing) -> Seen {
    let svcs = services().unwrap_or_default();
    Seen {
        wireless_on: svcs.iter().any(|s| s.kind == Kind::Connect) || pairing.found(&svcs).is_some(),
        scanned: pairing.found(&svcs).cloned(),
    }
}

/// A plain sentence for a device that's there but can't be used yet.
pub fn problem(d: &Device) -> Option<&'static str> {
    match d.state {
        State::Ready => None,
        State::Unauthorized => Some("Tap \"Allow\" on your phone (tick \"Always allow from this computer\")."),
        State::Offline => Some("The phone stopped answering. Unplug it and plug it back in, or turn Wireless debugging off and on."),
        State::Other(_) => Some("The phone isn't ready yet. Unlock it and wait a moment."),
    }
}

// ---------------------------------------------------------------------------------------------
// The quick mirror and Present: scrcpy's own window

#[derive(Clone, Debug, Default)]
pub struct WindowOptions {
    /// "Phone screen · Pixel 7"
    pub title: String,
    /// No frame (Present).
    pub borderless: bool,
    pub on_top: bool,
    pub show_touches: bool,
    pub stay_awake: bool,
    pub screen_off: bool,
    /// Play the phone's sound here too.
    pub sound: bool,
    /// Record to this file while it's open.
    pub record: Option<PathBuf>,
}

/// scrcpy's window for `serial`. Ends when the window is closed (or `proc::kill_tree`).
pub fn open_window(serial: &str, o: &WindowOptions) -> Result<Child> {
    let mut cmd = proc::command(dir().join("scrcpy.exe"));
    cmd.env("ADB", adb_exe())
        .env("ANDROID_ADB_SERVER_PORT", ADB_PORT.to_string())
        .env("SCRCPY_SERVER_PATH", dir().join("scrcpy-server"))
        .args(["-s", serial, "--window-title", &o.title, "--no-mouse-hover"]);
    if o.borderless {
        cmd.arg("--window-borderless");
    }
    if o.on_top {
        cmd.arg("--always-on-top");
    }
    if o.show_touches {
        cmd.arg("--show-touches");
    }
    if o.stay_awake {
        cmd.arg("--stay-awake");
    }
    if o.screen_off {
        cmd.arg("--turn-screen-off");
    }
    if !o.sound {
        cmd.arg("--no-audio");
    }
    if let Some(f) = &o.record {
        cmd.arg("--record").arg(f);
    }
    let child = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().context("couldn't start scrcpy")?;
    Ok(child)
}

/// "Phone screen · Pixel 7": easy to pick when sharing a window.
pub fn present_title(name: &str) -> String {
    format!("Phone screen · {name}")
}

// ---------------------------------------------------------------------------------------------
// Neria's own mirror: scrcpy's server, our client

#[derive(Clone, Debug)]
pub struct Options {
    pub video: bool,
    pub audio: bool,
    pub control: bool,
    /// The longer side, 0 = as the phone is.
    pub max_size: u32,
    pub bit_rate: u32,
    pub max_fps: u32,
    /// Keep playing the sound on the phone too (else the phone goes quiet while it's sent).
    pub keep_on_phone: bool,
    pub show_touches: bool,
    pub stay_awake: bool,
    /// Turn the phone's screen off once mirroring (it keeps running).
    pub screen_off: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            video: true,
            audio: true,
            control: true,
            max_size: 1920,
            bit_rate: 8_000_000,
            max_fps: 60,
            keep_on_phone: false,
            show_touches: false,
            stay_awake: true,
            screen_off: false,
        }
    }
}

impl Options {
    /// Sound only: no picture, no control.
    pub fn sound_only() -> Options {
        Options { video: false, control: false, keep_on_phone: true, ..Options::default() }
    }

    /// The server's `key=value` arguments.
    fn server_args(&self, scid: u32) -> Vec<String> {
        let mut a = vec![
            format!("scid={scid:08x}"),
            "log_level=info".into(),
            "tunnel_forward=true".into(),
            format!("video={}", self.video),
            format!("audio={}", self.audio),
            format!("control={}", self.control),
        ];
        if self.video {
            a.push("video_codec=h264".into());
            a.push(format!("video_bit_rate={}", self.bit_rate));
            if self.max_size > 0 {
                a.push(format!("max_size={}", self.max_size));
            }
            if self.max_fps > 0 {
                a.push(format!("max_fps={}", self.max_fps));
            }
            // A key frame every piece, so a save starts at most a piece early.
            a.push(format!("video_codec_options=i-frame-interval:int={}", capture::PIECE_SECS));
        }
        if self.audio {
            // 48 kHz stereo s16le: what our speaker and ffmpeg's sound inputs take as is.
            a.push("audio_codec=raw".into());
            if self.keep_on_phone {
                a.push("audio_source=playback".into());
                a.push("audio_dup=true".into());
            }
        }
        if self.show_touches {
            a.push("show_touches=true".into());
        }
        if self.stay_awake {
            a.push("stay_awake=true".into());
        }
        a
    }
}

/// One video packet as scrcpy sends it (Annex B H.264).
#[derive(Clone, Debug, PartialEq)]
pub enum Packet {
    /// A new capture session (start, rotation): the picture's size.
    Session { width: u32, height: u32 },
    Media { config: bool, key: bool, pts: u64, data: Vec<u8> },
}

/// Read one packet (the 12-byte header, then the payload).
pub fn read_packet(r: &mut impl Read) -> std::io::Result<Packet> {
    let mut h = [0u8; 12];
    r.read_exact(&mut h)?;
    if h[0] & 0x80 != 0 {
        let width = u32::from_be_bytes(h[4..8].try_into().unwrap());
        let height = u32::from_be_bytes(h[8..12].try_into().unwrap());
        return Ok(Packet::Session { width, height });
    }
    let flags = u64::from_be_bytes(h[0..8].try_into().unwrap());
    let size = u32::from_be_bytes(h[8..12].try_into().unwrap()) as usize;
    if size > 64 << 20 {
        return Err(std::io::Error::new(ErrorKind::InvalidData, "packet too large"));
    }
    let mut data = vec![0u8; size];
    r.read_exact(&mut data)?;
    Ok(Packet::Media {
        config: flags & (1 << 62) != 0,
        key: flags & (1 << 61) != 0,
        pts: flags & ((1 << 61) - 1),
        data,
    })
}

/// Write one packet the way the server does (the fake phone, tests).
pub fn write_packet(w: &mut impl Write, p: &Packet) -> std::io::Result<()> {
    match p {
        Packet::Session { width, height } => {
            w.write_all(&0x8000_0000u32.to_be_bytes())?;
            w.write_all(&width.to_be_bytes())?;
            w.write_all(&height.to_be_bytes())
        }
        Packet::Media { config, key, pts, data } => {
            let flags = if *config { 1u64 << 62 } else { *pts | if *key { 1 << 61 } else { 0 } };
            w.write_all(&flags.to_be_bytes())?;
            w.write_all(&(data.len() as u32).to_be_bytes())?;
            w.write_all(data)
        }
    }
}

const CODEC_H264: u32 = 0x6832_3634;
const CODEC_RAW: u32 = 0x0072_6177;

type Chunk = Arc<[u8]>;

struct Client {
    tx: SyncSender<Chunk>,
    /// Video: waiting for a key frame before anything is sent. Matroska: for its header.
    need_key: bool,
}

/// A fanout that sends Matroska (the picture and the sound together, with the phone's own
/// times) instead of the raw streams: what ffmpeg reads for the buffer and recordings.
struct Mkv {
    video: bool,
    audio: bool,
    width: u32,
    height: u32,
    /// The first time seen (µs), the stream's zero.
    origin: Option<u64>,
    /// Readers are let go when the phone turns (a new SPS): the replay buffer starts again
    /// with the new shape rather than mix two in one file.
    ends_on_turn: bool,
}

#[derive(Default)]
struct Fan {
    clients: Vec<Client>,
    /// The newest SPS/PPS, sent before the first key frame to whoever joins.
    config: Option<Chunk>,
    mkv: Option<Mkv>,
}

/// One stream handed to everyone who wants it: local TCP clients (mpv, ffmpeg) on `port`,
/// and in-process taps (the speaker).
pub struct Fanout {
    pub port: u16,
    video: bool,
    fan: Arc<Mutex<Fan>>,
    stop: Arc<AtomicBool>,
}

/// How much a slow reader may fall behind before it's dropped (about 10 s of 60 fps).
const BACKLOG: usize = 600;

impl Fanout {
    fn new(video: bool, stop: Arc<AtomicBool>) -> Result<Fanout> {
        Fanout::with(video, None, stop)
    }

    /// Matroska of the picture (when `video`) and the sound (when `audio`).
    fn mkv(video: bool, audio: bool, ends_on_turn: bool, stop: Arc<AtomicBool>) -> Result<Fanout> {
        Fanout::with(video, Some(Mkv { video, audio, width: 0, height: 0, origin: None, ends_on_turn }), stop)
    }

    fn with(video: bool, mkv: Option<Mkv>, stop: Arc<AtomicBool>) -> Result<Fanout> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let fan = Arc::new(Mutex::new(Fan { mkv, ..Fan::default() }));
        let (f, s) = (fan.clone(), stop.clone());
        std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_nodelay(true);
                        let rx = add(&f, video);
                        let s2 = s.clone();
                        std::thread::spawn(move || pump(rx, stream, s2));
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        });
        Ok(Fanout { port, video, fan, stop })
    }

    /// The stream in this process (from the next key frame, for video).
    pub fn tap(&self) -> Receiver<Chunk> {
        add(&self.fan, self.video)
    }

    /// `tcp://127.0.0.1:port`, for mpv and ffmpeg.
    pub fn url(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.port)
    }

    pub fn readers(&self) -> usize {
        self.fan.lock().unwrap().clients.len()
    }

    /// What the reading thread sends with.
    fn feed(&self) -> Feed {
        Feed(self.fan.clone())
    }

    #[cfg(test)]
    fn send(&self, data: &[u8], config: bool, key: bool) {
        self.feed().send(data, config, key)
    }

    fn close(&self) {
        self.feed().close()
    }
}

impl Drop for Fanout {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.close();
    }
}

#[derive(Clone)]
struct Feed(Arc<Mutex<Fan>>);

/// Send to every client (a reader that just joined gets `header` first, at a key frame);
/// the slow and the gone are dropped.
fn deliver(fan: &mut Fan, chunk: &Chunk, key: bool, header: Option<Chunk>) {
    fan.clients.retain_mut(|c| {
        if c.need_key {
            // Without its header (no SPS yet) a reader couldn't make sense of it: wait.
            if !key || header.is_none() {
                return true;
            }
            c.need_key = false;
            if let Some(h) = &header {
                if c.tx.try_send(h.clone()).is_err() {
                    return false;
                }
            }
        }
        c.tx.try_send(chunk.clone()).is_ok()
    });
}

impl Feed {
    /// Raw: a packet as it came (SPS/PPS go first to a reader joining at a key frame).
    fn send(&self, data: &[u8], config: bool, key: bool) {
        let chunk: Chunk = Arc::from(data);
        let mut fan = self.0.lock().unwrap();
        if config {
            fan.config = Some(chunk.clone());
            fan.clients.retain_mut(|c| c.need_key || c.tx.try_send(chunk.clone()).is_ok());
            return;
        }
        let header = if key { fan.config.clone() } else { None };
        deliver(&mut fan, &chunk, key, header);
    }

    /// Matroska: the picture's size (from the session packet).
    fn size(&self, w: u32, h: u32) {
        if let Some(m) = self.0.lock().unwrap().mkv.as_mut() {
            m.width = w;
            m.height = h;
        }
    }

    /// Matroska: one video packet.
    fn mkv_video(&self, data: &[u8], config: bool, key: bool, pts: u64) {
        let mut fan = self.0.lock().unwrap();
        if config {
            let turned = fan.config.as_deref().is_some_and(|c| c != data);
            if turned && fan.mkv.as_ref().is_some_and(|m| m.ends_on_turn) {
                fan.clients.clear();
            }
            fan.config = Some(Arc::from(data));
            return;
        }
        if fan.clients.is_empty() {
            return;
        }
        let ms = stamp(&mut fan, pts);
        let block: Chunk = Arc::from(mkv::block(1, ms, key, &mkv::avcc_frame(data)));
        let header = if key && fan.clients.iter().any(|c| c.need_key) {
            fan.mkv.as_ref().zip(fan.config.as_deref().and_then(mkv::avcc)).map(|(m, avcc)| {
                Arc::from(mkv::header(Some((&avcc, m.width, m.height)), m.audio)) as Chunk
            })
        } else {
            None
        };
        deliver(&mut fan, &block, key, header);
    }

    /// Matroska: one packet of sound (48 kHz stereo s16le).
    fn mkv_audio(&self, data: &[u8], pts: u64) {
        let mut fan = self.0.lock().unwrap();
        if fan.clients.is_empty() {
            return;
        }
        let video = fan.mkv.as_ref().is_some_and(|m| m.video);
        let ms = stamp(&mut fan, pts);
        let block: Chunk = Arc::from(mkv::block(if video { 2 } else { 1 }, ms, true, data));
        if video {
            // Sound waits for the picture's first key frame (and its header).
            fan.clients.retain_mut(|c| c.need_key || c.tx.try_send(block.clone()).is_ok());
        } else {
            let header: Chunk = Arc::from(mkv::header(None, true));
            deliver(&mut fan, &block, true, Some(header));
        }
    }

    /// Everyone reading gets the end of the stream.
    fn close(&self) {
        self.0.lock().unwrap().clients.clear();
    }

    /// (readers, readers still waiting for a key frame)
    fn readers(&self) -> (usize, bool) {
        let f = self.0.lock().unwrap();
        (f.clients.len(), f.clients.iter().any(|c| c.need_key))
    }
}

/// Milliseconds since the stream's first time.
fn stamp(fan: &mut Fan, pts: u64) -> u64 {
    let Some(m) = fan.mkv.as_mut() else { return 0 };
    let origin = *m.origin.get_or_insert(pts);
    pts.saturating_sub(origin) / 1000
}

fn add(fan: &Mutex<Fan>, video: bool) -> Receiver<Chunk> {
    let (tx, rx) = sync_channel(BACKLOG);
    let mut f = fan.lock().unwrap();
    // Matroska of sound alone waits too: for its header.
    let need_key = video || f.mkv.is_some();
    f.clients.push(Client { tx, need_key });
    rx
}

/// The little Matroska we write: an EBML header, a Segment of unknown size, the tracks, then
/// one Cluster per packet. Enough for ffmpeg to read live, with exact times.
mod mkv {
    fn id(out: &mut Vec<u8>, id: u32) {
        let b = id.to_be_bytes();
        let skip = b.iter().position(|&x| x != 0).unwrap_or(3);
        out.extend_from_slice(&b[skip..]);
    }

    fn size(out: &mut Vec<u8>, n: u64) {
        let mut len = 1;
        while len < 8 && n >= (1u64 << (7 * len)) - 1 {
            len += 1;
        }
        let v = n | (1u64 << (7 * len));
        out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
    }

    fn el(out: &mut Vec<u8>, i: u32, body: &[u8]) {
        id(out, i);
        size(out, body.len() as u64);
        out.extend_from_slice(body);
    }

    fn uint(out: &mut Vec<u8>, i: u32, v: u64) {
        let b = v.to_be_bytes();
        let skip = b.iter().position(|&x| x != 0).unwrap_or(7);
        el(out, i, &b[skip..]);
    }

    /// `(avcC, width, height)` for the picture; the sound is 48 kHz stereo 16-bit.
    pub fn header(video: Option<(&[u8], u32, u32)>, audio: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut ebml = Vec::new();
        uint(&mut ebml, 0x4286, 1);
        uint(&mut ebml, 0x42F7, 1);
        uint(&mut ebml, 0x42F2, 4);
        uint(&mut ebml, 0x42F3, 8);
        el(&mut ebml, 0x4282, b"matroska");
        uint(&mut ebml, 0x4287, 4);
        uint(&mut ebml, 0x4285, 2);
        el(&mut out, 0x1A45_DFA3, &ebml);
        // Segment, size unknown: it goes on as long as the phone does.
        id(&mut out, 0x1853_8067);
        out.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        let mut info = Vec::new();
        uint(&mut info, 0x2A_D7B1, 1_000_000);
        el(&mut info, 0x4D80, b"Neria");
        el(&mut info, 0x5741, b"Neria");
        el(&mut out, 0x1549_A966, &info);
        let mut tracks = Vec::new();
        let mut n = 0;
        if let Some((avcc, w, h)) = video {
            n += 1;
            let mut t = Vec::new();
            uint(&mut t, 0xD7, n);
            uint(&mut t, 0x73C5, n);
            uint(&mut t, 0x83, 1);
            el(&mut t, 0x86, b"V_MPEG4/ISO/AVC");
            el(&mut t, 0x63A2, avcc);
            let mut v = Vec::new();
            uint(&mut v, 0xB0, w as u64);
            uint(&mut v, 0xBA, h as u64);
            el(&mut t, 0xE0, &v);
            el(&mut tracks, 0xAE, &t);
        }
        if audio {
            n += 1;
            let mut t = Vec::new();
            uint(&mut t, 0xD7, n);
            uint(&mut t, 0x73C5, n);
            uint(&mut t, 0x83, 2);
            el(&mut t, 0x86, b"A_PCM/INT/LIT");
            let mut a = Vec::new();
            el(&mut a, 0xB5, &48_000f64.to_be_bytes());
            uint(&mut a, 0x9F, 2);
            uint(&mut a, 0x6264, 16);
            el(&mut t, 0xE1, &a);
            el(&mut tracks, 0xAE, &t);
        }
        el(&mut out, 0x1654_AE6B, &tracks);
        out
    }

    /// A Cluster holding one SimpleBlock at `ms`.
    pub fn block(track: u8, ms: u64, key: bool, data: &[u8]) -> Vec<u8> {
        let mut c = Vec::with_capacity(data.len() + 32);
        uint(&mut c, 0xE7, ms);
        let mut b = Vec::with_capacity(data.len() + 4);
        b.push(0x80 | track);
        b.extend_from_slice(&[0, 0]);
        b.push(if key { 0x80 } else { 0 });
        b.extend_from_slice(data);
        el(&mut c, 0xA3, &b);
        let mut out = Vec::with_capacity(c.len() + 12);
        el(&mut out, 0x1F43_B675, &c);
        out
    }

    /// The NAL units of an Annex B buffer, without their start codes.
    pub fn nals(annexb: &[u8]) -> Vec<&[u8]> {
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 3 <= annexb.len() {
            if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
                starts.push(i);
                i += 3;
            } else {
                i += 1;
            }
        }
        let mut out = Vec::new();
        for (k, &s) in starts.iter().enumerate() {
            let mut end = starts.get(k + 1).copied().unwrap_or(annexb.len());
            // A 4-byte start code's leading zero belongs to the next one.
            if k + 1 < starts.len() && end > s + 3 && annexb[end - 1] == 0 {
                end -= 1;
            }
            if end > s + 3 {
                out.push(&annexb[s + 3..end]);
            }
        }
        out
    }

    /// avcC (the decoder setup) from Annex B SPS/PPS.
    pub fn avcc(config: &[u8]) -> Option<Vec<u8>> {
        let n = nals(config);
        let sps: Vec<&[u8]> = n.iter().copied().filter(|x| x.first().is_some_and(|b| b & 0x1f == 7)).collect();
        let pps: Vec<&[u8]> = n.iter().copied().filter(|x| x.first().is_some_and(|b| b & 0x1f == 8)).collect();
        let first = sps.first().filter(|s| s.len() >= 4)?;
        let mut out = vec![1, first[1], first[2], first[3], 0xFF, 0xE0 | sps.len() as u8];
        for s in &sps {
            out.extend((s.len() as u16).to_be_bytes());
            out.extend_from_slice(s);
        }
        out.push(pps.len() as u8);
        for p in &pps {
            out.extend((p.len() as u16).to_be_bytes());
            out.extend_from_slice(p);
        }
        Some(out)
    }

    /// An access unit with 4-byte lengths instead of start codes (access unit delimiters dropped).
    pub fn avcc_frame(annexb: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(annexb.len() + 8);
        for n in nals(annexb) {
            if n.first().is_some_and(|b| b & 0x1f == 9) {
                continue;
            }
            out.extend((n.len() as u32).to_be_bytes());
            out.extend_from_slice(n);
        }
        out
    }
}

fn pump(rx: Receiver<Chunk>, mut stream: TcpStream, stop: Arc<AtomicBool>) {
    while let Ok(c) = rx.recv() {
        if stop.load(Ordering::SeqCst) || stream.write_all(&c).is_err() {
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// Touch actions (Android's `MotionEvent`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Touch {
    Down = 0,
    Up = 1,
    Move = 2,
}

/// Keys used by the page (Android's `KeyEvent` codes).
pub mod key {
    pub const HOME: i32 = 3;
    pub const BACK: i32 = 4;
    pub const VOLUME_UP: i32 = 24;
    pub const VOLUME_DOWN: i32 = 25;
    pub const POWER: i32 = 26;
    pub const ENTER: i32 = 66;
    pub const DEL: i32 = 67;
    pub const APP_SWITCH: i32 = 187;
}

/// A control message (big-endian, scrcpy's `ControlMessageReader`).
pub enum Control {
    Key { down: bool, code: i32, meta: i32 },
    Text(String),
    Touch { action: Touch, x: i32, y: i32, w: u16, h: u16 },
    Scroll { x: i32, y: i32, w: u16, h: u16, dx: f32, dy: f32 },
    /// The phone's screen on or off (mirroring goes on).
    ScreenPower(bool),
    Rotate,
    /// A key frame now.
    ResetVideo,
}

impl Control {
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(32);
        let pos = |b: &mut Vec<u8>, x: i32, y: i32, w: u16, h: u16| {
            b.extend(x.to_be_bytes());
            b.extend(y.to_be_bytes());
            b.extend(w.to_be_bytes());
            b.extend(h.to_be_bytes());
        };
        match self {
            Control::Key { down, code, meta } => {
                b.push(0);
                b.push(if *down { 0 } else { 1 });
                b.extend(code.to_be_bytes());
                b.extend(0i32.to_be_bytes());
                b.extend(meta.to_be_bytes());
            }
            Control::Text(t) => {
                // The server takes up to 300 bytes; cut on a character boundary.
                let mut end = t.len().min(300);
                while !t.is_char_boundary(end) {
                    end -= 1;
                }
                b.push(1);
                b.extend((end as u32).to_be_bytes());
                b.extend(&t.as_bytes()[..end]);
            }
            Control::Touch { action, x, y, w, h } => {
                b.push(2);
                b.push(*action as u8);
                // A finger, not a mouse: apps see a touchscreen.
                b.extend((-2i64).to_be_bytes());
                pos(&mut b, *x, *y, *w, *h);
                b.extend((if *action == Touch::Up { 0u16 } else { 0xffff }).to_be_bytes());
                b.extend(0i32.to_be_bytes());
                b.extend(0i32.to_be_bytes());
            }
            Control::Scroll { x, y, w, h, dx, dy } => {
                b.push(3);
                pos(&mut b, *x, *y, *w, *h);
                let fp = |v: f32| ((v / 16.0).clamp(-1.0, 1.0) * 32768.0).clamp(-32768.0, 32767.0) as i16;
                b.extend(fp(*dx).to_be_bytes());
                b.extend(fp(*dy).to_be_bytes());
                b.extend(0i32.to_be_bytes());
            }
            Control::ScreenPower(on) => {
                b.push(10);
                b.push(*on as u8);
            }
            Control::Rotate => b.push(11),
            Control::ResetVideo => b.push(17),
        }
        b
    }
}

/// What a running mirror shows, for the page.
#[derive(Clone, Debug, Default)]
pub struct Live {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub bytes: u64,
    /// When the last key frame came.
    pub last_key: Option<Instant>,
}

/// Neria's own mirror of one phone (or its sound alone).
pub struct Session {
    pub serial: String,
    /// The name the phone sent ("Pixel 7").
    pub name: String,
    pub options: Options,
    /// The raw picture (Annex B H.264), for the page's player and screenshots.
    pub video: Option<Fanout>,
    /// The raw sound (48 kHz stereo s16le), for the speaker.
    pub audio: Option<Fanout>,
    /// Both in Matroska with the phone's times, for recordings.
    pub mkv: Option<Fanout>,
    /// The same, its readers let go when the phone turns (the replay buffer).
    pub mkv_turn: Option<Fanout>,
    /// Why there's no sound ("" when there is, or it wasn't asked for).
    pub no_sound: String,
    pub live: Arc<Mutex<Live>>,
    pub started: Instant,
    control: Arc<Mutex<Option<TcpStream>>>,
    stop: Arc<AtomicBool>,
    /// Why it stopped by itself ("" while running).
    pub problem: Arc<Mutex<String>>,
    server: Mutex<Option<Child>>,
    forward: Option<u16>,
    sockets: Vec<TcpStream>,
}

fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

impl Session {
    /// Push the server, start it and connect. Blocking (a second or two); call off the UI thread.
    pub fn start(serial: &str, options: Options) -> Result<Session> {
        let server = dir().join("scrcpy-server");
        adb_out(&["-s", serial, "push", &server.to_string_lossy(), DEVICE_SERVER], Duration::from_secs(30))
            .context("couldn't send scrcpy's server to the phone")?;
        let scid = (random() as u32) & 0x7fff_ffff;
        let out = adb_out(
            &["-s", serial, "forward", "tcp:0", &format!("localabstract:scrcpy_{scid:08x}")],
            Duration::from_secs(10),
        )?;
        let port: u16 = out.trim().lines().last().unwrap_or("").trim().parse().context("adb forward gave no port")?;
        let mut cmd = adb();
        cmd.args(["-s", serial, "shell", &format!("CLASSPATH={DEVICE_SERVER}"), "app_process", "/"])
            .arg("com.genymobile.scrcpy.Server")
            .arg(VERSION)
            .args(options.server_args(scid))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("couldn't start adb")?;
        proc::end_with_neria(&child);
        // The server's own lines: kept for a plain error when it fails.
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        for out in [child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), child.stderr.take().map(|e| Box::new(e) as _)]
            .into_iter()
            .flatten()
        {
            let log = log.clone();
            std::thread::spawn(move || {
                for l in BufReader::new(out).lines().map_while(|l| l.ok()) {
                    let mut g = log.lock().unwrap();
                    if g.len() < 200 {
                        g.push(l);
                    }
                }
            });
        }
        match Session::connect(port, serial, options, Some(child), Some(port)) {
            Ok(mut s) => {
                // The server sends the model code ("2412DPC0AG"); the phone's own name is nicer.
                if let Ok(i) = info(serial) {
                    s.name = i.name;
                }
                Ok(s)
            }
            Err(e) => {
                let _ = adb_out(&["-s", serial, "forward", "--remove", &format!("tcp:{port}")], Duration::from_secs(5));
                let lines = log.lock().unwrap().clone();
                if let Some(l) = lines.iter().rev().find(|l| l.contains("ERROR") || l.contains("Exception")) {
                    bail!("{}", explain_server(l));
                }
                Err(e)
            }
        }
    }

    /// Connect to a server listening (through a tunnel) on `port`: the sockets in order,
    /// the dummy byte and the name on the first. Also used with the fake phone.
    pub fn connect(port: u16, serial: &str, options: Options, server: Option<Child>, forward: Option<u16>) -> Result<Session> {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        // The server takes a moment to listen; until then adb accepts and closes at once.
        let mut first = None;
        for _ in 0..100 {
            if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
                s.set_read_timeout(Some(Duration::from_secs(2)))?;
                let mut b = [0u8; 1];
                if s.read_exact(&mut b).is_ok() {
                    first = Some(s);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let Some(mut first) = first else {
            bail!("The phone didn't answer. Unlock it and try again.");
        };
        // Every socket first: the server sends the name once all are connected.
        let wanted = [options.video, options.audio, options.control].iter().filter(|w| **w).count();
        let mut rest = Vec::new();
        while rest.len() + 1 < wanted {
            rest.push(TcpStream::connect_timeout(&addr, Duration::from_secs(5))?);
        }
        first.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut name = [0u8; 64];
        first.read_exact(&mut name)?;
        let name = String::from_utf8_lossy(&name).trim_end_matches('\0').trim().to_string();
        let mut sockets = vec![first];
        sockets.extend(rest);
        for s in &sockets {
            s.set_read_timeout(Some(Duration::from_secs(10)))?;
            let _ = s.set_nodelay(true);
        }
        let mut it = sockets.iter();
        let video_sock = if options.video { it.next().map(|s| s.try_clone()).transpose()? } else { None };
        let audio_sock = if options.audio { it.next().map(|s| s.try_clone()).transpose()? } else { None };
        let control_sock = if options.control { it.next().map(|s| s.try_clone()).transpose()? } else { None };

        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(Mutex::new(Live::default()));
        let problem = Arc::new(Mutex::new(String::new()));
        let mut session = Session {
            serial: serial.to_string(),
            name,
            options: options.clone(),
            video: None,
            audio: None,
            mkv: None,
            mkv_turn: None,
            no_sound: String::new(),
            live: live.clone(),
            started: Instant::now(),
            control: Arc::new(Mutex::new(None)),
            stop: stop.clone(),
            problem: problem.clone(),
            server: Mutex::new(server),
            forward,
            sockets: Vec::new(),
        };
        // Both streams' formats first: the Matroska streams need to know what they carry.
        let mut video_sock = video_sock;
        if let Some(s) = video_sock.as_mut() {
            match read_u32(s)? {
                CODEC_H264 => {}
                0 | 1 => bail!("The phone couldn't start its screen capture."),
                c => bail!("The phone sent an unexpected picture format ({c:08x})."),
            }
        }
        let mut audio_sock = audio_sock;
        if let Some(s) = audio_sock.as_mut() {
            match read_u32(s)? {
                CODEC_RAW => {}
                c => {
                    session.no_sound = match c {
                        0 => "This phone can't send its sound (it needs Android 11 or newer).".into(),
                        1 => "The phone couldn't start sending its sound.".into(),
                        c => format!("The phone sent an unexpected sound format ({c:08x})."),
                    };
                    audio_sock = None;
                }
            }
        }
        let (has_v, has_a) = (video_sock.is_some(), audio_sock.is_some());
        if !has_v && !has_a {
            bail!("{}", if session.no_sound.is_empty() { "The phone sent nothing." } else { &session.no_sound });
        }
        if has_v {
            session.video = Some(Fanout::new(true, stop.clone())?);
        }
        if has_a {
            session.audio = Some(Fanout::new(false, stop.clone())?);
        }
        session.mkv = Some(Fanout::mkv(has_v, has_a, false, stop.clone())?);
        session.mkv_turn = Some(Fanout::mkv(has_v, has_a, true, stop.clone())?);
        let mkvs: Vec<Feed> = [&session.mkv, &session.mkv_turn].into_iter().flatten().map(|f| f.feed()).collect();
        if let Some(s) = video_sock {
            let f = session.video.as_ref().unwrap().feed();
            let m = mkvs.clone();
            let (l, st, pr) = (live.clone(), stop.clone(), problem.clone());
            s.set_read_timeout(None)?;
            std::thread::spawn(move || {
                let mut r = BufReader::with_capacity(1 << 20, s);
                loop {
                    match read_packet(&mut r) {
                        Ok(Packet::Session { width, height }) => {
                            let mut g = l.lock().unwrap();
                            g.width = width;
                            g.height = height;
                            for x in &m {
                                x.size(width, height);
                            }
                        }
                        Ok(Packet::Media { config, key, pts, data }) => {
                            {
                                let mut g = l.lock().unwrap();
                                g.bytes += data.len() as u64;
                                if !config {
                                    g.frames += 1;
                                }
                                if key && !config {
                                    g.last_key = Some(Instant::now());
                                }
                            }
                            f.send(&data, config, key);
                            for x in &m {
                                x.mkv_video(&data, config, key, pts);
                            }
                        }
                        Err(e) => {
                            if !st.load(Ordering::SeqCst) {
                                *pr.lock().unwrap() = if e.kind() == ErrorKind::UnexpectedEof {
                                    "The phone stopped sending (unplugged, locked by a call, or Wi-Fi dropped).".into()
                                } else {
                                    format!("The connection to the phone broke: {e}")
                                };
                                st.store(true, Ordering::SeqCst);
                            }
                            f.close();
                            for x in &m {
                                x.close();
                            }
                            break;
                        }
                    }
                }
            });
        }
        if let Some(s) = audio_sock {
            let f = session.audio.as_ref().unwrap().feed();
            let m = mkvs.clone();
            let (st, pr, video) = (stop.clone(), problem.clone(), has_v);
            s.set_read_timeout(None)?;
            std::thread::spawn(move || {
                let mut r = BufReader::with_capacity(1 << 16, s);
                loop {
                    match read_packet(&mut r) {
                        Ok(Packet::Media { config: false, pts, data, .. }) => {
                            f.send(&data, false, false);
                            for x in &m {
                                x.mkv_audio(&data, pts);
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {
                            // Sound alone: its end is the session's end.
                            if !video && !st.load(Ordering::SeqCst) {
                                *pr.lock().unwrap() = "The phone stopped sending its sound.".into();
                                st.store(true, Ordering::SeqCst);
                            }
                            f.close();
                            if !video {
                                for x in &m {
                                    x.close();
                                }
                            }
                            break;
                        }
                    }
                }
            });
        }
        if let Some(s) = control_sock {
            // Device messages (clipboard, acks): read and dropped, so the phone never waits on us.
            let mut r = s.try_clone()?;
            r.set_read_timeout(None)?;
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while matches!(r.read(&mut buf), Ok(n) if n > 0) {}
            });
            *session.control.lock().unwrap() = Some(s);
            if options.screen_off {
                session.send(Control::ScreenPower(false));
            }
            // Key frames on demand. Android counts the key frame interval in frames at an
            // assumed 60 fps, so a quiet screen (3–10 fps) sends one every 20 s or so: too few
            // for 2 s pieces, and whoever joins (a recording, a screenshot) would wait. Ask
            // for one ("reset video") when a reader waits, or when none came for 3 s.
            if has_v {
                let feeds: Vec<Feed> = [&session.video, &session.mkv, &session.mkv_turn].into_iter().flatten().map(|f| f.feed()).collect();
                let (ctl, l, st) = (session.control.clone(), live.clone(), stop.clone());
                std::thread::spawn(move || {
                    let mut asked = Instant::now();
                    while !st.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(250));
                        let (mut readers, mut waiting) = (0, false);
                        for f in &feeds {
                            let (n, w) = f.readers();
                            readers += n;
                            waiting |= w;
                        }
                        let stale = l.lock().unwrap().last_key.is_none_or(|t| t.elapsed() > Duration::from_secs(3));
                        if readers > 0 && (waiting || stale) && asked.elapsed() > Duration::from_millis(1500) {
                            asked = Instant::now();
                            let mut g = ctl.lock().unwrap();
                            if let Some(s) = g.as_mut() {
                                if s.write_all(&Control::ResetVideo.bytes()).is_err() {
                                    *g = None;
                                }
                            }
                        }
                    }
                });
            }
        }
        session.sockets = sockets;
        Ok(session)
    }

    pub fn running(&self) -> bool {
        !self.stop.load(Ordering::SeqCst)
    }

    /// Send a control message (dropped when there's no control socket).
    pub fn send(&self, c: Control) {
        let mut g = self.control.lock().unwrap();
        if let Some(s) = g.as_mut() {
            if s.write_all(&c.bytes()).is_err() {
                *g = None;
            }
        }
    }

    /// A tap or drag at `(x, y)` in the picture's own pixels.
    pub fn touch(&self, action: Touch, x: i32, y: i32) {
        let (w, h) = self.size();
        if w > 0 {
            self.send(Control::Touch { action, x, y, w: w as u16, h: h as u16 });
        }
    }

    pub fn scroll(&self, x: i32, y: i32, dx: f32, dy: f32) {
        let (w, h) = self.size();
        if w > 0 {
            self.send(Control::Scroll { x, y, w: w as u16, h: h as u16, dx, dy });
        }
    }

    /// Press and let go of a key.
    pub fn press(&self, code: i32) {
        self.send(Control::Key { down: true, code, meta: 0 });
        self.send(Control::Key { down: false, code, meta: 0 });
    }

    /// The picture's size now (0 before the first session packet).
    pub fn size(&self) -> (u32, u32) {
        let l = self.live.lock().unwrap();
        (l.width, l.height)
    }

    /// A full-size PNG of the next key frame (two seconds at most).
    pub fn screenshot(&self, out: &Path) -> Result<()> {
        let Some(v) = &self.video else { bail!("No picture to take.") };
        let res = proc::command(paths::tool("ffmpeg"))
            // A still screen sends little: don't wait to read megabytes first (the reader
            // starts at a key frame with its SPS, which is all there is to know).
            .args(["-hide_banner", "-v", "error", "-y", "-probesize", "32", "-analyzeduration", "0", "-f", "h264", "-i", &v.url()])
            .args(["-frames:v", "1", "-update", "1"])
            .arg(out)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()?;
        if !res.status.success() || !out.is_file() {
            bail!("Couldn't take the screenshot: {}", String::from_utf8_lossy(&res.stderr).lines().last().unwrap_or(""));
        }
        Ok(())
    }

    pub fn stop(&self) {
        if self.stop.swap(true, Ordering::SeqCst) && self.server.lock().unwrap().is_none() {
            return;
        }
        if self.options.screen_off {
            self.send(Control::ScreenPower(true));
        }
        for s in &self.sockets {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(f) = &self.video {
            f.close();
        }
        for f in [&self.audio, &self.mkv, &self.mkv_turn].into_iter().flatten() {
            f.close();
        }
        if let Some(mut c) = self.server.lock().unwrap().take() {
            proc::kill_tree(&mut c);
            let _ = c.wait();
        }
        if let Some(p) = self.forward {
            let _ = adb_out(&["-s", &self.serial, "forward", "--remove", &format!("tcp:{p}")], Duration::from_secs(5));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

fn explain_server(line: &str) -> String {
    let l = line.to_lowercase();
    if l.contains("version") && l.contains("server") {
        "The phone has a different scrcpy server. Try again (Neria sends its own each time).".into()
    } else if l.contains("encoder") || l.contains("mediacodec") {
        "The phone's video encoder refused to start. Try again, or lower the picture size.".into()
    } else {
        format!("The phone couldn't start mirroring: {}", line.trim())
    }
}

// ---------------------------------------------------------------------------------------------
// The replay buffer and recordings: ffmpeg reading the relay

/// ffmpeg's input: one of a session's Matroska streams (the picture and the sound with the
/// phone's own times). Returns whether it has a picture and sound.
fn input(cmd: &mut Command, s: &Session, f: &Fanout) -> (bool, bool) {
    // A short look is enough (the header says what's in it). Not `-fflags nobuffer`: it drops
    // what came during the look, and the sound then starts a second early.
    cmd.args(["-probesize", "100000", "-analyzeduration", "500000", "-f", "matroska", "-i", &f.url()]);
    (s.video.is_some(), s.audio.is_some())
}

/// One ffmpeg reading a session until it ends or is stopped.
struct Ffmpeg {
    child: Mutex<Option<Child>>,
    errors: Arc<Mutex<String>>,
}

impl Ffmpeg {
    fn spawn(mut cmd: Command) -> Result<Ffmpeg> {
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().context("failed to start ffmpeg")?;
        proc::end_with_neria(&child);
        let errors = Arc::new(Mutex::new(String::new()));
        if let Some(mut e) = child.stderr.take() {
            let errs = errors.clone();
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = e.read_to_string(&mut s);
                *errs.lock().unwrap() = s;
            });
        }
        Ok(Ffmpeg { child: Mutex::new(Some(child)), errors })
    }

    fn running(&self) -> bool {
        self.child.lock().unwrap().as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_none())
    }

    /// Ask ffmpeg to finish the file ("q"), then wait for it (killed after `wait`).
    fn finish(&self, wait: Duration) -> Result<()> {
        let Some(mut c) = self.child.lock().unwrap().take() else { return Ok(()) };
        if let Some(mut i) = c.stdin.take() {
            let _ = i.write_all(b"q");
        }
        let began = Instant::now();
        while began.elapsed() < wait {
            if c.try_wait()?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        proc::kill_tree(&mut c);
        let _ = c.wait();
        Ok(())
    }

    fn kill(&self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            proc::kill_tree(&mut c);
            let _ = c.wait();
        }
    }

    fn last_error(&self) -> String {
        self.errors.lock().unwrap().lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
    }
}

/// The phone's last `keep` seconds, always: the same 2 s pieces as instant replay. When the
/// phone turns it starts again (pieces of two shapes don't join into one file).
pub struct PhoneBuffer {
    pub dir: PathBuf,
    pub keep: u32,
    name: String,
    ff: Arc<Mutex<Option<Ffmpeg>>>,
    stop: Arc<AtomicBool>,
    /// Why it stopped ("" while running).
    problem: Arc<Mutex<String>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl PhoneBuffer {
    pub fn start(s: &Session, keep: u32) -> Result<PhoneBuffer> {
        let dir = paths::app_dir().join("phone-buffer");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let Some(f) = &s.mkv_turn else { bail!("Nothing to keep.") };
        let mut cmd = proc::command(paths::tool("ffmpeg"));
        cmd.args(["-hide_banner", "-v", "error", "-nostats", "-y"]);
        let (v, a) = input(&mut cmd, s, f);
        if v {
            cmd.args(["-map", "0:v", "-c:v", "copy"]);
        }
        if a {
            cmd.args(["-map", "0:a", "-c:a", "aac", "-b:a", "160k"]);
        }
        let wrap = keep / capture::PIECE_SECS + 3;
        cmd.args(["-f", "segment", "-segment_time", &capture::PIECE_SECS.to_string()]);
        cmd.args(["-segment_wrap", &wrap.to_string(), "-reset_timestamps", "1", "-flush_packets", "1"]);
        cmd.arg(dir.join("piece%03d.ts"));
        // The same command again after a turn: a `Command` can't be cloned, so keep its parts.
        let program = cmd.get_program().to_owned();
        let args: Vec<std::ffi::OsString> = cmd.get_args().map(|a| a.to_owned()).collect();
        let ff = Arc::new(Mutex::new(Some(Ffmpeg::spawn(cmd)?)));
        let stop = Arc::new(AtomicBool::new(false));
        let problem = Arc::new(Mutex::new(String::new()));
        let (ff2, st, pr, d, gone) = (ff.clone(), stop.clone(), problem.clone(), dir.clone(), s.stop.clone());
        let worker = std::thread::spawn(move || {
            while !st.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
                let ended = ff2.lock().unwrap().as_ref().is_some_and(|f| !f.running());
                if !ended {
                    continue;
                }
                if st.load(Ordering::SeqCst) {
                    break;
                }
                if gone.load(Ordering::SeqCst) {
                    // The phone itself went: nothing more to keep.
                    break;
                }
                // The phone turned: start again with the new shape.
                let old = ff2.lock().unwrap().take();
                let err = old.as_ref().map(|f| f.last_error()).unwrap_or_default();
                for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
                let mut cmd = proc::command(&program);
                cmd.args(&args);
                match Ffmpeg::spawn(cmd) {
                    Ok(f) => *ff2.lock().unwrap() = Some(f),
                    Err(e) => {
                        *pr.lock().unwrap() = format!("{e} {err}");
                        break;
                    }
                }
            }
        });
        Ok(PhoneBuffer { dir, keep, name: s.name.clone(), ff, stop, problem, worker: Mutex::new(Some(worker)) })
    }

    pub fn running(&self) -> bool {
        self.ff.lock().unwrap().as_ref().is_some_and(|f| f.running())
    }

    pub fn problem(&self) -> String {
        let p = self.problem.lock().unwrap().clone();
        if !p.is_empty() {
            return p;
        }
        match self.ff.lock().unwrap().as_ref() {
            Some(f) if !f.running() => f.last_error(),
            _ => String::new(),
        }
    }

    /// The last `seconds` as an MP4 in `out_dir`: "Pixel 7 (last 30s).mp4". Mirroring goes on.
    pub fn save(&self, seconds: u32, out_dir: &Path) -> Result<PathBuf> {
        save_from(&self.dir, seconds, out_dir, &self.name)
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(f) = self.ff.lock().unwrap().take() {
            f.kill();
        }
        if let Some(w) = self.worker.lock().unwrap().take() {
            let _ = w.join();
        }
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for PhoneBuffer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// [`PhoneBuffer::save`] from its folder, to run off the UI thread.
pub fn save_from(dir: &Path, seconds: u32, out_dir: &Path, name: &str) -> Result<PathBuf> {
    capture::save_from(dir, seconds, out_dir, &[], name)
}

/// Pieces left by a crash.
pub fn clean_leftovers() {
    let _ = std::fs::remove_dir_all(paths::app_dir().join("phone-buffer"));
}

/// A recording of the phone (MP4), or of its sound alone (M4A), until `stop`.
pub struct Recording {
    pub file: PathBuf,
    pub started: Instant,
    ff: Ffmpeg,
}

impl Recording {
    /// Starts recording `s` into `out_dir` as `name`.mp4 (.m4a without a picture), e.g.
    /// "Pixel 7 2026-11-22 14-31".
    pub fn start(s: &Session, out_dir: &Path, name: &str) -> Result<Recording> {
        std::fs::create_dir_all(out_dir)?;
        let Some(f) = &s.mkv else { bail!("Nothing to record.") };
        let ext = if s.video.is_some() { "mp4" } else { "m4a" };
        let name = crate::music::sanitize(name);
        let file = crate::clip::unique_path(out_dir, &name, ext);
        let mut cmd = proc::command(paths::tool("ffmpeg"));
        cmd.args(["-hide_banner", "-v", "error", "-nostats", "-y"]);
        let (v, a) = input(&mut cmd, s, f);
        if v {
            cmd.args(["-map", "0:v", "-c:v", "copy"]);
        }
        if a {
            cmd.args(["-map", "0:a", "-c:a", "aac", "-b:a", "192k"]);
        }
        cmd.args(["-movflags", "+faststart"]).arg(&file);
        let ff = Ffmpeg::spawn(cmd)?;
        Ok(Recording { file, started: Instant::now(), ff })
    }

    pub fn running(&self) -> bool {
        self.ff.running()
    }

    /// Finish the file. Returns it when it's there.
    pub fn stop(&self) -> Result<PathBuf> {
        self.ff.finish(Duration::from_secs(10))?;
        if self.file.metadata().is_ok_and(|m| m.len() > 0) {
            Ok(self.file.clone())
        } else {
            let e = self.ff.last_error();
            bail!("The recording couldn't be saved{}", if e.is_empty() { String::new() } else { format!(": {e}") })
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The speaker: the phone's sound on this PC

/// Plays a session's sound on Windows' default speakers, at its own volume. Stops with the
/// session (or when dropped). Keeps at most a fifth of a second queued, so it never drifts
/// behind the picture.
pub struct Speaker {
    volume: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Speaker {
    pub fn start(s: &Session, volume: f32) -> Result<Speaker> {
        let Some(a) = &s.audio else { bail!("{}", if s.no_sound.is_empty() { "No sound." } else { &s.no_sound }) };
        let rx = a.tap();
        let vol = Arc::new(AtomicU32::new(volume.to_bits()));
        let stop = Arc::new(AtomicBool::new(false));
        let (v, st) = (vol.clone(), stop.clone());
        let (ok_tx, ok_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            if let Err(e) = imp::play(rx, v, st, &ok_tx) {
                let _ = ok_tx.send(Err(e.to_string()));
            }
        });
        match ok_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => bail!("Couldn't play the sound: {e}"),
            Err(_) => bail!("Couldn't play the sound: the speakers didn't answer."),
        }
        Ok(Speaker { volume: vol, stop, thread: Some(thread) })
    }

    /// 0.0–1.0 (up to 2.0 boosts).
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 2.0).to_bits(), Ordering::SeqCst);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::SeqCst))
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Scale 16-bit samples, clipping.
fn apply_volume(pcm: &mut [u8], vol: f32) {
    if (vol - 1.0).abs() < 1e-3 {
        return;
    }
    for s in pcm.chunks_exact_mut(2) {
        let v = (i16::from_le_bytes([s[0], s[1]]) as f32 * vol).clamp(-32768.0, 32767.0) as i16;
        s.copy_from_slice(&v.to_le_bytes());
    }
}

/// Trim a queue of PCM to at most `max` bytes (oldest first), keeping whole frames.
fn trim_queue(q: &mut VecDeque<u8>, max: usize, keep: usize) {
    if q.len() > max {
        let drop = (q.len() - keep) / 4 * 4;
        q.drain(..drop);
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};

    const RATE: u32 = 48_000;
    const FRAME: usize = 4;

    pub fn play(rx: Receiver<Chunk>, vol: Arc<AtomicU32>, stop: Arc<AtomicBool>, ok: &std::sync::mpsc::Sender<std::result::Result<(), String>>) -> Result<()> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
            let fmt = WAVEFORMATEX {
                wFormatTag: 1,
                nChannels: 2,
                nSamplesPerSec: RATE,
                nAvgBytesPerSec: RATE * FRAME as u32,
                nBlockAlign: FRAME as u16,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            // A 100 ms buffer: small enough to stay close to the picture.
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &fmt, None)?;
            let size = client.GetBufferSize()? as usize;
            let render: IAudioRenderClient = client.GetService()?;
            client.Start()?;
            let _ = ok.send(Ok(()));
            let mut q: VecDeque<u8> = VecDeque::new();
            let ms = |n: usize| RATE as usize * FRAME * n / 1000;
            let mut started = false;
            while !stop.load(Ordering::SeqCst) {
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(c) => q.extend(c.iter()),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
                while let Ok(c) = rx.try_recv() {
                    q.extend(c.iter());
                }
                // Wait for a little before starting (no crackle), then never fall behind.
                if !started && q.len() < ms(40) {
                    continue;
                }
                started = true;
                trim_queue(&mut q, ms(200), ms(60));
                let pad = client.GetCurrentPadding()? as usize;
                let free = size.saturating_sub(pad).min(q.len() / FRAME);
                if free == 0 {
                    continue;
                }
                let data = render.GetBuffer(free as u32)?;
                let out = std::slice::from_raw_parts_mut(data, free * FRAME);
                for (o, b) in out.iter_mut().zip(q.drain(..free * FRAME)) {
                    *o = b;
                }
                apply_volume(out, f32::from_bits(vol.load(Ordering::SeqCst)));
                render.ReleaseBuffer(free as u32, 0)?;
            }
            let _ = client.Stop();
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn play(_: Receiver<Chunk>, _: Arc<AtomicU32>, _: Arc<AtomicBool>, _: &std::sync::mpsc::Sender<std::result::Result<(), String>>) -> Result<()> {
        bail!("not on this system")
    }
}

// ---------------------------------------------------------------------------------------------
// A fake phone, for checks without one

/// A pretend scrcpy server on a local port, sending `h264` (an Annex B file with access unit
/// delimiters, e.g. ffmpeg's `-x264-params aud=1`) at `fps`, looped, plus a 440 Hz tone as its
/// sound. `Session::connect(port, …)` talks to it as to a phone.
pub mod fake {
    use super::*;

    /// Split an Annex B stream into (config, access units with their key flag).
    pub fn split(h264: &[u8]) -> (Vec<u8>, Vec<(bool, Vec<u8>)>) {
        let mut nals: Vec<&[u8]> = Vec::new();
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 3 <= h264.len() {
            if h264[i] == 0 && h264[i + 1] == 0 && h264[i + 2] == 1 {
                let s = if i > 0 && h264[i - 1] == 0 { i - 1 } else { i };
                starts.push((s, i + 3));
                i += 3;
            } else {
                i += 1;
            }
        }
        for (k, &(s, _)) in starts.iter().enumerate() {
            let end = starts.get(k + 1).map(|n| n.0).unwrap_or(h264.len());
            nals.push(&h264[s..end]);
        }
        let ty = |n: &[u8]| {
            let off = if n.len() > 3 && n[2] == 1 { 3 } else { 4 };
            n.get(off).map(|b| b & 0x1f).unwrap_or(0)
        };
        let mut config = Vec::new();
        let mut units: Vec<(bool, Vec<u8>)> = Vec::new();
        let mut sliced = false;
        for n in nals {
            if matches!(ty(n), 1 | 5) {
                sliced = true;
            }
            match ty(n) {
                // The first SPS and PPS only (an encoder may repeat them at each key frame).
                7 | 8 if !sliced => config.extend_from_slice(n),
                7 | 8 => {}
                9 => units.push((false, Vec::new())),
                t => {
                    if units.is_empty() {
                        units.push((false, Vec::new()));
                    }
                    let u = units.last_mut().unwrap();
                    if t == 5 {
                        u.0 = true;
                    }
                    u.1.extend_from_slice(n);
                }
            }
        }
        units.retain(|u| !u.1.is_empty());
        (config, units)
    }

    /// Serve on a free port; returns it. Runs until `stop`.
    pub fn serve(h264: Vec<u8>, size: (u32, u32), fps: u32, sound: bool, stop: Arc<AtomicBool>) -> Result<u16> {
        let (config, units) = split(&h264);
        if units.is_empty() {
            bail!("no access units in the file (make it with -x264-params aud=1)");
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        std::thread::spawn(move || {
            let Ok((mut video, _)) = listener.accept() else { return };
            // As the real server: the dummy byte at once, the name once every socket is there.
            let _ = video.write_all(&[0]);
            let audio = if sound { listener.accept().ok().map(|a| a.0) } else { None };
            let control = listener.accept().ok().map(|c| c.0);
            let mut name = [0u8; 64];
            name[..10].copy_from_slice(b"Fake Phone");
            let _ = video.write_all(&name);
            if let Some(mut c) = control {
                std::thread::spawn(move || {
                    let mut b = [0u8; 256];
                    while matches!(c.read(&mut b), Ok(n) if n > 0) {}
                });
            }
            if let Some(mut a) = audio {
                let st = stop.clone();
                let _ = a.write_all(&CODEC_RAW.to_be_bytes());
                std::thread::spawn(move || {
                    let began = Instant::now();
                    let mut sent = 0u64;
                    while !st.load(Ordering::SeqCst) {
                        // 10 ms of a 440 Hz tone at a time, on time.
                        let due = (began.elapsed().as_secs_f64() * 100.0) as u64;
                        while sent < due {
                            let mut pcm = Vec::with_capacity(1920);
                            for k in 0..480u64 {
                                let t = (sent * 480 + k) as f64 / 48_000.0;
                                let v = ((t * 440.0 * std::f64::consts::TAU).sin() * 6000.0) as i16;
                                pcm.extend(v.to_le_bytes());
                                pcm.extend(v.to_le_bytes());
                            }
                            let p = Packet::Media { config: false, key: false, pts: sent * 10_000, data: pcm };
                            if write_packet(&mut a, &p).is_err() {
                                return;
                            }
                            sent += 1;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                });
            }
            let _ = video.write_all(&CODEC_H264.to_be_bytes());
            let _ = write_packet(&mut video, &Packet::Session { width: size.0, height: size.1 });
            let _ = write_packet(&mut video, &Packet::Media { config: true, key: false, pts: 0, data: config.clone() });
            let began = Instant::now();
            let mut n = 0u64;
            while !stop.load(Ordering::SeqCst) {
                let (key, unit) = &units[(n as usize) % units.len()];
                let pts = n * 1_000_000 / fps as u64;
                if write_packet(&mut video, &Packet::Media { config: false, key: *key, pts, data: unit.clone() }).is_err() {
                    return;
                }
                n += 1;
                let due = Duration::from_micros(n * 1_000_000 / fps as u64);
                if let Some(wait) = due.checked_sub(began.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
        });
        Ok(port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_devices() {
        let text = "List of devices attached\n\
            R58M12345      device usb:1-1 product:beyond1 model:SM_G973F device:beyond1 transport_id:1\n\
            adb-2B121FDH200ABC-x9Yq._adb-tls-connect._tcp device product:panther model:Pixel_7 device:panther transport_id:3\n\
            192.168.1.5:5555 unauthorized transport_id:2\n\
            adb-VOUSK-shbQqu (2)._adb-tls-connect._tcp device product:rodin_global model:2412DPC0AG transport_id:1\n";
        let d = parse_devices(text);
        assert_eq!(d.len(), 4);
        assert_eq!(d[3].serial, "adb-VOUSK-shbQqu (2)._adb-tls-connect._tcp");
        assert!(d[3].wifi && d[3].state == State::Ready && d[3].model == "2412DPC0AG");
        assert_eq!(d[0], Device { serial: "R58M12345".into(), state: State::Ready, model: "SM G973F".into(), wifi: false });
        assert!(d[1].wifi && d[1].model == "Pixel 7");
        assert_eq!(d[2].state, State::Unauthorized);
        assert!(problem(&d[2]).unwrap().contains("Allow"));
    }

    #[test]
    fn reads_services_and_finds_the_scan() {
        let text = "List of discovered mdns services\n\
            adb-2B121FDH200ABC-x9Yq\t_adb-tls-connect._tcp\t192.168.1.42:37011\n\
            neria-abc123\t_adb-tls-pairing._tcp\t192.168.1.42:41234\n";
        let s = parse_services(text);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].kind, Kind::Connect);
        assert_eq!(s[1].ip(), "192.168.1.42");
        let p = Pairing { name: "neria-abc123".into(), password: "x".into() };
        assert_eq!(p.found(&s).unwrap().addr, "192.168.1.42:41234");
        assert!(Pairing::new().qr_text().starts_with("WIFI:T:ADB;S:neria-"));
    }

    #[test]
    fn qr_codes() {
        let (w, m) = qr(&Pairing::new().qr_text()).unwrap();
        assert_eq!(m.len(), w * w);
        // Finder pattern in the top-left corner.
        assert!(m[0] && m[6] && !m[w + 1]);
    }

    #[test]
    fn packets_round_trip() {
        let mut buf = Vec::new();
        let ps = [
            Packet::Session { width: 1080, height: 2400 },
            Packet::Media { config: true, key: false, pts: 0, data: vec![0, 0, 0, 1, 0x67] },
            Packet::Media { config: false, key: true, pts: 16_666, data: vec![1, 2, 3] },
        ];
        for p in &ps {
            write_packet(&mut buf, p).unwrap();
        }
        let mut r = &buf[..];
        for p in &ps {
            assert_eq!(&read_packet(&mut r).unwrap(), p);
        }
    }

    #[test]
    fn control_messages_match_scrcpy() {
        // Sizes from scrcpy's control_msg.c: touch 32, scroll 21, key 14.
        assert_eq!(Control::Touch { action: Touch::Down, x: 1, y: 2, w: 3, h: 4 }.bytes().len(), 32);
        assert_eq!(Control::Scroll { x: 0, y: 0, w: 1, h: 1, dx: 0.0, dy: -1.0 }.bytes().len(), 21);
        assert_eq!(Control::Key { down: true, code: key::BACK, meta: 0 }.bytes().len(), 14);
        let t = Control::Touch { action: Touch::Move, x: 100, y: 200, w: 1080, h: 2400 }.bytes();
        assert_eq!(&t[0..2], &[2, 2]);
        assert_eq!(&t[2..10], &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe]);
        assert_eq!(&t[22..24], &[0xff, 0xff]);
        // Scroll of 16 is full scale.
        let s = Control::Scroll { x: 0, y: 0, w: 1, h: 1, dx: 16.0, dy: -16.0 }.bytes();
        assert_eq!(&s[13..17], &[0x7f, 0xff, 0x80, 0x00]);
        // Text is cut at 300 bytes on a character boundary.
        let long = "é".repeat(200);
        let b = Control::Text(long).bytes();
        assert_eq!(u32::from_be_bytes(b[1..5].try_into().unwrap()), 300);
    }

    #[test]
    fn server_args() {
        let a = Options::default().server_args(0x1234);
        assert!(a.contains(&"scid=00001234".to_string()));
        assert!(a.contains(&"audio_codec=raw".to_string()));
        assert!(a.iter().any(|x| x.starts_with("video_codec_options=i-frame-interval:int=")));
        let s = Options::sound_only().server_args(1);
        assert!(s.contains(&"video=false".to_string()) && s.contains(&"audio_dup=true".to_string()));
    }

    #[test]
    fn names_people_know() {
        assert_eq!(Info::from_props("", "", "Pixel 7", "15", "35", "x", "Google").name, "Pixel 7");
        assert_eq!(Info::from_props("Galaxy S23", "", "SM-S911B", "14", "34", "x", "samsung").name, "Galaxy S23");
        assert_eq!(Info::from_props("", "", "SM-S911B", "14", "34", "x", "samsung").name, "Samsung SM-S911B");
        assert!(!Info::from_props("", "", "", "10", "29", "", "").has_sound());
    }

    #[test]
    fn speaker_queue_stays_short() {
        let mut q: VecDeque<u8> = (0..10_000u32).map(|i| i as u8).collect();
        trim_queue(&mut q, 4000, 1000);
        assert!(q.len() <= 1003 && q.len() % 4 == 0);
        let mut pcm = 1000i16.to_le_bytes().to_vec();
        apply_volume(&mut pcm, 0.5);
        assert_eq!(i16::from_le_bytes([pcm[0], pcm[1]]), 500);
    }

    #[test]
    fn fanout_waits_for_a_key_frame() {
        let stop = Arc::new(AtomicBool::new(false));
        let f = Fanout::new(true, stop.clone()).unwrap();
        let rx = f.tap();
        f.send(b"cfg", true, false);
        f.send(b"p1", false, false);
        f.send(b"k1", false, true);
        f.send(b"p2", false, false);
        let got: Vec<Vec<u8>> = rx.try_iter().map(|c| c.to_vec()).collect();
        assert_eq!(got, vec![b"cfg".to_vec(), b"k1".to_vec(), b"p2".to_vec()]);
        stop.store(true, Ordering::SeqCst);
    }
}
