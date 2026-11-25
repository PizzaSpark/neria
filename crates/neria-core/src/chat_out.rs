//! Putting chat into a clip or a recording (Clip page → Chat):
//! - **As a track**: a caption track you turn on in the player (styled names in an .mkv;
//!   plain "name: text" lines in an .mp4, which only holds plain captions). The picture is
//!   untouched, nothing is re-encoded.
//! - **Burned in**: drawn onto the picture by `chat_draw`, as a panel beside it (below it
//!   for an upright video) or as lines over it. Frames are only made when chat changes (a
//!   message arrives, a line fades), and ffmpeg lays them over the video.
//!
//! The messages come in already cut to the clip (`chat::for_clip`): `t` is seconds into it.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::chat::{Kind, Message};
use crate::chat_draw::{self, Fonts, Look, Pictures};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum How {
    /// Leave it out.
    #[default]
    None,
    Track,
    /// Burned in, beside the picture.
    Panel,
    /// Burned in, over the picture.
    Overlay,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spec {
    pub how: How,
    pub light: bool,
    /// Emotes and emoji as pictures (else their names).
    pub emotes: bool,
}

/// What a download does with chat once it's saved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatJob {
    /// Fetch the stream's whole chat and save it next to the file (`.chat.json`).
    pub save: bool,
    /// Put these messages (`t` from the clip's start) into the file this way.
    pub spec: Spec,
    pub messages: Vec<Message>,
    /// How long the clip was asked to be: a lossless cut that starts on the keyframe before
    /// In comes out longer, and its chat moves by as much.
    pub clip_len: Option<f64>,
    /// Everything said during the clip (`t` from its start), saved next to it as its
    /// `.chat.json` ("Save the last" while watching live).
    pub beside: Vec<Message>,
}

impl Spec {
    pub fn is_none(&self) -> bool {
        self.how == How::None
    }

    pub fn reencodes(&self) -> bool {
        matches!(self.how, How::Panel | How::Overlay)
    }
}

/// What a chat message says as one caption line.
fn plain(m: &Message) -> String {
    let text = m.text().replace(['\n', '\r'], " ");
    let name = match &m.amount {
        Some(a) => format!("{} ({a})", m.name),
        None => m.name.clone(),
    };
    match (&m.note, text.trim().is_empty()) {
        (Some(n), true) => format!("{name}: {n}"),
        _ => format!("{name}: {}", text.trim()),
    }
}

fn ts_srt(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

fn ts_ass(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

/// The last few messages at each moment chat changes: a caption shows a rolling window, like
/// a chat box, not one message at a time.
fn windows(messages: &[Message], len: f64) -> Vec<(f64, f64, Vec<&Message>)> {
    const SHOW: usize = 4;
    const LINGER: f64 = 10.0;
    let mut out = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        let start = m.t.max(0.0);
        let next = messages.get(i + 1).map(|n| n.t).unwrap_or(f64::MAX);
        let end = next.min(start + LINGER).min(len.max(start + 0.5));
        if end - start < 0.05 {
            continue;
        }
        let from = (i + 1).saturating_sub(SHOW);
        let shown: Vec<&Message> = messages[from..=i].iter().filter(|o| m.t - o.t <= LINGER * 3.0).collect();
        out.push((start, end, shown));
    }
    out
}

pub fn srt(messages: &[Message], len: f64) -> String {
    let mut s = String::new();
    for (n, (a, b, shown)) in windows(messages, len).into_iter().enumerate() {
        let lines: Vec<String> = shown.iter().map(|m| plain(m)).collect();
        s.push_str(&format!("{}\n{} --> {}\n{}\n\n", n + 1, ts_srt(a), ts_srt(b), lines.join("\n")));
    }
    s
}

fn ass_color(hex: &str) -> String {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return "&HFFFFFF&".into();
    }
    format!("&H{}{}{}&", &h[4..6], &h[2..4], &h[0..2])
}

fn ass_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('{', "(").replace('}', ")").replace(['\n', '\r'], " ")
}

