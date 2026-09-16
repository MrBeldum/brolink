#!/usr/bin/env python3
"""Build crates/client/BroLink.icns from crates/core/assets/logo-1024.png.

macOS draws every app icon inside the same rounded shape, inset from the
edges of a 1024-pixel canvas, with the corners transparent. The logo is a
square: its rounded tile on a near-black pad. This crops the tile, gives it
Apple's shape and inset (824 of 1024 pixels, corner radius 22.37% of the
side), and writes the iconset iconutil turns into the .icns.

    python3 scripts/make-macos-icon.py
"""
import pathlib
import shutil
import subprocess
import tempfile

from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
LOGO = ROOT / "crates/core/assets/logo-1024.png"
ICNS = ROOT / "crates/client/BroLink.icns"

CANVAS = 1024
TILE = 824  # Apple's icon grid: the shape spans 824 of 1024 pixels.
RADIUS = round(TILE * 0.2237)
OVERSAMPLE = 4


def tile_bounds(im):
    """The rounded tile inside the logo: where the pad colour ends on the
    middle row and column."""
    px = im.load()
    pad = px[0, 0]
    mid = im.width // 2
    xs = [x for x in range(im.width) if px[x, mid] != pad]
    ys = [y for y in range(im.height) if px[mid, y] != pad]
    return xs[0], ys[0], xs[-1] + 1, ys[-1] + 1


def rounded_mask(size, radius):
    big = size * OVERSAMPLE
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, big - 1, big - 1), radius=radius * OVERSAMPLE, fill=255
    )
    return mask.resize((size, size), Image.LANCZOS)


def main():
    logo = Image.open(LOGO).convert("RGBA")
    tile = logo.crop(tile_bounds(logo)).resize((TILE, TILE), Image.LANCZOS)
    tile.putalpha(rounded_mask(TILE, RADIUS))
    canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    inset = (CANVAS - TILE) // 2
    canvas.paste(tile, (inset, inset), tile)

    with tempfile.TemporaryDirectory() as tmp:
        iconset = pathlib.Path(tmp) / "BroLink.iconset"
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
