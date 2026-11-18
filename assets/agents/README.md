# Tab icons

The files in `source/` exist only to identify the CLI running in a tab. The marks belong to their owners.

| Agent | Source |
| --- | --- |
| Claude | [Claude application icon](https://claude.ai/images/claude_app_icon.png) |
| Codex | Icon shipped in the official OpenAI `cloud-environment` plugin, file `assets/codex.svg` |
| Kiro | [Kiro site icon](https://kiro.dev/icon.svg) |
| Antigravity | [Google Antigravity site icon](https://antigravity.google/favicon.svg) |
| Cursor | [Cursor site icon](https://cursor.com/marketing-static/favicon.svg) |

The atlas order is Claude, Codex, Kiro, Antigravity, Cursor, the settings gear, and the Rustty icon. Each image sits in a transparent 64 × 64 box without distorting its original proportion. Rebuild it with `scripts/render-agent-icons.sh`. That needs Python 3, Pillow, and ImageMagick with SVG support. The application embeds the atlas and does not need those programs at runtime.