/// Styled captions for an .mkv: names in their colours, Super Chats in amber.
pub fn ass(messages: &[Message], len: f64, width: u32, height: u32) -> String {
    let size = (height as f32 / 30.0).round().max(14.0);
    let mut s = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {width}\nPlayResY: {height}\nWrapStyle: 0\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Chat,Segoe UI,{size},&H00F1EFE9,&H00F1EFE9,&H00000000,&H80000000,0,0,0,0,100,100,0,0,3,2,0,1,{m},{m},{m},1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        m = (height / 30).max(10)
    );
    for (a, b, shown) in windows(messages, len) {
        let lines: Vec<String> = shown
            .iter()
            .map(|m| {
                let c = if m.kind == Kind::Paid {
                    "#F5B14A".to_string()
                } else {
                    m.color.clone().unwrap_or_else(|| "#8E8C88".into())
                };
                let name = match &m.amount {
                    Some(amt) => format!("{} · {amt}", m.name),
                    None => m.name.clone(),
                };
                let text = m.text();
                let text = if text.trim().is_empty() { m.note.clone().unwrap_or_default() } else { text };
                format!(
                    "{{\\b1\\c{}}}{}{{\\b0\\c&HE9EFF1&}} {}",
                    ass_color(&c),
                    ass_escape(&name),
                    ass_escape(text.trim())
                )
            })
            .collect();
        s.push_str(&format!("Dialogue: 0,{},{},Chat,,0,0,0,,{}\n", ts_ass(a), ts_ass(b), lines.join("\\N")));
    }
    s
}

/// Where burned-in chat goes on a `w` × `h` picture, and how big its text is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// The chat's own frame size.
    pub cw: u32,
    pub ch: u32,
    /// Where it sits on the output (which is bigger than the video for a panel).
    pub x: u32,
    pub y: u32,
    pub out_w: u32,
    pub out_h: u32,
    pub font: f32,
}

fn even(v: f32) -> u32 {
    ((v / 2.0).round() as u32 * 2).max(2)
}

pub fn place(how: How, w: u32, h: u32) -> Place {
    let upright = h > w;
    match (how, upright) {
        // Beside a wide picture: a column as tall as it.
        (How::Panel, false) => {
            let cw = even(h as f32 * 0.42);
            Place { cw, ch: h, x: w, y: 0, out_w: w + cw, out_h: h, font: h as f32 / 46.0 }
        }
        // Under an upright one (a phone video stays a phone video).
        (How::Panel, true) => {
            let ch = even(h as f32 * 0.3);
            Place { cw: w, ch, x: 0, y: h, out_w: w, out_h: h + ch, font: w as f32 / 31.0 }
        }
        // Over the picture: the lower left.
        (_, false) => {
            let (cw, ch) = (even(w as f32 * 0.4), even(h as f32 * 0.6));
            let m = even(h as f32 * 0.03);
            Place { cw, ch, x: m, y: h - ch - m, out_w: w, out_h: h, font: h as f32 / 53.0 }
        }
        (_, true) => {
            let (cw, ch) = (even(w as f32 * 0.86), even(h as f32 * 0.4));
            let m = even(w as f32 * 0.05);
            Place { cw, ch, x: m, y: h - ch - (h as f32 * 0.12) as u32, out_w: w, out_h: h, font: w as f32 / 36.0 }
        }
    }
}

/// Put `messages` into `video`, written to `out`. `on` gets 0–1.
pub fn put_into(
    video: &Path,
    messages: &[Message],
    spec: &Spec,
    out: &Path,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(f32),
) -> Result<()> {
    let info = crate::media::file_info(video)?;
    let len = info.duration.max(0.1);
    let ext = out.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "mp4".into());
    let work = crate::paths::cache_dir().join(format!("chat-out-{}-{}", std::process::id(), crate::live::now() as u64));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let result = (|| -> Result<()> {
        match spec.how {
            How::None => {
                std::fs::copy(video, out)?;
                Ok(())
            }
            How::Track => {
                let (w, h) = shown_size(&info);
                let (file, codec) = match ext.as_str() {
                    "mkv" => {
                        let p = work.join("chat.ass");
                        std::fs::write(&p, ass(messages, len, w.max(640), h.max(360)))?;
                        (p, "ass")
                    }
                    "webm" => {
                        let p = work.join("chat.srt");
                        std::fs::write(&p, srt(messages, len))?;
                        (p, "webvtt")
                    }
                    _ => {
                        let p = work.join("chat.srt");
                        std::fs::write(&p, srt(messages, len))?;
                        (p, "mov_text")
                    }
                };
                let mut a: Vec<String> = ["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y", "-i"]
                    .map(String::from)
                    .to_vec();
                a.push(s(video));
                a.extend(["-i".into(), s(&file)]);
                a.extend(["-map", "0", "-map", "1:0", "-c", "copy"].map(String::from));
                let n = info.subtitles.len();
                a.extend([format!("-c:s:{n}"), codec.into(), format!("-metadata:s:s:{n}"), "title=Chat".into()]);
                a.extend([
                    format!("-metadata:s:s:{n}"),
                    "language=und".into(),
                    format!("-disposition:s:{n}"),
                    "0".into(),
                ]);
                if ext == "mp4" || ext == "mov" {
                    a.extend(["-movflags".into(), "+faststart".into()]);
                }
                a.push(s(out));
                #[allow(clippy::redundant_closure)] // `on` is borrowed immutably here
                crate::sound::run_ffmpeg(&a, len, cancel, |p| on(p)).map(|_| ())
            }
            How::Panel | How::Overlay => {
                if !info.has_video {
                    bail!("A sound file has no picture to put chat on: pick As a track");
                }
                burn(video, &info, messages, spec, out, &work, cancel, on)
            }
        }
    })();
    let _ = std::fs::remove_dir_all(&work);
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

