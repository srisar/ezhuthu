"""Draws the ezhuthu icon and writes the .ico files in crates/app/icons.

The mark is the Tamil letter எ followed by a text cursor on a dark rounded
tile ("on"); the "off" tray icon is the letter alone on grey. The letter is
the outline of U+0B8E from Anek Tamil ExtraBold (wght 800, wdth 100), SIL
Open Font License 1.1, read from the font file. The font is only needed to
regenerate the icons, never shipped:

    curl -L -o "work/AnekTamil[wdth,wght].ttf" \\
      "https://github.com/google/fonts/raw/main/ofl/anektamil/AnekTamil%5Bwdth,wght%5D.ttf"
    uv run make_icons.py "work/AnekTamil[wdth,wght].ttf" ../../crates/app/icons --preview work/preview.png

Small sizes (16 to 64 px) are laid out on whole pixels: the cursor and tile
are snapped, and the letter is nudged by a fraction of a pixel to the offset
that leaves the fewest half-covered pixels, so it stays crisp in the tray.
"""

import argparse
import io
import math
import struct
from pathlib import Path

import numpy as np
from fontTools.pens.basePen import BasePen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.ttLib import TTFont
from PIL import Image

TILE_ON = "#15171C"
TILE_OFF = "#6B7280"
GLYPH_ON = "#F7F7F5"
GLYPH_OFF = "#FFFFFF"
CARET = "#F5A524"
HAIRLINE = (1.0, 1.0, 1.0, 0.14)

APP_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
TRAY_SIZES = [16, 20, 24, 32, 40, 48]

# Whole-pixel layout per size: tile radius, letter height (ink), gap before
# the cursor, cursor width and height. Sizes not listed scale from 256.
SMALL = {
    16: dict(r=3, gh=8.0, gap=1, cw=2, ch=11),
    20: dict(r=4, gh=10.0, gap=1, cw=2, ch=13),
    24: dict(r=5, gh=12.0, gap=1, cw=2, ch=16),
    32: dict(r=7, gh=16.0, gap=2, cw=2, ch=21),
    40: dict(r=9, gh=20.0, gap=2, cw=3, ch=26),
    48: dict(r=11, gh=24.0, gap=2, cw=3, ch=31),
    64: dict(r=14, gh=31.0, gap=3, cw=4, ch=41),
}
LARGE = dict(r=0.22, gh=0.47, gap=0.035, cw=0.055, ch=0.62)


def rgba(hex_color, alpha=1.0):
    h = hex_color.lstrip("#")
    return tuple(int(h[i : i + 2], 16) / 255 for i in (0, 2, 4)) + (alpha,)


class FlattenPen(BasePen):
    """Collects a glyph as closed polygons in font units."""

    def __init__(self, glyphset, steps=24):
        super().__init__(glyphset)
        self.contours, self.steps = [], steps

    def _moveTo(self, p):
        self.contours.append([p])

    def _lineTo(self, p):
        self.contours[-1].append(p)

    def _qCurveToOne(self, p1, p2):
        p0 = self.contours[-1][-1]
        for i in range(1, self.steps + 1):
            t = i / self.steps
            u = 1 - t
            self.contours[-1].append(
                (u * u * p0[0] + 2 * u * t * p1[0] + t * t * p2[0],
                 u * u * p0[1] + 2 * u * t * p1[1] + t * t * p2[1])
            )

    def _curveToOne(self, p1, p2, p3):
        p0 = self.contours[-1][-1]
        for i in range(1, self.steps + 1):
            t = i / self.steps
            u = 1 - t
            self.contours[-1].append(
                (u**3 * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t**3 * p3[0],
                 u**3 * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t**3 * p3[1])
            )


def load_glyph(font_path):
    font = TTFont(font_path)
    glyphs = font.getGlyphSet(location={"wght": 800, "wdth": 100})
    glyph = glyphs[font.getBestCmap()[0x0B8E]]
    pen = FlattenPen(glyphs)
    glyph.draw(pen)
    svg = SVGPathPen(glyphs)
    glyph.draw(svg)
    pts = np.concatenate([np.array(c) for c in pen.contours])
    bounds = (*pts.min(axis=0), *pts.max(axis=0))
    return pen.contours, bounds, svg.getCommands()


