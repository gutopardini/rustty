use std::os::fd::AsFd;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::{CursorIcon, Icon, ResizeDirection, Window, WindowId};

use crate::agent::{self, Agent};
use crate::clipboard::{self, Clipboard};
use crate::color::{self, Theme};
use crate::confirm::{self, Choice, Dialog};
use crate::font::{Atlas, Face};
use crate::gpu::Renderer;
use crate::input::{self, Input};
use crate::npu::{Indexer, Scheduler};
use crate::graphics::DecodeJob;
use crate::pty::Pty;
use crate::settings::{self, Panel, PanelHit, Ui};
use crate::tabs::{self, Bar, BarHit};
use crate::term::{Snapshot, Term};
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
    /// The window thread only draws this tab while it is in front.
    visible: Arc<AtomicBool>,
    /// Latest grid, swapped in without the parser waiting on the GPU.
    frames: Arc<Mutex<Option<Snapshot>>>,
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
        let visible = Arc::new(AtomicBool::new(false));
        let shown = Arc::clone(&visible);
        let frames = Arc::new(Mutex::new(None));
        let published = Arc::clone(&frames);
        let reader = std::thread::Builder::new()
            .name("rustty-pty".into())
            .spawn(move || pty_loop(id, reader_fd, term_reader, pty_writer, indexer, proxy, pending, shown, published))
            .map_err(std::io::Error::other)?;
        Ok(Self {
            pty,
            term,
            reader: Some(reader),
            frame_pending,
            visible,
            frames,
        })
    }
}

impl Session {
    /// Mutates the grid and leaves the result in the slot the window draws.
    fn update<R>(&self, f: impl FnOnce(&mut Term) -> R) -> R {
        let frames = Arc::clone(&self.frames);
        let mut term = self.term.lock().unwrap_or_else(|err| err.into_inner());
        let result = f(&mut term);
        publish_frame(&mut term, &frames);
        result
    }

    /// Scrolls this tab, or returns bytes the program asked to receive instead.
    fn wheel(&self, steps: isize, col: u16, row: u16, pixel: (u32, u32)) -> Option<Vec<u8>> {
        let frames = Arc::clone(&self.frames);
        let mut term = self.term.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(bytes) = term.wheel_write(steps, col, row, pixel) {
            return Some(bytes);
        }
        term.scroll_view(steps);
        publish_frame(&mut term, &frames);
        None
    }

    fn take_frame(&self) -> Option<Snapshot> {
        self.frames.lock().unwrap_or_else(|err| err.into_inner()).take()
    }
}