/// The picture's size as it's shown (a phone video turned upright).
fn shown_size(info: &crate::media::FileInfo) -> (u32, u32) {
    if info.rotation.rem_euclid(180) == 90 { (info.height, info.width) } else { (info.width, info.height) }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[allow(clippy::too_many_arguments)]
fn burn(
    video: &Path,
    info: &crate::media::FileInfo,
    messages: &[Message],
    spec: &Spec,
    out: &Path,
    work: &Path,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(f32),
) -> Result<()> {
    let (w, h) = shown_size(info);
    if w == 0 || h == 0 {
        bail!("Couldn't read the video's size");
    }
    let len = info.duration.max(0.1);
    let p = place(spec.how, w, h);
    let look = Look {
        light: spec.light,
        overlay: spec.how == How::Overlay,
        show_site: mixed(messages),
        pictures: spec.emotes,
    };
    let fonts = Fonts::load();
    let mut pics = Pictures::fetching();
    // A frame each time chat changes; at most 4 a second (a busy chat comes in bursts).
    let mut times = chat_draw::change_times(messages, &look, len);
    let mut kept: Vec<f64> = Vec::new();
    for t in times.drain(..) {
        if kept.last().is_none_or(|l| t - l >= 0.25) {
            kept.push(t);
        }
    }
    let draw_share = 0.25;
    let mut list = String::from("ffconcat version 1.0\n");
    for (i, t) in kept.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("Cancelled");
        }
        let upto = messages.partition_point(|m| m.t <= *t + 1e-6);
        let frame = chat_draw::panel(&fonts, &mut pics, &messages[..upto], *t, p.cw, p.ch, &look, p.font);
        let path = work.join(format!("f{i:06}.png"));
        frame.save_png(&path).context("couldn't write a chat frame")?;
        let dur = kept.get(i + 1).map(|n| n - t).unwrap_or(len - t).max(0.04);
        list.push_str(&format!("file '{}'\nduration {dur:.3}\n", path.file_name().unwrap().to_string_lossy()));
        on((i as f32 / kept.len().max(1) as f32) * draw_share);
    }
    // The last frame again: the concat reader needs it to hold the last duration.
    if let Some(i) = kept.len().checked_sub(1) {
        list.push_str(&format!("file 'f{i:06}.png'\n"));
    }
    let list_path = work.join("frames.txt");
    std::fs::write(&list_path, list)?;
    let bg = if spec.light { "0xfffdf8" } else { "0x1b1b1e" };
    let graph = match spec.how {
        How::Panel => format!(
            "[0:v]pad={}:{}:0:0:color={bg},setsar=1[b];[b][1:v]overlay={}:{}:eof_action=repeat:format=auto,format=yuv420p[v]",
            p.out_w, p.out_h, p.x, p.y
        ),
        _ => {
            format!("[0:v]setsar=1[b];[b][1:v]overlay={}:{}:eof_action=repeat:format=auto,format=yuv420p[v]", p.x, p.y)
        }
    };
    let mut a: Vec<String> =
        ["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y", "-i"].map(String::from).to_vec();
    a.push(s(video));
    a.extend(["-f", "concat", "-safe", "0", "-i"].map(String::from));
    a.push(s(&list_path));
    a.extend(["-filter_complex".into(), graph, "-map".into(), "[v]".into(), "-map".into(), "0:a?".into()]);
    // The processor or the graphics card: filled in when it runs (`gpu::fill`).
    a.push(crate::gpu::H264.into());
    a.extend(["-c:a", "copy"].map(String::from));
    let ext = out.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if ext == "webm" {
        bail!("Burned-in chat is saved as MP4 or MKV");
    }
    if ext == "mp4" || ext == "mov" {
        a.extend(["-movflags".into(), "+faststart".into()]);
    }
    a.push(s(out));
    crate::gpu::or_on_cpu(|| {
        crate::sound::run_ffmpeg(&crate::gpu::fill(&a, 18), len, cancel, |x| on(draw_share + x * (1.0 - draw_share)))
    })
    .map_err(|e| if e.to_string() == "Cancelled" { e } else { anyhow::anyhow!("Couldn't put the chat in: {e}") })?;
    Ok(())
}

