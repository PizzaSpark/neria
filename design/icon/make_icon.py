"""Render Neria's app icon: an "N" whose left stem is a download arrow, on the yellow sticker.

    python -I design/icon/make_icon.py [variant] [out_dir]

Writes neria-<size>.png for each size and neria.ico (all sizes). Small sizes drop the hard
shadow and thicken the outline so the mark stays crisp at 16 px.
Variants: arrow (default, the chosen icon, 2026-11-11), arrow-line / arrow-clean (split strokes,
yellow), arrow-dark / arrow-dark-split / arrow-dark-pop (white on near-black), sticker / flat
(the nav rail's "n"). `preview [out_dir] [v1,v2,...]` writes a side-by-side comparison.
Install: `python -I design/icon/make_icon.py arrow <tmp>`, then copy neria.ico and
neria-256.png into apps/desktop/assets/; `... mark apps/desktop/assets` writes neria-mark.png,
the in-app logo (nav rail, first-run screen).
"""

import os
import sys

from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
FONT = os.path.join(HERE, "..", "..", "apps", "desktop", "ui", "fonts", "space-grotesk-latin-700-normal.ttf")
POP, HARD, ON_POP = (255, 210, 63), (21, 20, 18), (26, 20, 5)
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def arrow_n(img, x0, y0, box, ink, small, split=False, stem=None):
    """An "N" whose left stem ends in a download arrowhead, in a 100-unit grid.
    `split` cuts small gaps where the diagonal meets the stems (cleaner, like the first
    sketch); `stem` colours the arrow differently from the rest."""
    d = ImageDraw.Draw(img)
    k = box / 100
    w = 15 if small else 12  # stroke
    P = lambda pts: [(x0 + x * k, y0 + y * k) for x, y in pts]
    l, r = 22, 78 - w
    top, bot = 20, 80
    dw = w + 4  # the diagonal's horizontal width, so it looks as thick as the stems
    # Diagonal on its own mask, so the gaps can be cut out of it.
    mask = Image.new("L", img.size, 0)
    md = ImageDraw.Draw(mask)
    md.polygon(P([(l, top), (l + dw, top), (r + w, bot), (r + w - dw, bot)]), fill=255)
    if split:
        g = 6 if small else 4.5
        md.rectangle(P([(0, 0), (l + w + g, 100)]), fill=0)
        md.rectangle(P([(r - g, 0), (100, 100)]), fill=0)
    img.paste(Image.new("RGBA", img.size, ink + (255,)), (0, 0), mask)
    d.polygon(P([(r, top), (r + w, top), (r + w, bot), (r, bot)]), fill=ink)  # right stem
    stem = stem or ink
    d.polygon(P([(l, top), (l + w, top), (l + w, 56), (l, 56)]), fill=stem)  # left stem
    d.polygon(P([(l - 10, 54), (l + w + 10, 54), (l + w / 2, bot)]), fill=stem)  # arrowhead


