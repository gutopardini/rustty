# Tab icons

The files in `source/` exist only to identify the CLI running in a tab. The marks belong to their owners.

| Agent | Source |
| --- | --- |
| Claude | Transparent Claude starburst mark from [@lobehub/icons-static-svg](https://unpkg.com/@lobehub/icons-static-svg@latest/icons/claude-color.svg) |
| Codex | Transparent Codex cloud mark adapted from [@lobehub/icons-static-svg](https://unpkg.com/@lobehub/icons-static-svg@latest/icons/codex-color.svg) (without the white squircle) |
| Kiro | Transparent Kiro ghost mark adapted from the [Kiro site icon](https://kiro.dev/icon.svg) (without the purple square) |
| Antigravity | Transparent Google Antigravity mark adapted from the [Google Antigravity site icon](https://antigravity.google/favicon.svg) (without the white circle) |
| Cursor | Transparent 3D Cursor cube mark |

The atlas order is Claude, Codex, Kiro, Antigravity, Cursor, the settings gear, and the Rustty icon. Each mark has a transparent background, keeps its proportion inside a 64 × 64 premultiplied RGBA tile with smooth antialiased edges, and is sampled linearly on the GPU. Rebuild it with `scripts/render-agent-icons.sh`. That needs Python 3, Pillow, and ImageMagick with SVG support. The application embeds the atlas and does not need those programs at runtime.
