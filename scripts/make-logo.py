#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 DaftHD
"""Draws the Demon Cut logo and writes every icon the builds ship.

The mark is a horned "D" on a crimson tile, split by one diagonal cut. It is
defined here as plain geometry, so the SVG and every PNG come from the same
shapes and nothing needs a vector renderer: Pillow alone draws it,
supersampled and scaled down for clean edges.

    python3 scripts/make-logo.py

writes
    assets/demoncut_logo.svg                  the master, for editing
    assets/demoncut_logo_512.png              the 512px render
    assets/demoncut-mark.png                  the README's header image
    assets/icons/demoncut_logo_<n>.png        16, 32, 64, 128, 256, 512
    assets/icons/demoncut.ico                 the Windows icon, 16-256px
    src/crates/concat-android/res/mipmap-*/ic_launcher.png
"""

import math
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
UNIT = 512  # the design grid

TOP = (0xE3, 0x24, 0x3B)  # tile gradient, top
BOTTOM = (0x4E, 0x06, 0x14)  # tile gradient, bottom
INK = (0xFF, 0xF4, 0xEC)  # the mark
RADIUS = 112  # tile corner radius


def bezier(p0, p1, p2, steps=24):
    """Points along a quadratic Bezier, p0 to p2."""
    out = []
    for i in range(steps + 1):
        t = i / steps
        a = (1 - t) ** 2
        b = 2 * (1 - t) * t
        c = t * t
        out.append((a * p0[0] + b * p1[0] + c * p2[0], a * p0[1] + b * p1[1] + c * p2[1]))
    return out


def half_ellipse(cx, cy, rx, ry, steps=48):
    """The right half of an ellipse, top to bottom."""
    return [
        (cx + rx * math.cos(a), cy + ry * math.sin(a))
        for a in (-math.pi / 2 + math.pi * i / steps for i in range(steps + 1))
    ]


def shapes():
    """The mark as (polygon, filled) pairs, in drawing order: filled adds
    ink, unfilled removes it."""
    # The D: a stem and a bowl, with its counter cut out.
    left, top, bottom = 150, 205, 415
    cy = (top + bottom) / 2
    bowl_x, bowl_rx, ry = 262, 128, (bottom - top) / 2
    stroke = 60
    outer = [(left, top)] + half_ellipse(bowl_x, cy, bowl_rx, ry) + [(left, bottom)]
    inner = (
        [(left + stroke, top + stroke)]
        + half_ellipse(bowl_x, cy, bowl_rx - stroke, ry - stroke)
        + [(left + stroke, bottom - stroke)]
    )
    # The cut: a band from lower left to upper right through the D.
    angle = math.radians(-38)
    dx, dy = math.cos(angle), math.sin(angle)
    nx, ny = -dy, dx
    half, length = 10, 165
    cx0, cy0 = 258, 322
    cut = [
        (cx0 - dx * length + nx * half, cy0 - dy * length + ny * half),
        (cx0 + dx * length + nx * half, cy0 + dy * length + ny * half),
        (cx0 + dx * length - nx * half, cy0 + dy * length - ny * half),
        (cx0 - dx * length - nx * half, cy0 - dy * length - ny * half),
    ]
    # Two horns curling up and out from the top of the D, drawn last so the
    # cut stops at them.
    horn_left = bezier((150, top + 2), (126, 168), (112, 104)) + bezier((112, 104), (168, 148), (214, top + 2))
    horn_right = bezier((300, top + 6), (346, 150), (398, 104)) + bezier((398, 104), (396, 186), (370, top + 46))
    return [(outer, True), (inner, False), (cut, False), (horn_left, True), (horn_right, True)]


def render(size):
    """The logo at `size` pixels square, as RGBA."""
    scale = 4
    big = size * scale
    k = big / UNIT

    # Tile: a vertical gradient under a rounded-square mask.
    gradient = Image.new("RGB", (1, 256))
    for y in range(256):
        t = y / 255
        gradient.putpixel((0, y), tuple(round(TOP[i] + (BOTTOM[i] - TOP[i]) * t) for i in range(3)))
    gradient = gradient.resize((big, big))
    tile = Image.new("L", (big, big), 0)
    ImageDraw.Draw(tile).rounded_rectangle((0, 0, big - 1, big - 1), radius=RADIUS * k, fill=255)

    # Mark: ink added and removed in order.
    mark = Image.new("L", (big, big), 0)
    draw = ImageDraw.Draw(mark)
    for polygon, filled in shapes():
        draw.polygon([(x * k, y * k) for x, y in polygon], fill=255 if filled else 0)

    out = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    out.paste(gradient, (0, 0), tile)
    ink = Image.new("RGBA", (big, big), INK + (255,))
    out.paste(ink, (0, 0), mark)
    return out.resize((size, size), Image.LANCZOS)


def svg():
    """The same geometry as an SVG, with the cut and counter as a mask."""
    def path(points):
        return "M" + " L".join(f"{x:.1f},{y:.1f}" for x, y in points) + " Z"

    layers = "".join(
        f'<path d="{path(p)}" fill="{"#fff" if filled else "#000"}"/>' for p, filled in shapes()
    )
    top = "#%02X%02X%02X" % TOP
    bottom = "#%02X%02X%02X" % BOTTOM
    inkc = "#%02X%02X%02X" % INK
    # Ink first, holes after, horns on top: the same order as render().
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {UNIT} {UNIT}" width="{UNIT}" height="{UNIT}">
  <title>Demon Cut</title>
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{top}"/>
      <stop offset="1" stop-color="{bottom}"/>
    </linearGradient>
    <mask id="mark" maskUnits="userSpaceOnUse" x="0" y="0" width="{UNIT}" height="{UNIT}">
      {layers}
    </mask>
  </defs>
  <rect width="{UNIT}" height="{UNIT}" rx="{RADIUS}" fill="url(#tile)"/>
  <rect width="{UNIT}" height="{UNIT}" fill="{inkc}" mask="url(#mark)"/>
</svg>
"""


def main():
    assets = ROOT / "assets"
    icons = assets / "icons"
    icons.mkdir(parents=True, exist_ok=True)
    (assets / "demoncut_logo.svg").write_text(svg())
    master = render(512)
    master.save(assets / "demoncut_logo_512.png", optimize=True)
    master.save(assets / "demoncut-mark.png", optimize=True)
    for n in (16, 32, 64, 128, 256, 512):
        render(n).save(icons / f"demoncut_logo_{n}.png", optimize=True)
    render(256).save(icons / "demoncut.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    res = ROOT / "src" / "crates" / "concat-android" / "res"
    for density, n in (("mdpi", 48), ("hdpi", 72), ("xhdpi", 96), ("xxhdpi", 144), ("xxxhdpi", 192)):
        folder = res / f"mipmap-{density}"
        folder.mkdir(parents=True, exist_ok=True)
        render(n).save(folder / "ic_launcher.png", optimize=True)
    print("wrote the Demon Cut logo and icons")


if __name__ == "__main__":
    main()
