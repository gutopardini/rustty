#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
python3 - "$root" <<'PY'
from pathlib import Path
from tempfile import TemporaryDirectory
import subprocess
import sys

from PIL import Image

# Fit each transparent mark inside 58x58 centered in a 64x64 tile so linear GPU
# filtering keeps smooth antialiased edges without bleeding into neighboring tiles.
TILE = 64
CONTENT = 58
ALPHA_CLIP = 1


def content_box(image):
    mask = image.getchannel("A").point(lambda value: 255 if value >= ALPHA_CLIP else 0)
    return mask.getbbox() or image.getbbox() or (0, 0, image.width, image.height)


def to_premultiplied(image):
    pixels = bytearray(image.tobytes())
    for offset in range(0, len(pixels), 4):
        alpha = pixels[offset + 3]
        if alpha == 0:
            pixels[offset:offset + 3] = b"\0\0\0"
        elif alpha < 255:
            for channel in range(3):
                pixels[offset + channel] = (pixels[offset + channel] * alpha + 127) // 255
    return Image.frombytes("RGBA", image.size, bytes(pixels))


def fit_icon(image):
    image = image.crop(content_box(image))
    width, height = image.size
    scale = CONTENT / max(width, height)
    # Pillow's RGBA Lanczos resize handles premultiplication internally and returns straight RGBA.
    fitted = image.resize((max(1, round(width * scale)), max(1, round(height * scale))), Image.Resampling.LANCZOS)
    canvas = Image.new("RGBA", (TILE, TILE), (0, 0, 0, 0))
    canvas.paste(fitted, ((TILE - fitted.width) // 2, (TILE - fitted.height) // 2))
    return to_premultiplied(canvas)


root = Path(sys.argv[1])
source = root / "assets/agents/source"
names = ["claude", "codex", "kiro", "antigravity", "cursor", "settings", "rustty"]
atlas = Image.new("RGBA", (TILE * len(names), TILE), (0, 0, 0, 0))

with TemporaryDirectory(prefix="rustty-icons-") as tmp:
    for index, name in enumerate(names):
        if name in ("settings", "rustty"):
            path = root / "assets/brand" / ("settings.svg" if name == "settings" else "rustty-icon.svg")
        else:
            path = source / f"{name}.svg"
        raster = Path(tmp) / f"{name}.png"
        subprocess.run(
            ["magick", "-background", "none", "-density", "256", str(path), "-resize", "256x256", str(raster)],
            check=True,
        )
        icon = fit_icon(Image.open(raster).convert("RGBA"))
        atlas.paste(icon, (index * TILE, 0))

# Store premultiplied RGBA so linear sampling blends cleanly over dark glass and light themes.
(root / "assets/agents/atlas.rgba").write_bytes(atlas.tobytes())
PY
