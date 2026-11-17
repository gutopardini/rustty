use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::{Icon, ResizeDirection, Window, WindowId};

use crate::agent::{self, Agent};
use crate::color::{self, Theme};
use crate::confirm::{self, Choice, Dialog};
use crate::font::{Atlas, Face};
use crate::gpu::Renderer;
use crate::input::{self, Input};
use crate::npu::{Indexer, Scheduler};
use crate::pty::Pty;
use crate::settings::{self, Panel, PanelHit, Ui};
use crate::tabs::{self, Bar, BarHit};
use crate::term::Term;
use crate::titlebar::{self, TitleBar, TitleHit};

#[derive(Debug)]
enum Wake {
    Frame(u64),
    Closed(u64),
}

struct Session {
    pty: Arc<Pty>,
    term: Arc<Mutex<Term>>,
    reader: Option<JoinHandle<()>>,
    frame_pending: Arc<AtomicBool>,
}

impl Session {
    fn spawn(
        id: u64,
        cols: u16,
        rows: u16,
        indexer: Indexer,
        proxy: EventLoopProxy<Wake>,
        cwd: Option<&std::path::Path>,
    ) -> std::io::Result<Self> {
        let pty = Arc::new(Pty::spawn(cols, rows, cwd)?);
        let term = Arc::new(Mutex::new(Term::new(cols, rows)));
        let reader_fd = pty.reader_fd()?;
        let term_reader = Arc::clone(&term);
        let pty_writer = Arc::clone(&pty);
        let frame_pending = Arc::new(AtomicBool::new(false));
        let pending = Arc::clone(&frame_pending);
        let reader = std::thread::Builder::new()
            .name("rustty-pty".into())
            .spawn(move || pty_loop(id, reader_fd, term_reader, pty_writer, indexer, proxy, pending))
            .map_err(std::io::Error::other)?;
        Ok(Self {
            pty,
            term,
            reader: Some(reader),
            frame_pending,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.pty.hangup();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn pty_loop(
    id: u64,
    fd: std::os::fd::OwnedFd,
    term: Arc<Mutex<Term>>,
    pty: Arc<Pty>,
    indexer: Indexer,
    proxy: EventLoopProxy<Wake>,
    frame_pending: Arc<AtomicBool>,
) {
    let mut buf = [0u8; 64 * 1024];
    loop {
        let mut fds = [PollFd::new(fd.as_fd(), PollFlags::POLLIN | PollFlags::POLLHUP)];
        match nix::poll::poll(&mut fds, nix::poll::PollTimeout::NONE) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(err) => {
                tracing::debug!(%err, id, "poll do pty encerrou");
                let _ = proxy.send_event(Wake::Closed(id));
                break;
            }
        }
        match nix::unistd::read(&fd, &mut buf) {
            Ok(0) => {
                let _ = proxy.send_event(Wake::Closed(id));
                break;
            }
            Ok(n) => {
                let (lines, replies) = {
                    let mut term = term.lock().unwrap_or_else(|err| err.into_inner());
                    term.advance(&buf[..n]);
                    (term.take_indexed_lines(), term.take_replies())
                };
                for line in lines {
                    indexer.index_line(line);
                }
                if !replies.is_empty() {
                    let _ = pty.write_all(&replies);
                }
                if !frame_pending.swap(true, Ordering::AcqRel) {
                    let _ = proxy.send_event(Wake::Frame(id));
                }
            }
            Err(Errno::EAGAIN) | Err(Errno::EINTR) => continue,
            Err(err) => {
                tracing::debug!(%err, id, "leitura do pty encerrou");
                let _ = proxy.send_event(Wake::Closed(id));
                break;
            }
        }
    }
}

struct Tab {
    id: u64,
    title: String,
    agent: Option<Agent>,
    session: Session,
}

struct App {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    npu: Scheduler,
    proxy: EventLoopProxy<Wake>,
    window: Option<Arc<Window>>,
    gpu: Option<Renderer>,
    face: Face,
    atlas: Atlas,
    mods: ModifiersState,
    font_size: f32,
    vsync: bool,
    grid_cols: u16,
    grid_rows: u16,
    bar_dirty: bool,
    bar: Bar,
    title_bar: TitleBar,
    last_title_click: Option<Instant>,
    settings_hot: bool,
    panel_dirty: bool,
    panel: Option<Panel>,
    confirm_close: bool,
    confirm_dirty: bool,
    close_dialog: Option<Dialog>,
    exit_confirmed: bool,
    next_agent_scan: Instant,
    ui: Ui,
    cursor: Option<(f64, f64)>,
    wheel_frac: f32,
    scroll_drag: bool,
}

impl App {
    fn open_window(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let icon = Icon::from_rgba(
            include_bytes!("../assets/brand/rustty-window.rgba").to_vec(),
            64,
            64,
        )
        .context("carregando ícone da janela")?;
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Rustty")
                        .with_name("rustty", "rustty")
                        .with_window_icon(Some(icon))
                        .with_decorations(false)
                        .with_inner_size(LogicalSize::new(1100.0, 700.0)),
                )
                .context("criando janela")?,
        );
        self.rescale(window.scale_factor());
        let mut gpu = Renderer::new(Arc::clone(&window), self.vsync)?;
        gpu.set_clear(color::theme(self.ui.style).bg);
        self.window = Some(window);
        self.gpu = Some(gpu);
        self.fit();
        self.redraw();
        Ok(())
    }

    fn redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn rescale(&mut self, scale: f64) {
        let px = (self.font_size * scale as f32).clamp(8.0, 64.0);
        if (px - self.face.px()).abs() < 0.1 && self.atlas.cell_w() == self.face.cell_w() {
            return;
        }
        self.face.set_px(px);
        self.atlas.sync_cell(self.face.cell_w(), self.face.cell_h());
        self.atlas.warm(&self.face);
        for tab in &self.tabs {
            tab.session
                .term
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .damage_all();
        }
        self.bar_dirty = true;
    }

    fn fit(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        self.fit_size(size);
    }

    fn fit_size(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        if let Some(gpu) = &mut self.gpu {
            gpu.resize(size);
        }
        let cell_w = self.face.cell_w().max(1);
        let cell_h = self.face.cell_h().max(1);
        let cols = (size.width / cell_w).max(2) as u16;
        let chrome = self.chrome_height();
        let rows = (size.height.saturating_sub(chrome.saturating_mul(2)) / cell_h).max(1) as u16;
        self.grid_cols = cols;
        self.grid_rows = rows;
        self.bar_dirty = true;
        for tab in &self.tabs {
            let mut term = tab.session.term.lock().unwrap_or_else(|err| err.into_inner());
            if cols != term.cols() || rows != term.rows() {
                term.resize(cols, rows);
                drop(term);
                let _ = tab.session.pty.resize(cols, rows);
            } else {
                // A superfície pode mudar de tamanho mesmo sem mudar a grade.
                term.damage_all();
            }
        }
    }

    fn open_tab(&mut self) -> std::io::Result<()> {
        let cols = self.grid_cols.max(2);
        let rows = self.grid_rows.max(1);
        let id = self.next_id;
        self.next_id += 1;
        let session = Session::spawn(id, cols, rows, self.npu.indexer(), self.proxy.clone(), self.ui.cwd.as_deref())?;
        let theme = color::theme(self.ui.style);
        session
            .term
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .restyle(theme.fg, theme.bg);
        self.tabs.push(Tab {
            id,
            title: "shell".into(),
            agent: None,
            session,
        });
        self.focus_tab(self.tabs.len() - 1);
        Ok(())
    }

    fn focus_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.tabs[index]
            .session
            .term
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .damage_all();
        self.bar_dirty = true;
        if let Some(window) = &self.window {
            window.set_title(&self.tabs[index].title);
        }
        self.redraw();
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        self.confirm_dirty |= self.confirm_close;
        if self.tabs.is_empty() {
            return;
        }
        if self.active > index {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        let active = self.active;
        self.focus_tab(active);
    }

    fn note_title(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let title = self.tabs[index]
            .session
            .term
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .title()
            .to_string();
        if title.is_empty() || title == self.tabs[index].title {
            return;
        }
        self.tabs[index].title = title;
        self.bar_dirty = true;
        if index == self.active {
            if let Some(window) = &self.window {
                window.set_title(&self.tabs[self.active].title);
            }
        }
    }

    fn refresh_agents(&mut self) {
        let mut changed = false;
        for tab in &mut self.tabs {
            let pty = &tab.session.pty;
            let agent = pty.foreground_group().and_then(|group| agent::foreground(group, pty.shell_pid()));
            if agent != tab.agent {
                tab.agent = agent;
                changed = true;
            }
        }
        if changed {
            self.bar_dirty = true;
            self.redraw();
        }
    }

    fn draw(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let active = self.active.min(self.tabs.len() - 1);
        self.active = active;
        let show_terminal = !self.ui.open && !self.confirm_close;
        let (snap, composer) = {
            let mut term = self.tabs[active]
                .session
                .term
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            let composer = if show_terminal && self.tabs[active].agent == Some(Agent::Codex) {
                term.codex_composer()
            } else {
                None
            };
            (term.snapshot(), composer)
        };
        if snap.title_changed && snap.title != self.tabs[active].title && !snap.title.is_empty() {
            self.tabs[active].title = snap.title.clone();
            self.bar_dirty = true;
            if let Some(window) = &self.window {
                window.set_title(&snap.title);
            }
        }
        let terminal_dirty = !snap.suspended && snap.changed;
        let stamp_panel = self.ui.open && (self.panel_dirty || terminal_dirty);
        let draw_confirm =
            self.confirm_dirty || (self.confirm_close && (self.bar_dirty || terminal_dirty));
        if !terminal_dirty && !self.bar_dirty && !stamp_panel && !draw_confirm {
            return;
        }
        let chrome_h = self.chrome_height() as f32;
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        let theme = color::theme(self.ui.style);
        gpu.set_insets(chrome_h, chrome_h);
        gpu.set_clear(theme.bg);
        if !snap.suspended {
            gpu.upload(&mut self.atlas, &self.face, &snap, composer, theme.accent);
        }
        if self.bar_dirty {
            let titles: Vec<String> = self.tabs.iter().map(|tab| tab.title.clone()).collect();
            let agents: Vec<Option<Agent>> = self.tabs.iter().map(|tab| tab.agent).collect();
            let cols = self.grid_cols.max(1).min(1024);
            self.bar = tabs::render_bar(&titles, &agents, active, cols, theme, self.settings_hot || self.ui.open);
            gpu.upload_bar(&mut self.atlas, &self.face, &self.bar.cells, theme.bar);
            let title = self.tabs.get(active).map(|tab| tab.title.as_str()).unwrap_or("Rustty");
            let maximized = self.window.as_ref().is_some_and(|window| window.is_maximized());
            self.title_bar = titlebar::render(title, cols, theme, maximized);
            gpu.upload_title(&mut self.atlas, &self.face, &self.title_bar.cells);
            self.bar_dirty = false;
        }
        if self.ui.open {
            self.panel = settings::render_panel(
                self.grid_cols,
                self.grid_rows,
                theme,
                self.ui.font_name(),
                self.ui.size,
                self.ui.style,
            );
            if let Some(panel) = &self.panel {
                gpu.stamp(
                    &mut self.atlas,
                    &self.face,
                    panel.origin_col,
                    panel.origin_row,
                    panel.cols,
                    &panel.cells,
                );
            }
            self.panel_dirty = false;
        } else {
            self.panel = None;
        }
        if draw_confirm {
            self.close_dialog = if self.confirm_close {
                confirm::render(self.grid_cols, self.grid_rows, theme, self.tabs.len())
            } else {
                None
            };
            gpu.upload_dialog(&mut self.atlas, &self.face, self.close_dialog.as_ref());
            self.confirm_dirty = false;
        }
        if let Err(err) = gpu.render(show_terminal && !snap.suspended) {
            tracing::warn!(%err, "frame");
        }
    }

    fn theme(&self) -> Theme {
        color::theme(self.ui.style)
    }

    fn toggle_settings(&mut self) {
        self.ui.open = !self.ui.open;
        if !self.ui.open {
            self.close_settings();
            return;
        }
        self.panel_dirty = true;
        self.bar_dirty = true;
        self.redraw();
    }

    fn ask_close(&mut self) {
        self.close_settings();
        self.confirm_close = true;
        self.confirm_dirty = true;
        self.scroll_drag = false;
        self.redraw();
    }

    fn cancel_close(&mut self) {
        self.confirm_close = false;
        self.confirm_dirty = true;
        self.close_dialog = None;
        self.redraw();
    }

    fn close_settings(&mut self) {
        if !self.ui.open && self.panel.is_none() {
            return;
        }
        self.ui.open = false;
        self.panel = None;
        self.panel_dirty = false;
        self.bar_dirty = true;
        if let Some(tab) = self.tabs.get(self.active) {
            tab.session
                .term
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .damage_all();
        }
        self.redraw();
    }

    fn apply_style(&mut self, index: usize) {
        if index == self.ui.style || index >= color::THEMES.len() {
            return;
        }
        self.ui.style = index;
        let theme = self.theme();
        if let Some(gpu) = &mut self.gpu {
            gpu.set_clear(theme.bg);
        }
        for tab in &self.tabs {
            tab.session
                .term
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .restyle(theme.fg, theme.bg);
        }
        self.bar_dirty = true;
        self.panel_dirty = true;
        self.ui.save();
        self.redraw();
    }

    fn apply_size(&mut self, delta: f32) {
        let previous = self.ui.size;
        self.ui.nudge_size(delta);
        if (self.ui.size - previous).abs() < 0.1 {
            return;
        }
        self.font_size = self.ui.size;
        let scale = self.window.as_ref().map(|window| window.scale_factor()).unwrap_or(1.0);
        self.rescale(scale);
        self.fit();
        self.panel_dirty = true;
        self.ui.save();
        self.redraw();
    }

    fn apply_font(&mut self, delta: isize) {
        let previous = self.ui.font;
        self.ui.cycle_font(delta);
        if self.ui.font == previous {
            return;
        }
        let Some(path) = self.ui.font_path().map(|path| path.to_path_buf()) else {
            return;
        };
        let scale = self.window.as_ref().map(|window| window.scale_factor()).unwrap_or(1.0);
        let px = (self.font_size * scale as f32).clamp(8.0, 64.0);
        match Face::load_file(&path, px) {
            Ok(face) => {
                self.face = face;
                self.atlas.discard();
                self.atlas.sync_cell(self.face.cell_w(), self.face.cell_h());
                self.atlas.warm(&self.face);
                for tab in &self.tabs {
                    tab.session
                        .term
                        .lock()
                        .unwrap_or_else(|err| err.into_inner())
                        .damage_all();
                }
                self.fit();
                self.bar_dirty = true;
                self.panel_dirty = true;
                self.ui.save();
                self.redraw();
            }
            Err(err) => {
                self.ui.font = previous;
                tracing::warn!(%err, "fonte");
            }
        }
    }

    fn on_panel(&mut self, hit: PanelHit) {
        match hit {
            PanelHit::Close => self.close_settings(),
            PanelHit::Smaller => self.apply_size(-1.0),
            PanelHit::Larger => self.apply_size(1.0),
            PanelHit::PrevFont => self.apply_font(-1),
            PanelHit::NextFont => self.apply_font(1),
            PanelHit::Style(index) => self.apply_style(index),
        }
    }

    fn on_key(&mut self, event: &winit::event::KeyEvent) {
        if self.tabs.is_empty() || event.state == ElementState::Released {
            return;
        }
        if self.confirm_close {
            match event.logical_key {
                Key::Named(winit::keyboard::NamedKey::Escape) => self.cancel_close(),
                Key::Named(winit::keyboard::NamedKey::Enter) => self.exit_confirmed = true,
                _ => {}
            }
            return;
        }
        if self.ui.open && matches!(event.logical_key, Key::Named(winit::keyboard::NamedKey::Escape)) {
            self.close_settings();
            return;
        }
        let plain = event.key_without_modifiers();
        if self.mods.control_key() && !self.mods.shift_key() && !self.mods.alt_key() {
            let comma = matches!(plain.as_ref(), Key::Character(","))
                || matches!(event.logical_key.as_ref(), Key::Character(","));
            if comma {
                self.toggle_settings();
                return;
            }
        }
        let active = self.active.min(self.tabs.len() - 1);
        let (app_cursor, rows) = {
            let term = self.tabs[active]
                .session
                .term
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            (term.app_cursor(), term.rows())
        };
        let action = input::encode(event, self.mods, app_cursor, rows);
        if event.repeat && !matches!(action, Some(Input::Bytes(_)) | Some(Input::Scroll(_)) | None) {
            return;
        }
        match action {
            Some(Input::Bytes(bytes)) => {
                if let Err(err) = self.tabs[active].session.pty.write_all(&bytes) {
                    tracing::warn!(%err, "escrita no pty");
                }
            }
            Some(Input::Scroll(delta)) => {
                self.tabs[active]
                    .session
                    .term
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .scroll_view(delta);
                self.redraw();
            }
            Some(Input::NewTab) => {
                if let Err(err) = self.open_tab() {
                    tracing::warn!(%err, "não abriu a aba");
                }
            }
            Some(Input::CloseTab) => self.close_tab(active),
            Some(Input::NextTab) => {
                let next = (active + 1) % self.tabs.len();
                self.focus_tab(next);
            }
            Some(Input::PrevTab) => {
                let next = if active == 0 { self.tabs.len() - 1 } else { active - 1 };
                self.focus_tab(next);
            }
            Some(Input::GotoTab(index)) => self.focus_tab(index),
            None => {}
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        if self.tabs.is_empty() {
            return;
        }
        let active = self.active.min(self.tabs.len() - 1);
        let cell_h = self.face.cell_h().max(1) as f32;
        let raw = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 3.0,
            MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / cell_h,
        };
        self.wheel_frac += raw;
        let steps = self.wheel_frac.trunc() as isize;
        if steps == 0 {
            return;
        }
        self.wheel_frac -= steps as f32;
        let (alt, app_cursor) = {
            let term = self.tabs[active]
                .session
                .term
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            (term.alt_screen(), term.app_cursor())
        };
        if alt {
            let up = steps > 0;
            let seq: &[u8] = if app_cursor {
                if up { b"\x1bOA" } else { b"\x1bOB" }
            } else if up {
                b"\x1b[A"
            } else {
                b"\x1b[B"
            };
            let n = steps.unsigned_abs().min(40);
            for _ in 0..n {
                if let Err(err) = self.tabs[active].session.pty.write_all(seq) {
                    tracing::warn!(%err, "escrita no pty");
                    break;
                }
            }
            return;
        }
        self.tabs[active]
            .session
            .term
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .scroll_view(steps);
        self.redraw();
    }

    fn gutter_fraction(&self, x: f64, y: f64) -> Option<f32> {
        self.window.as_ref()?;
        let cell_w = self.face.cell_w().max(1) as f64;
        let cell_h = self.face.cell_h().max(1) as f64;
        let grid_w = f64::from(self.grid_cols.max(1)) * cell_w;
        let grid_right = grid_w;
        if x < grid_right - 8.0 || x > grid_right + 2.0 || y < 0.0 {
            return None;
        }
        let grid_h = f64::from(self.grid_rows.max(1)) * cell_h;
        let local = (y - self.chrome_height() as f64) / grid_h;
        if !(0.0..1.0).contains(&local) {
            return None;
        }
        Some(local as f32)
    }

    fn try_scroll_drag(&mut self) -> bool {
        if self.ui.open || self.tabs.is_empty() {
            return false;
        }
        let Some((x, y)) = self.cursor else {
            return false;
        };
        let Some(fraction) = self.gutter_fraction(x, y) else {
            return false;
        };
        let active = self.active.min(self.tabs.len() - 1);
        let mut term = self.tabs[active]
            .session
            .term
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if term.scroll_metrics().1 == 0.0 {
            return false;
        }
        term.scroll_to(fraction);
        drop(term);
        self.scroll_drag = true;
        self.redraw();
        true
    }

    fn drag_scroll(&mut self, x: f64, y: f64) {
        let Some(fraction) = self.gutter_fraction(x, y).or_else(|| {
            let cell_h = self.face.cell_h().max(1) as f64;
            let grid_h = f64::from(self.grid_rows.max(1)) * cell_h;
            let local = (y - self.chrome_height() as f64) / grid_h;
            Some(local.clamp(0.0, 1.0) as f32)
        }) else {
            return;
        };
        if self.tabs.is_empty() {
            return;
        }
        let active = self.active.min(self.tabs.len() - 1);
        self.tabs[active]
            .session
            .term
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .scroll_to(fraction);
        self.redraw();
    }

    fn cell_at(&self, x: f64, y: f64) -> Option<(u16, u16)> {
        let cell_w = self.face.cell_w().max(1) as f64;
        let cell_h = self.face.cell_h().max(1) as f64;
        let col = (x / cell_w).floor();
        let row = ((y - self.chrome_height() as f64) / cell_h).floor();
        if col < 0.0 || row < 0.0 {
            return None;
        }
        let col = col as u16;
        let row = row as u16;
        if col >= self.grid_cols || row >= self.grid_rows {
            return None;
        }
        Some((col, row))
    }

    fn on_click(&mut self) {
        let Some((x, y)) = self.cursor else {
            return;
        };
        if self.confirm_close {
            let choice = self.cell_at(x, y).and_then(|(col, row)| {
                self.close_dialog.as_ref().and_then(|dialog| dialog.hit(col, row))
            });
            match choice {
                Some(Choice::Cancel) => self.cancel_close(),
                Some(Choice::Close) => self.exit_confirmed = true,
                None => {}
            }
            return;
        }
        let cell_w = self.face.cell_w().max(1) as f64;
        if self.is_title_y(y) {
            self.on_title(x, cell_w);
            return;
        }
        if self.is_bar_y(y) {
            let col = (x / cell_w) as u16;
            match tabs::hit(&self.bar, col) {
                Some(BarHit::Select(index)) => self.focus_tab(index),
                Some(BarHit::Close(index)) => self.close_tab(index),
                Some(BarHit::Settings) => self.toggle_settings(),
                None => {}
            }
            return;
        }
        if self.ui.open {
            if let Some((col, row)) = self.cell_at(x, y) {
                if let Some(panel) = &self.panel {
                    if let Some(hit) = panel.hit(col, row) {
                        self.on_panel(hit);
                        return;
                    }
                    if panel.contains(col, row) {
                        return;
                    }
                }
            }
            self.close_settings();
        }
    }

    fn on_title(&mut self, x: f64, cell_w: f64) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let col = (x / cell_w) as u16;
        match titlebar::hit(&self.title_bar, col) {
            TitleHit::Close => {
                if self.tabs.len() > 1 {
                    self.ask_close();
                } else {
                    self.exit_confirmed = true;
                }
            }
            TitleHit::Minimize => window.set_minimized(true),
            TitleHit::Maximize => {
                window.set_maximized(!window.is_maximized());
                self.bar_dirty = true;
                self.redraw();
            }
            TitleHit::Drag => {
                let now = Instant::now();
                let double = self
                    .last_title_click
                    .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(350));
                if double {
                    self.last_title_click = None;
                    window.set_maximized(!window.is_maximized());
                    self.bar_dirty = true;
                    self.redraw();
                } else {
                    self.last_title_click = Some(now);
                    let _ = window.drag_window();
                }
            }
        }
    }

    fn begin_resize(&self, x: f64, y: f64) -> bool {
        let Some(window) = &self.window else {
            return false;
        };
        if window.is_maximized() {
            return false;
        }
        let Some(direction) = resize_edge(x, y, window.inner_size(), window.scale_factor()) else {
            return false;
        };
        window.drag_resize_window(direction).is_ok()
    }

    fn is_title_y(&self, y: f64) -> bool {
        y >= 0.0 && y < f64::from(self.chrome_height())
    }

    fn is_bar_y(&self, y: f64) -> bool {
        let Some(window) = &self.window else { return false };
        bar_contains_y(window.inner_size().height, self.chrome_height(), y)
    }

    fn chrome_height(&self) -> u32 {
        let scale = self.window.as_ref().map(|window| window.scale_factor()).unwrap_or(1.0);
        bar_height(scale, self.face.cell_h())
    }
}