def raster(contours, size, ss):
    """Coverage (0..1) of polygons under the nonzero rule, ss x ss samples per pixel."""
    n = size * ss
    acc = np.zeros((n, n + 1))
    for contour in contours:
        pts = np.asarray(contour, dtype=float) * ss
        for (x0, y0), (x1, y1) in zip(pts, np.roll(pts, -1, axis=0)):
            if y0 == y1:
                continue
            direction = 1 if y1 > y0 else -1
            lo, hi = sorted((y0, y1))
            rows = np.arange(max(0, math.ceil(lo - 0.5)), min(n, math.ceil(hi - 0.5)))
            if rows.size == 0:
                continue
            xs = x0 + (rows + 0.5 - y0) * (x1 - x0) / (y1 - y0)
            cols = np.clip(np.ceil(xs - 0.5), 0, n).astype(int)
            np.add.at(acc, (rows, cols), direction)
    inside = np.cumsum(acc, axis=1)[:, :n] != 0
    return inside.reshape(size, ss, size, ss).mean(axis=(1, 3))


def rounded_rect(x0, y0, x1, y1, r, steps=24):
    pts = []
    for cx, cy, a0 in ((x1 - r, y0 + r, -90), (x1 - r, y1 - r, 0), (x0 + r, y1 - r, 90), (x0 + r, y0 + r, 180)):
        for i in range(steps + 1):
            a = math.radians(a0 + 90 * i / steps)
            pts.append((cx + r * math.cos(a), cy + r * math.sin(a)))
    return pts


def over(dst, cov, color):
    """Composites a flat colour with per-pixel coverage onto straight-alpha RGBA."""
    a = cov * color[3]
    out_a = a + dst[..., 3] * (1 - a)
    rgb = np.array(color[:3])
    num = rgb * a[..., None] + dst[..., :3] * (dst[..., 3] * (1 - a))[..., None]
    dst[..., :3] = np.where(out_a[..., None] > 0, num / np.maximum(out_a, 1e-9)[..., None], 0)
    dst[..., 3] = out_a


def layout(size, glyph, ss, caret=True):
    """Places tile, letter and cursor for one size; returns the pieces to paint.

    Without the cursor (the "off" icon) the letter is centred on its own.
    """
    contours, (gx0, gy0, gx1, gy1), _ = glyph
    p = dict(SMALL.get(size) or {k: v * size for k, v in LARGE.items()})
    if not caret:
        p["gap"] = p["cw"] = 0
    k = p["gh"] / (gy1 - gy0)
    gw = (gx1 - gx0) * k
    left = (size - (gw + p["gap"] + p["cw"])) / 2
    top = (size - p["gh"]) / 2

    def placed(dx, dy):
        return [[(left + dx + (x - gx0) * k, top + dy + (gy1 - y) * k) for x, y in c] for c in contours]

    if size not in SMALL:
        x = left + gw + p["gap"]
        caret = (x, (size - p["ch"]) / 2, x + p["cw"], (size + p["ch"]) / 2)
        return p, raster(placed(0, 0), size, ss), caret

    # Try sub-pixel offsets; keep the one with the least partial coverage.
    best = None
    for dx in np.arange(-0.5, 0.5, 0.125):
        for dy in np.arange(-0.5, 0.5, 0.125):
            cov = raster(placed(dx, dy), size, 8)
            blur = float((cov * (1 - cov)).sum()) + 0.05 * (abs(dx) + abs(dy))
            if best is None or blur < best[0]:
                best = (blur, dx, dy)
    cov = raster(placed(best[1], best[2]), size, ss)
    cols = np.nonzero(cov.max(axis=0) > 0.25)[0]
    caret_x = cols[-1] + 1 + (p["gap"] if caret else 0)
    # Re-centre letter + cursor on whole pixels.
    shift = round((size - (caret_x + p["cw"] - cols[0])) / 2 - cols[0])
    cov = raster(placed(best[1] + shift, best[2]), size, ss)
    caret_x += shift
    caret_y = (size - p["ch"]) // 2
    return p, cov, (caret_x, caret_y, caret_x + p["cw"], caret_y + p["ch"])


def render(size, glyph, on, ss=16):
    p, glyph_cov, caret = layout(size, glyph, ss, caret=on)
    tile = raster([rounded_rect(0, 0, size, size, p["r"])], size, ss)
    img = np.zeros((size, size, 4))
    over(img, tile, rgba(TILE_ON if on else TILE_OFF))
    if on:
        w = max(1.0, size / 128)
        inner = raster([rounded_rect(w, w, size - w, size - w, max(p["r"] - w, 0))], size, ss)
        over(img, np.clip(tile - inner, 0, 1), HAIRLINE)
    over(img, glyph_cov * tile, rgba(GLYPH_ON if on else GLYPH_OFF))
    if on:
        over(img, raster([[(caret[0], caret[1]), (caret[2], caret[1]), (caret[2], caret[3]), (caret[0], caret[3])]], size, ss) * tile, rgba(CARET))
    return Image.fromarray(np.round(img * 255).astype(np.uint8), "RGBA")


