//! Generates the five README screenshots in `docs/` using Rustty's real Vulkan renderer.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::Result;
use rustty::agent::Agent;
use rustty::color::{self, Rgb, Theme};
use rustty::confirm;
use rustty::desktop::ButtonLayout;
use rustty::font::{Atlas, Face};
use rustty::gpu::{
    GlassShape, Renderer, GLASS_BAR, GLASS_BUTTON, GLASS_DIVIDER, GLASS_DOT, GLASS_LIMITS_CARD,
    GLASS_LIMITS_ICON, GLASS_SHEET, GLASS_TAB_HOVER, GLASS_TAB_STRIP, ICON_CLOSE,
    ICON_LIMITS_CRIT, ICON_LIMITS_OK, ICON_LIMITS_WARN, ICON_PLUS,
};
use rustty::limits::{self, Entry, Health, Popover, Snapshot as LimitsSnapshot, SubLimit};
use rustty::settings::{self, GlassTune};
use rustty::tabs;
use rustty::term::Term;
use rustty::titlebar::{self, TitleBar};

const WIDTH: u32 = 1100;
const HEIGHT: u32 = 711;
const FONT_SIZE: f32 = 21.0;

fn main() -> Result<()> {
    let theme = color::theme(color::theme_index("ink"));
    let tune = GlassTune::default();
    let face = load_face()?;
    let mut atlas = Atlas::new(face.cell_w(), face.cell_h());
    atlas.warm(&face);

    let mut gpu = Renderer::new_offscreen(WIDTH, HEIGHT)?;
    gpu.set_clear(theme.bg);
    gpu.set_glass(Some(tune.opacity));
    gpu.set_corner_radius(15.0);

    let chrome_h = (tune.bar.ceil() as u32).max(face.cell_h().saturating_add(tune.bar_pad.round() as u32));
    let cell_w = face.cell_w().max(1);
    let cell_h = face.cell_h().max(1);
    let pad = 12u32;
    let body_h = HEIGHT.saturating_sub(chrome_h * 2);
    let cols = ((WIDTH - 2 * pad) / cell_w).max(2) as u16;
    let rows = ((body_h - pad) / cell_h).max(1) as u16;
    let strip_cols = (WIDTH / cell_w).max(1) as u16;

    gpu.set_insets(pad as f32, chrome_h as f32, pad as f32, chrome_h as f32);

    let backdrop = make_blurred_backdrop(WIDTH, HEIGHT);
    let layout = ButtonLayout::parse("appmenu:close");
    let track_l = (tune.margin / cell_w as f32).round() as u16;
    let track_r = ((WIDTH as f32 - tune.margin) / cell_w as f32).round() as u16;

    let sample_limits = LimitsSnapshot {
        title: "AI Limits".into(),
        subtitle: "Quota · Models".into(),
        entries: vec![
            Entry {
                id: "claude".into(),
                label: "Claude".into(),
                percent: 84,
                color: Rgb::new(0xD9, 0x77, 0x57),
                hint: Some("4d".into()),
                sub: Some(SubLimit {
                    label: "5h".into(),
                    percent: 81,
                    color: Rgb::new(0xF2, 0xA4, 0x7E),
                    hint: Some("3h".into()),
                }),
                agent: Some(Agent::Claude),
            },
            Entry {
                id: "codex".into(),
                label: "Codex".into(),
                percent: 39,
                color: Rgb::new(0x7A, 0x9D, 0xFF),
                hint: Some("3d".into()),
                sub: Some(SubLimit {
                    label: "5h".into(),
                    percent: 100,
                    color: Rgb::new(0xB1, 0xA7, 0xFF),
                    hint: Some("reset".into()),
                }),
                agent: Some(Agent::Codex),
            },
            Entry {
                id: "gemini".into(),
                label: "Gemini".into(),
                percent: 75,
                color: Rgb::new(0x31, 0x86, 0xFF),
                hint: Some("6d".into()),
                sub: Some(SubLimit {
                    label: "5h".into(),
                    percent: 31,
                    color: Rgb::new(0x74, 0x9B, 0xFF),
                    hint: Some("2h33m".into()),
                }),
                agent: Some(Agent::Antigravity),
            },
            Entry {
                id: "cursor".into(),
                label: "Cursor".into(),
                percent: 98,
                color: Rgb::new(0x9E, 0xA6, 0xB2),
                hint: Some("ctx".into()),
                sub: None,
                agent: Some(Agent::Cursor),
            },
        ],
        updated_at: None,
    };

    let shell_prompt = "\x1b[48;2;31;36;46m\x1b[38;2;96;165;250m \u{f30a} \x1b[38;2;250;204;21m18:46:18 \x1b[38;2;34;197;94m\u{f07b} Git \x1b[0m\x1b[38;2;31;36;46m\u{e0b0}\x1b[0m\r\n\x1b[38;2;244;63;94m➜\x1b[0m ";
    let cursor_screen = format!(
        "{shell_prompt}agent\r\n\r\n\
         \x1b[1mCursor Agent\x1b[0m\r\n\
         \x1b[38;2;148;163;184mv2026.11.25-f532c45\r\n\
         Tip: Use /plan to iterate on an implementation plan before code\r\n\
         changes.\x1b[0m\r\n\r\n\
         \x1b[48;2;30;35;43m                                                               \x1b[0m\r\n\
         \x1b[48;2;30;35;43m \x1b[38;2;148;163;184m→ \x1b[7mP\x1b[27mlan, search, build anything                                \x1b[0m\r\n\
         \x1b[48;2;30;35;43m                                                               \x1b[0m\r\n\r\n\
         \x1b[38;2;148;163;184mGrok 4.7 256K High Fast                       \x1b[38;2;192;132;252mRun Everything\x1b[0m\r\n\
         \x1b[38;2;148;163;184m~/Git\x1b[0m\x1b[?25l"
    );

    let shell_tabs = vec!["Git".to_string(), "Git".to_string(), "Git".to_string()];
    let shell_agents = vec![None, None, None];
    let three_agent_tabs = vec![
        "Claude Code".to_string(),
        "Codex".to_string(),
        "Cursor Agent".to_string(),
    ];
    let three_agent_icons = vec![
        Some(Agent::Claude),
        Some(Agent::Codex),
        Some(Agent::Cursor),
    ];
    let agent_tabs = vec![
        "Claude Code".to_string(),
        "Codex".to_string(),
        "Antigravity".to_string(),
        "Cursor Agent".to_string(),
    ];
    let agent_icons = vec![
        Some(Agent::Claude),
        Some(Agent::Codex),
        Some(Agent::Antigravity),
        Some(Agent::Cursor),
    ];
    let limits_tabs = vec![
        "Git".to_string(),
        "Claude Code".to_string(),
        "Codex".to_string(),
        "Antigravity".to_string(),
    ];
    let limits_icons = vec![
        None,
        Some(Agent::Claude),
        Some(Agent::Codex),
        Some(Agent::Antigravity),
    ];

    // 1. docs/rustty-glass.png
    render_shot(
        "docs/rustty-glass.png",
        &mut gpu,
        &mut atlas,
        &face,
        &backdrop,
        theme,
        &tune,
        &layout,
        chrome_h,
        pad,
        cols,
        rows,
        strip_cols,
        (track_l, track_r),
        &shell_tabs,
        &shell_agents,
        2,
        shell_prompt,
        Health::Ok,
        Overlay::None,
    )?;

    // 2. docs/rustty-appearance.png
    render_shot(
        "docs/rustty-appearance.png",
        &mut gpu,
        &mut atlas,
        &face,
        &backdrop,
        theme,
        &tune,
        &layout,
        chrome_h,
        pad,
        cols,
        rows,
        strip_cols,
        (track_l, track_r),
        &three_agent_tabs,
        &three_agent_icons,
        2,
        &cursor_screen,
        Health::Ok,
        Overlay::Settings,
    )?;

    // 3. docs/rustty-agents.png
    render_shot(
        "docs/rustty-agents.png",
        &mut gpu,
        &mut atlas,
        &face,
        &backdrop,
        theme,
        &tune,
        &layout,
        chrome_h,
        pad,
        cols,
        rows,
        strip_cols,
        (track_l, track_r),
        &agent_tabs,
        &agent_icons,
        3,
        &cursor_screen,
        Health::Ok,
        Overlay::None,
    )?;

    // 4. docs/rustty-limits.png
    render_shot(
        "docs/rustty-limits.png",
        &mut gpu,
        &mut atlas,
        &face,
        &backdrop,
        theme,
        &tune,
        &layout,
        chrome_h,
        pad,
        cols,
        rows,
        strip_cols,
        (track_l, track_r),
        &limits_tabs,
        &limits_icons,
        0,
        shell_prompt,
        sample_limits.health(),
        Overlay::Limits(&sample_limits),
    )?;

    // 5. docs/rustty-close.png
    render_shot(
        "docs/rustty-close.png",
        &mut gpu,
        &mut atlas,
        &face,
        &backdrop,
        theme,
        &tune,
        &layout,
        chrome_h,
        pad,
        cols,
        rows,
        strip_cols,
        (track_l, track_r),
        &three_agent_tabs,
        &three_agent_icons,
        2,
        &cursor_screen,
        Health::Ok,
        Overlay::ConfirmClose(3),
    )?;

    println!("Generated all 5 screenshots in docs/");
    Ok(())
}

