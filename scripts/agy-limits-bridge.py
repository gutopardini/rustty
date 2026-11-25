#!/usr/bin/env python3
"""Antigravity CLI (agy) statusLine bridge: mirrors real Gemini weekly/5h quota into ~/.config/rustty/limits.json."""

import datetime
import json
import os
import sys
import time
from pathlib import Path


def parse_remaining_percent(bucket):
    if not isinstance(bucket, dict):
        return None
    frac = bucket.get("remaining_fraction")
    if isinstance(frac, (int, float)):
        return max(0.0, min(100.0, float(frac) * 100.0))
    for key in ("remaining_percent", "remaining_percentage", "percent"):
        val = bucket.get(key)
        if isinstance(val, (int, float)):
            return max(0.0, min(100.0, float(val)))
    for key in ("used_fraction",):
        val = bucket.get(key)
        if isinstance(val, (int, float)):
            return max(0.0, min(100.0, (1.0 - float(val)) * 100.0))
    for key in ("used_percent", "used_percentage"):
        val = bucket.get(key)
        if isinstance(val, (int, float)):
            return max(0.0, min(100.0, 100.0 - float(val)))
    return None


def parse_resets_at(bucket):
    if not isinstance(bucket, dict):
        return None
    secs = bucket.get("reset_in_seconds")
    if isinstance(secs, (int, float)) and secs > 0:
        return int(time.time() + secs)
    for key in ("reset_time", "resets_at"):
        val = bucket.get(key)
        if isinstance(val, (int, float)) and val > 0:
            return int(val)
        if isinstance(val, str) and val:
            try:
                dt = datetime.datetime.fromisoformat(val.replace("Z", "+00:00"))
                return int(dt.timestamp())
            except ValueError:
                pass
    return None


def config_path():
    base = Path(os.environ.get("XDG_CONFIG_HOME") or (Path.home() / ".config"))
    return base / "rustty" / "limits.json"


def update_limits_file(gemini_entry):
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

    items = [item for item in data["items"] if not (isinstance(item, dict) and item.get("id") == "gemini")]
    items.append(gemini_entry)
    data["items"] = items

    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, path)


def pick_gemini_windows(quota):
    if not isinstance(quota, dict):
        return None, None
    # Prefer buckets explicitly tied to Gemini; skip Claude/other model buckets if present.
    candidates = []
    for key, bucket in quota.items():
        if not isinstance(bucket, dict):
            continue
        low = str(key).lower()
        if "claude" in low or "gpt" in low:
            continue
        rem = parse_remaining_percent(bucket)
        if rem is None:
            continue
        resets_at = parse_resets_at(bucket)
        secs = bucket.get("reset_in_seconds")
        is_gemini = "gemini" in low or "neon" in low or "carbon" in low or "argon" in low
        candidates.append((is_gemini, low, rem, resets_at, secs))

    gemini_only = [c for c in candidates if c[0]]
    pool = gemini_only or candidates

    weekly = None
    five_hour = None
    for _, low, rem, resets_at, secs in pool:
        if "week" in low or "7d" in low:
            weekly = (rem, resets_at)
        elif "5h" in low or "five" in low or "hour" in low or "short" in low or "rolling" in low:
            five_hour = (rem, resets_at)
        elif isinstance(secs, (int, float)):
            if secs > 86_400 and weekly is None:
                weekly = (rem, resets_at)
            elif 0 < secs <= 86_400 and five_hour is None:
                five_hour = (rem, resets_at)
        elif weekly is None:
            weekly = (rem, resets_at)
    return weekly, five_hour


def main():
    raw = sys.stdin.read()
    if not raw.strip():
        return
    try:
        payload = json.loads(raw)
    except Exception:
        return

    quota = payload.get("quota")
    if not isinstance(quota, dict) or not quota:
        return

    weekly, five_hour = pick_gemini_windows(quota)
    if weekly is None and five_hour is None:
        return

    if weekly is not None and five_hour is not None:
        entry = {
            "id": "gemini",
            "label": "Gemini",
            "percent": round(weekly[0], 1),
            "color": "#3186FF",
            "agent": "antigravity",
            "sub": {
                "label": "5h",
                "percent": round(five_hour[0], 1),
                "color": "#749BFF",
            },
        }
        if weekly[1]:
            entry["resets_at"] = weekly[1]
        if five_hour[1]:
            entry["sub"]["resets_at"] = five_hour[1]
    else:
        win = weekly if weekly is not None else five_hour
        entry = {
            "id": "gemini",
            "label": "Gemini",
            "percent": round(win[0], 1),
            "color": "#3186FF",
            "agent": "antigravity",
        }
        if win[1]:
            entry["resets_at"] = win[1]

    try:
        update_limits_file(entry)
    except Exception:
        pass


if __name__ == "__main__":
    main()