fn mixed(messages: &[Message]) -> bool {
    let first = messages.first().map(|m| m.site);
    messages.iter().any(|m| Some(m.site) != first)
}

/// Put chat into `file` itself (a link clip after its download, a saved clip): the file is
/// replaced; a burned-in .webm becomes an .mp4. Returns where it is now.
pub fn put_in_place(
    file: &Path,
    messages: &[Message],
    spec: &Spec,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(f32),
) -> Result<PathBuf> {
    if spec.is_none() {
        return Ok(file.to_path_buf());
    }
    let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "mp4".into());
    let audio_only = ["m4a", "mp3", "opus", "ogg", "flac", "wav", "aac"].contains(&ext.as_str());
    if audio_only && spec.reencodes() {
        bail!("A sound file has no picture to put chat on");
    }
    let out_ext = if (spec.reencodes() && ext == "webm") || (audio_only && ext != "m4a") {
        // Burned in: a format that holds H.264. A sound file's track: an .mka can't be
        // played everywhere, so a sound file keeps its chat beside it instead.
        if audio_only {
            return Ok(file.to_path_buf());
        }
        "mp4".to_string()
    } else {
        ext
    };
    let tmp = file.with_extension(format!("neria-chat.{out_ext}"));
    put_into(file, messages, spec, &tmp, cancel, on)?;
    let mut dest = file.with_extension(&out_ext);
    if dest != file && dest.exists() {
        let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        dest = crate::clip::unique_path(file.parent().unwrap_or(Path::new(".")), &stem, &out_ext);
    }
    std::fs::remove_file(file)?;
    std::fs::rename(&tmp, &dest).or_else(|_| {
        std::fs::copy(&tmp, &dest)?;
        std::fs::remove_file(&tmp)
    })?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Part;

    fn m(t: f64, name: &str, text: &str) -> Message {
        Message {
            t,
            name: name.into(),
            parts: vec![Part::Text { s: text.into() }],
            color: Some("#FF9F43".into()),
            ..Message::default()
        }
    }

    #[test]
    fn captions_roll_like_a_chat_box() {
        let ms: Vec<Message> = (0..6).map(|i| m(i as f64, &format!("u{i}"), "hi")).collect();
        let text = srt(&ms, 20.0);
        assert!(text.starts_with("1\n00:00:00,000 --> 00:00:01,000\nu0: hi\n"));
        // The sixth window shows the last four.
        let last = text.split("\n\n").filter(|b| !b.is_empty()).last().unwrap();
        assert!(last.contains("u2: hi\nu3: hi\nu4: hi\nu5: hi"));
        assert!(last.contains("00:00:05,000 --> 00:00:15,000"));
        let a = ass(&ms, 20.0, 1280, 720);
        assert!(a.contains("Dialogue: 0,0:00:00.00,0:00:01.00,Chat,,0,0,0,,{\\b1\\c&H439FFF&}u0"));
    }

    #[test]
    fn places_chat_by_shape() {
        let wide = place(How::Panel, 1920, 1080);
        assert_eq!((wide.out_w, wide.out_h, wide.x, wide.ch), (1920 + 454, 1080, 1920, 1080));
        let tall = place(How::Panel, 1080, 1920);
        assert_eq!((tall.out_w, tall.out_h, tall.y), (1080, 1920 + 576, 1920));
        let over = place(How::Overlay, 1920, 1080);
        assert_eq!((over.out_w, over.out_h), (1920, 1080));
        assert!(over.y + over.ch <= 1080 && over.x + over.cw <= 1920);
        assert_eq!(wide.cw % 2, 0);
    }
}