fn bar_height(scale: f64, cell_h: u32) -> u32 {
    ((32.0 * scale).ceil() as u32).max(cell_h.max(1))
}

fn bar_contains_y(height: u32, bar_h: u32, y: f64) -> bool {
    y >= f64::from(height.saturating_sub(bar_h)) && y < f64::from(height)
}

fn resize_edge(x: f64, y: f64, size: PhysicalSize<u32>, scale: f64) -> Option<ResizeDirection> {
    let margin = (5.0 * scale).clamp(4.0, 12.0);
    let width = f64::from(size.width);
    let height = f64::from(size.height);
    let left = x < margin;
    let right = x > width - margin;
    let bottom = y > height - margin;
    let top = y < margin;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(err) = self.open_window(event_loop) {
            tracing::error!(%err, "Rustty não subiu a janela");
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if self.tabs.len() > 1 {
                    self.ask_close();
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(size) => {
                self.fit_size(size);
                self.redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.rescale(scale_factor);
                self.fit();
                self.redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.mods = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                self.on_key(&event);
                if self.tabs.is_empty() || self.exit_confirmed {
                    event_loop.exit();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.confirm_close {
                    self.on_wheel(delta);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some((position.x, position.y));
                if self.scroll_drag {
                    self.drag_scroll(position.x, position.y);
                }
                let cell_w = self.face.cell_w().max(1) as f64;
                let hot = self.is_bar_y(position.y)
                    && self
                        .bar
                        .settings
                        .as_ref()
                        .is_some_and(|settings| {
                            let col = (position.x / cell_w) as u16;
                            settings.contains(&col)
                        });
                if hot != self.settings_hot {
                    self.settings_hot = hot;
                    self.bar_dirty = true;
                    self.redraw();
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Released => self.scroll_drag = false,
                ElementState::Pressed => {
                    if let Some((x, y)) = self.cursor {
                        if self.begin_resize(x, y) {
                            return;
                        }
                    }
                    if self.confirm_close || !self.try_scroll_drag() {
                        self.on_click();
                    }
                    if self.tabs.is_empty() || self.exit_confirmed {
                        event_loop.exit();
                    }
                }
            },
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Wake) {
        match event {
            Wake::Frame(id) => {
                if let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) {
                    tab.session.frame_pending.store(false, Ordering::Release);
                }
                self.note_title(id);
                self.redraw();
            }
            Wake::Closed(id) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                    self.close_tab(index);
                }
                if self.tabs.is_empty() {
                    event_loop.exit();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_agent_scan {
            self.refresh_agents();
            self.next_agent_scan = now + Duration::from_secs(1);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_agent_scan));
    }
}

pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let ui = Ui::load();
    let font_size = ui.size;
    let vsync = std::env::var("RUSTTY_VSYNC").map(|value| value != "0").unwrap_or(true);
    let face = match ui.font_path() {
        Some(path) => Face::load_file(path, font_size).or_else(|_| Face::load(font_size))?,
        None => Face::load(font_size)?,
    };
    let mut atlas = Atlas::new(face.cell_w(), face.cell_h());
    atlas.warm(&face);

    let event_loop = EventLoop::<Wake>::with_user_event().build().context("event loop")?;
    let proxy = event_loop.create_proxy();
    let npu = Scheduler::start();
    let mut app = App {
        tabs: Vec::new(),
        active: 0,
        next_id: 1,
        npu,
        proxy,
        window: None,
        gpu: None,
        face,
        atlas,
        mods: ModifiersState::empty(),
        font_size,
        vsync,
        grid_cols: 80,
        grid_rows: 24,
        bar_dirty: true,
        bar: Bar { cells: Vec::new(), spans: Vec::new(), settings: None },
        title_bar: TitleBar {
            cells: Vec::new(),
            minimize: 0..0,
            maximize: 0..0,
            close: 0..0,
        },
        last_title_click: None,
        settings_hot: false,
        panel_dirty: false,
        panel: None,
        confirm_close: false,
        confirm_dirty: false,
        close_dialog: None,
        exit_confirmed: false,
        next_agent_scan: Instant::now(),
        ui,
        cursor: None,
        wheel_frac: 0.0,
        scroll_drag: false,
    };
    app.open_tab().context("abrindo pty")?;
    event_loop.run_app(&mut app).context("loop da janela")?;
    std::thread::sleep(Duration::from_millis(20));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{bar_contains_y, bar_height};

    #[test]
    fn bottom_bar_scales_and_uses_its_full_height_for_clicks() {
        assert_eq!(bar_height(1.0, 19), 32);
        assert_eq!(bar_height(2.0, 38), 64);
        assert_eq!(bar_height(1.0, 40), 40);
        assert!(!bar_contains_y(700, 32, 667.9));
        assert!(bar_contains_y(700, 32, 668.0));
        assert!(bar_contains_y(700, 32, 699.9));
        assert!(!bar_contains_y(700, 32, 700.0));
    }
}
