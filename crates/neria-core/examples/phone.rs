//! Your Android phone without the UI:
//! `NERIA_HOME=target/debug cargo run -p neria-core --example phone -- <what> [options]`
//!
//! - `--get` / `--remove` / `--status`: scrcpy's release in `bin/phone/` (fetched on first use).
//! - `devices`: what Neria's adb sees (phones, with their names), phones on the Wi-Fi with
//!   Wireless debugging on, the phones remembered in `phones.json`.
//! - `qr`: show the pairing QR code here, wait for the phone to scan it (Developer options →
//!   Wireless debugging → Pair device with QR code), pair, connect, remember it.
//! - `pair <ip:port> <code>`: pair with the 6 digits instead; `connect <ip:port>`.
//! - `mirror <serial|fake> <seconds>`: Neria's own mirror (no window): the replay buffer, the
//!   sound on the speakers, then stops. `fake` is a pretend phone (a test picture and a tone),
//!   so the whole way (relay → buffer → save) can be checked without one.
//!   `--clip <s>@<t>` saves the last s seconds at t; `--record` records the whole run;
//!   `--shot <t>` a screenshot at t; `--no-sound`, `--mute` (sound kept, not played),
//!   `--screen-off` (`--screen-on <t>`: on again at t), `--keep <s>` (60); `--tap x,y@t`, `--key back|home|apps@t`;
//!   `--out <dir>` (else `target/phone-out`).
//! - `sound <serial|fake> <seconds> [--record] [--volume 0.5]`: Sound only.
//! - `window <serial> [--present] [--seconds <n>]`: scrcpy's own window (the quick mirror;
//!   `--present`: bare, titled "Phone screen · …", on top, showing taps).
//!
//! `<serial>` can be left as `-` for the only phone connected.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use neria_core::phone::{self, Options, PhoneBuffer, Recording, Session, Speaker, Touch};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let has = |name: &str| args.iter().any(|a| a == name);
    let free: Vec<&String> = {
        let mut out = Vec::new();
        let mut skip = false;
        for a in &args {
            if skip {
                skip = false;
                continue;
            }
            if a.starts_with("--") {
                skip = ["--clip", "--shot", "--keep", "--out", "--volume", "--seconds", "--tap", "--key", "--screen-on"].contains(&a.as_str());
                continue;
            }
            out.push(a);
        }
        out
    };

    if has("--remove") {
        phone::remove()?;
        println!("removed {}", phone::dir().display());
        return Ok(());
    }
    if has("--status") {
        println!("scrcpy {} {}", phone::VERSION, if phone::ready() { "ready" } else { "not fetched" });
        println!("another adb on 5037: {}", phone::other_adb_running());
        return Ok(());
    }
    if has("--get") || !phone::ready() {
        let stop = AtomicBool::new(false);
        let mut last = 101;
        phone::install(&stop, |d, t| {
            let pct = t.map(|t| d * 100 / t.max(1)).unwrap_or(0);
            if pct / 20 != last / 20 {
                eprintln!("downloading scrcpy {}… {pct}%", phone::VERSION);
                last = pct;
            }
        })?;
        println!("scrcpy {} ready in {}", phone::VERSION, phone::dir().display());
        if has("--get") {
            return Ok(());
        }
    }

    let what = free.first().map(|s| s.as_str()).unwrap_or("devices");
    let out_dir = val("--out").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("target/phone-out"));
    let fake = free.get(1).is_some_and(|s| s.as_str() == "fake");
    if !fake {
        phone::start_server()?;
    }
    let serial = || -> Result<String> {
        match free.get(1).map(|s| s.as_str()) {
            Some("-") | None => {
                // A paired phone reconnects a few seconds after the server starts.
                let mut ready = Vec::new();
                for _ in 0..20 {
                    ready = phone::devices()?.into_iter().filter(|d| d.state == phone::State::Ready).collect::<Vec<_>>();
                    if !ready.is_empty() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                match ready.as_slice() {
                    [one] => Ok(one.serial.clone()),
                    [] => bail!("no phone connected (try `devices` or `qr`)"),
                    _ => bail!("several phones: name one ({})", ready.iter().map(|d| d.serial.as_str()).collect::<Vec<_>>().join(", ")),
                }
            }
            Some(s) => Ok(s.to_string()),
        }
    };

    match what {
        "devices" => {
            // Paired phones reconnect a moment after the server starts.
            std::thread::sleep(Duration::from_secs(2));
            println!("adb on port {} (another adb on 5037: {})", phone::ADB_PORT, phone::other_adb_running());
            let devs = phone::devices()?;
            if devs.is_empty() {
                println!("no phones connected");
            }
            for d in &devs {
                let info = if d.state == phone::State::Ready { phone::info(&d.serial).ok() } else { None };
                println!(
                    "{}  {:?}  {}  {}{}",
                    d.serial,
                    d.state,
                    if d.wifi { "Wi-Fi" } else { "USB" },
                    info.as_ref().map(|i| format!("{} · Android {} (sdk {})", i.name, i.android, i.sdk)).unwrap_or(d.model.clone()),
                    phone::problem(d).map(|p| format!("  → {p}")).unwrap_or_default()
                );
                if let Some(i) = info {
                    phone::remember(&i, d.wifi);
                }
            }
            for s in phone::services().unwrap_or_default() {
                println!("on the Wi-Fi: {} {:?} {}", s.instance, s.kind, s.addr);
            }
            for k in phone::known() {
                println!("remembered: {} · Android {} · {}", k.name, k.android, if k.wifi { "Wi-Fi" } else { "USB" });
            }
        }
        "qr" => {
            let p = phone::Pairing::new();
            print_qr(&p.qr_text())?;
            println!("On the phone: Developer options → Wireless debugging → Pair device with QR code, then scan this.");
            println!("(looking for {} on the network…)", p.name);
            let began = Instant::now();
            let mut said_on = false;
            loop {
                let seen = phone::look(&p);
                if seen.wireless_on && !said_on {
                    println!("a phone with Wireless debugging on is on this Wi-Fi");
                    said_on = true;
                }
                if let Some(s) = seen.scanned {
                    println!("scanned: pairing with {}…", s.addr);
                    phone::pair(&s.addr, &p.password)?;
                    println!("paired; connecting…");
                    let d = phone::wait_connected(s.ip(), Duration::from_secs(20))?;
                    let info = phone::info(&d.serial)?;
                    phone::remember(&info, true);
                    println!("connected: {} · Android {} ({})", info.name, info.android, d.serial);
                    break;
                }
                if began.elapsed() > Duration::from_secs(300) {
                    bail!("no scan in 5 minutes");
                }
                std::thread::sleep(Duration::from_millis(700));
            }
        }
        "pair" => {
            let (Some(addr), Some(code)) = (free.get(1), free.get(2)) else { bail!("pair <ip:port> <code>") };
            phone::pair(addr, code)?;
            let ip = addr.rsplit_once(':').map(|x| x.0).unwrap_or(addr);
            let d = phone::wait_connected(ip, Duration::from_secs(20))?;
            println!("paired and connected: {}", d.serial);
        }
        "connect" => {
            let Some(addr) = free.get(1) else { bail!("connect <ip:port>") };
            phone::connect(addr)?;
            println!("connected to {addr}");
        }
        "mirror" | "sound" => {
            let secs: u64 = free.get(2).and_then(|s| s.parse().ok()).unwrap_or(20);
            let sound_only = what == "sound";
            let mut opts = if sound_only { Options::sound_only() } else { Options::default() };
            opts.audio = !has("--no-sound");
            opts.screen_off = has("--screen-off");
            let stop = Arc::new(AtomicBool::new(false));
            let started = Instant::now();
            let session = if fake {
                let h264 = fake_h264()?;
                let port = phone::fake::serve(h264, (540, 1200), 30, opts.audio, stop.clone())?;
                if sound_only {
                    bail!("the fake phone has no Sound only mode; use `mirror fake`");
                }
                Session::connect(port, "fake", opts, None, None)?
            } else {
                Session::start(&serial()?, opts)?
            };
            println!("connected to {} in {} ms", session.name, started.elapsed().as_millis());
            println!(
                "relay: picture {}  sound {}  matroska {}",
                session.video.as_ref().map(|v| v.url()).unwrap_or("-".into()),
                session.audio.as_ref().map(|v| v.url()).unwrap_or("-".into()),
                session.mkv.as_ref().map(|v| v.url()).unwrap_or("-".into())
            );
            if !session.no_sound.is_empty() {
                println!("no sound: {}", session.no_sound);
            }
            let speaker = if session.audio.is_some() && !has("--mute") {
                let v: f32 = val("--volume").and_then(|v| v.parse().ok()).unwrap_or(if fake { 0.15 } else { 1.0 });
                match Speaker::start(&session, v) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        println!("speaker: {e}");
                        None
                    }
                }
            } else {
                None
            };
            let keep: u32 = val("--keep").and_then(|s| s.parse().ok()).unwrap_or(60);
            let buffer = if sound_only || has("--no-buffer") { None } else { Some(PhoneBuffer::start(&session, keep)?) };
            let name = format!("{} {}", session.name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs());
            let rec = if has("--record") { Some(Recording::start(&session, &out_dir, &name)?) } else { None };
            let clip = val("--clip").and_then(|c| {
                let (s, t) = c.split_once('@')?;
                Some((s.parse::<u32>().ok()?, t.parse::<u64>().ok()?))
            });
            let shot: Option<u64> = val("--shot").and_then(|s| s.parse().ok());
            let tap = val("--tap").and_then(|t| {
                let (xy, at) = t.split_once('@')?;
                let (x, y) = xy.split_once(',')?;
                Some((x.parse::<i32>().ok()?, y.parse::<i32>().ok()?, at.parse::<u64>().ok()?))
            });
            let key = val("--key").and_then(|k| {
                let (name, at) = k.split_once('@')?;
                let code = match name {
                    "back" => phone::key::BACK,
                    "home" => phone::key::HOME,
                    "apps" => phone::key::APP_SWITCH,
                    _ => return None,
                };
                Some((code, at.parse::<u64>().ok()?))
            });
            let screen_on: Option<u64> = val("--screen-on").and_then(|s| s.parse().ok());
            let began = Instant::now();
            let mut done = (false, false, false, false);
            let mut screen_done = false;
            let mut last = Instant::now();
            let mut last_frames = 0u64;
            while began.elapsed() < Duration::from_secs(secs) && session.running() {
                let t = began.elapsed().as_secs();
                if let (Some((s, at)), false) = (clip, done.0) {
                    if t >= at {
                        done.0 = true;
                        let b = buffer.as_ref().unwrap();
                        match b.save(s, &out_dir) {
                            Ok(f) => println!("clipped the last {s} s → {} ({} KB)", f.display(), f.metadata()?.len() / 1024),
                            Err(e) => println!("clip failed: {e}"),
                        }
                    }
                }
                if let (Some(at), false) = (shot, done.1) {
                    if t >= at {
                        done.1 = true;
                        let f = out_dir.join(format!("{name}.png"));
                        std::fs::create_dir_all(&out_dir)?;
                        match session.screenshot(&f) {
                            Ok(()) => println!("screenshot → {}", f.display()),
                            Err(e) => println!("screenshot failed: {e}"),
                        }
                    }
                }
                if let (Some((x, y, at)), false) = (tap, done.2) {
                    if t >= at {
                        done.2 = true;
                        session.touch(Touch::Down, x, y);
                        session.touch(Touch::Up, x, y);
                        println!("tapped {x},{y}");
                    }
                }
                if let (Some((code, at)), false) = (key, done.3) {
                    if t >= at {
                        done.3 = true;
                        session.press(code);
                        println!("pressed key {code}");
                    }
                }
                if let (Some(at), false) = (screen_on, screen_done) {
                    if t >= at {
                        screen_done = true;
                        session.send(phone::Control::ScreenPower(true));
                        println!("asked the phone to turn its screen on");
                    }
                }
                if last.elapsed() >= Duration::from_secs(2) {
                    last = Instant::now();
                    let l = session.live.lock().unwrap().clone();
                    let el = session.started.elapsed().as_secs_f64();
                    // Frames in the last 2 s (a stall shows as 0).
                    let fps = (l.frames - last_frames) as f64 / 2.0;
                    last_frames = l.frames;
                    println!(
                        "{:>4.0}s  {}×{}  {:.0} fps  {:.1} Mbit/s  sound packets {}  readers v{} a{}{}",
                        el,
                        l.width,
                        l.height,
                        fps,
                        l.bytes as f64 * 8.0 / el / 1e6,
                        l.audio_packets,
                        session.video.as_ref().map(|v| v.readers()).unwrap_or(0),
                        session.audio.as_ref().map(|v| v.readers()).unwrap_or(0),
                        buffer.as_ref().filter(|b| !b.running()).map(|b| format!("  buffer stopped: {}", b.problem())).unwrap_or_default()
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if !session.running() {
                println!("stopped by itself: {}", session.problem.lock().unwrap());
            }
            if let Some(r) = rec {
                match r.stop() {
                    Ok(f) => println!("recorded → {} ({} KB)", f.display(), f.metadata()?.len() / 1024),
                    Err(e) => println!("recording failed: {e}"),
                }
            }
            drop(speaker);
            drop(buffer);
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
            session.stop();
            println!("stopped");
        }
        "window" => {
            let s = serial()?;
            let info = phone::info(&s)?;
            let present = has("--present");
            let o = phone::WindowOptions {
                title: if present { phone::present_title(&info.name) } else { info.name.clone() },
                borderless: present,
                on_top: present,
                show_touches: present,
                stay_awake: true,
                screen_off: false,
                sound: if present { phone::Place::Phone } else { phone::Place::Pc },
                record: None,
                max_size: 0,
                bit_rate: 0,
            };
            let mut child = phone::open_window(&s, &o)?;
            println!("scrcpy's window is open: \"{}\"", o.title);
            let secs: u64 = val("--seconds").and_then(|s| s.parse().ok()).unwrap_or(0);
            if secs > 0 {
                std::thread::sleep(Duration::from_secs(secs));
                neria_core::proc::kill_tree(&mut child);
            }
            let status = child.wait()?;
            println!("closed ({status})");
        }
        other => bail!("unknown: {other} (devices, qr, pair, connect, mirror, sound, window)"),
    }
    // Neria's adb is left running (the app stops it on quit; `--stop-adb` here).
    if has("--stop-adb") {
        phone::stop_server();
    }
    Ok(())
}

/// A QR code in the terminal: two rows per line with half blocks, a quiet zone around it.
fn print_qr(text: &str) -> Result<()> {
    let (w, m) = phone::qr(text)?;
    let dark = |x: isize, y: isize| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < w && m[y as usize * w + x as usize];
    let q = 2isize;
    let mut y = -q;
    while y < w as isize + q {
        let mut line = String::new();
        for x in -q..w as isize + q {
            // Light on dark terminals: print the light modules.
            line.push(match (!dark(x, y), !dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        println!("{line}");
        y += 2;
    }
    Ok(())
}

/// A test picture the fake phone sends: a phone-shaped 540×1200 at 30 fps, a key frame every
/// 2 s, with access unit delimiters (made once with ffmpeg, kept in the temp folder).
fn fake_h264() -> Result<Vec<u8>> {
    let f = std::env::temp_dir().join("neria-fake-phone.h264");
    if !f.is_file() {
        let st = neria_core::proc::command(neria_core::paths::tool("ffmpeg"))
            .args(["-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=540x1200:rate=30", "-t", "6"])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-bf", "0", "-g", "60", "-pix_fmt", "yuv420p"])
            .args(["-x264-params", "aud=1", "-f", "h264"])
            .arg(&f)
            .status()?;
        if !st.success() {
            bail!("couldn't make the fake picture");
        }
    }
    Ok(std::fs::read(f)?)
}
