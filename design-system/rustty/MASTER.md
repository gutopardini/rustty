# Visual identity — Rustty

## Idea

Rustty is a performance-focused CLI for LLMs: a native Linux terminal for people who spend the day in a shell. The project combines a Vulkan cell grid, independent tab sessions, and scrollback staged for the Intel NPU. The identity should feel precise, quick, and self-contained, and it should leave the terminal content in front.

The `>` mark is the prompt. The block is the cursor, ready for the next command. Copper points back to the name Rustty and works as a wayfinding color, never as a background for body text. The dark rounded box makes the icon read as a terminal window.

**Voice:** direct, technical, calm, and welcoming. Suggested supporting line: “Your session, without the noise.” Describe the GPU as the path that draws the screen today. Describe the NPU as the plan for scrollback search. A graph runs only after a compiled command buffer is configured, so copy should not claim that the NPU is already executing one.

## Brand

- [Vector icon](../../assets/brand/rustty-icon.svg): the application, launchers, and profiles. Keep the original proportion and transparent margin.
- [512 px PNG icon](../../assets/brand/rustty-icon.png): places that cannot take SVG.
- [Mark without the box](../../assets/brand/rustty-mark.svg): compositions on dark backgrounds, always with enough contrast.
- [Lockup for dark backgrounds](../../assets/brand/rustty-lockup.svg) and [for light backgrounds](../../assets/brand/rustty-lockup-light.svg): icon and name together. Leave the spacing and proportion alone.
- [Wordmark](../../assets/brand/rustty-wordmark.svg): drawn from Red Hat Display Bold and converted to curves so it does not depend on an installed font. In running text the product name stays **Rustty**.

At small sizes, use the icon alone. Do not stretch, rotate, add strokes, or place the mark on a low-contrast background. The icon and the wordmark are the sources for the other files. After editing them, run `scripts/render-brand-assets.sh` to regenerate the mark, the lockups, the PNG, and the RGBA embedded in the window. After changing the Rustty icon, also run `scripts/render-agent-icons.sh` so the tab icon updates. The scripts need Python 3 and ImageMagick with SVG support. The tab atlas also uses Pillow.

## Rustty theme palette

The values live in `src/color.rs`. The other themes stay available and are identified by their own IDs.

| Role | Color | Use |
| --- | --- | --- |
| Background | `#131619` | Workspace and the active tab |
| Text | `#EDEAE4` | Default content and primary text |
| Bar | `#1B1E22` | Tab bar background |
| Surface | `#252A2E` | Inactive tabs and the appearance panel |
| Secondary text | `#B6B8BB` | Inactive titles and captions |
| Copper | `#F0A77D` | Active tab number, selection, and emphasized controls |

Measured WCAG pairs: text on background **15.12:1**; copper on surface **7.26:1**; secondary on surface **7.29:1**. The ANSI palette stays independent, so programs keep the colors they request. Use copper sparingly, and keep text contrast when adding a component.

## Type and interface

The lockup uses Red Hat Display Bold as curves. In the grid and in the application controls, keep the monospace face the person chose. JetBrains Mono is a useful visual reference. It is not a dependency, and it does not replace a saved preference. Grid sizes follow the existing configuration.

The bar is compact and sits at the bottom, leaving room for commands. The active tab connects visually to the terminal background, and its number takes copper. The panel uses a slightly lighter surface. Agent icons share one square box and keep each mark’s proportion. Hierarchy also comes from position, text, and background, not from color alone. Space the native interface in multiples of a cell. Skip glows, textures, and motion that get in the way of reading or slow the terminal down.

Theme choice stays in the appearance panel (`Ctrl+,`) and in `ui.conf`. Rustty is the default when there is no preference yet. An earlier choice such as `noite` or `papel` remains valid.
