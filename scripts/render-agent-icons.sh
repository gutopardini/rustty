#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
python3 - "$root" <<'PY'
from pathlib import Path
from tempfile import TemporaryDirectory
import subprocess
import sys

from PIL import Image

# Solid core of the artwork. The soft rim around the sources is a light fringe, and a
# 56 px thumbnail inside the 64 px cell leaves a gap, so the mark looks unseated.
OPAQUE = 160
OVERSCAN = 68


def content_box(image):
    mask = image.getchannel("A").point(lambda value: 255 if value >= OPAQUE else 0)
    return mask.getbbox() or image.getbbox() or (0, 0, image.width, image.height)


def resize_straight(image, size):
    # Premultiply so the resize does not blend the light RGB stored in clear pixels.
    pixels = bytearray(image.tobytes())
    for offset in range(0, len(pixels), 4):
        alpha = pixels[offset + 3]
        if alpha == 255:
            continue
        for channel in range(3):
            pixels[offset + channel] = pixels[offset + channel] * alpha // 255
    scaled = Image.frombytes("RGBA", image.size, bytes(pixels)).resize(size, Image.Resampling.LANCZOS)
    pixels = bytearray(scaled.tobytes())
    for offset in range(0, len(pixels), 4):
        alpha = pixels[offset + 3]
        if alpha == 0:
            pixels[offset:offset + 3] = b"\0\0\0"
            continue
        if alpha == 255:
            continue
        for channel in range(3):
            pixels[offset + channel] = min(255, pixels[offset + channel] * 255 // alpha)
    return Image.frombytes("RGBA", size, bytes(pixels))


def fit_icon(image, bleed=True):
    image = image.crop(content_box(image))
    width, height = image.size
    side = OVERSCAN if bleed else 64
    if abs(width - height) <= max(width, height) * 0.02:
        fitted = resize_straight(image, (side, side))
    else:
        scale = side / max(width, height)
        fitted = resize_straight(image, (max(1, round(width * scale)), max(1, round(height * scale))))
    left = max(0, (fitted.width - 64) // 2)
    top = max(0, (fitted.height - 64) // 2)
    icon = fitted.crop((left, top, min(fitted.width, left + 64), min(fitted.height, top + 64)))
    if icon.size != (64, 64):
        canvas = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
        canvas.alpha_composite(icon, ((64 - icon.width) // 2, (64 - icon.height) // 2))
        icon = canvas
    # Cut the filtered rim the resize puts back, without eating the shape.
    alpha = icon.getchannel("A").point(lambda value: 255 if value >= 128 else 0)
    red, green, blue, _ = icon.split()
    return Image.merge("RGBA", (red, green, blue, alpha))


root = Path(sys.argv[1])
source = root / "assets/agents/source"
names = ["claude", "codex", "kiro", "antigravity", "cursor", "settings", "rustty"]
atlas = Image.new("RGBA", (64 * len(names), 64))

with TemporaryDirectory(prefix="rustty-icons-") as tmp:
    for index, name in enumerate(names):
        if name in ("settings", "rustty"):
            path = root / "assets/brand" / ("settings.svg" if name == "settings" else "rustty-icon.svg")
        else:
            path = source / (name + (".png" if name in ("claude", "codex") else ".svg"))
        if path.suffix == ".svg":
            svg = path.read_text()
            # The Cursor mark draws a pale stroke around the squircle. On a dark tab that stroke
            # reads as a white rim that does not meet the icon box.
            if name == "cursor":
                svg = svg.replace(' stroke="#edecec" stroke-opacity=".2" stroke-width="8"', "")
            raster = Path(tmp) / (name + ".png")
            subprocess.run(
                ["magick", "-background", "none", "-density", "192", "svg:-", "-resize", "256x256", str(raster)],
                input=svg.encode(),
                check=True,
            )
            path = raster
        icon = fit_icon(Image.open(path).convert("RGBA"), bleed=name != "settings")
        atlas.alpha_composite(icon, (index * 64, 0))

(root / "assets/agents/atlas.rgba").write_bytes(atlas.tobytes())
PY