/// Moves a finished grid into the slot the window thread draws. The lock covers the swap only.
fn publish_frame(term: &mut Term, frames: &Mutex<Option<Snapshot>>) -> bool {
    let mut slot = frames.lock().unwrap_or_else(|err| err.into_inner());
    if slot.is_some() {
        term.damage_all();
    }
    let snap = term.snapshot();
    if snap.changed || snap.title_changed {
        *slot = Some(snap);
    }
    slot.is_some()
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
    visible: Arc<AtomicBool>,
    frames: Arc<Mutex<Option<Snapshot>>>,
) {
    let mut buf = [0u8; 64 * 1024];
    // Newest image per id. While a key is in flight we keep the previous picture on screen.
    let mut held: Vec<DecodeJob> = Vec::new();
    loop {
        let mut master_flags = PollFlags::POLLIN | PollFlags::POLLHUP;
        if pty.has_writes() {
            master_flags |= PollFlags::POLLOUT;
        }
        let timeout = if held.is_empty() { PollTimeout::NONE } else { PollTimeout::from(40u16) };
        let (master_ready, wake_ready) = {
            let mut fds = [
                PollFd::new(fd.as_fd(), master_flags),
                PollFd::new(pty.wake_fd(), PollFlags::POLLIN),
            ];
            match nix::poll::poll(&mut fds, timeout) {
                Ok(_) => {}
                Err(Errno::EINTR) => continue,
                Err(err) => {
                    tracing::debug!(%err, id, "pty poll ended");
                    let _ = proxy.send_event(Wake::Closed(id));
                    break;
                }
            }
            (
                fds[0].revents().unwrap_or(PollFlags::empty()),
                fds[1].revents().unwrap_or(PollFlags::empty()),
            )
        };
        if wake_ready.contains(PollFlags::POLLIN) {
            pty.drain_wake();
        }
        if master_ready.contains(PollFlags::POLLOUT) || pty.has_writes() {
            pty.flush_writes();
        }
        let mut saw_output = false;
        if master_ready.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR) {
            match nix::unistd::read(&fd, &mut buf) {
                Ok(0) => {
                    let _ = proxy.send_event(Wake::Closed(id));
                    break;
                }
                Ok(n) => {
                    let mut pending = Vec::with_capacity(n);
                    pending.extend_from_slice(&buf[..n]);
                    while pending.len() < 1024 * 1024 {
                        let mut probe = [PollFd::new(fd.as_fd(), PollFlags::POLLIN)];
                        match nix::poll::poll(&mut probe, PollTimeout::ZERO) {
                            Ok(ready) if ready > 0 => {}
                            _ => break,
                        }
                        match nix::unistd::read(&fd, &mut buf) {
                            Ok(0) | Err(Errno::EAGAIN) | Err(Errno::EINTR) => break,
                            Ok(more) => pending.extend_from_slice(&buf[..more]),
                            Err(_) => break,
                        }
                    }
                    let mut offset = 0;
                    while offset < pending.len() {
                        let (used, job) = {
                            let mut term = term.lock().unwrap_or_else(|err| err.into_inner());
                            term.advance_until_image(&pending[offset..])
                        };
                        if used == 0 {
                            break;
                        }
                        offset += used;
                        if let Some(job) = job {
                            keep_latest_image(&mut held, job);
                        }
                    }
                    saw_output = true;
                }
                Err(Errno::EAGAIN) | Err(Errno::EINTR) => {}
                Err(err) => {
                    tracing::debug!(%err, id, "pty read ended");
                    let _ = proxy.send_event(Wake::Closed(id));
                    break;
                }
            }
        }
        if !held.is_empty() && !pty.typing() {
            for job in held.drain(..) {
                let image = Term::decode_image(&job);
                let mut term = term.lock().unwrap_or_else(|err| err.into_inner());
                term.finish_image(job, image);
            }
            saw_output = true;
        }
        if !saw_output {
            continue;
        }
        let (lines, replies, draw) = {
            let mut term = term.lock().unwrap_or_else(|err| err.into_inner());
            let lines = term.take_indexed_lines();
            let replies = term.take_replies();
            // The grid is published here, on the reader thread. The window only swaps it in.
            let draw = visible.load(Ordering::Relaxed) && publish_frame(&mut term, &frames);
            (lines, replies, draw)
        };
        for line in lines {
            indexer.index_line(line);
        }
        if !replies.is_empty() {
            let _ = pty.write_all(&replies);
            pty.flush_writes();
        }
        // A hidden tab keeps parsing on its own thread. Waking the window for
        // every chunk makes the key handler wait behind the other tab's frame.
        if draw && !frame_pending.swap(true, Ordering::AcqRel) {
            let _ = proxy.send_event(Wake::Frame(id));
        }
    }
}

