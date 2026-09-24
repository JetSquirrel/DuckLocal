#!/usr/bin/env python3
"""Draw the app icon and the dmg background from assets/logo.png.

The outputs are committed; nothing in the build runs this. Re-run it after
changing the logo or the dmg layout (needs Pillow, and macOS for iconutil
and tiffutil):

    python3 scripts/make-icons.py

assets/AppIcon.icns
    The duck on Apple's macOS icon grid: an 824px rounded square centred
    in a 1024px canvas. macOS 26 shrinks an icon that is not that shape
    into a grey rounded square of its own, which is why the old icon, a
    tile with a soft shadow baked in, looked small and boxed.

assets/dmg-background.tiff
    The dmg window's background at 1x and 2x: an arrow from the app to
    Applications, and nothing else. The icon positions it
    is drawn for live in scripts/dmg-settings.py.
"""

import os
import subprocess
import tempfile

from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "assets")

# The drawing inside logo.png (DRAWING), with paper round it to fade out
# (ART_BOX), and the paper colour.
DRAWING = (269, 358, 979, 907)
ART_BOX = (200, 290, 1050, 975)
PAPER = (253, 252, 250, 255)

# Apple's grid, at 1024px.
CANVAS = 1024
BODY = 824

# The dmg window, in points; scripts/dmg-settings.py uses the same numbers.
WINDOW = (640, 320)
APP_AT = (170, 150)
APPLICATIONS_AT = (470, 150)
ICON_SIZE = 128


def squircle(size, scale=4):
    """A mask of Apple's continuous-corner rounded square, antialiased."""
    big = size * scale
    mask = Image.new("L", (big, big), 0)
    px = mask.load()
    half = big / 2
    n = 5.0  # a superellipse this round matches the macOS shape closely
    for y in range(big):
        dy = abs((y + 0.5 - half) / half) ** n
        if dy >= 1:
            continue
        dx = (1 - dy) ** (1 / n) * half
        for x in range(int(half - dx), int(half + dx) + 1):
            if 0 <= x < big:
                px[x, y] = 255
    return mask.resize((size, size), Image.LANCZOS)


def icon():
    art = Image.open(os.path.join(ASSETS, "logo.png")).convert("RGBA").crop(ART_BOX)
    # The drawing at 78% of the body's width: big enough to read in the
    # Dock at 32px, with the paper still showing round it.
    scale = BODY * 0.78 / (DRAWING[2] - DRAWING[0])
    art = art.resize((round(art.width * scale), round(art.height * scale)), Image.LANCZOS)
    # Matte out the paper: the logo's paper is not quite flat, and pasting
    # it as it is shows its edge as a faint rectangle. What is left is the
    # ink, over a paper that is flat.
    ink = ImageChops.difference(art.convert("RGB"), Image.new("RGB", art.size, PAPER[:3]))
    ink = ink.convert("L").point(lambda v: min(255, max(0, (v - 6) * 8)))
    art.putalpha(ink)

    body = Image.new("RGBA", (BODY, BODY), PAPER)
    body.alpha_composite(art, ((BODY - art.width) // 2, (BODY - art.height) // 2 + 8))
    mask = squircle(BODY)
    body.putalpha(mask)

    # The drop shadow Apple's template carries under the body.
    canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    shadow = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    shade = Image.new("RGBA", (BODY, BODY), (0, 0, 0, 90))
    shade.putalpha(ImageChops.multiply(mask, Image.new("L", mask.size, 90)))
    offset = (CANVAS - BODY) // 2
    shadow.alpha_composite(shade, (offset, offset + 10))
    canvas.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(14)))
    canvas.alpha_composite(body, (offset, offset))

    work = tempfile.mkdtemp()
    iconset = os.path.join(work, "AppIcon.iconset")
    os.mkdir(iconset)
    for size in (16, 32, 128, 256, 512):
        for factor, suffix in ((1, ""), (2, "@2x")):
            px = size * factor
            canvas.resize((px, px), Image.LANCZOS).save(
                os.path.join(iconset, f"icon_{size}x{size}{suffix}.png")
            )
    subprocess.run(
        ["iconutil", "-c", "icns", iconset, "-o", os.path.join(ASSETS, "AppIcon.icns")],
        check=True,
    )
    canvas.save(os.path.join(work, "preview.png"))
    print("wrote assets/AppIcon.icns; preview:", os.path.join(work, "preview.png"))


def background(scale):
    w, h = WINDOW[0] * scale, WINDOW[1] * scale
    img = Image.new("RGB", (w, h), (250, 248, 243))
    draw = ImageDraw.Draw(img)

    # The arrow: between the two icons, at their centre height.
    amber = (201, 150, 20)
    y = APP_AT[1] * scale
    x0 = (APP_AT[0] + ICON_SIZE // 2 + 30) * scale
    x1 = (APPLICATIONS_AT[0] - ICON_SIZE // 2 - 30) * scale
    stroke = 5 * scale
    draw.line([(x0, y), (x1 - 8 * scale, y)], fill=amber, width=stroke)
    head = 14 * scale
    draw.polygon([(x1 + 2 * scale, y), (x1 - head, y - head * 0.8), (x1 - head, y + head * 0.8)], fill=amber)
    draw.ellipse([x0 - stroke, y - stroke, x0 + stroke, y + stroke], fill=amber)

    return img


def dmg_background():
    work = tempfile.mkdtemp()
    one = os.path.join(work, "bg.png")
    two = os.path.join(work, "bg@2x.png")
    background(1).save(one, dpi=(72, 72))
    background(2).save(two, dpi=(144, 144))
    out = os.path.join(ASSETS, "dmg-background.tiff")
    subprocess.run(["tiffutil", "-cathidpicheck", one, two, "-out", out], check=True)
    print("wrote assets/dmg-background.tiff; preview:", two)


if __name__ == "__main__":
    icon()
    dmg_background()
