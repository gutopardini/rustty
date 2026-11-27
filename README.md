<div align="center">

<img src="assets/brand/rustty-lockup.svg?v=5" alt="Rustty" width="280">

A performance-focused CLI for LLMs. Written in Rust, drawn on the GPU, with the NPU on the roadmap.

<img src="docs/rustty-glass.png?v=5" alt="Rustty in Glass mode" width="820">

</div>

Rustty is a native Linux terminal for the model CLIs that stay open all day: Claude, Codex, Kiro, Antigravity, and Cursor. The cell grid is rendered with Vulkan. Glyphs live in VRAM, the parser stays off the present path, and only the rows that changed are uploaded. Scrollback is already staged in an Intel NPU buffer. The next step is to run search on that processor once a compiled command graph is in place.

## Run

Each tab is its own shell. The starting directory is `cwd` in `~/.config/rustty/ui.conf`.

```bash
rustty
```

The gear in the title bar, or `Ctrl+,`, opens font, size, and theme. The choice is stored in the same file.

<div align="center">

<img src="docs/rustty-appearance.png?v=3" alt="Appearance panel with Glass mode on and the Ink style" width="820">

</div>

The same panel turns on **Glass mode**. The window becomes translucent and the desktop behind it is blurred by the compositor through `ext-background-effect-v1`, which GNOME 51 and Plasma 6.7 offer. The bottom tab bar adopts compact, Chrome-inspired tabs with concave curves connecting the active tab seamlessly to the terminal, subtle dividers, a `+` new-tab button, smooth title overflow fade, and hover tooltips, while the top bar controls become round glass. The shapes are drawn in a shader. Text and explicit colors stay opaque. With Glass mode off, the window is opaque.

With Glass mode on, **Look** in that panel opens the glass controls. `[-]` and `[+]` change the value for the style on screen, and the window follows at once. **Restore default** puts every glass value back. The same values are stored in `ui.conf`. A pair there is the dark theme, then the light theme.

`TERM` is `xterm-256color`. `TERM_PROGRAM` is `rustty-kitty`, so Codex can see that Kitty graphics are available and draw `/pets` in the session.

## The screen

The window is a grid of cells, not a texture of the whole frame. A dirty row is the unit of upload. Box drawing and Nerd Font symbols are rasterized to meet the cell edge, so prompts stay aligned. Images from the Kitty graphics protocol, including Codex pets, are composited on the GPU with the alpha of the PNG.

<div align="center">

<img src="docs/rustty-agents.png?v=3" alt="Tabs for Claude, Codex, Antigravity, and Cursor Agent" width="820">

</div>

While Claude, Codex, Kiro, Antigravity, or Cursor is in the foreground, the tab shows that icon. Otherwise it shows Rustty. The title bar mirrors the active tab's icon and title.

## AI Limits

Next to the settings gear sits a colored three-layer icon (`Ctrl+Shift+L`). Its color reflects the lowest remaining quota at a glance — cyan/lavender when healthy, amber below 45%, coral below 20% — without opening the popover. Clicking it opens a floating overlay directly below the top bar, rendered on the GPU above the grid: frosted glass with an iridescent rim in Glass mode, or a flat elevated card matching the active theme when Glass mode is off.

<div align="center">

<img src="docs/rustty-limits.png?v=5" alt="AI Limits popover in Glass mode showing Claude, Codex, Gemini, and Cursor quotas" width="820">

</div>

Each row shows the agent's icon and a bar tinted with that icon's dominant palette color. Providers with two rolling windows combine them into a single row: the weekly (`7d`) limit on the main bar and the five-hour (`5h`) limit on a thin bar underneath. Click a row to inspect both values, click `↻` (or press `R`) to refresh, or press `Esc` to close. The addon can be turned off in **Appearance** (`limits=off` in `ui.conf`).

Collection stays off the render path and does not poll in the background:

- **Built-in local readers (O(1), zero subprocesses):** reads `7d` and `5h` rate limits from the tail of the latest Codex session (`~/.codex/sessions`), local context usage from Antigravity, Claude Code, and Cursor transcripts, and passively captures Antigravity's `/usage` view when shown in a tab. While closed, Rustty only checks file metadata (`stat`) every 30 seconds and re-reads only when a source changes.
- **Automatic `7d` + `5h` bridges for Claude Code and Antigravity (`agy`):** both CLIs pipe live rate-limit JSON to a `statusLine` command on each turn. Install the bundled scripts to mirror them into `~/.config/rustty/limits.json`:

```bash
install -Dm755 scripts/claude-limits-bridge.py "$HOME/.config/rustty/claude-limits-bridge.py"
install -Dm755 scripts/agy-limits-bridge.py "$HOME/.config/rustty/agy-limits-bridge.py"
```

Then add `statusLine` to `~/.claude/settings.json`:

```json
"statusLine": {
  "type": "command",
  "command": "python3 ~/.config/rustty/claude-limits-bridge.py"
}
```

And to `~/.gemini/antigravity-cli/settings.json` (`stack_with_default: true` keeps the default bar intact):

```json
"statusLine": {
  "type": "command",
  "command": "python3 ~/.config/rustty/agy-limits-bridge.py",
  "enabled": true,
  "padding": 0,
  "stack_with_default": true
}
```

- **Custom entries (`~/.config/rustty/limits.json`):** any script or hook can write entries (`id`, `label`, `percent` or `used_percent`, `color`, `agent`, `resets_at`, and optional `sub` for a nested `5h` bar). An optional `limits_cmd=...` key in `ui.conf` runs on demand when refreshing (with a 30-second cooldown and 4-second timeout).

## The NPU

`rustty-probe` reports the Vulkan device and, when present, the Intel NPU. Lines that leave the screen are hashed and written into a buffer mapped on that processor. The firmware does not run a graph until a compiled `RTY1` command buffer is submitted. That graph is the planned path for scrollback search. Until then, the index stays on the CPU and the screen stays on the GPU.

`rustty-bench` measures the parser.

## Keys

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+T` | open a tab |
| `Ctrl+Shift+W` or `Ctrl+Shift+Q` | close the tab |
| `Ctrl+Shift+←` / `→` | previous / next tab |
| `Ctrl+PageUp` / `PageDown` | previous / next tab |
| `Ctrl+Shift+1` … `9` | jump to a tab |
| `Shift+PageUp` / `PageDown` | scroll the scrollback |
| drag | select text; the selection is copied when the button is released |
| drag a tab | move that tab along the bar |
| double-click / triple-click | select a word / the line |
| `Ctrl+Shift+C` | copy the selection |
| `Ctrl+Shift+V` | paste |
| middle-click | paste the primary selection |
| `Ctrl+,` | appearance |
| `Ctrl+Shift+L` | AI limits |

The close button on the title bar asks before it closes the window when more than one tab is open. One tab closes immediately.

<div align="center">

<img src="docs/rustty-close.png?v=3" alt="Confirmation before closing three open tabs" width="820">

</div>

Anything else goes to the shell.

A click on an `http://` or `https://` address opens it in the browser. A click on a file path opens that file in the default application for its type. A trailing `:line` or `:line:column` is ignored, and a relative path is resolved from the shell's directory. An application that is already open receives the target. A new window appears only when none is open.

## Roadmap

Planned features and open areas for feedback and contributions:

- **Task completion & attention alerts:** detect when a background tab finishes a task or needs user input (via terminal notifications / BEL / agent signals), trigger native GNOME and KDE desktop notifications, show an unread badge, and highlight or tint the tab in the bottom bar.
- **File drag-and-drop:** drop files and folders onto the terminal window to insert shell-escaped paths directly into the active session.
- **Chrome-style tab & session restore:** reopen recently closed tabs and automatically restore open windows, tabs, custom labels, and working directories after an abrupt close or crash.
- **Right-click "Open with…" on paths and files:** context menu on clicked file paths and links to pick which installed application should open the target and configure per-type defaults.
- **NPU scrollback search:** run scrollback search on the Intel NPU once a compiled `RTY1` command buffer graph is integrated.

## Install

You need Rust, `pkg-config`, Vulkan, and FreeType.

```bash
git clone https://github.com/gutopardini/rustty && cd rustty
cargo build --release --bin rustty
install -Dm755 target/release/rustty "$HOME/.local/bin/rustty"
install -Dm644 assets/brand/rustty-icon.svg "$HOME/.local/share/icons/hicolor/scalable/apps/rustty.svg"
install -Dm644 packaging/linux/rustty.desktop "$HOME/.local/share/applications/rustty.desktop"
```

`~/.local/bin` needs to be on the `PATH` of the graphical session.

## License

MIT.
