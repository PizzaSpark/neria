//! Drawing chat: one message as a row (the Clip page's chat panel shows these), and a whole
//! frame of chat as it's burned into a clip (a side panel, or lines over the picture). The
//! same drawing in both places, so what you see is what the clip gets.
//!
//! Text is Neria's own Inter Tight (bundled), with Windows' fonts behind it for whatever it
//! hasn't got (Japanese, Korean, symbols…). No shaping: chat is short lines, and scripts that
//! join letters (Arabic) come out unjoined. Emoji and emotes are pictures (`chat::emojify`).
//! Colours are Neria's own (`docs/DESIGN.md`), dark or light.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use ab_glyph::{Font, FontArc, FontVec, GlyphId, PxScale, ScaleFont, point};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Transform};

use crate::chat::{Kind, Message, Part};

static INTER_400: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/desktop/ui/fonts/inter-tight-latin-400-normal.ttf"
));
static INTER_600: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/desktop/ui/fonts/inter-tight-latin-600-normal.ttf"
));
static MONO_400: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/desktop/ui/fonts/jetbrains-mono-latin-400-normal.ttf"
));
static GROTESK_700: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/desktop/ui/fonts/space-grotesk-latin-700-normal.ttf"
));

/// Windows' fonts tried, in order, for letters Inter Tight hasn't got.
const FALLBACKS: &[&str] = &[
    "segoeui.ttf",
    "seguisym.ttf",
    "YuGothM.ttc",
    "msyh.ttc",
    "malgun.ttf",
    "Nirmala.ttf",
    "seguiemj.ttf",
    "arial.ttf",
];

pub struct Fonts {
    regular: FontArc,
    bold: FontArc,
    mono: FontArc,
    tag: FontArc,
    fallback: &'static [FontArc],
}

fn system_fallbacks() -> &'static [FontArc] {
    static F: OnceLock<Vec<FontArc>> = OnceLock::new();
    F.get_or_init(|| {
        let dir = std::env::var_os("WINDIR")
            .map(|w| Path::new(&w).join("Fonts"))
            .unwrap_or_else(|| "C:\\Windows\\Fonts".into());
        FALLBACKS
            .iter()
            .filter_map(|n| std::fs::read(dir.join(n)).ok())
            .filter_map(|b| FontVec::try_from_vec_and_index(b, 0).ok().map(FontArc::new))
            .collect()
    })
}

impl Fonts {
    pub fn load() -> Fonts {
        let f = |b: &'static [u8]| FontArc::try_from_slice(b).expect("bundled font");
        Fonts {
            regular: f(INTER_400),
            bold: f(INTER_600),
            mono: f(MONO_400),
            tag: f(GROTESK_700),
            fallback: system_fallbacks(),
        }
    }

    /// The font that has `c` (the given one, else a fallback), and its glyph.
    fn pick<'a>(&'a self, base: &'a FontArc, c: char) -> (&'a FontArc, GlyphId) {
        let g = base.glyph_id(c);
        if g.0 != 0 || c.is_whitespace() {
            return (base, g);
        }
        for f in self.fallback {
            let g = f.glyph_id(c);
            if g.0 != 0 {
                return (f, g);
            }
        }
        (base, g)
    }
}

/// Pictures (emotes, emoji) as drawn, by address and height.
#[derive(Default)]
pub struct Pictures {
    /// Fetch what isn't in the cache yet (exports); else only use what is, and list the rest
    /// in `wanted` (the panel fetches those off the UI thread, then draws again).
    pub fetch: bool,
    decoded: HashMap<String, Option<Arc<image::RgbaImage>>>,
    sized: HashMap<(String, u32), Option<Arc<Pixmap>>>,
    pub wanted: Vec<String>,
}

impl Pictures {
    pub fn fetching() -> Pictures {
        Pictures { fetch: true, ..Pictures::default() }
    }

