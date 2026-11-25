#!/usr/bin/env python3
"""Claude Code statusLine bridge: mirrors real 7d/5h rate_limits into ~/.config/rustty/limits.json."""

import datetime
import json
import os
import sys
from pathlib import Path


def parse_used(obj):
    if not isinstance(obj, dict):
        return None
    for key in ("used_percentage", "used_percent", "utilization"):
        val = obj.get(key)
        if isinstance(val, (int, float)):
            # Some versions report 0..1 fraction, others 0..100 percentage.
            return val * 100.0 if 0.0 < val <= 1.0 and key == "utilization" else float(val)
    return None


def parse_resets_at(obj):
    if not isinstance(obj, dict):
        return None
    val = obj.get("resets_at")
    if isinstance(val, (int, float)) and val > 0:
        return int(val)
    if isinstance(val, str) and val:
        try:
            dt = datetime.datetime.fromisoformat(val.replace("Z", "+00:00"))
            return int(dt.timestamp())
        except ValueError:
            return None
    return None


def config_path():
    base = Path(os.environ.get("XDG_CONFIG_HOME") or (Path.home() / ".config"))
    return base / "rustty" / "limits.json"


def update_limits_file(claude_entry):
    path = config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    data = {"title": "AI Limits", "subtitle": "Quota · Models", "items": []}
    if path.is_file():
        try:
            existing = json.loads(path.read_text(encoding="utf-8"))
            if isinstance(existing, list):
                data["items"] = existing
            elif isinstance(existing, dict):
                data.update(existing)
                if not isinstance(data.get("items"), list):
                    data["items"] = []
        except Exception:
            pass

    items = [item for item in data["items"] if not (isinstance(item, dict) and item.get("id") == "claude")]
    items.append(claude_entry)
    data["items"] = items

    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, path)


def main():
    raw = sys.stdin.read()
    if not raw.strip():
        return
    try:
        payload = json.loads(raw)
    except Exception:
        return

    rate = payload.get("rate_limits") or {}
    seven = rate.get("seven_day") or rate.get("7d")
    five = rate.get("five_hour") or rate.get("5h")
    used_7d = parse_used(seven)
    used_5h = parse_used(five)
    reset_7d = parse_resets_at(seven)
    reset_5h = parse_resets_at(five)

    parts = []
    model = payload.get("model")
    if isinstance(model, dict) and model.get("display_name"):
        parts.append(str(model["display_name"]))
    elif isinstance(model, str) and model:
        parts.append(model)

    ctx = payload.get("context_window") or {}
    if isinstance(ctx.get("used_percentage"), (int, float)):
        parts.append(f"ctx {max(0, min(100, round(100 - ctx['used_percentage'])))}%")

    if used_7d is not None or used_5h is not None:
        if used_7d is not None and used_5h is not None:
            entry = {
                "id": "claude",
                "label": "Claude",
                "used_percent": round(used_7d, 1),
                "color": "#D97757",
                "agent": "claude",
                "sub": {
                    "label": "5h",
                    "used_percent": round(used_5h, 1),
                    "color": "#F2A47E",
                },
            }
            if reset_7d:
                entry["resets_at"] = reset_7d
            if reset_5h:
                entry["sub"]["resets_at"] = reset_5h
            parts.append(f"7d {max(0, min(100, round(100 - used_7d)))}%")
            parts.append(f"5h {max(0, min(100, round(100 - used_5h)))}%")
        else:
            used = used_7d if used_7d is not None else used_5h
            reset = reset_7d if used_7d is not None else reset_5h
            entry = {
                "id": "claude",
                "label": "Claude",
                "used_percent": round(used, 1),
                "color": "#D97757",
                "agent": "claude",
            }
            if reset:
                entry["resets_at"] = reset
            tag = "7d" if used_7d is not None else "5h"
            parts.append(f"{tag} {max(0, min(100, round(100 - used)))}%")
        try:
            update_limits_file(entry)
        except Exception:
            pass

    if parts:
        print(" · ".join(parts))


if __name__ == "__main__":
    main()
