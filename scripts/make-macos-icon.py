#!/usr/bin/env python3
"""Build crates/client/BroLink.icns from crates/core/assets/logo-1024.png.

The logo is a rounded tile on a near-black pad. This crops the tile and
fills a 1024 canvas with it. macOS applies the rounded app-icon shape;
pre-masking the tile and leaving transparent corners made a square plate
on macOS 26.

    python3 scripts/make-macos-icon.py
"""
import pathlib
import shutil
import subprocess
import tempfile

from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parent.parent
LOGO = ROOT / "crates/core/assets/logo-1024.png"
ICNS = ROOT / "crates/client/BroLink.icns"

CANVAS = 1024


def tile_bounds(im):
    """The rounded tile inside the logo: where the pad colour ends on the
    middle row and column."""
    px = im.load()
    pad = px[0, 0]
    mid = im.width // 2
    xs = [x for x in range(im.width) if px[x, mid] != pad]
    ys = [y for y in range(im.height) if px[mid, y] != pad]
    return xs[0], ys[0], xs[-1] + 1, ys[-1] + 1


def main():
    logo = Image.open(LOGO).convert("RGBA")
    # Opaque fill: the Dock's shape comes from the system, not from alpha.
    canvas = (
        logo.crop(tile_bounds(logo))
        .resize((CANVAS, CANVAS), Image.LANCZOS)
        .convert("RGB")
        .convert("RGBA")
    )

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