    fn get(&mut self, url: &str, height: u32) -> Option<Arc<Pixmap>> {
        let key = (url.to_string(), height);
        if let Some(p) = self.sized.get(&key) {
            return p.clone();
        }
        if !self.decoded.contains_key(url) {
            let file = if self.fetch {
                crate::chat::emote_file(url)
            } else {
                let f = cached_file(url);
                if f.is_none() && !self.wanted.iter().any(|w| w == url) {
                    self.wanted.push(url.to_string());
                }
                f
            };
            let Some(file) = file else {
                // Not had yet: not remembered as missing, so it's tried again later.
                return None;
            };
            let img = image::ImageReader::open(&file)
                .ok()
                .and_then(|r| r.with_guessed_format().ok())
                .and_then(|r| r.decode().ok())
                .map(|i| Arc::new(i.to_rgba8()));
            self.decoded.insert(url.to_string(), img);
        }
        let img = self.decoded.get(url)?.clone()?;
        let w = ((img.width() as f32 / img.height().max(1) as f32) * height as f32)
            .round()
            .clamp(1.0, height as f32 * 4.0) as u32;
        let small = image::imageops::resize(img.as_ref(), w, height, image::imageops::FilterType::Triangle);
        let mut px = Pixmap::new(w, height)?;
        for (dst, src) in px.pixels_mut().iter_mut().zip(small.pixels()) {
            let [r, g, b, a] = src.0;
            *dst = tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
        }
        let px = Some(Arc::new(px));
        self.sized.insert(key, px.clone());
        px
    }
}

/// The cache file of a picture already fetched (no network).
fn cached_file(url: &str) -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    let p = crate::paths::cache_dir().join("emotes").join(format!("{:016x}", h.finish()));
    p.is_file().then_some(p)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub light: bool,
    /// Lines over the picture (each on its own dark strip), not a panel beside it.
    pub overlay: bool,
    /// The small YT / TW mark (when a chat mixes both sites).
    pub show_site: bool,
    /// Pictures for emotes and emoji (else their names as text).
    pub pictures: bool,
}

impl Default for Look {
    fn default() -> Self {
        Look { light: false, overlay: false, show_site: false, pictures: true }
    }
}

/// Neria's colours (`docs/DESIGN.md`), as RGBA.
#[derive(Clone, Copy)]
struct Palette {
    bg: [u8; 4],
    panel: [u8; 4],
    line: [u8; 4],
    ink: [u8; 4],
    muted: [u8; 4],
    dim: [u8; 4],
    warn: [u8; 4],
    warn_bg: [u8; 4],
    ok: [u8; 4],
    ok_bg: [u8; 4],
    tag_bg: [u8; 4],
}

fn hex(s: &str) -> Option<[u8; 4]> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8, 255])
}

fn palette(light: bool) -> Palette {
    let h = |s: &str| hex(s).unwrap();
    if light {
        Palette {
            bg: h("#f4f0e6"),
            panel: h("#fffdf8"),
            line: h("#e2dccd"),
            ink: h("#151412"),
            muted: h("#6b665c"),
            dim: h("#878276"),
            warn: h("#975900"),
            warn_bg: h("#fbe7c4"),
            ok: h("#1c7649"),
            ok_bg: h("#d5f2e0"),
            tag_bg: h("#e2dccd"),
        }
    } else {
        Palette {
            bg: h("#141416"),
            panel: h("#1b1b1e"),
            line: h("#26262a"),
            ink: h("#f1efe9"),
            muted: h("#8e8c88"),
            dim: h("#6e6c68"),
            warn: h("#f5b14a"),
            warn_bg: h("#3a2a12"),
            ok: h("#7fd6a4"),
            ok_bg: h("#173326"),
            tag_bg: h("#34343a"),
        }
    }
}

fn lum(c: [u8; 4]) -> f32 {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
}

fn contrast(a: [u8; 4], b: [u8; 4]) -> f32 {
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// A name's own colour, lightened or darkened until it reads on `bg` (Twitch lets people
/// pick near-black, which vanishes on a dark panel).
fn readable(c: [u8; 4], bg: [u8; 4]) -> [u8; 4] {
    let mut c = c;
    let dark_bg = lum(bg) < 0.4;
    for _ in 0..12 {
        if contrast(c, bg) >= 4.0 {
            break;
        }
        c = if dark_bg {
            [c[0].saturating_add(24), c[1].saturating_add(24), c[2].saturating_add(24), 255]
        } else {
            [c[0].saturating_sub(24), c[1].saturating_sub(24), c[2].saturating_sub(24), 255]
        };
    }
    c
}

fn color(c: [u8; 4], alpha: f32) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], (c[3] as f32 * alpha).round().clamp(0.0, 255.0) as u8)
}

fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.quad_to(x + w, y, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.quad_to(x + w, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.quad_to(x, y + h, x, y + h - r);
    p.line_to(x, y + r);
    p.quad_to(x, y, x + r, y);
    p.close();
    p.finish()
}

fn fill_round(px: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color) {
    if let Some(path) = round_rect(x, y, w, h, r) {
        let mut paint = Paint::default();
        paint.set_color(c);
        paint.anti_alias = true;
        px.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Draw `text` with `font` at (x, baseline), `alpha` over it. Returns the width.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    px: &mut Pixmap,
    fonts: &Fonts,
    font: &FontArc,
    size: f32,
    x: f32,
    baseline: f32,
    text: &str,
    c: [u8; 4],
    alpha: f32,
) -> f32 {
    let mut pen = x;
    let mut last: Option<(GlyphId, *const FontArc)> = None;
    let (w, h) = (px.width() as i32, px.height() as i32);
    for ch in text.chars() {
        let (f, gid) = fonts.pick(font, ch);
        let scale = PxScale::from(size * scale_fix(f));
        let sf = f.as_scaled(scale);
        if let Some((prev, pf)) = last
            && std::ptr::eq(pf, f)
        {
            pen += sf.kern(prev, gid);
        }
        let glyph = gid.with_scale_and_position(scale, point(pen, baseline));
        if let Some(out) = f.outline_glyph(glyph) {
            let b = out.px_bounds();
            let data = px.data_mut();
            out.draw(|gx, gy, cov| {
                let (x, y) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if x < 0 || y < 0 || x >= w || y >= h || cov <= 0.0 {
                    return;
                }
                let a = (cov.min(1.0) * alpha * c[3] as f32 / 255.0).clamp(0.0, 1.0);
                let i = ((y * w + x) * 4) as usize;
                for k in 0..3 {
                    let d = data[i + k] as f32;
                    data[i + k] = (c[k] as f32 * a + d * (1.0 - a)).round() as u8;
                }
                let d = data[i + 3] as f32;
                data[i + 3] = (255.0 * a + d * (1.0 - a)).round() as u8;
            });
        }
        pen += sf.h_advance(gid);
        last = Some((gid, f as *const FontArc));
    }
    pen - x
}

/// `size` is the em size, like a font size in the UI (13 px text); ab_glyph's `PxScale` is
/// the font's whole height (ascent to descent), which is more than an em and differs by
/// font: this is the factor between them, so every font sits at the same size.
fn scale_fix(f: &FontArc) -> f32 {
    let units = f.units_per_em().unwrap_or(1000.0);
    let h = f.ascent_unscaled() - f.descent_unscaled();
    (h / units).clamp(0.9, 1.7)
}

fn text_width(fonts: &Fonts, font: &FontArc, size: f32, text: &str) -> f32 {
    let mut w = 0.0;
    let mut last: Option<(GlyphId, *const FontArc)> = None;
    for ch in text.chars() {
        let (f, gid) = fonts.pick(font, ch);
        let sf = f.as_scaled(PxScale::from(size * scale_fix(f)));
        if let Some((prev, pf)) = last
            && std::ptr::eq(pf, f)
        {
            w += sf.kern(prev, gid);
        }
        w += sf.h_advance(gid);
        last = Some((gid, f as *const FontArc));
    }
    w
}

/// A piece of a laid-out message (widths kept for the layout; a picture keeps its name).
#[allow(dead_code)]
enum Item {
    Text { s: String, bold: bool, c: [u8; 4], w: f32 },
    Pic { url: String, name: String, w: f32 },
    Tag { s: String, fg: [u8; 4], bg: [u8; 4], w: f32 },
}

struct Placed {
    item: Item,
    x: f32,
    line: usize,
}

/// Words (with the space after them) and pictures, placed on lines of `width`.
fn layout(fonts: &Fonts, size: f32, items: Vec<Item>, width: f32, start_x: f32) -> (Vec<Placed>, usize) {
    let mut out = Vec::new();
    let (mut x, mut line) = (start_x, 0usize);
    for it in items {
        // Text is placed a word at a time; a word longer than the line is broken by letters.
        match it {
            Item::Text { s, bold, c, .. } => {
                let font = if bold { &fonts.bold } else { &fonts.regular };
                let mut words: Vec<String> = Vec::new();
                let mut cur = String::new();
                for ch in s.chars() {
                    cur.push(ch);
                    if ch == ' ' {
                        words.push(std::mem::take(&mut cur));
                    }
                }
                if !cur.is_empty() {
                    words.push(cur);
                }
                for word in words {
                    let w = text_width(fonts, font, size, &word);
                    let trimmed = text_width(fonts, font, size, word.trim_end());
                    if x + trimmed > width && x > 0.0 {
                        line += 1;
                        x = 0.0;
                        if word.trim().is_empty() {
                            continue;
                        }
                    }
                    if trimmed > width {
                        // Break the long word where it runs out.
                        let mut piece = String::new();
                        for ch in word.chars() {
                            let try_w = text_width(fonts, font, size, &format!("{piece}{ch}"));
                            if x + try_w > width && !piece.is_empty() {
                                let pw = text_width(fonts, font, size, &piece);
                                out.push(Placed {
                                    item: Item::Text { s: std::mem::take(&mut piece), bold, c, w: pw },
                                    x,
                                    line,
                                });
                                line += 1;
                                x = 0.0;
                            }
                            piece.push(ch);
                        }
                        let pw = text_width(fonts, font, size, &piece);
                        out.push(Placed { item: Item::Text { s: piece, bold, c, w: pw }, x, line });
                        x += pw;
                        continue;
                    }
                    out.push(Placed { item: Item::Text { s: word, bold, c, w }, x, line });
                    x += w;
                }
            }
            other => {
                let w = match &other {
                    Item::Pic { w, .. } | Item::Tag { w, .. } => *w,
                    _ => 0.0,
                };
                if x + w > width && x > 0.0 {
                    line += 1;
                    x = 0.0;
                }
                let gap = if matches!(other, Item::Tag { .. }) { size * 0.35 } else { size * 0.12 };
                out.push(Placed { item: other, x, line });
                x += w + gap;
            }
        }
    }
    (out, line + 1)
}

/// What a row shows besides the message (the panel's own bits).
#[derive(Clone, Debug, Default)]
pub struct RowExtras {
    /// "41:58" in front, in the mono face.
    pub time: Option<String>,
    /// Greyed (a bot, when bots are left out).
    pub dim: bool,
}

fn badge_tag(b: &str) -> Option<&'static str> {
    match b {
        "broadcaster" | "owner" => Some("HOST"),
        "moderator" => Some("MOD"),
        "vip" => Some("VIP"),
        _ => None,
    }
}

/// The parts of a message as items: tags, the name, then the text and pictures.
#[allow(clippy::too_many_arguments)]
fn items_of(
    fonts: &Fonts,
    pics: &mut Pictures,
    m: &Message,
    size: f32,
    pal: &Palette,
    look: &Look,
    on: [u8; 4],
    header_only: bool,
) -> Vec<Item> {
    let mut items = Vec::new();
    let tag_size = size * 0.72;
    let tag = |s: &str, fg: [u8; 4], bg: [u8; 4]| {
        let w = text_width(fonts, &fonts.tag, tag_size, s) + tag_size * 0.9;
        Item::Tag { s: s.to_string(), fg, bg, w }
    };
    if look.show_site {
        let (fg, bg) = match m.site {
            crate::chat::Site::YouTube => (hex("#ff8a80").unwrap(), hex("#3a1a1a").unwrap()),
            crate::chat::Site::Twitch => (hex("#c4a8ff").unwrap(), hex("#2a2140").unwrap()),
            crate::chat::Site::Kick => (hex("#7ef08a").unwrap(), hex("#16301a").unwrap()),
        };
        let (fg, bg) = if look.light {
            match m.site {
                crate::chat::Site::YouTube => (hex("#b3261e").unwrap(), hex("#fde0dc").unwrap()),
                crate::chat::Site::Twitch => (hex("#5b2fb8").unwrap(), hex("#ece4fb").unwrap()),
                crate::chat::Site::Kick => (hex("#1d6b2a").unwrap(), hex("#dcf6df").unwrap()),
            }
        } else {
            (fg, bg)
        };
        items.push(tag(m.site.mark(), fg, bg));
    }
    for b in &m.badges {
        if let Some(t) = badge_tag(b) {
            items.push(tag(t, pal.muted, pal.tag_bg));
        }
    }
    let name_c = match (m.kind, &m.color) {
        (Kind::Paid, _) => pal.warn,
        (_, Some(c)) => readable(hex(c).unwrap_or(pal.ink), on),
        _ => pal.muted,
    };
    let name = match &m.amount {
        Some(a) => format!("{} · {a} ", m.name),
        None => format!("{} ", m.name),
    };
    let w = text_width(fonts, &fonts.bold, size, &name);
    items.push(Item::Text { s: name, bold: true, c: name_c, w });
    if header_only {
        return items;
    }
    let pic_h = (size * 1.45).round() as u32;
    for p in &m.parts {
        match p {
            Part::Text { s } => {
                let w = text_width(fonts, &fonts.regular, size, s);
                items.push(Item::Text { s: s.clone(), bold: false, c: pal.ink, w });
            }
            Part::Emote { name, url } => {
                let pic = if look.pictures { pics.get(url, pic_h) } else { None };
                match pic {
                    Some(p) => items.push(Item::Pic { url: url.clone(), name: name.clone(), w: p.width() as f32 }),
                    // Not there (yet): its name, so the message still reads.
                    None => {
                        let s = name.clone();
                        let w = text_width(fonts, &fonts.regular, size, &s);
                        items.push(Item::Text { s, bold: false, c: pal.ink, w });
                    }
                }
            }
        }
    }
    items
}

/// One message drawn at `width` px: what the chat panel shows per row, and what a burned-in
/// frame stacks. Transparent where nothing is drawn (cards have their own background).
pub fn message(
    fonts: &Fonts,
    pics: &mut Pictures,
    m: &Message,
    width: u32,
    look: &Look,
    size: f32,
    extras: &RowExtras,
) -> Pixmap {
    let pal = palette(look.light);
    let on = if look.overlay { hex("#000000").unwrap() } else { pal.bg };
    let line_h = (size * 1.42).round();
    let pad = (size * 0.45).round();
    let card = m.kind != Kind::Chat;
    let inset = if card || look.overlay { pad } else { 0.0 };
    let time_w =
        extras.time.as_ref().map(|t| text_width(fonts, &fonts.mono, size * 0.88, t) + size * 0.6).unwrap_or(0.0);
    let body_x = inset + time_w;
    let body_w = (width as f32 - body_x - inset).max(size * 4.0);

    // A card (Super Chat, sub, raid): the site's line, the name (and amount), then the text.
    let mut note_lines: Vec<String> = Vec::new();
    if let Some(note) = m.note.as_ref().filter(|_| card) {
        let (placed, _) = layout(
            fonts,
            size * 0.9,
            vec![Item::Text { s: note.clone(), bold: false, c: pal.muted, w: 0.0 }],
            body_w,
            0.0,
        );
        let mut by_line: Vec<String> = Vec::new();
        for p in placed {
            if let Item::Text { s, .. } = p.item {
                if by_line.len() <= p.line {
                    by_line.resize(p.line + 1, String::new());
                }
                by_line[p.line].push_str(&s);
            }
        }
        note_lines = by_line;
    }
    let mut items = items_of(fonts, pics, m, size, &pal, look, on, false);
    let note_c = if m.kind == Kind::Paid {
        pal.warn
    } else if m.kind == Kind::Member {
        pal.ok
    } else {
        pal.muted
    };
    // On a card the name sits on its own line, the text under it.
    let mut header: Vec<Item> = Vec::new();
    if card {
        let split = items.iter().position(|i| matches!(i, Item::Text { bold: true, .. })).map(|i| i + 1).unwrap_or(0);
        header = items.drain(..split).collect();
    }
    let (head_placed, head_lines) = if card { layout(fonts, size, header, body_w, 0.0) } else { (Vec::new(), 0) };
    let has_text = items.iter().any(|i| !matches!(i, Item::Text { s, .. } if s.trim().is_empty()));
    let (placed, lines) = if card && !has_text { (Vec::new(), 0) } else { layout(fonts, size, items, body_w, 0.0) };
    let note_h = note_lines.len() as f32 * (size * 1.4).round();
    let total_lines = head_lines + lines;
    let content_h = note_h + total_lines as f32 * line_h;
    let vpad = if card || look.overlay { pad * 0.8 } else { (size * 0.3).round() };
    let height = (content_h + vpad * 2.0).ceil().max(line_h) as u32;
    // Over the picture a line's strip is only as wide as what it says.
    let width = if look.overlay && !card {
        let used = placed
            .iter()
            .map(|p| {
                p.x + match &p.item {
                    Item::Text { s, w, .. } => w - text_width(fonts, &fonts.regular, size, &s[s.trim_end().len()..]),
                    Item::Pic { w, .. } | Item::Tag { w, .. } => *w,
                }
            })
            .fold(0.0f32, f32::max);
        ((body_x + used + inset).ceil() as u32).min(width)
    } else {
        width
    };
    let mut px = Pixmap::new(width.max(1), height.max(1)).unwrap();

    if card {
        let bg = match m.kind {
            Kind::Paid => pal.warn_bg,
            Kind::Member => pal.ok_bg,
            _ => pal.line,
        };
        fill_round(&mut px, 0.0, 0.0, width as f32, height as f32, size * 0.5, color(bg, 1.0));
    } else if look.overlay {
        fill_round(&mut px, 0.0, 0.0, width as f32, height as f32, size * 0.4, color([0, 0, 0, 255], 0.74));
    }
    let alpha = if extras.dim { 0.5 } else { 1.0 };
    let ascent = fonts.regular.as_scaled(PxScale::from(size * scale_fix(&fonts.regular))).ascent();
    let base_of = |line: usize, extra: f32| vpad + extra + line as f32 * line_h + (line_h - size * 1.2) / 2.0 + ascent;
    let mut y_extra = 0.0;
    if let Some(t) = &extras.time {
        let tc = if m.kind == Kind::Paid { pal.warn } else { pal.dim };
        draw_text(&mut px, fonts, &fonts.mono, size * 0.88, inset, base_of(0, 0.0), t, tc, alpha);
    }
    for (i, l) in note_lines.iter().enumerate() {
        let b = vpad + i as f32 * (size * 1.4).round() + ascent * 0.9;
        draw_text(&mut px, fonts, &fonts.regular, size * 0.9, body_x, b, l, note_c, alpha);
    }
    y_extra += note_h;
    let mut draw_placed = |px: &mut Pixmap, placed: Vec<Placed>, first_line: usize| {
        for p in placed {
            let line = first_line + p.line;
            let base = base_of(line, y_extra);
            let x = body_x + p.x;
            match p.item {
                Item::Text { s, bold, c, .. } => {
                    let font = if bold { &fonts.bold } else { &fonts.regular };
                    draw_text(px, fonts, font, size, x, base, &s, c, alpha);
                }
                Item::Pic { url, .. } => {
                    let pic_h = (size * 1.45).round() as u32;
                    if let Some(pic) = pics.get(&url, pic_h) {
                        let top = vpad + y_extra + line as f32 * line_h + (line_h - pic_h as f32) / 2.0;
                        let paint = PixmapPaint { opacity: alpha, ..PixmapPaint::default() };
                        px.draw_pixmap(
                            x.round() as i32,
                            top.round() as i32,
                            pic.as_ref().as_ref(),
                            &paint,
                            Transform::identity(),
                            None,
                        );
                    }
                }
                Item::Tag { s, fg, bg, w } => {
                    let th = size * 1.05;
                    let top = vpad + y_extra + line as f32 * line_h + (line_h - th) / 2.0;
                    fill_round(px, x, top, w, th, size * 0.25, color(bg, alpha));
                    let tsize = size * 0.72;
                    let tb = top + th / 2.0 + tsize * 0.36;
                    draw_text(px, fonts, &fonts.tag, tsize, x + tsize * 0.45, tb, &s, fg, alpha);
                }
            }
        }
    };
    draw_placed(&mut px, head_placed, 0);
    draw_placed(&mut px, placed, head_lines);
    px
}

/// A whole frame of chat as burned in, at second `now` of the clip: the newest at the
/// bottom, as many as fit. `shown` is what's been said by then (oldest first). A side panel
/// has Neria's background; over the picture, lines fade out after a while and few are kept.
#[allow(clippy::too_many_arguments)]
pub fn panel(
    fonts: &Fonts,
    pics: &mut Pictures,
    shown: &[Message],
    now: f64,
    width: u32,
    height: u32,
    look: &Look,
    size: f32,
) -> Pixmap {
    let pal = palette(look.light);
    let mut px = Pixmap::new(width.max(1), height.max(1)).unwrap();
    let pad = (size * 0.7).round();
    if !look.overlay {
        px.fill(color(pal.panel, 1.0));
        // A hairline on the side that meets the picture.
        if let Some(r) = Rect::from_xywh(0.0, 0.0, (size * 0.08).max(1.0), height as f32) {
            let mut paint = Paint::default();
            paint.set_color(color(pal.line, 1.0));
            px.fill_rect(r, &paint, Transform::identity(), None);
        }
    }
    let inner_w = width.saturating_sub((pad * 2.0) as u32).max(10);
    let gap = if look.overlay { (size * 0.3).round() } else { (size * 0.2).round() };
    let mut y = height as f32 - pad;
    // Over the picture: at most 8 lines, each gone 14 s after it was said (fading in its
    // last 2 s), so the picture stays the point.
    let (max_n, life) = if look.overlay { (8usize, 14.0) } else { (usize::MAX, f64::MAX) };
    for (n, m) in shown.iter().rev().enumerate() {
        if n >= max_n || y <= 0.0 {
            break;
        }
        let age = now - m.t;
        if age > life {
            break;
        }
        let fade = if look.overlay { ((life - age) / 2.0).clamp(0.0, 1.0) as f32 } else { 1.0 };
        let row = message(fonts, pics, m, inner_w, look, size, &RowExtras::default());
        y -= row.height() as f32;
        let paint = PixmapPaint { opacity: fade, ..PixmapPaint::default() };
        px.draw_pixmap(pad as i32, y.round() as i32, row.as_ref(), &paint, Transform::identity(), None);
        y -= gap;
    }
    px
}

/// When a burned-in frame changes, from `shown`'s times: each message's arrival, and (over
/// the picture) when lines start fading and are gone.
pub fn change_times(messages: &[Message], look: &Look, len: f64) -> Vec<f64> {
    let mut t: Vec<f64> = vec![0.0];
    for m in messages {
        t.push(m.t);
        if look.overlay {
            for k in [12.0, 13.0, 14.0] {
                t.push(m.t + k);
            }
        }
    }
    t.retain(|x| *x >= 0.0 && *x < len);
    t.sort_by(|a, b| a.total_cmp(b));
    t.dedup_by(|a, b| (*a - *b).abs() < 0.05);
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Site;

    fn msg(name: &str, text: &str) -> Message {
        Message {
            name: name.into(),
            site: Site::Twitch,
            parts: vec![Part::Text { s: text.into() }],
            color: Some("#0E0D0F".into()),
            ..Message::default()
        }
    }

    #[test]
    fn long_messages_wrap_and_grow() {
        let fonts = Fonts::load();
        let mut pics = Pictures::default();
        let look = Look::default();
        let short = message(&fonts, &mut pics, &msg("kaiyo", "CLEAN"), 300, &look, 14.0, &RowExtras::default());
        let long = message(
            &fonts,
            &mut pics,
            &msg("kaiyo", &"that was so clean ".repeat(8)),
            300,
            &look,
            14.0,
            &RowExtras::default(),
        );
        assert_eq!(short.width(), 300);
        assert!(long.height() > short.height() * 2, "{} vs {}", long.height(), short.height());
        let word = message(&fonts, &mut pics, &msg("x", &"A".repeat(200)), 120, &look, 14.0, &RowExtras::default());
        assert!(word.height() > short.height());
    }

    #[test]
    fn dark_names_are_lifted() {
        let bg = hex("#141416").unwrap();
        let c = readable(hex("#0E0D0F").unwrap(), bg);
        assert!(contrast(c, bg) >= 4.0);
        let fine = hex("#FF9F43").unwrap();
        assert_eq!(readable(fine, bg), fine);
    }

    #[test]
    fn panel_and_changes() {
        let fonts = Fonts::load();
        let mut pics = Pictures::default();
        let mut ms: Vec<Message> = (0..40).map(|i| Message { t: i as f64, ..msg("a", "hello there") }).collect();
        ms[5].kind = Kind::Paid;
        ms[5].amount = Some("$5.00".into());
        let px = panel(&fonts, &mut pics, &ms, 40.0, 300, 400, &Look::default(), 14.0);
        assert_eq!((px.width(), px.height()), (300, 400));
        let look = Look { overlay: true, ..Look::default() };
        let t = change_times(&ms[..2], &look, 100.0);
        assert_eq!(t, vec![0.0, 1.0, 12.0, 13.0, 14.0, 15.0]);
    }
}
