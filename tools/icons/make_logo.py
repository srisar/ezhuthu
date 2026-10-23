"""Renders the Gravitide logo for the welcome window's last card.

Writes two images next to the SVG, as raw premultiplied BGRA with a small
header (u32 width, u32 height, little-endian), so the app can draw them
without an image decoder:

- gravitide-light.bgra: the logo as designed, for light backgrounds;
- gravitide-dark.bgra: for dark backgrounds, with the small grey
  "technology.simplified" line turned light (the red wordmark and the icon
  tile stay as they are).

Run from tools/icons:

    uv run --with resvg-py make_logo.py ../../crates/app/logo/gravitide-logo-full.svg
"""

import argparse
import io
import struct
from pathlib import Path

import resvg_py
from PIL import Image

# Rendered width in pixels: 2x the logo's 240 px on screen at 96 DPI, so it
# stays sharp up to 200 % scaling (the app scales it down for smaller DPIs).
WIDTH = 480
LIGHT_TEXT = "#F1F2F6"


def dark_variant(svg: str) -> str:
    """Recolours the first group (the "technology.simplified" line)."""
    start = svg.index("<g>")
    end = svg.index("</g>", start)
    group = svg[start:end].replace('class="cls-2"', f'fill="{LIGHT_TEXT}"')
    return svg[:start] + group + svg[end:]


def render(svg: str, out: Path) -> None:
    png = bytes(resvg_py.svg_to_bytes(svg_string=svg, width=WIDTH))
    image = Image.open(io.BytesIO(png)).convert("RGBA")
    raw = image.tobytes()
    pixels = bytearray(len(raw))
    for i in range(0, len(raw), 4):
        r, g, b, a = raw[i : i + 4]
        # Premultiplied BGRA, as the app's canvas expects.
        pixels[i : i + 4] = bytes((b * a // 255, g * a // 255, r * a // 255, a))
    out.write_bytes(struct.pack("<II", image.width, image.height) + pixels)
    print(f"wrote {out} ({image.width} x {image.height})")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("svg", type=Path, help="gravitide-logo-full.svg")
    args = parser.parse_args()
    svg = args.svg.read_text(encoding="utf-8")
    folder = args.svg.parent
    render(svg, folder / "gravitide-light.bgra")
    render(dark_variant(svg), folder / "gravitide-dark.bgra")


if __name__ == "__main__":
    main()