def render(size, variant="sticker"):
    ss = 8  # supersample, then downscale for clean edges
    s = size * ss
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    small = size <= 32
    shadow = 0 if (small or variant not in ("sticker", "arrow")) else round(s * 0.07)
    # Gaps blur below 24 px, so tiny sizes draw the strokes joined.
    split = size > 24 and variant in ("arrow-clean", "arrow-line", "arrow-dark-split", "arrow-dark-pop")
    pad = round(s * (0.04 if small else 0.06))
    box = s - pad * 2 - shadow
    x0 = y0 = pad
    radius = round(box * 0.24)
    outline = max(round(s * (0.075 if small else 0.045)), ss) if variant in ("sticker", "arrow", "arrow-line") else 0
    fill, ink = (POP, ON_POP)
    if shadow:
        d.rounded_rectangle((x0 + shadow, y0 + shadow, x0 + shadow + box, y0 + shadow + box), radius, fill=HARD)
    if variant.startswith("arrow-dark"):
        # Near-black with a soft top-to-bottom sheen, white mark, a 1 px lighter rim.
        grad = Image.new("RGBA", (box, box))
        gd = ImageDraw.Draw(grad)
        for yy in range(box):
            v = round(46 - 40 * yy / box)
            gd.line((0, yy, box, yy), fill=(v, v, v + 1, 255))
        mask = Image.new("L", (box, box), 0)
        ImageDraw.Draw(mask).rounded_rectangle((0, 0, box - 1, box - 1), radius, fill=255)
        img.paste(grad, (x0, y0), mask)
        d.rounded_rectangle((x0, y0, x0 + box, y0 + box), radius, outline=(70, 70, 72), width=max(ss, round(s * 0.008)))
        arrow_n(img, x0, y0, box, (250, 250, 248), small, split, POP if variant == "arrow-dark-pop" else None)
        return img.resize((size, size), Image.LANCZOS)
    d.rounded_rectangle((x0, y0, x0 + box, y0 + box), radius, fill=fill, outline=HARD if outline else None, width=outline)
    if variant.startswith("arrow"):
        arrow_n(img, x0, y0, box, ink, small, split)
        return img.resize((size, size), Image.LANCZOS)
    font = ImageFont.truetype(FONT, round(box * (0.78 if small else 0.7)))
    cx, cy = x0 + box / 2, y0 + box / 2
    l, t, r, b = d.textbbox((0, 0), "n", font=font)
    d.text((cx - (l + r) / 2, cy - (t + b) / 2), "n", font=font, fill=ink)
    return img.resize((size, size), Image.LANCZOS)


def preview(out):
    picks = sys.argv[3].split(",") if len(sys.argv) > 3 else ["sticker", "flat", "arrow", "arrow-dark"]
    variants = [(chr(ord("A") + i), v) for i, v in enumerate(picks)]
    cw, ch = 300, 190
    sheet = Image.new("RGB", (cw * len(variants), ch * 2 + 44), (255, 255, 255))
    d = ImageDraw.Draw(sheet)
    label = ImageFont.truetype(FONT, 22)
    for col, (letter, v) in enumerate(variants):
        for row, bg in enumerate([(244, 240, 230), (32, 32, 34)]):
            x, y = col * cw, row * ch
            d.rectangle((x, y, x + cw - 1, y + ch - 1), fill=bg)
            big = render(128, v)
            sheet.paste(big, (x + 18, y + 31), big)
            # What Windows shows: Alt-Tab 48, taskbar 32/24, Explorer list 16.
            ox = x + 162
            for sz in [48, 32]:
                im = render(sz, v)
                sheet.paste(im, (ox, y + 40), im)
                ox += sz + 12
            ox = x + 162
            for sz in [24, 20, 16]:
                im = render(sz, v)
                sheet.paste(im, (ox, y + 112), im)
                ox += sz + 14
        d.text((col * cw + 18, ch * 2 + 9), f"{letter}: {v}", font=label, fill=(20, 20, 20))
    sheet.save(out)


def main():
    variant = sys.argv[1] if len(sys.argv) > 1 else "arrow"
    out_dir = sys.argv[2] if len(sys.argv) > 2 else HERE
    os.makedirs(out_dir, exist_ok=True)
    if variant == "preview":
        preview(os.path.join(out_dir, "icon-options.png"))
        return
    if variant == "mark":
        # Just the arrow-N on transparent, for the in-app logo: Slint draws the sticker
        # (yellow, outline, shadow) and lays this over the whole square.
        s = 384
        img = Image.new("RGBA", (s * 4, s * 4), (0, 0, 0, 0))
        # Thick strokes: the nav rail shows it at 34 px.
        arrow_n(img, 0, 0, s * 4, ON_POP, True)
        img.resize((s, s), Image.LANCZOS).save(os.path.join(out_dir, "neria-mark.png"))
        return
    images = [render(sz, variant) for sz in SIZES]
    for sz, im in zip(SIZES, images):
        im.save(os.path.join(out_dir, f"neria-{sz}.png"))
    images[-1].save(os.path.join(out_dir, "neria.ico"), sizes=[(s, s) for s in SIZES], append_images=images[:-1])


if __name__ == "__main__":
    main()