enum Overlay<'a> {
    None,
    Settings,
    Limits(&'a LimitsSnapshot),
    ConfirmClose(usize),
}

#[allow(clippy::too_many_arguments)]
fn render_shot(
    out_path: &str,
    gpu: &mut Renderer,
    atlas: &mut Atlas,
    face: &Face,
    backdrop: &[u8],
    theme: Theme,
    tune: &GlassTune,
    layout: &ButtonLayout,
    chrome_h: u32,
    pad: u32,
    cols: u16,
    rows: u16,
    strip_cols: u16,
    _track: (u16, u16),
    titles: &[String],
    agents: &[Option<Agent>],
    active: usize,
    screen_ansi: &str,
    health: Health,
    overlay_mode: Overlay<'_>,
) -> Result<()> {
    let mut term = Term::new(cols, rows);
    term.restyle(theme.fg, theme.bg);
    term.advance(screen_ansi.as_bytes());
    let snap = term.snapshot();
    gpu.upload(atlas, face, &snap, false, theme.accent, 0..0);

    let bar = tabs::render_bar(titles, agents, active, strip_cols, theme, true);
    gpu.upload_bar(atlas, face, &bar.cells, theme.bar, theme.bar, 4.0);

    let title = titlebar::render(
        &titles[active],
        agents[active],
        strip_cols,
        theme,
        false,
        layout,
        true,
        true,
        true,
    );
    gpu.upload_title(atlas, face, &title.cells, theme.bar);

    let cell = (face.cell_w().max(1) as f32, face.cell_h().max(1) as f32);
    let origin = (pad as f32, (chrome_h + pad) as f32);

    let mut shapes = build_bar_shapes(
        &bar,
        &title,
        chrome_h,
        face.cell_w().max(1),
        theme,
        matches!(overlay_mode, Overlay::Settings),
        matches!(overlay_mode, Overlay::Limits(_)),
        health,
        tune,
    );

    gpu.upload_dialog(atlas, face, None);

    match overlay_mode {
        Overlay::None => {
            let overlay = shapes.len();
            gpu.set_glass_shapes(&shapes, overlay, 1.0, false, tune);
        }
        Overlay::Settings => {
            let mut panel = settings::render_panel(
                cols,
                rows,
                theme,
                "JetBrainsMono Nerd Font",
                23.0,
                color::theme_index("ink"),
                true,
                true,
                None,
                None,
            )
            .expect("settings panel");
            for cell in &mut panel.cells {
                cell.bg = theme.bg.pack();
            }
            gpu.stamp(atlas, face, panel.origin_col, panel.origin_row, panel.cols, &panel.cells);
            shapes.push(sheet(
                (panel.origin_col, panel.origin_row, panel.cols, panel.rows),
                origin,
                cell,
                0.0,
                theme,
                tune.sheet,
                tune.radius,
            ));
            let overlay = shapes.len();
            gpu.set_glass_shapes(&shapes, overlay, 1.0, false, tune);
        }
        Overlay::Limits(limits_snap) => {
            let anchor = Some(title.settings.end.max(title.limits.end));
            let popover = limits::render_popover(
                cols,
                rows,
                theme,
                limits_snap,
                agents[active],
                None,
                false,
                anchor,
            )
            .expect("limits popover");
            gpu.upload_popover(atlas, face, Some(&popover));
            let overlay = shapes.len();
            push_limits_shapes(&mut shapes, &popover, origin, cell, theme, tune);
            gpu.set_glass_shapes(&shapes, overlay, 1.0, false, tune);
        }
        Overlay::ConfirmClose(count) => {
            let mut dialog = confirm::render(cols, rows, theme, count).expect("confirm dialog");
            dialog.set_background(theme.bg.pack());
            gpu.upload_dialog(atlas, face, Some(&dialog));
            let overlay = shapes.len();
            shapes.push(sheet(
                (dialog.origin_col, dialog.origin_row, dialog.cols, dialog.rows),
                origin,
                cell,
                tune.dialog_pad,
                theme,
                tune.dialog,
                tune.radius,
            ));
            let inset = tune.button_inset.round();
            for (c_range, r_range, choice) in dialog.buttons() {
                let x = origin.0 + f32::from(dialog.origin_col + c_range.start) * cell.0 + inset;
                let y = origin.1 + f32::from(dialog.origin_row + r_range.start) * cell.1 + inset;
                let w = f32::from(c_range.end - c_range.start) * cell.0 - 2.0 * inset;
                let h = f32::from(r_range.end - r_range.start) * cell.1 - 2.0 * inset;
                let btn = GlassShape::new(x, y, w, h, GLASS_BUTTON);
                shapes.push(match choice {
                    confirm::Choice::Close => btn.with_tint(theme.danger, tune.danger),
                    confirm::Choice::Cancel => btn,
                });
            }
            gpu.set_glass_shapes(&shapes, overlay, 1.0, false, tune);
        }
    }

    let fg = gpu.render_to_rgba(matches!(overlay_mode, Overlay::None))?;
    let composed = composite_over(backdrop, &fg);
    write_png(Path::new(out_path), WIDTH, HEIGHT, &composed)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_bar_shapes(
    bar: &tabs::Bar,
    title: &TitleBar,
    chrome_h: u32,
    cell_w: u32,
    theme: Theme,
    settings_lit: bool,
    limits_lit: bool,
    health: Health,
    tune: &GlassTune,
) -> Vec<GlassShape> {
    let cw = cell_w as f32;
    let chrome = chrome_h as f32;
    let inset = tune.inset.round();
    let cap_h = (chrome - 2.0 * inset).max(1.0);
    let bottom_pad = 4.0;
    let bar_y = HEIGHT as f32 - chrome;
    let tab_h = (chrome - bottom_pad).max(1.0);
    let mut shapes = Vec::new();
    if !bar.spans.is_empty() {
        let (active_x0, active_x1) = bar
            .spans
            .get(bar.selected)
            .map(|span| (f32::from(span.start) * cw, f32::from(span.end) * cw))
            .unwrap_or((0.0, 0.0));
        let radius = 9.0_f32.min(tab_h * 0.36).min(active_x0.max(4.0));
        shapes.push(
            GlassShape::new(0.0, bar_y, WIDTH as f32, chrome, GLASS_TAB_STRIP)
                .with_radius(radius)
                .with_icon(active_x0)
                .with_lit(active_x1)
                .with_tint(Rgb::new(0x00, 0x00, 0x00), 0.34),
        );
        let div_h = (tab_h * 0.48).max(8.0);
        let div_y = bar_y + (tab_h - div_h) * 0.5;
        let count = bar.spans.len();
        for index in 1..count {
            if index != bar.selected && index - 1 != bar.selected {
                let div_x = (f32::from(bar.spans[index].start) * cw - 0.5).round();
                shapes.push(GlassShape::new(div_x, div_y, 1.0, div_h, GLASS_DIVIDER));
            }
        }
        if let Some(last) = bar.spans.last()
            && bar.new_tab.is_some()
            && count - 1 != bar.selected
        {
            let div_x = (f32::from(last.end) * cw - 0.5).round();
            shapes.push(GlassShape::new(div_x, div_y, 1.0, div_h, GLASS_DIVIDER));
        }
        if let Some(range) = &bar.new_tab {
            let cx = (f32::from(range.start) + f32::from(range.end)) * 0.5 * cw;
            let cy = bar_y + tab_h * 0.5;
            let side = (tab_h * 0.72).clamp(18.0, 28.0);
            shapes.push(
                GlassShape::new(cx - side * 0.5, cy - side * 0.5, side, side, GLASS_TAB_HOVER)
                    .with_icon(ICON_PLUS)
                    .with_tint(theme.tab_fg, 0.82),
            );
        }
    }
    let lim_icon = health_icon(health);
    let buttons = [
        (&title.limits, Some(lim_icon)),
        (&title.settings, None),
        (&title.close, Some(ICON_CLOSE)),
    ];
    for (idx, (range, icon)) in buttons.into_iter().enumerate() {
        if range.is_empty() {
            continue;
        }
        let mark = f32::from(range.start + (range.end - range.start) / 2);
        let center = (mark + 0.5) * cw;
        let side = (cap_h - tune.button_inset).min(f32::from(range.end - range.start) * cw).max(1.0);
        let btn = GlassShape::new(center - side * 0.5, (chrome - side) * 0.5, side, side, GLASS_BUTTON);
        shapes.push(match (idx, icon) {
            (0, Some(icon)) => {
                let b = btn.lit(limits_lit).with_icon(icon);
                match health {
                    Health::Crit => b.with_tint(Rgb::new(0xF8, 0x5A, 0x62), 0.16),
                    Health::Warn => b.with_tint(Rgb::new(0xFB, 0x92, 0x3C), 0.12),
                    Health::Ok => b,
                }
            }
            (_, Some(icon)) => btn.with_icon(icon),
            (_, None) => btn.lit(settings_lit).with_gear(),
        });
    }
    shapes
}

fn push_limits_shapes(
    shapes: &mut Vec<GlassShape>,
    popover: &Popover,
    origin: (f32, f32),
    cell: (f32, f32),
    _theme: Theme,
    tune: &GlassTune,
) {
    let pad = tune.dialog_pad;
    let x = origin.0 + f32::from(popover.origin_col) * cell.0 - pad;
    let y = origin.1 + f32::from(popover.origin_row) * cell.1 - pad * 0.5;
    let w = f32::from(popover.cols) * cell.0 + 2.0 * pad;
    let h = f32::from(popover.rows) * cell.1 + pad;
    shapes.push(
        GlassShape::new(x, y, w, h, GLASS_LIMITS_CARD)
            .with_radius(20.0)
            .with_tint(Rgb::new(0x10, 0x13, 0x1B), tune.dialog.max(0.86))
            .lit(true),
    );
    let (ic, ir, iw, ih) = popover.hero_icon;
    let ix = origin.0 + f32::from(popover.origin_col + ic) * cell.0;
    let iy = origin.1 + f32::from(popover.origin_row + ir) * cell.1;
    let icon_w = f32::from(iw) * cell.0;
    let icon_h = f32::from(ih) * cell.1;
    let side = icon_w.min(icon_h).max(16.0);
    shapes.push(
        GlassShape::new(ix + (icon_w - side) * 0.5, iy + (icon_h - side) * 0.5, side, side, GLASS_LIMITS_ICON)
            .with_icon(health_icon(popover.health)),
    );
    if let Some((col, r0, r1)) = popover.divider_v {
        let dx = origin.0 + (f32::from(popover.origin_col + col) + 0.5) * cell.0 - 0.5;
        let dy = origin.1 + f32::from(popover.origin_row + r0) * cell.1;
        let dh = f32::from(r1.saturating_sub(r0)) * cell.1;
        shapes.push(GlassShape::new(dx.round(), dy, 1.0, dh, GLASS_DIVIDER));
    }
    let dot_d = (cell.1 * 0.36).clamp(6.0, 10.0);
    let bar_h = (cell.1 * 0.38).clamp(6.0, 10.0);
    for meter in &popover.meters {
        let row_cy = origin.1 + (f32::from(popover.origin_row + meter.row) + 0.5) * cell.1;
        if let Some(dot_col) = meter.dot_col {
            let dot_cx = origin.0 + (f32::from(popover.origin_col + dot_col) + 0.5) * cell.0;
            shapes.push(
                GlassShape::new(dot_cx - dot_d * 0.5, row_cy - dot_d * 0.5, dot_d, dot_d, GLASS_DOT)
                    .with_tint(meter.color, 1.0)
                    .with_icon(1.0),
            );
        }
        let bx = origin.0 + f32::from(popover.origin_col + meter.bar_col) * cell.0;
        let bw = f32::from(meter.bar_cols) * cell.0;
        if let Some((sub_fill, sub_color)) = meter.sub {
            let main_h = (cell.1 * 0.28).clamp(5.0, 8.0);
            let sub_h = (cell.1 * 0.14).clamp(2.5, 4.0);
            let gap = 2.0;
            let top_y = row_cy - (main_h + gap + sub_h) * 0.5;
            shapes.push(
                GlassShape::new(bx, top_y, bw, main_h, GLASS_BAR)
                    .with_amount(meter.fill)
                    .with_tint(meter.color, 1.0)
                    .with_icon(1.0),
            );
            shapes.push(
                GlassShape::new(bx, top_y + main_h + gap, bw, sub_h, GLASS_BAR)
                    .with_amount(sub_fill)
                    .with_tint(sub_color, 1.0)
                    .with_icon(1.0),
            );
        } else {
            shapes.push(
                GlassShape::new(bx, row_cy - bar_h * 0.5, bw, bar_h, GLASS_BAR)
                    .with_amount(meter.fill)
                    .with_tint(meter.color, 1.0)
                    .with_icon(1.0),
            );
        }
    }
}

fn sheet(
    rect: (u16, u16, u16, u16),
    origin: (f32, f32),
    cell: (f32, f32),
    pad: f32,
    theme: Theme,
    alpha: f32,
    radius: f32,
) -> GlassShape {
    let x = origin.0 + f32::from(rect.0) * cell.0 - pad;
    let y = origin.1 + f32::from(rect.1) * cell.1 - pad;
    let w = f32::from(rect.2) * cell.0 + 2.0 * pad;
    let h = f32::from(rect.3) * cell.1 + 2.0 * pad;
    GlassShape::new(x, y, w, h, GLASS_SHEET)
        .with_radius(radius)
        .with_tint(theme.panel, alpha)
}

fn health_icon(health: Health) -> f32 {
    match health {
        Health::Ok => ICON_LIMITS_OK,
        Health::Warn => ICON_LIMITS_WARN,
        Health::Crit => ICON_LIMITS_CRIT,
    }
}

fn load_face() -> Result<Face> {
    if let Ok(home) = std::env::var("HOME") {
        let nerd = Path::new(&home)
            .join(".local/share/fonts/JetBrainsMonoNerdFont/JetBrainsMonoNerdFont-Medium.ttf");
        if nerd.is_file() {
            return Face::load_file(&nerd, FONT_SIZE);
        }
    }
    Face::load(FONT_SIZE)
}

/// Soft-blurred dark desktop backdrop that mimics GNOME's background blur behind Glass mode.
fn make_blurred_backdrop(w: u32, h: u32) -> Vec<u8> {
    let mut rgb = vec![0u8; (w * h * 3) as usize];
    for y in 0..h {
        let ny = y as f32 / h as f32;
        for x in 0..w {
            let nx = x as f32 / w as f32;
            let mut r = 13.0 + 10.0 * (1.0 - ny);
            let mut g = 17.0 + 12.0 * (1.0 - ny);
            let mut b = 24.0 + 18.0 * (1.0 - ny);
            // Soft ambient desktop orbs (cyan-blue top-left, warm rose mid-right, emerald bottom-left).
            let d1 = ((nx - 0.18).powi(2) * 1.4 + (ny - 0.22).powi(2) * 1.8).sqrt();
            let g1 = (-d1 * d1 * 4.2).exp();
            r += g1 * 18.0;
            g += g1 * 56.0;
            b += g1 * 88.0;

            let d2 = ((nx - 0.78).powi(2) * 1.3 + (ny - 0.38).powi(2) * 1.7).sqrt();
            let g2 = (-d2 * d2 * 3.8).exp();
            r += g2 * 64.0;
            g += g2 * 28.0;
            b += g2 * 68.0;

            let d3 = ((nx - 0.42).powi(2) * 1.2 + (ny - 0.72).powi(2) * 2.0).sqrt();
            let g3 = (-d3 * d3 * 4.5).exp();
            r += g3 * 16.0;
            g += g3 * 48.0;
            b += g3 * 44.0;

            let idx = ((y * w + x) * 3) as usize;
            rgb[idx] = r.clamp(0.0, 255.0) as u8;
            rgb[idx + 1] = g.clamp(0.0, 255.0) as u8;
            rgb[idx + 2] = b.clamp(0.0, 255.0) as u8;
        }
    }
    rgb
}

/// Composites premultiplied RGBA foreground over an opaque RGB backdrop.
fn composite_over(bg: &[u8], fg: &[u8]) -> Vec<u8> {
    let pixels = fg.len() / 4;
    let mut out = vec![0u8; pixels * 3];
    for i in 0..pixels {
        let fr = fg[i * 4] as f32;
        let fg_c = fg[i * 4 + 1] as f32;
        let fb = fg[i * 4 + 2] as f32;
        let fa = fg[i * 4 + 3] as f32 / 255.0;
        let inv = 1.0 - fa;
        out[i * 3] = (fr + bg[i * 3] as f32 * inv).clamp(0.0, 255.0) as u8;
        out[i * 3 + 1] = (fg_c + bg[i * 3 + 1] as f32 * inv).clamp(0.0, 255.0) as u8;
        out[i * 3 + 2] = (fb + bg[i * 3 + 2] as f32 * inv).clamp(0.0, 255.0) as u8;
    }
    out
}

fn write_png(path: &Path, width: u32, height: u32, rgb: &[u8]) -> Result<()> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgb)?;
    Ok(())
}
