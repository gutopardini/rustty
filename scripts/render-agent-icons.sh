#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
python3 - "$root" <<'PY'
from pathlib import Path
from tempfile import TemporaryDirectory
import subprocess
import sys

from PIL import Image

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
            raster = Path(tmp) / (name + ".png")
            density = "144" if name == "settings" else "72"
            subprocess.run(["magick", "-background", "none", "-density", density, str(path), "-resize", "128x128", str(raster)], check=True)
            path = raster
        icon = Image.open(path).convert("RGBA")
        icon.thumbnail((56, 56), Image.Resampling.LANCZOS)
        atlas.alpha_composite(icon, (index * 64 + (64 - icon.width) // 2, (64 - icon.height) // 2))

(root / "assets/agents/atlas.rgba").write_bytes(atlas.tobytes())
PY
