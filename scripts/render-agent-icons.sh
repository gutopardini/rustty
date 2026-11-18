#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
python3 - "$root" <<'PY'
from pathlib import Path
from tempfile import TemporaryDirectory
import subprocess
import sys

from PIL import Image, ImageDraw

root = Path(sys.argv[1])
source = root / "assets/agents/source"
names = ["claude", "codex", "kiro", "antigravity", "cursor", "settings", "rustty"]
atlas = Image.new("RGBA", (64 * len(names), 64))

with TemporaryDirectory(prefix="rustty-icons-") as tmp:
    for index, name in enumerate(names):
        if name in ("settings", "rustty"):
            path = root / "assets/brand" / ("settings.svg" if name == "settings" else "rustty-icon.svg")
        else:
            path = source / (name + (".png" if name == "claude" else ".svg"))
        if path.suffix == ".svg":
            raster = Path(tmp) / (name + ".png")
            density = "576" if name == "codex" else "144" if name == "settings" else "72"
            subprocess.run(["magick", "-background", "none", "-density", density, str(path), "-resize", "128x128", str(raster)], check=True)
            path = raster
        icon = Image.open(path).convert("RGBA")
        if name == "codex":
            # The official dark version needs a light surface on the dark theme.
            tile = Image.new("RGBA", (56, 56))
            ImageDraw.Draw(tile).rounded_rectangle((0, 0, 55, 55), radius=10, fill="#edeae4")
            icon.thumbnail((48, 48), Image.Resampling.LANCZOS)
            tile.alpha_composite(icon, ((56 - icon.width) // 2, (56 - icon.height) // 2))
            icon = tile
        else:
            icon.thumbnail((56, 56), Image.Resampling.LANCZOS)
        atlas.alpha_composite(icon, (index * 64 + (64 - icon.width) // 2, (64 - icon.height) // 2))

(root / "assets/agents/atlas.rgba").write_bytes(atlas.tobytes())
PY
