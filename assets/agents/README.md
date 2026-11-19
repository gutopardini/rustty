# Tab icons

The files in `source/` exist only to identify the CLI running in a tab. The marks belong to their owners.

| Agent | Source |
| --- | --- |
| Claude | [Claude application icon](https://claude.ai/images/claude_app_icon.png) |
| Codex | Default blue Codex app icon, resized from the [macOS app asset](https://github.com/backnotprop/orchestrator/blob/583acf4b469b91131f96ae2136797749c788b4c7/assets/providers/codex-icon-light.png) |
| Kiro | [Kiro site icon](https://kiro.dev/icon.svg) |
| Antigravity | [Google Antigravity site icon](https://antigravity.google/favicon.svg) |
| Cursor | [Cursor site icon](https://cursor.com/marketing-static/favicon.svg) |

The atlas order is Claude, Codex, Kiro, Antigravity, Cursor, the settings gear, and the Rustty icon. Each mark keeps its proportion and fills the 64 × 64 tile; the soft light rim on the source files is cropped off so the edge sits flush on a dark tab. Rebuild it with `scripts/render-agent-icons.sh`. That needs Python 3, Pillow, and ImageMagick with SVG support. The application embeds the atlas and does not need those programs at runtime.