fn keep_latest_image(held: &mut Vec<DecodeJob>, job: DecodeJob) {
    let id = job.id();
    held.retain(|old| old.id() != id);
    held.push(job);
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
    link_cell: Option<(u16, u16)>,
    hover_icon: CursorIcon,
    selection: Option<((u16, u16), (u16, u16))>,
    selecting: bool,
    press_moved: bool,
    press_cell: (u16, u16),
    anchor_cell: (u16, u16),
    last_click: Option<(Instant, (u16, u16), u8)>,
    pending_link: Option<(Instant, String)>,
    selection_dirty: bool,
    clipboard: Clipboard,
    key_app_cursor: bool,
    key_alt: bool,
    key_bracketed: bool,
    key_composer: Option<[u16; 4]>,
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
        .context("loading the window icon")?;
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
                .context("creating the window")?,
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
            tab.session.update(|term| term.damage_all());
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
        let Some(window) = &self.window else {
            return;
        };
        let scrollbar = self.scrollbar(window, size);
        if let Some(gpu) = &mut self.gpu {
            gpu.resize(size);
        }
        let cell_w = self.face.cell_w().max(1);
        let cell_h = self.face.cell_h().max(1);
        let cols = (scrollbar.gutter_left() / cell_w).max(2) as u16;
        let rows = (scrollbar.height / cell_h).max(1) as u16;
        self.grid_cols = cols;
        self.grid_rows = rows;
        self.bar_dirty = true;
        for tab in &self.tabs {
            let resized = tab.session.update(|term| {
                if cols != term.cols() || rows != term.rows() {
                    term.resize(cols, rows);
                    true
                } else {
                    // The surface can resize even when the grid does not.
                    term.damage_all();
                    false
                }
            });
            if resized {
                let _ = tab.session.pty.resize(cols, rows);
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
        session.update(|term| term.restyle(theme.fg, theme.bg));
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
        self.clear_selection();
        for (tab_index, tab) in self.tabs.iter().enumerate() {
            tab.session.visible.store(tab_index == index, Ordering::Relaxed);
        }
        let title = self.tabs[index].session.update(|term| {
            term.damage_all();
            term.title().to_string()
        });
        if !title.is_empty() {
            self.tabs[index].title = title;
        }
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

    fn note_title(&mut self, id: u64) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        // The reader owns this lock while it parses. Waiting here stalls every key.
        let Ok(term) = self.tabs[index].session.term.try_lock() else {
            return false;
        };
        let title = term.title().to_string();
        drop(term);
        if title.is_empty() || title == self.tabs[index].title {
            return false;
        }
        self.tabs[index].title = title;
        self.bar_dirty = true;
        if index == self.active {
            if let Some(window) = &self.window {
                window.set_title(&self.tabs[self.active].title);
            }
        }
        true
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
            if let Ok(term) = tab.session.term.try_lock() {
                let title = term.title().to_string();
                if !title.is_empty() && title != tab.title {
                    tab.title = title;
                    changed = true;
                }
            }
        }
        if changed {
            self.bar_dirty = true;
            self.redraw();
        }
    }

    fn draw(&mut self) {
        if self.tabs.is_empty() || self.gpu.is_none() {
            return;
        }
        let active = self.active.min(self.tabs.len() - 1);
        self.active = active;
        let show_terminal = !self.ui.open && !self.confirm_close;
        let snap = self.tabs[active].session.take_frame();
        let composer = self.composer_for(active, show_terminal);
        let cleared = snap.as_ref().is_some_and(|snap| snap.content_changed)
            && !self.selecting
            && self.selection.take().is_some();
        let paint_selection = self.selection_dirty || cleared;
        self.selection_dirty = false;
        if let Some(snap) = &snap {
            if snap.title_changed && snap.title != self.tabs[active].title && !snap.title.is_empty() {
                self.tabs[active].title = snap.title.clone();
                self.bar_dirty = true;
                if let Some(window) = &self.window {
                    window.set_title(&snap.title);
                }
            }
        }
        let terminal_dirty = snap.as_ref().is_some_and(|snap| !snap.suspended && snap.changed);
        let stamp_panel = self.ui.open && (self.panel_dirty || terminal_dirty);
        let draw_confirm =
            self.confirm_dirty || (self.confirm_close && (self.bar_dirty || terminal_dirty));
        if snap.is_none() && !self.bar_dirty && !stamp_panel && !draw_confirm && !paint_selection {
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        let scrollbar = self.scrollbar(window, size);
        let chrome_h = scrollbar.top as f32;
        let theme = color::theme(self.ui.style);
        let selection = self.selection_span();
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        gpu.set_insets(chrome_h, chrome_h);
        gpu.set_clear(theme.bg);
        gpu.set_selection(selection);
        let suspended = snap.as_ref().is_some_and(|snap| snap.suspended);
        if let Some(snap) = snap.as_ref().filter(|snap| !snap.suspended) {
            gpu.upload(&mut self.atlas, &self.face, snap, composer, theme.accent, scrollbar.track());
        } else {
            gpu.refresh_cached(composer, theme.accent, scrollbar.track());
        }
        if self.bar_dirty {
            let titles: Vec<String> = self.tabs.iter().map(|tab| tab.title.clone()).collect();
            let agents: Vec<Option<Agent>> = self.tabs.iter().map(|tab| tab.agent).collect();
            let cols = (size.width / self.face.cell_w().max(1)).clamp(1, 1024) as u16;
            let settings_lit = self.settings_hot || self.ui.open;
            self.bar = tabs::render_bar(&titles, &agents, active, cols, theme, settings_lit);
            let edge = if settings_lit && self.bar.settings.is_some() { theme.tab } else { theme.bar };
            gpu.upload_bar(&mut self.atlas, &self.face, &self.bar.cells, theme.bar, edge);
            let title = self.tabs.get(active).map(|tab| tab.title.as_str()).unwrap_or("Rustty");
            self.title_bar = titlebar::render(title, cols, theme, window.is_maximized());
            gpu.upload_title(&mut self.atlas, &self.face, &self.title_bar.cells, theme.bar);
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
        if let Err(err) = gpu.render(show_terminal && !suspended) {
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
            tab.session.update(|term| term.damage_all());
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
            tab.session.update(|term| term.restyle(theme.fg, theme.bg));
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
                    tab.session.update(|term| term.damage_all());
                }
                self.fit();
                self.bar_dirty = true;
                self.panel_dirty = true;
                self.ui.save();
                self.redraw();
            }
            Err(err) => {
                self.ui.font = previous;
                    tracing::warn!(%err, "font");
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
        self.pending_link = None;
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
        if self.selection.is_some() && matches!(event.logical_key, Key::Named(winit::keyboard::NamedKey::Escape)) {
            self.clear_selection();
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
        let (app_cursor, _, rows) = self.key_mode(active);
        let action = input::encode(event, self.mods, app_cursor, rows);
        if event.repeat && !matches!(action, Some(Input::Bytes(_)) | Some(Input::Scroll(_)) | None) {
            return;
        }
        match action {
            Some(Input::Bytes(bytes)) => {
                self.clear_selection();
                self.tabs[active].session.pty.note_input();
                if let Err(err) = self.tabs[active].session.pty.write_all(&bytes) {
                    tracing::warn!(%err, "pty write");
                }
            }
            Some(Input::Copy) => self.copy_selection(),
            Some(Input::Paste) => self.paste(false),
            Some(Input::Scroll(delta)) => {
                self.clear_selection();
                self.tabs[active].session.update(|term| term.scroll_view(delta));
                self.redraw();
            }
            Some(Input::NewTab) => {
                if let Err(err) = self.open_tab() {
                    tracing::warn!(%err, "tab did not open");
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
        if self.tabs.is_empty() || self.selecting {
            return;
        }
        self.clear_selection();
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
        let (col, row, pixel) = self
            .cursor
            .and_then(|(x, y)| {
                let (col, row) = self.cell_at(x, y)?;
                let top = self.chrome_height() as f64;
                Some((col.saturating_add(1), row.saturating_add(1), (x.max(0.0) as u32 + 1, (y - top).max(0.0) as u32 + 1)))
            })
            .unwrap_or((1, 1, (1, 1)));
        // Shift keeps the wheel on this tab's history even when the program grabbed the mouse.
        let report = if self.mods.shift_key() {
            self.tabs[active].session.update(|term| term.scroll_view(steps));
            None
        } else {
            self.tabs[active].session.wheel(steps, col, row, pixel)
        };
        if let Some(bytes) = report {
            self.tabs[active].session.pty.note_input();
            if let Err(err) = self.tabs[active].session.pty.write_all(&bytes) {
                tracing::warn!(%err, "pty write");
            }
            return;
        }
        self.redraw();
    }

    fn scrollbar(&self, window: &Window, size: PhysicalSize<u32>) -> Scrollbar {
        Scrollbar::new(size, self.chrome_height(), window.scale_factor(), window.is_maximized())
    }

    fn try_scroll_drag(&mut self) -> bool {
        if self.ui.open || self.tabs.is_empty() {
            return false;
        }
        let (Some((x, y)), Some(window)) = (self.cursor, &self.window) else {
            return false;
        };
        let Some(fraction) = self.scrollbar(window, window.inner_size()).hit(x, y) else {
            return false;
        };
        let active = self.active.min(self.tabs.len() - 1);
        let moved = self.tabs[active].session.update(|term| {
            if term.scroll_metrics().1 == 0.0 {
                return false;
            }
            term.scroll_to(fraction);
            true
        });
        if !moved {
            return false;
        }
        self.clear_selection();
        self.scroll_drag = true;
        self.redraw();
        true
    }

    fn drag_scroll(&mut self, y: f64) {
        let Some(window) = &self.window else {
            return;
        };
        let Some(fraction) = self.scrollbar(window, window.inner_size()).fraction(y) else {
            return;
        };
        if self.tabs.is_empty() {
            return;
        }
        let active = self.active.min(self.tabs.len() - 1);
        self.tabs[active].session.update(|term| term.scroll_to(fraction));
        self.redraw();
    }

    /// Cursor-key mode without waiting on the parser. Image decode must not delay a key.
    fn key_mode(&mut self, active: usize) -> (bool, bool, u16) {
        let fresh = self.tabs[active].session.term.try_lock().ok().map(|term| {
            (
                term.app_cursor(),
                term.alt_screen(),
                term.rows(),
                term.bracketed_paste(),
                term.codex_composer(),
            )
        });
        if let Some((app_cursor, alt, rows, bracketed, composer)) = fresh {
            self.key_app_cursor = app_cursor;
            self.key_alt = alt;
            self.key_bracketed = bracketed;
            self.key_composer = composer;
            (app_cursor, alt, rows)
        } else {
            (self.key_app_cursor, self.key_alt, self.grid_rows)
        }
    }

    fn composer_for(&mut self, active: usize, show_terminal: bool) -> Option<[u16; 4]> {
        if !show_terminal || self.tabs[active].agent != Some(Agent::Codex) {
            return None;
        }
        let fresh = self.tabs[active]
            .session
            .term
            .try_lock()
            .ok()
            .map(|term| term.codex_composer());
        if let Some(composer) = fresh {
            self.key_composer = composer;
        }
        self.key_composer
    }

    fn link_at(&self, x: f64, y: f64) -> Option<String> {
        if self.tabs.is_empty() || self.is_title_y(y) || self.is_bar_y(y) {
            return None;
        }
        let (col, row) = self.cell_at(x, y)?;
        let active = self.active.min(self.tabs.len() - 1);
        let raw = {
            let Ok(term) = self.tabs[active].session.term.try_lock() else {
                return None;
            };
            term.link_at(row, col)?
        };
        openable(&raw, &self.shell_cwd(active))
    }

    fn shell_cwd(&self, active: usize) -> PathBuf {
        let pid = self.tabs[active].session.pty.shell_pid();
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .or_else(|| self.ui.cwd.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
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

    fn selection_span(&self) -> (u32, u32) {
        let Some((anchor, focus)) = self.selection else {
            return (0, 0);
        };
        let cols = u32::from(self.grid_cols.max(1));
        let linear = |cell: (u16, u16)| {
            u32::from(cell.1) * cols + u32::from(cell.0.min(self.grid_cols.saturating_sub(1)))
        };
        let (lo, hi) = {
            let a = linear(anchor);
            let b = linear(focus);
            if a <= b { (a, b) } else { (b, a) }
        };
        (lo, hi.saturating_add(1))
    }

    fn note_click(&mut self, cell: (u16, u16)) -> u8 {
        let now = Instant::now();
        let count = match self.last_click {
            Some((at, prev, count))
                if prev == cell && now.saturating_duration_since(at) < Duration::from_millis(400) =>
            {
                if count >= 3 { 1 } else { count + 1 }
            }
            _ => 1,
        };
        self.last_click = Some((now, cell, count));
        count
    }

    fn begin_pointer(&mut self) {
        self.pending_link = None;
        if let Some((x, y)) = self.cursor {
            if self.begin_resize(x, y) {
                return;
            }
        }
        if self.confirm_close {
            self.on_click();
            return;
        }
        if self.try_scroll_drag() || self.begin_selection() {
            return;
        }
        self.on_click();
    }

    fn end_pointer(&mut self) {
        self.scroll_drag = false;
        if !self.selecting {
            return;
        }
        self.selecting = false;
        if self.press_moved {
            self.copy_selection();
            return;
        }
        self.clear_selection();
        if let Some(url) = self.cursor.and_then(|(x, y)| self.link_at(x, y)) {
            self.pending_link = Some((Instant::now() + Duration::from_millis(400), url));
            return;
        }
        self.on_click();
    }

    fn begin_selection(&mut self) -> bool {
        if self.ui.open || self.tabs.is_empty() {
            return false;
        }
        let Some((x, y)) = self.cursor else {
            return false;
        };
        if self.is_title_y(y) || self.is_bar_y(y) {
            return false;
        }
        let Some(cell) = self.cell_at(x, y) else {
            return false;
        };
        self.selecting = true;
        self.press_cell = cell;
        self.anchor_cell = cell;
        self.press_moved = false;
        let count = self.note_click(cell);
        if self.mods.shift_key() {
            let anchor = self.selection.map(|(anchor, _)| anchor).unwrap_or(cell);
            self.anchor_cell = anchor;
            self.selection = Some((anchor, cell));
            self.press_moved = cell != anchor;
            self.selection_dirty = true;
            self.redraw();
            return true;
        }
        if count >= 2 {
            let active = self.active.min(self.tabs.len() - 1);
            let span = match self.tabs[active].session.term.try_lock() {
                Ok(term) if count >= 3 => ((0, cell.1), (term.cols().saturating_sub(1), cell.1)),
                Ok(term) => term.word_span(cell.0, cell.1),
                Err(_) => (cell, cell),
            };
            self.anchor_cell = span.0;
            self.selection = Some(span);
            self.press_moved = true;
            self.selection_dirty = true;
            self.redraw();
        }
        true
    }

    fn extend_selection(&mut self, x: f64, y: f64) {
        let Some(cell) = self.cell_at(x, y) else {
            return;
        };
        if cell == self.press_cell {
            return;
        }
        let anchor = self.selection.map(|(anchor, _)| anchor).unwrap_or(self.anchor_cell);
        if self.selection == Some((anchor, cell)) {
            return;
        }
        self.selection = Some((anchor, cell));
        self.press_moved = true;
        self.selection_dirty = true;
        self.redraw();
    }

    fn clear_selection(&mut self) {
        self.selecting = false;
        self.press_moved = false;
        if self.selection.take().is_some() {
            self.selection_dirty = true;
            self.redraw();
        }
    }

    fn copy_selection(&mut self) {
        let Some((anchor, focus)) = self.selection else {
            return;
        };
        if self.tabs.is_empty() {
            return;
        }
        let active = self.active.min(self.tabs.len() - 1);
        let text = {
            let Ok(term) = self.tabs[active].session.term.try_lock() else {
                return;
            };
            term.selected_text(anchor, focus)
        };
        if text.is_empty() {
            return;
        }
        self.clipboard.set(&text, true);
    }

    fn paste(&mut self, primary: bool) {
        if self.tabs.is_empty() || self.ui.open || self.confirm_close {
            return;
        }
        let Some(text) = self.clipboard.get(primary) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let active = self.active.min(self.tabs.len() - 1);
        if let Some(bracketed) = self.tabs[active]
            .session
            .term
            .try_lock()
            .ok()
            .map(|term| term.bracketed_paste())
        {
            self.key_bracketed = bracketed;
        }
        let bracketed = self.key_bracketed;
        let bytes = clipboard::paste_bytes(&text, bracketed);
        self.clear_selection();
        self.tabs[active].session.pty.note_input();
        if let Err(err) = self.tabs[active].session.pty.write_all(&bytes) {
            tracing::warn!(%err, "pty write");
        }
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
        if self.is_title_y(y) {
            self.on_title(x);
            return;
        }
        if self.is_bar_y(y) {
            let col = strip_col(x, self.face.cell_w(), self.bar.cells.len());
            match tabs::hit(&self.bar, col) {
                Some(BarHit::Select(index)) => self.focus_tab(index),
                Some(BarHit::Close(index)) => self.close_tab(index),
                Some(BarHit::Settings) => self.toggle_settings(),
                None => {}
            }
            return;
        }
        if !self.confirm_close && !self.ui.open {
                if let Some(target) = self.link_at(x, y) {
                    open_target(&target);
                    return;
                }
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

    fn on_title(&mut self, x: f64) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let col = strip_col(x, self.face.cell_w(), self.title_bar.cells.len());
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

/// A web address opens in the browser. A file path opens in the default application for its type.
fn openable(raw: &str, cwd: &Path) -> Option<String> {
    if raw.contains(['\n', '\r', '\0']) {
        return None;
    }
    if raw.starts_with("https://") || raw.starts_with("http://") {
        return Some(raw.to_string());
    }
    existing_file(raw, cwd).map(|path| path.display().to_string())
}

fn existing_file(token: &str, cwd: &Path) -> Option<PathBuf> {
    let expanded = expand_home(token)?;
    let path = if expanded.is_absolute() { expanded } else { cwd.join(expanded) };
    let canon = path.canonicalize().ok()?;
    let meta = std::fs::metadata(&canon).ok()?;
    (meta.is_file() || meta.is_dir()).then_some(canon)
}

fn expand_home(token: &str) -> Option<PathBuf> {
    if let Some(rest) = token.strip_prefix("~/") {
        let home = std::env::var_os("HOME")?;
        return Some(PathBuf::from(home).join(rest));
    }
    if token == "~" {
        return None;
    }
    Some(PathBuf::from(token))
}

fn open_target(target: &str) {
    if target.contains(['\n', '\r', '\0']) {
        return;
    }
    let web = target.starts_with("https://") || target.starts_with("http://");
    let path = Path::new(target);
    let file = target.starts_with('/') && (path.is_file() || path.is_dir());
    if !web && !file {
        return;
    }
    // xdg-open picks the registered application for the address or the file type.
    // An application that is already open receives the target. A new window appears only when none is open.
    if let Err(err) = std::process::Command::new("xdg-open")
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        tracing::warn!(%err, "link did not open");
    }
}

fn bar_height(scale: f64, cell_h: u32) -> u32 {
    ((32.0 * scale).ceil() as u32).max(cell_h.max(1))
}

fn bar_contains_y(height: u32, bar_h: u32, y: f64) -> bool {
    y >= f64::from(height.saturating_sub(bar_h)) && y < f64::from(height)
}

fn strip_col(x: f64, cell_w: u32, cols: usize) -> u16 {
    ((x / f64::from(cell_w)) as usize).min(cols.saturating_sub(1)) as u16
}

#[derive(Clone, Copy)]
struct Scrollbar {
    right: u32,
    width: u32,
    top: u32,
    height: u32,
}

impl Scrollbar {
    fn new(size: PhysicalSize<u32>, chrome: u32, scale: f64, maximized: bool) -> Self {
        let border = if maximized { 0 } else { resize_margin(scale).ceil() as u32 };
        Self {
            right: size.width.saturating_sub(border),
            width: (4.0 * scale).round().max(1.0) as u32,
            top: chrome,
            height: size.height.saturating_sub(chrome.saturating_mul(2)),
        }
    }

    fn gutter_left(self) -> u32 {
        self.right.saturating_sub(self.width.saturating_mul(2))
    }

    fn track(self) -> Range<u32> {
        self.right.saturating_sub(self.width)..self.right
    }

    fn fraction(self, y: f64) -> Option<f32> {
        (self.height > 0).then(|| ((y - f64::from(self.top)) / f64::from(self.height)) as f32)
    }

    fn hit(self, x: f64, y: f64) -> Option<f32> {
        if !(f64::from(self.gutter_left())..f64::from(self.right)).contains(&x) {
            return None;
        }
        self.fraction(y).filter(|fraction| (0.0..1.0).contains(fraction))
    }
}

fn resize_margin(scale: f64) -> f64 {
    (5.0 * scale).clamp(4.0, 12.0)
}

fn resize_edge(x: f64, y: f64, size: PhysicalSize<u32>, scale: f64) -> Option<ResizeDirection> {
    let margin = resize_margin(scale);
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
            tracing::error!(%err, "Rustty did not open a window");
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
                    self.drag_scroll(position.y);
                }
                let col = strip_col(position.x, self.face.cell_w(), self.bar.cells.len());
                let hot = self.is_bar_y(position.y)
                    && self
                        .bar
                        .settings
                        .as_ref()
                        .is_some_and(|settings| settings.contains(&col));
                if hot != self.settings_hot {
                    self.settings_hot = hot;
                    self.bar_dirty = true;
                    self.redraw();
                }
                let cell = if self.ui.open
                    || self.confirm_close
                    || self.is_title_y(position.y)
                    || self.is_bar_y(position.y)
                {
                    None
                } else {
                    self.cell_at(position.x, position.y)
                };
                if self.selecting {
                    self.extend_selection(position.x, position.y);
                }
                if cell != self.link_cell {
                    self.link_cell = cell;
                    let icon = if cell.is_some() && self.link_at(position.x, position.y).is_some() {
                        CursorIcon::Pointer
                    } else if cell.is_some() {
                        CursorIcon::Text
                    } else {
                        CursorIcon::Default
                    };
                    if icon != self.hover_icon {
                        self.hover_icon = icon;
                        if let Some(window) = &self.window {
                            window.set_cursor(icon);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                match (button, state) {
                    (MouseButton::Left, ElementState::Released) => self.end_pointer(),
                    (MouseButton::Left, ElementState::Pressed) => self.begin_pointer(),
                    (MouseButton::Middle, ElementState::Pressed) => self.paste(true),
                    _ => {}
                }
                if self.tabs.is_empty() || self.exit_confirmed {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Wake) {
        match event {
            Wake::Frame(id) => {
                if let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) {
                    tab.session.frame_pending.store(false, Ordering::Release);
                }
                let title_changed = self.note_title(id);
                let active = self.tabs.get(self.active).is_some_and(|tab| tab.id == id);
                if active || title_changed {
                    self.redraw();
                }
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
        let mut until = self.next_agent_scan;
        if let Some((at, url)) = self.pending_link.clone() {
            if now >= at {
                self.pending_link = None;
                open_target(&url);
            } else {
                until = until.min(at);
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(until));
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
        link_cell: None,
        hover_icon: CursorIcon::Default,
        selection: None,
        selecting: false,
        press_moved: false,
        press_cell: (0, 0),
        anchor_cell: (0, 0),
        last_click: None,
        pending_link: None,
        selection_dirty: false,
        clipboard: Clipboard::default(),
        key_app_cursor: false,
        key_alt: false,
        key_bracketed: false,
        key_composer: None,
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
    app.open_tab().context("opening the pty")?;
    event_loop.run_app(&mut app).context("window loop")?;
    std::thread::sleep(Duration::from_millis(20));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PhysicalSize, ResizeDirection, Scrollbar, bar_contains_y, bar_height, existing_file, resize_edge, strip_col};

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

    #[test]
    fn scrollbar_reaches_the_window_edge_and_stops_at_the_chrome() {
        let scroll = Scrollbar::new(PhysicalSize::new(2560, 1408), 32, 1.0, true);
        assert_eq!(scroll.track(), 2556..2560);
        assert_eq!(scroll.hit(2559.0, 32.0), Some(0.0));
        assert_eq!(scroll.hit(2559.0, 704.0), Some(0.5));
        assert!(scroll.hit(2559.0, 1375.0).is_some());
        assert_eq!(scroll.hit(2559.0, 1376.0), None);
        assert_eq!(scroll.hit(2551.0, 704.0), None);
        assert_eq!(strip_col(2559.0, 13, 196), 195);
    }

    #[test]
    fn floating_scrollbar_leaves_room_to_resize() {
        let size = PhysicalSize::new(2200, 1400);
        let scroll = Scrollbar::new(size, 64, 2.0, false);
        assert_eq!(scroll.track(), 2182..2190);
        assert_eq!(scroll.hit(2189.0, 700.0), Some(0.5));
        assert_eq!(resize_edge(2189.0, 700.0, size, 2.0), None);
        assert_eq!(scroll.hit(2199.0, 700.0), None);
        assert_eq!(resize_edge(2199.0, 700.0, size, 2.0), Some(ResizeDirection::East));
    }

    #[test]
    fn relative_path_resolves_inside_the_shell_directory() {
        let dir = std::env::temp_dir().join(format!("rustty-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nota.txt"), "ok").unwrap();
        let found = existing_file("nota.txt", &dir).expect("file");
        assert_eq!(found, dir.join("nota.txt").canonicalize().unwrap());
        assert!(existing_file("ausente.txt", &dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