def ico_bytes(images):
    """ICO with 32-bit BMP entries below 256 px and a PNG entry at 256 px."""
    blobs = []
    for im in images:
        s = im.width
        if s >= 256:
            buf = io.BytesIO()
            im.save(buf, "PNG", optimize=True)
            blobs.append(buf.getvalue())
            continue
        px = np.asarray(im)
        bgra = px[::-1, :, [2, 1, 0, 3]].tobytes()
        row = ((s + 31) // 32) * 4
        mask = bytearray()
        for y in range(s - 1, -1, -1):
            bits = np.packbits(px[y, :, 3] == 0).tobytes()
            mask += bits + bytes(row - len(bits))
        header = struct.pack("<IiiHHIIiiII", 40, s, 2 * s, 1, 32, 0, len(bgra) + len(mask), 0, 0, 0, 0)
        blobs.append(header + bgra + bytes(mask))
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for im, blob in zip(images, blobs):
        s = im.width if im.width < 256 else 0
        out += struct.pack("<BBBBHHII", s, s, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    return out + b"".join(blobs)


def svg_master(glyph, size=256):
    """The large layout as an SVG, for the website and docs."""
    contours, (gx0, gy0, gx1, gy1), path = glyph
    p = {k: v * size for k, v in LARGE.items()}
    k = p["gh"] / (gy1 - gy0)
    gw = (gx1 - gx0) * k
    left = (size - (gw + p["gap"] + p["cw"])) / 2
    top = (size - p["gh"]) / 2
    x = left + gw + p["gap"]
    f = lambda v: f"{v:.2f}".rstrip("0").rstrip(".")
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" role="img" aria-label="ezhuthu">\n'
        f'  <rect width="{size}" height="{size}" rx="{f(p["r"])}" fill="{TILE_ON}"/>\n'
        f'  <path fill="{GLYPH_ON}" transform="matrix({f(k)} 0 0 {f(-k)} {f(left - gx0 * k)} {f(top + gy1 * k)})" d="{path}"/>\n'
        f'  <rect x="{f(x)}" y="{f((size - p["ch"]) / 2)}" width="{f(p["cw"])}" height="{f(p["ch"])}" fill="{CARET}"/>\n'
        f"</svg>\n"
    )


def preview(glyph, path):
    """Each size at 1x and 8x, on dark and light grounds, on and off."""
    sizes = TRAY_SIZES + [64]
    zoom = 8
    width = 24 + sum(s * zoom + 24 for s in sizes)
    band = max(sizes) * (zoom + 1) + 48
    sheet = Image.new("RGBA", (width, band * 4), (0, 0, 0, 255))
    grounds = [(28, 28, 28), (236, 236, 236)]
    for gi, ground in enumerate(grounds):
        for oi, on in enumerate((True, False)):
            y = (gi * 2 + oi) * band
            sheet.paste(Image.new("RGBA", (width, band), ground + (255,)), (0, y))
            x = 24
            for s in sizes:
                im = render(s, glyph, on)
                big = im.resize((s * zoom, s * zoom), Image.NEAREST)
                sheet.alpha_composite(big, (x, y + 16))
                sheet.alpha_composite(im, (x, y + 16 + s * zoom + 8))
                x += s * zoom + 24
    sheet.save(path)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("font")
    ap.add_argument("out", type=Path)
    ap.add_argument("--preview", type=Path)
    args = ap.parse_args()
    glyph = load_glyph(args.font)
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "ezhuthu.ico").write_bytes(ico_bytes([render(s, glyph, True) for s in APP_SIZES]))
    (args.out / "tray-on.ico").write_bytes(ico_bytes([render(s, glyph, True) for s in TRAY_SIZES]))
    (args.out / "tray-off.ico").write_bytes(ico_bytes([render(s, glyph, False) for s in TRAY_SIZES]))
    (args.out / "ezhuthu.svg").write_text(svg_master(glyph), encoding="utf-8")
    if args.preview:
        preview(glyph, args.preview)


if __name__ == "__main__":
    main()
