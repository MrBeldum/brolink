#!/usr/bin/env python3
"""Build the app icon from the logo's geometry.

Writes crates/core/assets/logo-1024.png (the bitmap every window, the
Windows executable and the Mac bundle start from) and crates/client/Latch.icns.

The mark is drawn from the same rectangles as docs/brand/logo.svg, on a
view box whose mark runs 14..86: a square with the lower right corner cut
away, and a smaller square in the cut. The icon is a full-bleed ink tile
with the mark knocked out in white; macOS applies the rounded app-icon
shape and Windows 11 rounds the square, so nothing here is pre-masked. The
blue is the logo's, lightened to read on ink.

    python3 scripts/make-icons.py
"""
import pathlib
import shutil
import subprocess
import tempfile

from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
LOGO = ROOT / "crates/core/assets/logo-1024.png"
ICNS = ROOT / "crates/client/Latch.icns"

INK = (0x1C, 0x1D, 0x1A)
KNOCKOUT = (0xFF, 0xFF, 0xFF)
BLUE = (0x4F, 0x8F, 0xE8)

CANVAS = 1024
# How much of the tile the mark's 72-unit span fills.
FILL = 0.60
SUPERSAMPLE = 4


def draw(size):
    big = size * SUPERSAMPLE
    im = Image.new("RGB", (big, big), INK)
    d = ImageDraw.Draw(im)
    unit = big * FILL / 72.0
    origin = (big - 72 * unit) / 2 - 14 * unit

    def rect(x0, y0, x1, y1, colour):
        d.rectangle(
            [
                round(origin + x0 * unit),
                round(origin + y0 * unit),
                round(origin + x1 * unit) - 1,
                round(origin + y1 * unit) - 1,
            ],
            fill=colour,
        )

    rect(14, 14, 86, 54, KNOCKOUT)
    rect(14, 54, 54, 86, KNOCKOUT)
    rect(60, 60, 86, 86, BLUE)
    return im.resize((size, size), Image.LANCZOS).convert("RGBA")


def main():
    canvas = draw(CANVAS)
    canvas.save(LOGO, optimize=True)
    print(f"wrote {LOGO}")
    with tempfile.TemporaryDirectory() as tmp:
        iconset = pathlib.Path(tmp) / "Latch.iconset"
        iconset.mkdir()
        for points in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                px = points * scale
                name = f"icon_{points}x{points}" + ("@2x" if scale == 2 else "") + ".png"
                canvas.resize((px, px), Image.LANCZOS).save(iconset / name)
        subprocess.run(
            ["iconutil", "--convert", "icns", "--output", str(ICNS), str(iconset)],
            check=True,
        )
        shutil.copy(iconset / "icon_512x512@2x.png", pathlib.Path(tmp) / "preview.png")
        print(f"wrote {ICNS}")


if __name__ == "__main__":
    main()
