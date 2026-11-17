#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
icon="$root/assets/brand/rustty-icon.svg"

# Os SVGs do ícone e do nome são as fontes; as assinaturas combinam os dois.
python3 - "$root" <<'PY'
import copy
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

root = Path(sys.argv[1])
brand = root / "assets/brand"
svg = "http://www.w3.org/2000/svg"
ET.register_namespace("", svg)
icon = ET.parse(brand / "rustty-icon.svg").getroot()
word = ET.parse(brand / "rustty-wordmark.svg").getroot()

prompt = icon.find(f"{{{svg}}}path[@id='prompt']")
cursor = icon.find(f"{{{svg}}}rect[@id='cursor']")
if prompt is None or cursor is None:
    raise SystemExit("O ícone precisa dos elementos prompt e cursor")
mark = ET.Element(f"{{{svg}}}svg", {
    "width": "320", "height": "220", "viewBox": "96 146 310 220",
    "role": "img", "aria-labelledby": "title",
})
ET.SubElement(mark, f"{{{svg}}}title", {"id": "title"}).text = "Rustty — símbolo"
for original in (prompt, cursor):
    shape = copy.deepcopy(original)
    shape.attrib.pop("id", None)
    shape.set("fill", cursor.get("fill", "#f0a77d"))
    mark.append(shape)
ET.ElementTree(mark).write(brand / "rustty-mark.svg", encoding="unicode", xml_declaration=True)

for filename, color in [("rustty-lockup.svg", "#edeae4"), ("rustty-lockup-light.svg", "#252a2e")]:
    logo = ET.Element(f"{{{svg}}}svg", {
        "width": "710", "height": "256", "viewBox": "0 0 710 256",
        "role": "img", "aria-labelledby": "title",
    })
    ET.SubElement(logo, f"{{{svg}}}title", {"id": "title"}).text = "Rustty"
    emblem = copy.deepcopy(icon)
    emblem.attrib.pop("aria-labelledby", None)
    emblem.attrib.pop("role", None)
    for title in emblem.findall(f"{{{svg}}}title"):
        emblem.remove(title)
    emblem.set("width", "256")
    emblem.set("height", "256")
    logo.append(emblem)
    letters = ET.SubElement(logo, f"{{{svg}}}g", {"transform": "translate(280 38)", "fill": color})
    for path in word.findall(f"{{{svg}}}path"):
        letter = copy.deepcopy(path)
        letter.attrib.pop("fill", None)
        letters.append(letter)
    ET.ElementTree(logo).write(brand / filename, encoding="unicode", xml_declaration=True)
PY

# O RGBA é incorporado ao binário para o ícone no X11.
magick -background none -density 144 "$icon" -resize 512x512 -strip PNG32:"$root/assets/brand/rustty-icon.png"
magick -background none -density 144 "$icon" -resize 64x64 -depth 8 RGBA:"$root/assets/brand/rustty-window.rgba"
