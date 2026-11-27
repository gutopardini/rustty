use std::os::fd::AsFd;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::platform::x11::WindowAttributesExtX11;
use winit::window::{CursorIcon, Icon, ResizeDirection, Window, WindowId};

use crate::agent::{self, Agent};
use crate::blur::Blur;
use crate::desktop::{TitleAction, TitlebarPrefs};
use crate::clipboard::{self, Clipboard};
use crate::color::{self, Theme};
use crate::confirm::{self, Choice, Dialog};
use crate::font::{Atlas, Face};
use crate::gpu::{
    GlassShape, Renderer, GLASS_BAR, GLASS_BUTTON, GLASS_DIVIDER, GLASS_DOT, GLASS_LIMITS_CARD,
    GLASS_LIMITS_ICON, GLASS_SHEET, GLASS_TAB_HOVER, GLASS_TAB_STRIP, GLASS_TOOLTIP, ICON_CLOSE,
    ICON_LIMITS_CRIT, ICON_LIMITS_OK, ICON_LIMITS_WARN, ICON_MAXIMIZE, ICON_MINIMIZE, ICON_PLUS,
    ICON_RESTORE,
};
use crate::input::{self, Input};
use crate::limits::{self, Health as LimitsHealth, Popover, PopoverHit};
use crate::npu::{Indexer, Scheduler};
use crate::pty::Pty;
use crate::settings::{self, Panel, PanelHit, Ui};
use crate::tabs::{self, Bar, BarHit};
use crate::term::{Snapshot, Term};
use crate::titlebar::{self, TitleBar, TitleHit};

#[derive(Debug)]
enum Wake {
    Frame(u64),
    Closed(u64),
    Limits(limits::Snapshot),
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

    /// Scrolls the program that owns the live screen, or this tab's history.
    fn follow_selection(&self, steps: isize, col: u16, row: u16, pixel: (u32, u32)) -> Follow {
        let frames = Arc::clone(&self.frames);
        let mut term = self.term.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(bytes) = term.selection_scroll(steps, col, row, pixel) {
            return Follow::Program(bytes);
        }
        let before = term.view_top();
        term.scroll_view(steps);
        let top = (term.view_top() != before).then(|| term.view_top());
        if top.is_some() {
            publish_frame(&mut term, &frames);
        }
        Follow::History(top)
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

enum Follow {
    /// Bytes for the program that owns the viewport.
    Program(Vec<u8>),
    /// `Some` is the new absolute line of the first row, after this tab's history moved.
    History(Option<u64>),
}

/// Moves a finished grid into the slot the window thread draws. The lock covers the swap only.
fn publish_frame(term: &mut Term, frames: &Mutex<Option<Snapshot>>) -> bool {
    let mut slot = frames.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(pending) = slot.as_ref() {
        term.keep_pending_damage(pending);
    }
    let mut snap = term.snapshot();
    if snap.suspended && let Some(ready) = slot.as_mut() {
        // A partial synchronized update must leave the last complete frame drawable.
        if snap.title_changed {
            ready.title = snap.title;
            ready.title_changed = true;
        }
        return true;
    }
    if (!snap.suspended && snap.changed) || snap.title_changed {
        if !snap.title_changed && let Some(previous) = slot.as_mut().filter(|old| old.title_changed) {
            snap.title = std::mem::take(&mut previous.title);
            snap.title_changed = true;
        }
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
    let mut pending = Vec::new();
    loop {
        let mut master_flags = PollFlags::POLLIN | PollFlags::POLLHUP;
        if pty.has_writes() {
            master_flags |= PollFlags::POLLOUT;
        }
        let (master_ready, wake_ready) = {
            let mut fds = [
                PollFd::new(fd.as_fd(), master_flags),
                PollFd::new(pty.wake_fd(), PollFlags::POLLIN),
            ];
            match nix::poll::poll(&mut fds, PollTimeout::NONE) {
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
                    fill_pending(&fd, &mut buf, n, &mut pending);
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
                            // Decode outside the lock, then apply before parsing the next command.
                            let image = Term::decode_image(&job);
                            let mut term = term.lock().unwrap_or_else(|err| err.into_inner());
                            term.finish_image(job, image);
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
        if !saw_output {
            continue;
        }
        let (records, replies, draw) = {
            let mut term = term.lock().unwrap_or_else(|err| err.into_inner());
            let records = term.take_index_records();
            let replies = term.take_replies();
            // The grid is published here, on the reader thread. The window only swaps it in.
            let draw = visible.load(Ordering::Relaxed) && publish_frame(&mut term, &frames);
            (records, replies, draw)
        };
        indexer.index_records(records);
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

struct Tab {
    id: u64,
    title: String,
    /// Name typed on the tab. The shell title stays in `title` and comes back when this is cleared.
    label: Option<String>,
    agent: Option<Agent>,
    session: Session,
}

/// Appends bytes already read, then drains whatever else is waiting, up to 1 MiB.
/// `pending` keeps its allocation across reads.
fn fill_pending(fd: &std::os::fd::OwnedFd, buf: &mut [u8], first: usize, pending: &mut Vec<u8>) {
    pending.clear();
    pending.extend_from_slice(&buf[..first]);
    while pending.len() < 1024 * 1024 {
        let mut probe = [PollFd::new(fd.as_fd(), PollFlags::POLLIN)];
        match nix::poll::poll(&mut probe, PollTimeout::ZERO) {
            Ok(ready) if ready > 0 => {}
            _ => break,
        }
        match nix::unistd::read(fd, buf) {
            Ok(0) | Err(Errno::EAGAIN) | Err(Errno::EINTR) => break,
            Ok(more) => pending.extend_from_slice(&buf[..more]),
            Err(_) => break,
        }
    }
}

fn visible_title(tab: &Tab) -> String {
    if let Some(label) = &tab.label {
        label.clone()
    } else {
        tabs::display_title(&tab.title).to_string()
    }
}

#[derive(Clone, Copy)]
struct TabDrag {
    index: usize,
    origin_x: f64,
    armed: bool,
}

struct TabLabelEdit {
    index: usize,
    draft: tabs::LabelDraft,
}

/// Pause between blinks of the caret in a tab label.
const LABEL_BLINK: Duration = Duration::from_millis(530);

/// A cell as `(col, absolute line)`. The line survives scrolling, so a selection stays on its text.
type Point = (u16, u64);

/// Pause between steps of the scroll that follows a selection dragged past the grid.
const AUTOSCROLL_TICK: Duration = Duration::from_millis(50);

struct App {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    npu: Scheduler,
    proxy: EventLoopProxy<Wake>,
    window: Option<Arc<Window>>,
    gpu: Option<Renderer>,
    /// Compositor blur behind the window. Asked for the first time glass mode is on.
    blur: Option<Blur>,
    blur_tried: bool,
    /// The tab whose complete frame is currently on the GPU.
    presented_tab: Option<u64>,
    face: Face,
    atlas: Atlas,
    mods: ModifiersState,
    font_size: f32,
    vsync: bool,
    grid_left: u32,
    /// Space between the title bar and the first row.
    grid_pad_top: u32,
    grid_cols: u16,
    grid_rows: u16,
    bar_dirty: bool,
    bar: Bar,
    title_bar: TitleBar,
    last_title_click: Option<Instant>,
    settings_hot: bool,
    limits_hot: bool,
    limits_open: bool,
    limits_dirty: bool,
    limits_loading: bool,
    limits_selected: Option<usize>,
    limits_snap: limits::Snapshot,
    limits_popover: Option<Popover>,
    limits_fingerprint: limits::Fingerprint,
    next_limits_probe: Instant,
    last_limits_cmd: Option<Instant>,
    last_screen_capture: Option<(String, u8, Option<u8>)>,
    bar_hover: Option<tabs::BarHover>,
    tooltip_shown: bool,
    tooltip_at: Option<Instant>,
    tooltip_uploaded: bool,
    link_cell: Option<(u16, u16)>,
    hover_icon: CursorIcon,
    /// Ends of the selection as `(col, absolute line)`. See `Term::view_top`.
    selection: Option<(Point, Point)>,
    selecting: bool,
    press_moved: bool,
    press_cell: Point,
    anchor_cell: Point,
    /// Absolute line in the first row of the frame on screen.
    view_top: u64,
    /// Next step of the scroll that follows a selection dragged past the grid.
    autoscroll_at: Option<Instant>,
    /// Text under the finished selection. The selection stays while that text stays.
    selection_text: Option<String>,
    /// Output arrived since the selection text was last compared.
    selection_stale: bool,
    last_click: Option<(Instant, (u16, u16), u8)>,
    pending_link: Option<(Instant, String)>,
    selection_dirty: bool,
    clipboard: Clipboard,
    key_app_cursor: bool,
    key_alt: bool,
    key_bracketed: bool,
    panel_dirty: bool,
    panel: Option<Panel>,
    confirm_close: bool,
    confirm_dirty: bool,
    close_dialog: Option<Dialog>,
    exit_confirmed: bool,
    next_agent_scan: Instant,
    ui: Ui,
    /// `ui.conf` as last read, so a calibration shows up without a restart.
    glass_stamp: Option<SystemTime>,
    /// The glass controls inside the appearance panel, and how far that list is scrolled.
    glass_page: bool,
    glass_scroll: u16,
    /// The shortcut map, and how far that list is scrolled.
    keys_page: bool,
    keys_scroll: u16,
    desktop: TitlebarPrefs,
    cursor: Option<(f64, f64)>,
    wheel_frac: f32,
    scroll_drag: bool,
    /// Tab being reordered. `armed` is set once the pointer has moved enough to count as a drag.
    tab_drag: Option<TabDrag>,
    /// Label opened by a double-click on its tab.
    tab_edit: Option<TabLabelEdit>,
    last_tab_click: Option<(Instant, usize)>,
    label_blink_on: bool,
    label_blink_at: Option<Instant>,
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
                        // The corners are cut round, as on the desktop's own windows.
                        .with_transparent(true)
                        .with_inner_size(LogicalSize::new(1100.0, 700.0)),
                )
                .context("creating the window")?,
        );
        self.rescale(window.scale_factor());
        let mut gpu = Renderer::new(Arc::clone(&window), self.vsync)?;
        gpu.set_clear(color::theme(self.ui.style).bg);
        gpu.set_glass(self.ui.glass.then_some(self.ui.glass_tune.opacity));
        self.window = Some(window);
        self.gpu = Some(gpu);
        self.ensure_blur();
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
        let width = scrollbar.gutter_left();
        let padding = (12.0 * window.scale_factor()).round() as u32;
        self.grid_left = padding.min(width.saturating_sub(2 * cell_w) / 2);
        // The first row keeps the same distance from the title bar as the text from the left edge.
        self.grid_pad_top = padding.min(scrollbar.height.saturating_sub(cell_h) / 2);
        let cols = ((width - 2 * self.grid_left) / cell_w).max(2) as u16;
        let rows = ((scrollbar.height - self.grid_pad_top) / cell_h).max(1) as u16;
        self.grid_cols = cols;
        self.grid_rows = rows;
        self.bar_dirty = true;
        self.limits_dirty = true;
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
            label: None,
            agent: None,
            session,
        });
        if self.tabs.len() == 2 {
            // The tab bar appears and takes its rows from the grid.
            self.fit();
        }
        self.focus_tab(self.tabs.len() - 1);
        Ok(())
    }

    fn focus_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.tooltip_shown = false;
        self.tooltip_at = None;
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
        self.sync_window_title();
        self.redraw();
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tab_edit.as_ref().is_some_and(|edit| edit.index == index) {
            self.tab_edit = None;
            self.label_blink_at = None;
        } else if let Some(edit) = &mut self.tab_edit {
            if edit.index > index {
                edit.index -= 1;
            }
        }
        self.tabs.remove(index);
        self.bar_hover = None;
        self.tooltip_shown = false;
        self.tooltip_at = None;
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
        if self.tabs.len() == 1 {
            // The tab bar goes away and gives its rows back to the grid.
            self.commit_label_edit();
            self.fit();
        }
        let active = self.active;
        self.focus_tab(active);
    }

    fn refresh_agents(&mut self) {
        let mut changed = false;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let pty = &tab.session.pty;
            let agent = pty.foreground_group().and_then(|group| agent::foreground(group, pty.shell_pid()));
            if agent != tab.agent {
                tab.agent = agent;
                changed = true;
            }
            // The active title arrives with the published terminal frame.
            if index != self.active
                && let Ok(term) = tab.session.term.try_lock()
                && !term.title().is_empty()
                && term.title() != tab.title
            {
                tab.title = term.title().to_owned();
                changed = true;
            }
        }
        if changed {
            self.bar_dirty = true;
            self.redraw();
        }
        self.scan_screen_limits();
    }

    /// Passively captures on-screen quota panels (such as Antigravity `agy /usage`) across open
    /// tabs (including recent scrollback lines) and persists them to `~/.config/rustty/limits.json`.
    fn scan_screen_limits(&mut self) {
        if !self.ui.limits || self.tabs.is_empty() {
            return;
        }
        let now_unix = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let active = self.active.min(self.tabs.len() - 1);
        let mut found = None;
        for (idx, tab) in self.tabs.iter().enumerate() {
            if idx != active && tab.agent != Some(Agent::Antigravity) {
                continue;
            }
            let Ok(term) = tab.session.term.try_lock() else {
                continue;
            };
            let screen = term.recent_text(80);
            drop(term);
            if let Some(captured) = limits::parse_screen_limits(&screen, now_unix) {
                found = Some(captured);
                break;
            }
        }
        let Some(captured) = found else {
            return;
        };
        let key = (
            captured.id.clone(),
            captured.percent,
            captured.sub.as_ref().map(|sub| sub.percent),
        );
        if self.last_screen_capture.as_ref() == Some(&key) {
            return;
        }
        self.last_screen_capture = Some(key);
        limits::persist_captured(&captured);
        self.refresh_limits(false);
    }

    /// Picks up glass keys edited in `ui.conf` while the window is open.
    fn refresh_glass_tune(&mut self) {
        let stamp = settings::config_stamp();
        if stamp == self.glass_stamp {
            return;
        }
        self.glass_stamp = stamp;
        let Some(tune) = settings::read_glass_tune() else { return };
        if tune.text() == self.ui.glass_tune.text() {
            return;
        }
        let relayout = tune.bar != self.ui.glass_tune.bar || tune.bar_pad != self.ui.glass_tune.bar_pad;
        let opacity = tune.opacity;
        self.ui.glass_tune = tune;
        if !self.ui.glass {
            return;
        }
        if let Some(gpu) = &mut self.gpu {
            gpu.set_glass(Some(opacity));
        }
        if relayout {
            self.fit();
            for tab in &self.tabs {
                tab.session.update(|term| term.damage_all());
            }
        }
        self.bar_dirty = true;
        self.panel_dirty = true;
        self.confirm_dirty = true;
        self.redraw();
    }

    fn draw(&mut self) {
        if self.tabs.is_empty() || self.gpu.is_none() {
            return;
        }
        let active = self.active.min(self.tabs.len() - 1);
        self.active = active;
        let show_terminal = !self.ui.open && !self.confirm_close;
        let show_composer = show_terminal && self.tabs[active].agent == Some(Agent::Codex);
        let snap = self.tabs[active].session.take_frame();
        // Output elsewhere, such as an agent's spinner, keeps the selection. It goes only when
        // the selected text itself changes.
        self.selection_stale |= snap.as_ref().is_some_and(|snap| snap.content_changed);
        let cleared = self.selection_stale && !self.selecting && self.selection_changed(active);
        let paint_selection = self.selection_dirty || cleared;
        self.selection_dirty = false;
        if let Some(snap) = snap.as_ref().filter(|snap| !snap.suspended) {
            self.view_top = snap.view_top;
        }
        if let Some(snap) = &snap {
            if snap.title_changed && snap.title != self.tabs[active].title && !snap.title.is_empty() {
                self.tabs[active].title = snap.title.clone();
                self.bar_dirty = true;
                if self.tabs[active].label.is_none() && self.tab_edit.is_none() {
                    self.sync_window_title();
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
        let tab_bar_px = self.tab_bar_height();
        let tab_bar_h = tab_bar_px as f32;
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        gpu.set_insets(self.grid_left as f32, chrome_h, self.grid_pad_top as f32, tab_bar_h);
        gpu.set_corner_radius(corner_radius(window));
        gpu.set_clear(theme.bg);
        gpu.set_selection(selection);
        let tab_id = self.tabs[active].id;
        if let Some(snap) = snap.as_ref().filter(|snap| !snap.suspended) {
            gpu.upload(
                &mut self.atlas, &self.face, snap, show_composer, theme.accent, scrollbar.track(),
            );
            self.presented_tab = Some(tab_id);
        } else {
            gpu.refresh_cached(
                show_composer && self.presented_tab == Some(tab_id), theme.accent, scrollbar.track(),
            );
        }
        if self.bar_dirty {
            let titles: Vec<String> = self
                .tabs
                .iter()
                .map(|tab| tab.label.clone().unwrap_or_else(|| tab.title.clone()))
                .collect();
            let custom: Vec<bool> = self.tabs.iter().map(|tab| tab.label.is_some()).collect();
            let agents: Vec<Option<Agent>> = self.tabs.iter().map(|tab| tab.agent).collect();
            let edited = self.tab_edit.as_ref().map(|edit| {
                (edit.index, edit.draft.text.clone(), edit.draft.cursor, self.label_blink_on)
            });
            let cell_w = self.face.cell_w().max(1);
            let cols = (size.width / cell_w).clamp(1, 1024) as u16;
            let one_tab = self.tabs.len() <= 1;
            self.bar = if one_tab {
                Bar { cells: Vec::new(), spans: Vec::new(), new_tab: None, selected: 0 }
            } else {
                let track = self.ui.glass.then(|| {
                    let margin = (self.ui.glass_tune.margin * window.scale_factor() as f32).round();
                    glass_track_cols(cols, size.width, cell_w, margin)
                });
                let edit = edited.as_ref().map(|(index, text, cursor, on)| tabs::LabelEdit {
                    index: *index,
                    text,
                    cursor: *cursor,
                    cursor_on: *on,
                });
                tabs::render_bar_in(
                    &titles,
                    &agents,
                    active,
                    cols,
                    theme,
                    self.ui.glass,
                    track,
                    edit,
                    &custom,
                    self.bar_hover,
                )
            };
            let bottom_pad = (4.0 * window.scale_factor() as f32).round();
            gpu.upload_bar(&mut self.atlas, &self.face, &self.bar.cells, theme.bar, theme.bar, bottom_pad);
            let caption = if let Some((_, text, _, _)) = edited.as_ref().filter(|(index, ..)| *index == active) {
                text.clone()
            } else {
                visible_title(&self.tabs[active])
            };
            let tab = &self.tabs[active];
            self.title_bar = titlebar::render(
                &caption,
                tab.agent,
                cols,
                theme,
                window.is_maximized(),
                &self.desktop.buttons,
                true,
                self.ui.limits,
                self.ui.glass,
            );
            gpu.upload_title(&mut self.atlas, &self.face, &self.title_bar.cells, theme.bar);
            self.bar_dirty = false;
        }
        if let Some(blur) = &mut self.blur {
            // Sent before the present, whose commit applies the region.
            blur.sync(self.ui.glass, size.width, size.height, corner_radius(window), window.scale_factor());
        }
        if self.ui.open {
            let page = self.glass_page.then_some(settings::GlassPage {
                tune: self.ui.glass_tune,
                light: is_light(theme),
                scroll: self.glass_scroll,
            });
            let keys = self.keys_page.then_some(self.keys_scroll);
            self.panel = settings::render_panel(
                self.grid_cols,
                self.grid_rows,
                theme,
                self.ui.font_name(),
                self.ui.size,
                self.ui.style,
                self.ui.glass,
                self.ui.limits,
                page,
                keys,
            );
            if let Some(panel) = self.panel.as_mut().filter(|_| self.ui.glass) {
                for cell in &mut panel.cells {
                    cell.bg = theme.bg.pack();
                }
            }
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
        let scale = window.scale_factor();
        let tune = self.ui.glass_tune;
        let light = is_light(theme);
        let cell = (self.face.cell_w().max(1) as f32, self.face.cell_h().max(1) as f32);
        let origin = (self.grid_left as f32, (scrollbar.top + self.grid_pad_top) as f32);
        let mut tooltip_card: Option<[f32; 4]> = None;
        if draw_confirm || self.tab_edit.is_some() {
            self.tooltip_uploaded = false;
            self.close_dialog = if self.confirm_close {
                confirm::render(self.grid_cols, self.grid_rows, theme, self.tabs.len())
            } else if let Some(edit) = &self.tab_edit {
                confirm::render_rename(
                    self.grid_cols,
                    self.grid_rows,
                    theme,
                    &edit.draft.text,
                    edit.draft.cursor,
                    self.label_blink_on,
                )
            } else {
                None
            };
            if let Some(dialog) = self.close_dialog.as_mut().filter(|_| self.ui.glass) {
                dialog.set_background(theme.bg.pack());
            }
            gpu.upload_dialog(&mut self.atlas, &self.face, self.close_dialog.as_ref());
            self.confirm_dirty = false;
        } else if self.limits_dirty || self.limits_open {
            self.tooltip_uploaded = false;
            if self.limits_open {
                let anchor = (!self.title_bar.limits.is_empty()).then_some(self.title_bar.settings.end.max(self.title_bar.limits.end));
                self.limits_popover = limits::render_popover(
                    self.grid_cols,
                    self.grid_rows,
                    theme,
                    &self.limits_snap,
                    self.tabs[active].agent,
                    self.limits_selected,
                    self.limits_loading,
                    anchor,
                );
            } else {
                self.limits_popover = None;
            }
            gpu.upload_popover(&mut self.atlas, &self.face, self.limits_popover.as_ref());
            self.limits_dirty = false;
        } else if !self.ui.open
            && !self.confirm_close
            && self.tab_edit.is_none()
            && self.tab_drag.is_none()
            && self.tooltip_shown
            && let Some(tabs::BarHover::Tab { tab, .. }) = self.bar_hover
            && let Some(span) = self.bar.spans.get(tab)
            && let Some(tab_state) = self.tabs.get(tab)
        {
            let full_title = visible_title(tab_state);
            let max_cols = ((size.width / self.face.cell_w().max(1)).saturating_sub(4)).clamp(1, 72) as u16;
            if let Some(tooltip) = tabs::render_tooltip(&full_title, max_cols, span.start, theme) {
                let (rect, tx, ty) = tooltip_layout(&tooltip, size, tab_bar_px, cell, scale);
                gpu.upload_tooltip(&mut self.atlas, &self.face, Some(&tooltip), tx, ty);
                self.tooltip_uploaded = true;
                tooltip_card = Some(rect);
            } else if self.tooltip_uploaded {
                gpu.upload_tooltip(&mut self.atlas, &self.face, None, 0.0, 0.0);
                self.tooltip_uploaded = false;
            }
        } else if self.tooltip_uploaded {
            gpu.upload_tooltip(&mut self.atlas, &self.face, None, 0.0, 0.0);
            self.tooltip_uploaded = false;
        }
        let limits_health = self.limits_snap.health();
        let mut shapes = Vec::with_capacity(16);
        push_tab_bar_shapes(
            &mut shapes,
            &self.bar,
            self.bar_hover,
            size,
            tab_bar_px,
            self.face.cell_w().max(1),
            scale,
            theme,
            self.ui.glass,
            light,
        );
        if self.ui.glass {
            push_glass_title_shapes(
                &mut shapes,
                &self.title_bar,
                scrollbar.top,
                self.face.cell_w().max(1),
                scale,
                self.settings_hot || self.ui.open,
                self.limits_hot || self.limits_open,
                limits_health,
                window.is_maximized(),
                &tune,
            );
        }
        if self.ui.glass
            && let Some(panel) = &self.panel
        {
            // The panel replaces the grid cells it covers, so its sheet goes under the grid.
            let rect = (panel.origin_col, panel.origin_row, panel.cols, panel.rows);
            shapes.push(sheet(rect, origin, cell, 0.0, scale, theme, tune.sheet, tune.radius));
        }
        let overlay = shapes.len();
        if !self.ui.glass {
            // In opaque mode, draw both the AI limits layers and settings gear as matched,
            // centered vector icons on top of the title bar with the same optical size.
            if !self.title_bar.limits.is_empty() {
                shapes.push(opaque_title_icon(
                    &self.title_bar.limits,
                    scrollbar.top,
                    self.face.cell_w().max(1),
                    scale,
                    self.limits_hot || self.limits_open,
                    limits_icon(limits_health),
                    None,
                ));
            }
            if !self.title_bar.settings.is_empty() {
                shapes.push(opaque_title_icon(
                    &self.title_bar.settings,
                    scrollbar.top,
                    self.face.cell_w().max(1),
                    scale,
                    self.settings_hot || self.ui.open,
                    crate::gpu::ICON_GEAR,
                    Some((theme.fg, 0.86)),
                ));
            }
        }
        if self.ui.glass
            && let Some(dialog) = self.close_dialog.as_ref().filter(|_| self.confirm_close || self.tab_edit.is_some())
        {
            let rect = (dialog.origin_col, dialog.origin_row, dialog.cols, dialog.rows);
            let pad = (f64::from(tune.dialog_pad) * scale) as f32;
            shapes.push(sheet(rect, origin, cell, pad, scale, theme, tune.dialog, tune.radius));
            let inset = (f64::from(tune.button_inset) * scale).round() as f32;
            for (cols, rows, choice) in dialog.buttons() {
                let x = origin.0 + f32::from(dialog.origin_col + cols.start) * cell.0 + inset;
                let y = origin.1 + f32::from(dialog.origin_row + rows.start) * cell.1 + inset;
                let w = f32::from(cols.end - cols.start) * cell.0 - 2.0 * inset;
                let h = f32::from(rows.end - rows.start) * cell.1 - 2.0 * inset;
                let button = GlassShape::new(x, y, w, h, GLASS_BUTTON);
                shapes.push(match choice {
                    confirm::Choice::Close => button.with_tint(theme.danger, tune.danger),
                    confirm::Choice::Cancel => button,
                });
            }
        } else if self.limits_open
            && let Some(popover) = &self.limits_popover
        {
            push_limits_shapes(&mut shapes, popover, origin, cell, scale, theme, self.ui.glass, light, &tune);
        } else if let Some(rect) = tooltip_card {
            let (card_rgb, card_alpha) = if self.ui.glass && !light {
                (crate::color::Rgb::new(0x14, 0x17, 0x1E), tune.dialog.max(0.92))
            } else if self.ui.glass {
                (theme.panel, tune.dialog.max(0.94))
            } else {
                (theme.panel, 1.0)
            };
            shapes.push(
                GlassShape::new(rect[0], rect[1], rect[2], rect[3], GLASS_TOOLTIP)
                    .with_radius(8.0 * scale as f32)
                    .with_tint(card_rgb, card_alpha),
            );
        }
        gpu.set_glass_shapes(&shapes, overlay, scale as f32, light, &tune);
        if let Err(err) = gpu.render(show_terminal && self.presented_tab == Some(tab_id)) {
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
        self.close_limits();
        self.panel_dirty = true;
        self.bar_dirty = true;
        self.redraw();
    }

    fn toggle_limits(&mut self) {
        if !self.ui.limits {
            return;
        }
        if self.limits_open {
            self.close_limits();
            return;
        }
        self.close_settings();
        self.scan_screen_limits();
        self.limits_open = true;
        self.limits_dirty = true;
        self.limits_selected = None;
        self.bar_dirty = true;
        self.refresh_limits(false);
        self.redraw();
    }

    fn close_limits(&mut self) {
        if !self.limits_open && self.limits_popover.is_none() {
            return;
        }
        self.limits_open = false;
        self.limits_popover = None;
        self.limits_selected = None;
        self.limits_dirty = true;
        self.bar_dirty = true;
        self.redraw();
    }

    fn refresh_limits(&mut self, force_cmd: bool) {
        if self.limits_loading {
            return;
        }
        let allow_cmd = self.ui.limits_cmd.is_some()
            && (force_cmd
                || self
                    .last_limits_cmd
                    .is_none_or(|at| at.elapsed() >= limits::cmd_cooldown()));
        if allow_cmd {
            self.last_limits_cmd = Some(Instant::now());
        }
        self.limits_loading = true;
        if self.limits_open {
            self.limits_dirty = true;
        }
        let cmd = self.ui.limits_cmd.clone();
        let proxy = self.proxy.clone();
        std::thread::Builder::new()
            .name("rustty-limits".into())
            .spawn(move || {
                let snap = limits::collect(cmd.as_deref(), allow_cmd);
                let _ = proxy.send_event(Wake::Limits(snap));
            })
            .ok();
    }

    fn toggle_limits_addon(&mut self) {
        self.ui.limits = !self.ui.limits;
        if !self.ui.limits {
            self.close_limits();
        } else {
            self.refresh_limits(false);
        }
        self.bar_dirty = true;
        self.panel_dirty = true;
        self.ui.save();
        self.redraw();
    }

    fn ask_close(&mut self) {
        self.close_settings();
        self.close_limits();
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
        self.glass_page = false;
        self.keys_page = false;
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
        self.limits_dirty = true;
        self.ui.save();
        self.redraw();
    }

    /// Glass mode on or off. Off draws exactly the opaque window of before.
    fn toggle_glass(&mut self) {
        self.ui.glass = !self.ui.glass;
        let opacity = self.ui.glass.then_some(self.ui.glass_tune.opacity);
        if let Some(gpu) = &mut self.gpu {
            gpu.set_glass(opacity);
            if self.ui.glass && !gpu.supports_glass() {
                tracing::warn!("the surface is not premultiplied, glass mode has no effect");
            }
        }
        self.ensure_blur();
        // The bars grow in glass mode to hold the capsules, so the grid is measured again.
        self.fit();
        for tab in &self.tabs {
            tab.session.update(|term| term.damage_all());
        }
        self.bar_dirty = true;
        self.panel_dirty = true;
        self.limits_dirty = true;
        self.ui.save();
        self.redraw();
    }

    fn ensure_blur(&mut self) {
        if !self.ui.glass || self.blur_tried {
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        self.blur_tried = true;
        self.blur = Blur::new(window);
        if self.blur.as_ref().is_some_and(|blur| !blur.supported()) {
            tracing::info!("the compositor offers background effects but not blur");
        }
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
            PanelHit::Glass => self.toggle_glass(),
            PanelHit::Limits => self.toggle_limits_addon(),
            PanelHit::Style(index) => self.apply_style(index),
            PanelHit::GlassLook => {
                self.glass_page = true;
                self.glass_scroll = 0;
                self.panel_dirty = true;
                self.redraw();
            }
            PanelHit::GlassBack => {
                self.glass_page = false;
                self.panel_dirty = true;
                self.redraw();
            }
            PanelHit::GlassNudge(control, dir) => self.nudge_glass(control, i32::from(dir)),
            PanelHit::GlassScroll(delta) => self.scroll_glass(delta),
            PanelHit::Keys => {
                self.keys_page = true;
                self.keys_scroll = 0;
                self.panel_dirty = true;
                self.redraw();
            }
            PanelHit::KeysBack => {
                self.keys_page = false;
                self.panel_dirty = true;
                self.redraw();
            }
            PanelHit::KeysScroll(delta) => self.scroll_keys(delta),
            PanelHit::GlassReset => {
                let bars = (self.ui.glass_tune.bar, self.ui.glass_tune.bar_pad);
                self.ui.glass_tune = settings::GlassTune::default();
                self.apply_glass_tune((self.ui.glass_tune.bar, self.ui.glass_tune.bar_pad) != bars);
            }
        }
    }

    fn nudge_glass(&mut self, control: settings::GlassControl, dir: i32) {
        let light = is_light(self.theme());
        let bars = (self.ui.glass_tune.bar, self.ui.glass_tune.bar_pad);
        self.ui.glass_tune.nudge(control, dir, light);
        self.apply_glass_tune((self.ui.glass_tune.bar, self.ui.glass_tune.bar_pad) != bars);
    }

    fn apply_glass_tune(&mut self, relayout: bool) {
        if self.ui.glass {
            if let Some(gpu) = &mut self.gpu {
                gpu.set_glass(Some(self.ui.glass_tune.opacity));
            }
            if relayout {
                self.fit();
                for tab in &self.tabs {
                    tab.session.update(|term| term.damage_all());
                }
            }
            self.bar_dirty = true;
        }
        self.panel_dirty = true;
        self.ui.save();
        self.glass_stamp = settings::config_stamp();
        self.redraw();
    }

    fn scroll_glass(&mut self, delta: i16) {
        let max = settings::glass_scroll_max(self.grid_rows);
        let next = (i32::from(self.glass_scroll) + i32::from(delta)).clamp(0, i32::from(max)) as u16;
        if next == self.glass_scroll {
            return;
        }
        self.glass_scroll = next;
        self.panel_dirty = true;
        self.redraw();
    }

    fn scroll_keys(&mut self, delta: i16) {
        let max = settings::keys_scroll_max(self.grid_rows);
        let next = (i32::from(self.keys_scroll) + i32::from(delta)).clamp(0, i32::from(max)) as u16;
        if next == self.keys_scroll {
            return;
        }
        self.keys_scroll = next;
        self.panel_dirty = true;
        self.redraw();
    }

    fn wheel_on_panel(&self) -> bool {
        let Some((x, y)) = self.cursor else { return false };
        let Some((col, row)) = self.cell_at(x, y) else { return false };
        self.panel.as_ref().is_some_and(|panel| panel.contains(col, row))
    }

    fn on_key(&mut self, event: &winit::event::KeyEvent) {
        if self.tabs.is_empty() || event.state == ElementState::Released {
            return;
        }
        if self.tab_edit.is_some() {
            self.on_label_key(event);
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
        if self.limits_open {
            match event.logical_key.as_ref() {
                Key::Named(NamedKey::Escape) => {
                    self.close_limits();
                    return;
                }
                Key::Character("r") | Key::Character("R") => {
                    self.refresh_limits(true);
                    self.redraw();
                    return;
                }
                _ => {}
            }
        }
        if self.ui.open && matches!(event.logical_key, Key::Named(winit::keyboard::NamedKey::Escape)) {
            if self.keys_page {
                self.keys_page = false;
            } else if self.glass_page {
                self.glass_page = false;
            } else {
                self.close_settings();
                return;
            }
            self.panel_dirty = true;
            self.redraw();
            return;
        }
        if self.selection.is_some() && matches!(event.logical_key, Key::Named(winit::keyboard::NamedKey::Escape)) {
            self.clear_selection();
            return;
        }
        if self.mods.control_key()
            && self.mods.alt_key()
            && self.mods.shift_key()
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyW))
        {
            self.start_label_edit(self.active.min(self.tabs.len() - 1));
            return;
        }
        if self.mods.control_key()
            && self.mods.shift_key()
            && !self.mods.alt_key()
            && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyL))
        {
            self.toggle_limits();
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
            Some(Input::RenameTab) => self.start_label_edit(active),
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
        if self.ui.open && self.wheel_on_panel() {
            let delta = -(steps.clamp(-32, 32) as i16);
            if self.keys_page {
                self.scroll_keys(delta);
                return;
            }
            if self.glass_page {
                self.scroll_glass(delta);
                return;
            }
        }
        let (col, row, pixel) = self
            .cursor
            .and_then(|(x, y)| {
                let (col, row) = self.cell_at(x, y)?;
                let grid_x = x - f64::from(self.grid_left);
                let grid_y = y - f64::from(self.grid_top());
                Some((col + 1, row + 1, (grid_x as u32 + 1, grid_y as u32 + 1)))
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
            if let Err(err) = self.tabs[active].session.pty.write_all(&bytes) {
                tracing::warn!(%err, "pty write");
            }
            return;
        }
        self.redraw();
    }

    fn scrollbar(&self, window: &Window, size: PhysicalSize<u32>) -> Scrollbar {
        Scrollbar::new(size, self.chrome_height(), self.tab_bar_height(), window.scale_factor(), window.is_maximized())
    }

    fn try_scroll_drag(&mut self) -> bool {
        if self.ui.open || self.limits_open || self.tabs.is_empty() {
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
            let height = term.scroll_metrics().1;
            if height <= 0.0 || height >= 1.0 {
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

    fn on_bar_press(&mut self) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        let Some((x, y)) = self.cursor else {
            return false;
        };
        if !self.is_bar_y(y) {
            return false;
        }
        let col = strip_col(x, self.face.cell_w(), self.bar.cells.len());
        let Some(hit) = tabs::hit(&self.bar, col) else {
            return false;
        };
        let BarHit::Select(index) = hit else {
            self.last_tab_click = None;
            return false;
        };
        if self.note_tab_click(index) {
            self.focus_tab(index);
            self.start_label_edit(index);
            return true;
        }
        if self.ui.open || self.limits_open {
            return false;
        }
        self.tooltip_shown = false;
        self.tooltip_at = None;
        self.focus_tab(index);
        self.tab_drag = Some(TabDrag { index, origin_x: x, armed: false });
        true
    }

    fn note_tab_click(&mut self, index: usize) -> bool {
        let now = Instant::now();
        let double = self.last_tab_click.is_some_and(|(at, previous)| {
            previous == index && now.saturating_duration_since(at) < self.desktop.double_click_time
        });
        if double {
            self.last_tab_click = None;
        } else {
            self.last_tab_click = Some((now, index));
        }
        double
    }

    fn start_label_edit(&mut self, index: usize) {
        self.close_limits();
        let Some(tab) = self.tabs.get(index) else { return };
        let text = visible_title(tab);
        self.tab_drag = None;
        self.tab_edit = Some(TabLabelEdit { index, draft: tabs::LabelDraft::new(text) });
        self.confirm_dirty = true;
        self.poke_label_cursor();
    }

    fn hovering_label_edit(&self, x: f64, y: f64) -> bool {
        let Some(edit) = &self.tab_edit else { return false };
        if !self.is_bar_y(y) {
            return false;
        }
        let col = strip_col(x, self.face.cell_w(), self.bar.cells.len());
        matches!(tabs::hit(&self.bar, col), Some(BarHit::Select(index)) if index == edit.index)
    }

    fn rename_click(&mut self) -> bool {
        let Some((x, y)) = self.cursor else { return false };
        let geometry = self.close_dialog.as_ref().map(|dialog| {
            (dialog.field, dialog.origin_col, dialog.origin_row, dialog.cols, dialog.rows)
        });
        let Some((field, origin_col, origin_row, cols, rows)) = geometry else { return false };
        if self.is_title_y(y) || self.is_bar_y(y) {
            return false;
        }
        let Some((col, row)) = self.cell_at(x, y) else { return false };
        let Some(local_col) = col.checked_sub(origin_col) else { return false };
        let Some(local_row) = row.checked_sub(origin_row) else { return false };
        if local_col >= cols || local_row >= rows {
            return false;
        }
        if let Some((field_col, field_row, field_cols)) = field {
            if local_row == field_row && (field_col..field_col + field_cols).contains(&local_col) {
                let offset = usize::from(local_col - field_col);
                let width = usize::from(field_cols);
                if let Some(edit) = &mut self.tab_edit {
                    edit.draft.click(width, offset);
                }
                self.poke_label_cursor();
            }
        }
        true
    }

    fn place_label_cursor(&mut self) -> bool {
        let Some(edit) = &self.tab_edit else { return false };
        let index = edit.index;
        let Some((x, y)) = self.cursor else { return false };
        if !self.is_bar_y(y) {
            return false;
        }
        let col = strip_col(x, self.face.cell_w(), self.bar.cells.len());
        if !matches!(tabs::hit(&self.bar, col), Some(BarHit::Select(hit)) if hit == index) {
            return false;
        }
        let Some(span) = self.bar.spans.get(index) else { return false };
        let width = usize::from(span.label_cols);
        let offset = usize::from(col.saturating_sub(span.label_at)).min(width);
        if let Some(edit) = &mut self.tab_edit {
            edit.draft.click(width, offset);
        }
        self.poke_label_cursor();
        true
    }

    fn on_label_key(&mut self, event: &winit::event::KeyEvent) {
        let ctrl = self.mods.control_key();
        let word = ctrl || self.mods.alt_key();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.cancel_label_edit();
                return;
            }
            Key::Named(NamedKey::Enter) => {
                self.commit_label_edit();
                return;
            }
            Key::Named(NamedKey::Backspace) => self.edit_draft(|draft| draft.backspace(word)),
            Key::Named(NamedKey::Delete) => self.edit_draft(|draft| draft.delete_forward(word)),
            Key::Named(NamedKey::ArrowLeft) => self.edit_draft(|draft| draft.left(word)),
            Key::Named(NamedKey::ArrowRight) => self.edit_draft(|draft| draft.right(word)),
            Key::Named(NamedKey::Home) => self.edit_draft(tabs::LabelDraft::home),
            Key::Named(NamedKey::End) => self.edit_draft(tabs::LabelDraft::end),
            _ if self.mods.super_key() => return,
            _ if label_paste(event, self.mods) => {
                self.paste_label();
                return;
            }
            _ if ctrl => return,
            _ => {
                let Some(text) = event.text.as_deref() else { return };
                let text = text.to_string();
                self.edit_draft(|draft| draft.insert(&text));
            }
        }
        self.poke_label_cursor();
    }

    fn edit_draft(&mut self, edit: impl FnOnce(&mut tabs::LabelDraft)) {
        if let Some(label) = &mut self.tab_edit {
            edit(&mut label.draft);
        }
    }

    fn paste_label(&mut self) {
        let Some(text) = self.clipboard.get(false) else { return };
        self.edit_draft(|draft| draft.insert(&text));
        self.poke_label_cursor();
    }

    fn commit_label_edit(&mut self) {
        let Some(edit) = self.tab_edit.take() else { return };
        if let Some(tab) = self.tabs.get_mut(edit.index) {
            tab.label = tabs::committed_label(&edit.draft.text, &tab.title);
        }
        self.finish_label_edit();
    }

    fn cancel_label_edit(&mut self) {
        if self.tab_edit.take().is_some() {
            self.finish_label_edit();
        }
    }

    fn finish_label_edit(&mut self) {
        self.label_blink_at = None;
        self.confirm_dirty = true;
        if self.hover_icon == CursorIcon::Text {
            self.hover_icon = CursorIcon::Default;
            if let Some(window) = &self.window {
                window.set_cursor(CursorIcon::Default);
            }
        }
        self.bar_dirty = true;
        self.sync_window_title();
        self.redraw();
    }

    fn poke_label_cursor(&mut self) {
        self.label_blink_on = true;
        self.label_blink_at = Some(Instant::now() + LABEL_BLINK);
        self.bar_dirty = true;
        self.redraw();
    }

    fn sync_window_title(&self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let Some(window) = &self.window else { return };
        window.set_title(tab.label.as_deref().unwrap_or(&tab.title));
    }

    fn drag_tab(&mut self, x: f64) {
        let Some(mut drag) = self.tab_drag else {
            return;
        };
        if !drag.armed {
            let scale = self.window.as_ref().map(|window| window.scale_factor()).unwrap_or(1.0);
            let slop = (8.0 * scale).clamp(6.0, 24.0);
            if (x - drag.origin_x).abs() < slop {
                return;
            }
            drag.armed = true;
            self.tab_drag = Some(drag);
            self.hover_icon = CursorIcon::Grabbing;
            if let Some(window) = &self.window {
                window.set_cursor(CursorIcon::Grabbing);
            }
        }
        let Some(target) = self.tab_drop_index(x) else {
            return;
        };
        if target != drag.index {
            self.move_tab(drag.index, target);
        }
    }

    fn retarget_label_edit(&mut self, from: usize, to: usize) {
        let Some(edit) = &mut self.tab_edit else { return };
        let index = edit.index;
        edit.index = if index == from {
            to
        } else if from < index && index <= to {
            index - 1
        } else if to <= index && index < from {
            index + 1
        } else {
            index
        };
    }

    fn tab_drop_index(&self, x: f64) -> Option<usize> {
        let col = strip_col(x, self.face.cell_w(), self.bar.cells.len());
        tabs::drop_index(&self.bar, col)
    }

    fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = to;
        if let Some(drag) = &mut self.tab_drag {
            drag.index = to;
        }
        self.retarget_label_edit(from, to);
        self.last_tab_click = None;
        for (index, tab) in self.tabs.iter().enumerate() {
            tab.session.visible.store(index == to, Ordering::Relaxed);
        }
        self.bar_dirty = true;
        self.redraw();
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
            )
        });
        if let Some((app_cursor, alt, rows, bracketed)) = fresh {
            self.key_app_cursor = app_cursor;
            self.key_alt = alt;
            self.key_bracketed = bracketed;
            (app_cursor, alt, rows)
        } else {
            (self.key_app_cursor, self.key_alt, self.grid_rows)
        }
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
        let col = ((x - f64::from(self.grid_left)) / cell_w).floor();
        let row = ((y - f64::from(self.grid_top())) / cell_h).floor();
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

    /// The selection in the frame's cells, as a half-open range of row-major indices.
    fn selection_span(&self) -> (u32, u32) {
        let Some((anchor, focus)) = self.selection else {
            return (0, 0);
        };
        let (lo, hi) = ordered(anchor, focus);
        let cols = u64::from(self.grid_cols.max(1));
        let rows = u64::from(self.grid_rows.max(1));
        let top = self.view_top;
        if hi.1 < top || lo.1 >= top + rows {
            return (0, 0);
        }
        let last_col = u64::from(self.grid_cols.saturating_sub(1));
        let start = if lo.1 < top { 0 } else { (lo.1 - top) * cols + u64::from(lo.0).min(last_col) };
        let end = if hi.1 >= top + rows {
            rows * cols
        } else {
            (hi.1 - top) * cols + u64::from(hi.0).min(last_col) + 1
        };
        (start as u32, end as u32)
    }

    /// The cell under the pointer, held to the grid's edge when the pointer is past it.
    fn clamped_cell(&self, x: f64, y: f64) -> (u16, u16) {
        let cell_w = self.face.cell_w().max(1) as f64;
        let cell_h = self.face.cell_h().max(1) as f64;
        let col = ((x - f64::from(self.grid_left)) / cell_w).floor();
        let row = ((y - f64::from(self.grid_top())) / cell_h).floor();
        let col = col.clamp(0.0, f64::from(self.grid_cols.saturating_sub(1))) as u16;
        let row = row.clamp(0.0, f64::from(self.grid_rows.saturating_sub(1))) as u16;
        (col, row)
    }

    fn point(&self, cell: (u16, u16)) -> Point {
        (cell.0, self.view_top + u64::from(cell.1))
    }

    /// Lines to scroll while a selection is dragged above or below the grid. Positive reveals
    /// older lines. The farther the pointer is from the edge, the faster the view moves.
    fn autoscroll_lines(&self) -> isize {
        let Some((_, y)) = self.cursor.filter(|_| self.selecting) else {
            return 0;
        };
        let cell_h = f64::from(self.face.cell_h().max(1));
        let top = f64::from(self.grid_top());
        let bottom = top + f64::from(self.grid_rows) * cell_h;
        let lines = |distance: f64| (1.0 + distance / cell_h).min(8.0) as isize;
        if y < top {
            lines(top - y)
        } else if y >= bottom {
            -lines(y - bottom)
        } else {
            0
        }
    }

    /// One step of the scroll that follows the selection. Returns false when neither this
    /// tab nor the program can move any further that way.
    fn autoscroll_step(&mut self) -> bool {
        let lines = self.autoscroll_lines();
        if lines == 0 || self.tabs.is_empty() {
            return false;
        }
        let active = self.active.min(self.tabs.len() - 1);
        let (col, row, pixel) = self.autoscroll_pointer();
        match self.tabs[active].session.follow_selection(lines, col, row, pixel) {
            Follow::Program(bytes) => {
                if let Err(err) = self.tabs[active].session.pty.write_all(&bytes) {
                    tracing::warn!(%err, "pty write");
                    return false;
                }
                true
            }
            Follow::History(Some(top)) => {
                self.view_top = top;
                if let Some((x, y)) = self.cursor {
                    self.extend_selection(x, y);
                }
                self.redraw();
                true
            }
            Follow::History(None) => false,
        }
    }

    /// Cell at the grid edge the pointer has crossed, in the same coordinates as a wheel event.
    fn autoscroll_pointer(&self) -> (u16, u16, (u32, u32)) {
        let (x, y) = self.cursor.unwrap_or((0.0, 0.0));
        let (col, row) = self.clamped_cell(x, y);
        let cell_w = self.face.cell_w().max(1) as f64;
        let cell_h = self.face.cell_h().max(1) as f64;
        let width = f64::from(self.grid_cols.max(1)) * cell_w;
        let height = f64::from(self.grid_rows.max(1)) * cell_h;
        let grid_x = (x - f64::from(self.grid_left)).clamp(0.0, width - 1.0);
        let grid_y = (y - f64::from(self.grid_top())).clamp(0.0, height - 1.0);
        (col.saturating_add(1), row.saturating_add(1), ((grid_x as u32) + 1, (grid_y as u32) + 1))
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
        if self.tab_edit.is_some() && !self.rename_click() && !self.place_label_cursor() {
            self.cancel_label_edit();
        }
        if self.try_scroll_drag() || self.on_bar_press() || self.begin_selection() {
            return;
        }
        self.on_click();
    }

    fn end_pointer(&mut self) {
        self.scroll_drag = false;
        if self.tab_drag.take().is_some() {
            self.hover_icon = CursorIcon::Default;
            if let Some(window) = &self.window {
                window.set_cursor(CursorIcon::Default);
            }
        }
        self.autoscroll_at = None;
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
        if self.ui.open || self.limits_open || self.tabs.is_empty() {
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
        self.autoscroll_at = None;
        let point = self.point(cell);
        self.press_cell = point;
        self.anchor_cell = point;
        self.press_moved = false;
        let count = self.note_click(cell);
        if self.mods.shift_key() {
            let anchor = self.selection.map(|(anchor, _)| anchor).unwrap_or(point);
            self.anchor_cell = anchor;
            self.selection = Some((anchor, point));
            self.selection_text = None;
            self.press_moved = point != anchor;
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
            let span = (self.point(span.0), self.point(span.1));
            self.anchor_cell = span.0;
            self.selection = Some(span);
            self.selection_text = None;
            self.press_moved = true;
            self.selection_dirty = true;
            self.redraw();
        }
        true
    }

    fn extend_selection(&mut self, x: f64, y: f64) {
        let cell = self.point(self.clamped_cell(x, y));
        if cell == self.press_cell {
            return;
        }
        let anchor = self.selection.map(|(anchor, _)| anchor).unwrap_or(self.anchor_cell);
        if self.selection == Some((anchor, cell)) {
            return;
        }
        self.selection = Some((anchor, cell));
        self.selection_text = None;
        self.press_moved = true;
        self.selection_dirty = true;
        self.redraw();
    }

    /// Compares the text under the selection with the text it had. Drops the selection when
    /// it differs. A parser holding the grid only postpones the check to the next frame.
    fn selection_changed(&mut self, active: usize) -> bool {
        let Some((anchor, focus)) = self.selection else {
            self.selection_stale = false;
            return false;
        };
        let Ok(term) = self.tabs[active].session.term.try_lock() else {
            return false;
        };
        let text = term.selected_text(anchor, focus);
        drop(term);
        self.selection_stale = false;
        match &self.selection_text {
            Some(kept) if *kept != text => {
                self.selection = None;
                self.selection_text = None;
                true
            }
            Some(_) => false,
            None => {
                self.selection_text = Some(text);
                false
            }
        }
    }

    fn clear_selection(&mut self) {
        self.selection_text = None;
        self.selecting = false;
        self.autoscroll_at = None;
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
        // A busy parser holds the grid only for one chunk. Waiting for it beats losing the copy.
        let text = {
            let term = self.tabs[active].session.term.lock().unwrap_or_else(|err| err.into_inner());
            term.selected_text(anchor, focus)
        };
        self.selection_text = Some(text.clone());
        if text.is_empty() {
            return;
        }
        self.clipboard.set(&text, true);
    }

    fn paste(&mut self, primary: bool) {
        if self.tabs.is_empty() || self.ui.open || self.limits_open || self.confirm_close {
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
            self.tooltip_shown = false;
            self.tooltip_at = None;
            match tabs::hit(&self.bar, col) {
                Some(BarHit::Select(index)) => self.focus_tab(index),
                Some(BarHit::Close(index)) => self.close_tab(index),
                Some(BarHit::NewTab) => {
                    if let Err(err) = self.open_tab() {
                        tracing::warn!(%err, "new tab");
                    }
                }
                None => {}
            }
            return;
        }
        if !self.confirm_close && !self.ui.open && !self.limits_open {
                if let Some(target) = self.link_at(x, y) {
                    open_target(&target);
                    return;
                }
        }
        if self.limits_open {
            if let Some((col, row)) = self.cell_at(x, y)
                && let Some(popover) = &self.limits_popover
            {
                if let Some(hit) = popover.hit(col, row) {
                    match hit {
                        PopoverHit::Refresh => {
                            self.refresh_limits(true);
                            self.redraw();
                        }
                        PopoverHit::Row(idx) => {
                            self.limits_selected = if self.limits_selected == Some(idx) { None } else { Some(idx) };
                            self.limits_dirty = true;
                            self.redraw();
                        }
                    }
                    return;
                }
                if popover.contains(col, row) {
                    return;
                }
            }
            self.close_limits();
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
            TitleHit::Settings => self.toggle_settings(),
            TitleHit::Limits => self.toggle_limits(),
            TitleHit::Minimize => window.set_minimized(true),
            TitleHit::Maximize => {
                window.set_maximized(!window.is_maximized());
                self.bar_dirty = true;
                self.redraw();
            }
            TitleHit::Drag => {
                let now = Instant::now();
                let limit = self.desktop.double_click_time;
                let double = self
                    .last_title_click
                    .is_some_and(|at| now.saturating_duration_since(at) < limit);
                if double {
                    self.last_title_click = None;
                    self.title_action(self.desktop.double_click);
                } else {
                    self.last_title_click = Some(now);
                    let _ = window.drag_window();
                }
            }
        }
    }

    /// What the desktop asks a click on the title bar to do.
    fn title_action(&mut self, action: TitleAction) {
        let Some(window) = self.window.clone() else {
            return;
        };
        match action {
            TitleAction::ToggleMaximize => {
                window.set_maximized(!window.is_maximized());
                self.bar_dirty = true;
                self.redraw();
            }
            TitleAction::Minimize => window.set_minimized(true),
            TitleAction::Menu => {
                let (x, y) = self.cursor.unwrap_or((0.0, 0.0));
                window.show_window_menu(PhysicalPosition::new(x, y));
            }
            TitleAction::None => {}
        }
    }

    /// A middle or right click on the title bar. Returns false when the click is elsewhere.
    fn title_click(&mut self, button: MouseButton) -> bool {
        let on_title = self.cursor.is_some_and(|(x, y)| {
            self.is_title_y(y)
                && titlebar::hit(&self.title_bar, strip_col(x, self.face.cell_w(), self.title_bar.cells.len()))
                    == TitleHit::Drag
        });
        if !on_title || self.ui.open || self.limits_open || self.confirm_close {
            return false;
        }
        let action = match button {
            MouseButton::Middle => self.desktop.middle_click,
            MouseButton::Right => self.desktop.right_click,
            _ => return false,
        };
        self.title_action(action);
        true
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
        let bar_h = self.tab_bar_height();
        bar_h > 0 && bar_contains_y(window.inner_size().height, bar_h, y)
    }

    /// The tab bar shows from the second tab on. With one tab its subject is in the title bar.
    fn tab_bar_height(&self) -> u32 {
        if self.tabs.len() > 1 { self.chrome_height() } else { 0 }
    }

    /// Top of the first row: the title bar and the space below it.
    fn grid_top(&self) -> u32 {
        self.chrome_height() + self.grid_pad_top
    }

    fn chrome_height(&self) -> u32 {
        let scale = self.window.as_ref().map(|window| window.scale_factor()).unwrap_or(1.0);
        if self.ui.glass {
            glass_bar_height(scale, self.face.cell_h(), &self.ui.glass_tune)
        } else {
            bar_height(scale, self.face.cell_h())
        }
    }
}

fn ordered(a: Point, b: Point) -> (Point, Point) {
    if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) }
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

/// Glass bars leave room around the text for the capsule and the pill inside it.
fn glass_bar_height(scale: f64, cell_h: u32, tune: &crate::settings::GlassTune) -> u32 {
    let bar = (f64::from(tune.bar) * scale).ceil() as u32;
    let pad = (f64::from(tune.bar_pad) * scale).round() as u32;
    bar.max(cell_h.max(1).saturating_add(pad))
}

fn is_light(theme: Theme) -> bool {
    let luma = 0.2126 * f32::from(theme.bg.r) + 0.7152 * f32::from(theme.bg.g) + 0.0722 * f32::from(theme.bg.b);
    luma > 128.0
}

/// Columns under the glass capsule, so the labels split the same interval the pill does.
fn glass_track_cols(cols: u16, width: u32, cell_w: u32, margin: f32) -> (u16, u16) {
    let cols = cols.max(1);
    let cw = cell_w.max(1) as f32;
    let left = (margin / cw).round().clamp(0.0, f32::from(cols - 1));
    let right = ((width as f32 - margin) / cw).round().clamp(left + 1.0, f32::from(cols));
    (left as u16, right as u16)
}

/// Maps quota health to its colored 3-layer icon variant.
fn limits_icon(health: LimitsHealth) -> f32 {
    match health {
        LimitsHealth::Ok => ICON_LIMITS_OK,
        LimitsHealth::Warn => ICON_LIMITS_WARN,
        LimitsHealth::Crit => ICON_LIMITS_CRIT,
    }
}

/// Centered top-bar vector icon (AI limits or settings gear) sized harmoniously for opaque mode.
fn opaque_title_icon(
    range: &Range<u16>,
    chrome_h: u32,
    cell_w: u32,
    scale: f64,
    lit: bool,
    icon: f32,
    tint: Option<(crate::color::Rgb, f32)>,
) -> GlassShape {
    let cw = cell_w.max(1) as f32;
    let chrome = chrome_h as f32;
    let s = scale as f32;
    let mark = f32::from(range.start + (range.end - range.start) / 2);
    let center = (mark + 0.5) * cw;
    let slot_w = f32::from(range.end - range.start) * cw;
    let side = (chrome * 0.72).min(slot_w * 0.84).clamp(16.0 * s, 26.0 * s);
    let mut shape = GlassShape::new(center - side * 0.5, (chrome - side) * 0.5, side, side, GLASS_LIMITS_ICON)
        .with_icon(icon)
        .lit(lit);
    if let Some((color, alpha)) = tint {
        shape = shape.with_tint(color, alpha);
    }
    shape
}

/// Floating AI Limits card, hero 3-layer icon, divider, status dots and smooth gradient bars.
#[allow(clippy::too_many_arguments)]
fn push_limits_shapes(
    shapes: &mut Vec<GlassShape>,
    popover: &Popover,
    origin: (f32, f32),
    cell: (f32, f32),
    scale: f64,
    theme: Theme,
    glass: bool,
    light: bool,
    tune: &crate::settings::GlassTune,
) {
    let s = scale as f32;
    let pad = (f64::from(tune.dialog_pad) * scale) as f32;
    let x = origin.0 + f32::from(popover.origin_col) * cell.0 - pad;
    let y = origin.1 + f32::from(popover.origin_row) * cell.1 - pad * 0.5;
    let w = f32::from(popover.cols) * cell.0 + 2.0 * pad;
    let h = f32::from(popover.rows) * cell.1 + pad;
    let radius = if glass { 20.0 * s } else { 12.0 * s };
    let (card_rgb, card_alpha) = if glass && !light {
        (crate::color::Rgb::new(0x10, 0x13, 0x1B), tune.dialog.max(0.86))
    } else if glass {
        (theme.panel, tune.dialog.max(0.88))
    } else {
        (theme.panel, 1.0)
    };
    shapes.push(
        GlassShape::new(x, y, w, h, GLASS_LIMITS_CARD)
            .with_radius(radius)
            .with_tint(card_rgb, card_alpha)
            .lit(glass),
    );

    // Hero 3-layer icon on the left side (or header in compact layout).
    let (ic, ir, iw, ih) = popover.hero_icon;
    let ix = origin.0 + f32::from(popover.origin_col + ic) * cell.0;
    let iy = origin.1 + f32::from(popover.origin_row + ir) * cell.1;
    let icon_w = f32::from(iw) * cell.0;
    let icon_h = f32::from(ih) * cell.1;
    let side = icon_w.min(icon_h).max(16.0 * s);
    shapes.push(
        GlassShape::new(
            ix + (icon_w - side) * 0.5,
            iy + (icon_h - side) * 0.5,
            side,
            side,
            GLASS_LIMITS_ICON,
        )
        .with_icon(limits_icon(popover.health)),
    );

    // Divider hairline.
    let line_w = (1.0 * s).max(1.0);
    if let Some((col, r0, r1)) = popover.divider_v {
        let dx = origin.0 + (f32::from(popover.origin_col + col) + 0.5) * cell.0 - line_w * 0.5;
        let dy = origin.1 + f32::from(popover.origin_row + r0) * cell.1;
        let dh = f32::from(r1.saturating_sub(r0)) * cell.1;
        if dh >= 4.0 {
            shapes.push(GlassShape::new(dx.round(), dy, line_w, dh, GLASS_DIVIDER));
        }
    }
    if let Some((row, c0, c1)) = popover.divider_h {
        let dx = origin.0 + f32::from(popover.origin_col + c0) * cell.0;
        let dy = origin.1 + (f32::from(popover.origin_row + row) + 0.5) * cell.1 - line_w * 0.5;
        let dw = f32::from(c1.saturating_sub(c0)) * cell.0;
        if dw >= 8.0 {
            shapes.push(GlassShape::new(dx, dy.round(), dw, line_w, GLASS_DIVIDER));
        }
    }

    // Status dots and smooth rounded gradient progress bars.
    let glass_flag = if glass { 1.0 } else { 0.0 };
    let dot_d = (cell.1 * 0.36).clamp(6.0 * s, 10.0 * s);
    let bar_h = (cell.1 * 0.38).clamp(6.0 * s, 10.0 * s);
    for meter in &popover.meters {
        let row_cy = origin.1 + (f32::from(popover.origin_row + meter.row) + 0.5) * cell.1;
        if let Some(dot_col) = meter.dot_col {
            let dot_cx = origin.0 + (f32::from(popover.origin_col + dot_col) + 0.5) * cell.0;
            shapes.push(
                GlassShape::new(dot_cx - dot_d * 0.5, row_cy - dot_d * 0.5, dot_d, dot_d, GLASS_DOT)
                    .with_tint(meter.color, 1.0)
                    .with_icon(glass_flag),
            );
        }
        let bx = origin.0 + f32::from(popover.origin_col + meter.bar_col) * cell.0;
        let bw = f32::from(meter.bar_cols) * cell.0;
        if let Some((sub_fill, sub_color)) = meter.sub {
            let main_h = (cell.1 * 0.28).clamp(5.0 * s, 8.0 * s);
            let sub_h = (cell.1 * 0.14).clamp(2.5 * s, 4.0 * s);
            let gap = (2.0 * s).max(1.5);
            let total_h = main_h + gap + sub_h;
            let top_y = row_cy - total_h * 0.5;
            shapes.push(
                GlassShape::new(bx, top_y, bw, main_h, GLASS_BAR)
                    .with_amount(meter.fill)
                    .with_tint(meter.color, 1.0)
                    .with_icon(glass_flag),
            );
            shapes.push(
                GlassShape::new(bx, top_y + main_h + gap, bw, sub_h, GLASS_BAR)
                    .with_amount(sub_fill)
                    .with_tint(sub_color, 1.0)
                    .with_icon(glass_flag),
            );
        } else {
            shapes.push(
                GlassShape::new(bx, row_cy - bar_h * 0.5, bw, bar_h, GLASS_BAR)
                    .with_amount(meter.fill)
                    .with_tint(meter.color, 1.0)
                    .with_icon(glass_flag),
            );
        }
    }
}

const TOOLTIP_DELAY: Duration = Duration::from_millis(180);

/// Builds the Chrome-style bottom tab bar shapes (strip + inverted concave active tab,
/// hover highlight, close circle hover, vertical dividers, and the `+` new-tab button).
#[allow(clippy::too_many_arguments)]
fn push_tab_bar_shapes(
    shapes: &mut Vec<GlassShape>,
    bar: &Bar,
    hover: Option<tabs::BarHover>,
    size: PhysicalSize<u32>,
    chrome_h: u32,
    cell_w: u32,
    scale: f64,
    theme: Theme,
    glass: bool,
    light: bool,
) {
    if bar.spans.is_empty() || chrome_h == 0 {
        return;
    }
    let cw = cell_w.max(1) as f32;
    let s = scale as f32;
    let chrome = chrome_h as f32;
    let bottom_pad = (4.0 * s).round();
    let bar_y = size.height as f32 - chrome;
    let tab_h = (chrome - bottom_pad).max(1.0);
    let (active_x0, active_x1) = bar
        .spans
        .get(bar.selected)
        .map(|span| (f32::from(span.start) * cw, f32::from(span.end) * cw))
        .unwrap_or((0.0, 0.0));
    let radius = (9.0 * s).min(tab_h * 0.36).min(active_x0.max(4.0 * s));
    let (strip_rgb, strip_alpha) = if glass {
        let alpha = if light { 0.11 } else { 0.34 };
        (crate::color::Rgb::new(0x00, 0x00, 0x00), alpha)
    } else {
        (theme.bar, 1.0)
    };
    shapes.push(
        GlassShape::new(0.0, bar_y, size.width as f32, chrome, GLASS_TAB_STRIP)
            .with_radius(radius)
            .with_icon(active_x0)
            .with_lit(active_x1)
            .with_tint(strip_rgb, strip_alpha),
    );

    let hovered_tab = match hover {
        Some(tabs::BarHover::Tab { tab, .. }) => Some(tab),
        _ => None,
    };
    if let Some(index) = hovered_tab
        && index != bar.selected
        && let Some(span) = bar.spans.get(index)
    {
        let hx0 = f32::from(span.start) * cw + 3.0 * s;
        let hx1 = f32::from(span.end) * cw - 3.0 * s;
        let hy0 = bar_y + 3.0 * s;
        let hh = (tab_h - 6.0 * s).max(1.0);
        if hx1 - hx0 >= 2.0 {
            shapes.push(
                GlassShape::new(hx0, hy0, hx1 - hx0, hh, GLASS_TAB_HOVER)
                    .with_radius((7.0 * s).min(hh * 0.5))
                    .with_lit(if light { 0.06 } else { 0.08 }),
            );
        }
    }

    if let Some(tabs::BarHover::Tab { tab, close: true }) = hover
        && let Some(span) = bar.spans.get(tab)
        && let Some(col) = span.close
    {
        let cx = (f32::from(col) + 0.5) * cw;
        let cy = bar_y + tab_h * 0.5;
        let side = (tab_h * 0.62).clamp(16.0 * s, 24.0 * s);
        shapes.push(
            GlassShape::new(cx - side * 0.5, cy - side * 0.5, side, side, GLASS_TAB_HOVER)
                .with_lit(if light { 0.11 } else { 0.14 }),
        );
    }

    let div_w = (1.0 * s).max(1.0);
    let div_h = (tab_h * 0.48).max(8.0);
    let div_y = bar_y + (tab_h - div_h) * 0.5;
    let count = bar.spans.len();
    for index in 1..count {
        let touch_active = index == bar.selected || index - 1 == bar.selected;
        let touch_hover = hovered_tab == Some(index) || hovered_tab == Some(index - 1);
        if !touch_active && !touch_hover {
            let div_x = (f32::from(bar.spans[index].start) * cw - div_w * 0.5).round();
            shapes.push(GlassShape::new(div_x, div_y, div_w, div_h, GLASS_DIVIDER));
        }
    }
    if let Some(last) = bar.spans.last()
        && bar.new_tab.is_some()
        && count - 1 != bar.selected
        && hovered_tab != Some(count - 1)
        && hover != Some(tabs::BarHover::NewTab)
    {
        let div_x = (f32::from(last.end) * cw - div_w * 0.5).round();
        shapes.push(GlassShape::new(div_x, div_y, div_w, div_h, GLASS_DIVIDER));
    }

    if let Some(range) = &bar.new_tab {
        let new_tab_hot = hover == Some(tabs::BarHover::NewTab);
        let cx = (f32::from(range.start) + f32::from(range.end)) * 0.5 * cw;
        let cy = bar_y + tab_h * 0.5;
        let side = (tab_h * 0.72).clamp(18.0 * s, 28.0 * s);
        let plus_color = if new_tab_hot { theme.fg } else { theme.tab_fg };
        shapes.push(
            GlassShape::new(cx - side * 0.5, cy - side * 0.5, side, side, GLASS_TAB_HOVER)
                .with_icon(ICON_PLUS)
                .with_lit(if new_tab_hot { if light { 0.10 } else { 0.13 } } else { 0.0 })
                .with_tint(plus_color, if new_tab_hot { 0.96 } else { 0.82 }),
        );
    }
}

/// Computes `(card_rect, text_x, text_y)` for a tab tooltip anchored just above the hovered tab.
fn tooltip_layout(
    tooltip: &tabs::TabTooltip,
    size: PhysicalSize<u32>,
    bar_h: u32,
    cell: (f32, f32),
    scale: f64,
) -> ([f32; 4], f32, f32) {
    let s = scale as f32;
    let pad_x = (10.0 * s).round();
    let pad_y = (6.0 * s).round();
    let text_w = f32::from(tooltip.cols) * cell.0;
    let text_h = f32::from(tooltip.rows) * cell.1;
    let card_w = text_w + 2.0 * pad_x;
    let card_h = text_h + 2.0 * pad_y;
    let margin = (8.0 * s).round();
    let ideal_x = f32::from(tooltip.anchor_col) * cell.0;
    let max_x = (size.width as f32 - margin - card_w).max(margin);
    let card_x = ideal_x.clamp(margin, max_x).round();
    let bar_y = size.height as f32 - bar_h as f32;
    let card_y = (bar_y - 8.0 * s - card_h).max(margin).round();
    ([card_x, card_y, card_w, card_h], card_x + pad_x, card_y + pad_y)
}

/// Round glass buttons behind the AI limits icon, the settings gear, and each window control.
#[allow(clippy::too_many_arguments)]
fn push_glass_title_shapes(
    shapes: &mut Vec<GlassShape>,
    title: &TitleBar,
    chrome_h: u32,
    cell_w: u32,
    scale: f64,
    settings_lit: bool,
    limits_lit: bool,
    limits_health: LimitsHealth,
    maximized: bool,
    tune: &crate::settings::GlassTune,
) {
    let cw = cell_w as f32;
    let scale = scale as f32;
    let chrome = chrome_h as f32;
    let inset = (tune.inset * scale).round();
    let cap_h = (chrome - 2.0 * inset).max(1.0);
    // The addon icon, the gear and the window buttons share one size and one rule for their center.
    let maximize = if maximized { ICON_RESTORE } else { ICON_MAXIMIZE };
    let buttons = [
        (&title.limits, Some(limits_icon(limits_health))),
        (&title.settings, None),
        (&title.minimize, Some(ICON_MINIMIZE)),
        (&title.maximize, Some(maximize)),
        (&title.close, Some(ICON_CLOSE)),
    ];
    for (index, (range, icon)) in buttons.into_iter().enumerate() {
        if range.is_empty() {
            continue;
        }
        // The mark sits in the middle cell, as titlebar::paint_button places it.
        let mark = f32::from(range.start + (range.end - range.start) / 2);
        let center = (mark + 0.5) * cw;
        let side = (cap_h - tune.button_inset * scale).min(f32::from(range.end - range.start) * cw).max(1.0);
        let button = GlassShape::new(center - side * 0.5, (chrome - side) * 0.5, side, side, GLASS_BUTTON);
        shapes.push(match (index, icon) {
            (0, Some(icon)) => {
                let b = button.lit(limits_lit).with_icon(icon);
                match limits_health {
                    LimitsHealth::Crit => b.with_tint(crate::color::Rgb::new(0xF8, 0x5A, 0x62), 0.16),
                    LimitsHealth::Warn => b.with_tint(crate::color::Rgb::new(0xFB, 0x92, 0x3C), 0.12),
                    LimitsHealth::Ok => b,
                }
            }
            (_, Some(icon)) => button.with_icon(icon),
            (_, None) => button.lit(settings_lit).with_gear(),
        });
    }
}

/// A glass sheet behind a block of grid cells `(col, row, cols, rows)`, grown by `pad` pixels.
fn sheet(
    rect: (u16, u16, u16, u16),
    origin: (f32, f32),
    cell: (f32, f32),
    pad: f32,
    scale: f64,
    theme: Theme,
    alpha: f32,
    radius: f32,
) -> GlassShape {
    let x = origin.0 + f32::from(rect.0) * cell.0 - pad;
    let y = origin.1 + f32::from(rect.1) * cell.1 - pad;
    let w = f32::from(rect.2) * cell.0 + 2.0 * pad;
    let h = f32::from(rect.3) * cell.1 + 2.0 * pad;
    GlassShape::new(x, y, w, h, GLASS_SHEET)
        .with_radius((f64::from(radius) * scale) as f32)
        .with_tint(theme.panel, alpha)
}

fn bar_contains_y(height: u32, bar_h: u32, y: f64) -> bool {
    y >= f64::from(height.saturating_sub(bar_h)) && y < f64::from(height)
}

fn label_paste(event: &winit::event::KeyEvent, mods: ModifiersState) -> bool {
    if matches!(event.logical_key, Key::Named(NamedKey::Insert)) && mods.shift_key() {
        return true;
    }
    if !mods.control_key() {
        return false;
    }
    matches!(event.key_without_modifiers().as_ref(), Key::Character(value) if value.eq_ignore_ascii_case("v"))
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
    /// `top` is the title bar, `bottom` the tab bar, zero while it is hidden.
    fn new(size: PhysicalSize<u32>, top: u32, bottom: u32, scale: f64, maximized: bool) -> Self {
        let border = if maximized { 0 } else { resize_margin(scale).ceil() as u32 };
        Self {
            right: size.width.saturating_sub(border),
            width: (4.0 * scale).round().max(1.0) as u32,
            top,
            height: size.height.saturating_sub(top.saturating_add(bottom)),
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

/// Corner radius of an Adwaita window, 15 logical pixels in libadwaita 1.10. Square when maximized
/// or fullscreen, as GNOME draws its own windows.
fn corner_radius(window: &Window) -> f32 {
    if window.is_maximized() || window.fullscreen().is_some() {
        0.0
    } else {
        (15.0 * window.scale_factor()) as f32
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
                if self.tab_drag.is_some() {
                    self.drag_tab(position.x);
                }
                let title_col = strip_col(position.x, self.face.cell_w(), self.title_bar.cells.len());
                let on_title = self.is_title_y(position.y);
                let hot = on_title && self.title_bar.settings.contains(&title_col);
                let lim_hot = on_title && self.title_bar.limits.contains(&title_col);
                if hot != self.settings_hot || lim_hot != self.limits_hot {
                    self.settings_hot = hot;
                    self.limits_hot = lim_hot;
                    self.bar_dirty = true;
                    self.redraw();
                }
                let bar_hover = if self.tabs.len() > 1
                    && !self.ui.open
                    && !self.limits_open
                    && !self.confirm_close
                    && self.tab_drag.is_none()
                    && self.is_bar_y(position.y)
                {
                    let col = strip_col(position.x, self.face.cell_w(), self.bar.cells.len());
                    tabs::hover(&self.bar, col)
                } else {
                    None
                };
                if bar_hover != self.bar_hover {
                    let prev_tab = match self.bar_hover {
                        Some(tabs::BarHover::Tab { tab, .. }) => Some(tab),
                        _ => None,
                    };
                    let next_tab = match bar_hover {
                        Some(tabs::BarHover::Tab { tab, .. }) => Some(tab),
                        _ => None,
                    };
                    if next_tab != prev_tab {
                        if next_tab.is_some() {
                            if !self.tooltip_shown {
                                self.tooltip_at = Some(Instant::now() + TOOLTIP_DELAY);
                            }
                        } else {
                            self.tooltip_shown = false;
                            self.tooltip_at = None;
                        }
                    }
                    self.bar_hover = bar_hover;
                    self.bar_dirty = true;
                    self.redraw();
                }
                let cell = if self.ui.open
                    || self.limits_open
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
                    // Past the top or bottom of the grid the view follows the selection.
                    // The first step waits for the tick so a quick pass over the edge does not jump.
                    if self.autoscroll_lines() == 0 {
                        self.autoscroll_at = None;
                    } else if self.autoscroll_at.is_none() {
                        self.autoscroll_at = Some(Instant::now() + AUTOSCROLL_TICK);
                    }
                }
                let on_label = self.tab_drag.is_none() && self.hovering_label_edit(position.x, position.y);
                let on_bar_btn = self.tab_drag.is_none()
                    && matches!(
                        bar_hover,
                        Some(tabs::BarHover::Tab { close: true, .. } | tabs::BarHover::NewTab)
                    );
                let on_popover_hit = self.limits_open
                    && self
                        .cell_at(position.x, position.y)
                        .and_then(|(col, row)| self.limits_popover.as_ref().and_then(|p| p.hit(col, row)))
                        .is_some();
                let on_top_btn = self.limits_hot || self.settings_hot;
                let bar_cursor = on_label
                    || on_bar_btn
                    || on_popover_hit
                    || on_top_btn
                    || (cell.is_none() && matches!(self.hover_icon, CursorIcon::Text | CursorIcon::Pointer));
                if self.tab_drag.is_none() && (cell != self.link_cell || bar_cursor) {
                    self.link_cell = cell;
                    let icon = if on_label {
                        CursorIcon::Text
                    } else if on_bar_btn || on_popover_hit || on_top_btn {
                        CursorIcon::Pointer
                    } else if cell.is_some() && self.link_at(position.x, position.y).is_some() {
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
            WindowEvent::CursorLeft { .. } => {
                let cleared_bar = self.bar_hover.take().is_some();
                let cleared_tip = std::mem::replace(&mut self.tooltip_shown, false);
                self.tooltip_at = None;
                let cleared_settings = std::mem::replace(&mut self.settings_hot, false);
                let cleared_limits = std::mem::replace(&mut self.limits_hot, false);
                if cleared_bar || cleared_tip || cleared_settings || cleared_limits {
                    self.bar_dirty = true;
                    self.redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                match (button, state) {
                    (MouseButton::Left, ElementState::Released) => self.end_pointer(),
                    (MouseButton::Left, ElementState::Pressed) => self.begin_pointer(),
                    (MouseButton::Middle | MouseButton::Right, ElementState::Pressed)
                        if self.title_click(button) => {}
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

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // The blur objects ride on winit's wl_display, which closes when the loop ends.
        self.blur = None;
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Wake) {
        match event {
            Wake::Frame(id) => {
                if let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) {
                    tab.session.frame_pending.store(false, Ordering::Release);
                }
                let active = self.tabs.get(self.active).is_some_and(|tab| tab.id == id);
                if active {
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
            Wake::Limits(snap) => {
                self.limits_loading = false;
                let health_changed = snap.health() != self.limits_snap.health();
                self.limits_snap = snap;
                self.limits_fingerprint = limits::sources_fingerprint();
                if self.limits_open {
                    self.limits_dirty = true;
                }
                if health_changed || self.limits_open {
                    self.bar_dirty = true;
                    self.redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_agent_scan {
            self.refresh_agents();
            self.refresh_glass_tune();
            if self.ui.limits && now >= self.next_limits_probe {
                self.next_limits_probe = now + Duration::from_secs(30);
                let fp = limits::sources_fingerprint();
                if fp != self.limits_fingerprint {
                    self.limits_fingerprint = fp;
                    self.refresh_limits(false);
                }
            }
            self.next_agent_scan = now + Duration::from_secs(1);
        }
        let mut until = self.next_agent_scan;
        if let Some(at) = self.autoscroll_at {
            let at = if now >= at {
                if self.autoscroll_step() { Some(now + AUTOSCROLL_TICK) } else { None }
            } else {
                Some(at)
            };
            // At the end of the history the step waits for the pointer to move again.
            self.autoscroll_at = at;
            if let Some(at) = at {
                until = until.min(at);
            }
        }
        if self.tab_edit.is_some() {
            match self.label_blink_at {
                Some(at) if now >= at => {
                    self.label_blink_on = !self.label_blink_on;
                    self.label_blink_at = Some(now + LABEL_BLINK);
                    self.bar_dirty = true;
                    self.redraw();
                    until = until.min(now + LABEL_BLINK);
                }
                Some(at) => until = until.min(at),
                None => {
                    self.label_blink_at = Some(now + LABEL_BLINK);
                    until = until.min(now + LABEL_BLINK);
                }
            }
        }
        if let Some(at) = self.tooltip_at {
            if now >= at {
                self.tooltip_at = None;
                self.tooltip_shown = true;
                self.bar_dirty = true;
                self.redraw();
            } else {
                until = until.min(at);
            }
        }
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
        blur: None,
        blur_tried: false,
        presented_tab: None,
        face,
        atlas,
        mods: ModifiersState::empty(),
        font_size,
        vsync,
        grid_left: 0,
        grid_pad_top: 0,
        grid_cols: 80,
        grid_rows: 24,
        bar_dirty: true,
        bar: Bar { cells: Vec::new(), spans: Vec::new(), new_tab: None, selected: 0 },
        title_bar: TitleBar {
            cells: Vec::new(),
            minimize: 0..0,
            maximize: 0..0,
            close: 0..0,
            settings: 0..0,
            limits: 0..0,
        },
        last_title_click: None,
        settings_hot: false,
        limits_hot: false,
        limits_open: false,
        limits_dirty: false,
        limits_loading: false,
        limits_selected: None,
        limits_snap: limits::Snapshot::default(),
        limits_popover: None,
        limits_fingerprint: limits::Fingerprint::default(),
        next_limits_probe: Instant::now() + Duration::from_secs(30),
        last_limits_cmd: None,
        last_screen_capture: None,
        bar_hover: None,
        tooltip_shown: false,
        tooltip_at: None,
        tooltip_uploaded: false,
        link_cell: None,
        hover_icon: CursorIcon::Default,
        selection: None,
        selecting: false,
        view_top: 0,
        autoscroll_at: None,
        selection_text: None,
        selection_stale: false,
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
        panel_dirty: false,
        panel: None,
        confirm_close: false,
        confirm_dirty: false,
        close_dialog: None,
        exit_confirmed: false,
        next_agent_scan: Instant::now(),
        ui,
        glass_stamp: settings::config_stamp(),
        glass_page: false,
        glass_scroll: 0,
        keys_page: false,
        keys_scroll: 0,
        desktop: TitlebarPrefs::load(),
        cursor: None,
        wheel_frac: 0.0,
        scroll_drag: false,
        tab_drag: None,
        tab_edit: None,
        last_tab_click: None,
        label_blink_on: true,
        label_blink_at: None,
    };
    app.open_tab().context("opening the pty")?;
    if app.ui.limits {
        app.refresh_limits(false);
    }
    event_loop.run_app(&mut app).context("window loop")?;
    std::thread::sleep(Duration::from_millis(20));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Mutex, PhysicalSize, ResizeDirection, Scrollbar, Term, bar_contains_y, bar_height,
        existing_file, glass_bar_height, publish_frame, push_glass_title_shapes,
        push_tab_bar_shapes, resize_edge, strip_col, tooltip_layout,
    };
    use crate::desktop::ButtonLayout;
    use crate::gpu::{
        GLASS_BUTTON, GLASS_DIVIDER, GLASS_SHEET, GLASS_TAB_HOVER, GLASS_TAB_STRIP, ICON_PLUS,
    };

    #[test]
    fn glass_bars_are_taller_than_the_text() {
        let tune = crate::settings::GlassTune::default();
        assert_eq!(glass_bar_height(1.0, 19, &tune), 44);
        assert_eq!(glass_bar_height(1.0, 30, &tune), 50);
        assert!(glass_bar_height(2.0, 38, &tune) >= bar_height(2.0, 38) + 16);
    }

    #[test]
    fn chrome_tab_bar_draws_strip_dividers_and_new_tab_button() {
        let theme = crate::color::theme(0);
        let titles = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let bar = crate::tabs::render_bar(&titles, &[], 1, 100, theme, true);
        let title = crate::titlebar::render("shell", None, 100, theme, false, &ButtonLayout::parse("appmenu:close"), true, false, true);
        let size = PhysicalSize::new(1000, 600);
        let mut shapes = Vec::new();
        push_tab_bar_shapes(&mut shapes, &bar, None, size, 40, 10, 1.0, theme, true, false);
        push_glass_title_shapes(
            &mut shapes,
            &title,
            40,
            10,
            1.0,
            true,
            false,
            crate::limits::Health::Ok,
            false,
            &crate::settings::GlassTune::default(),
        );
        // Active tab is middle (1): strip + divider after last inactive tab (before '+') + '+' button + 2 title buttons.
        let kinds: Vec<u32> = shapes.iter().map(|shape| shape.kind).collect();
        assert_eq!(kinds, vec![GLASS_TAB_STRIP, GLASS_DIVIDER, GLASS_TAB_HOVER, GLASS_BUTTON, GLASS_BUTTON]);
        let [strip, divider, plus, settings, close] = [shapes[0], shapes[1], shapes[2], shapes[3], shapes[4]];
        assert_eq!(strip.rect, [0.0, 560.0, 1000.0, 40.0]);
        assert_eq!(strip.icon, f32::from(bar.spans[1].start) * 10.0);
        assert_eq!(strip.lit, f32::from(bar.spans[1].end) * 10.0);
        assert!((divider.rect[0] - (f32::from(bar.spans[2].end) * 10.0 - 0.5)).abs() < 1.0);
        assert_eq!(plus.icon, ICON_PLUS);
        assert!(settings.lit > 0.0 && settings.icon == crate::gpu::ICON_GEAR);
        assert_eq!(close.icon, crate::gpu::ICON_CLOSE);
        assert!(title.cells.iter().all(|cell| cell.cp != u32::from('\u{00D7}')));
        assert_eq!(settings.rect[1..], close.rect[1..]);
        let step = f32::from(title.close.end - title.close.start) * 10.0;
        assert!((close.rect[0] - settings.rect[0] - step).abs() < 0.01);
        assert!(close.rect[1] < 40.0 && close.rect[2] == close.rect[3]);
    }

    #[test]
    fn chrome_dividers_vanish_next_to_active_and_hovered_tabs() {
        let theme = crate::color::theme(0);
        let titles = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let size = PhysicalSize::new(1000, 600);
        // Active = 0 -> divider between 1 and 2, plus divider between 2 and '+' (2 total).
        let bar = crate::tabs::render_bar(&titles, &[], 0, 100, theme, false);
        let mut shapes = Vec::new();
        push_tab_bar_shapes(&mut shapes, &bar, None, size, 36, 10, 1.0, theme, false, false);
        let dividers: Vec<_> = shapes.iter().filter(|shape| shape.kind == GLASS_DIVIDER).collect();
        assert_eq!(dividers.len(), 2);

        // Active = 2 (last tab) -> divider only between 0 and 1 (no divider before '+').
        let bar_last = crate::tabs::render_bar(&titles, &[], 2, 100, theme, false);
        let mut shapes_last = Vec::new();
        push_tab_bar_shapes(&mut shapes_last, &bar_last, None, size, 36, 10, 1.0, theme, false, false);
        let dividers_last: Vec<_> = shapes_last.iter().filter(|shape| shape.kind == GLASS_DIVIDER).collect();
        assert_eq!(dividers_last.len(), 1);

        // Tooltip stays inside the window and sits above the bottom bar.
        let tip = crate::tabs::render_tooltip("Full tab title for hover card", 60, 1, theme).expect("tip");
        let (rect, tx, ty) = tooltip_layout(&tip, size, 36, (10.0, 20.0), 1.0);
        assert!(rect[0] >= 8.0);
        assert!(rect[1] + rect[3] < 600.0 - 36.0);
        assert!(tx > rect[0] && ty > rect[1]);
    }

    #[test]
    fn glass_dialog_cells_turn_see_through_over_a_sheet_with_a_red_close_button() {
        let theme = crate::color::theme(0);
        let mut dialog = crate::confirm::render(80, 24, theme, 3).expect("dialog");
        dialog.set_background(theme.bg.pack());
        assert!(dialog.cells.iter().all(|cell| cell.bg == theme.bg.pack()));
        let buttons: Vec<_> = dialog.buttons().collect();
        assert_eq!(buttons.len(), 2);
        assert!(buttons.iter().any(|(_, _, choice)| *choice == crate::confirm::Choice::Close));
        let rect = (dialog.origin_col, dialog.origin_row, dialog.cols, dialog.rows);
        let shape = super::sheet(rect, (12.0, 50.0), (10.0, 20.0), 8.0, 1.0, theme, 0.94, 14.0);
        assert_eq!(shape.kind, GLASS_SHEET);
        assert_eq!(shape.rect[0], 12.0 + f32::from(dialog.origin_col) * 10.0 - 8.0);
        assert_eq!(shape.rect[2], f32::from(dialog.cols) * 10.0 + 16.0);
        assert_eq!(shape.radius, 14.0);
        assert!((shape.tint[3] - 0.94).abs() < 1e-6);
    }

    #[test]
    fn synchronized_pet_replacement_keeps_the_complete_frame_visible() {
        let frames = Mutex::new(None);
        let mut term = Term::new(20, 8);
        term.advance(
            "\x1b[4;1H\x1b[48;2;45;47;51m\x1b[1m›\x1b[22m           \x1b[0m\x1b[4;3H".as_bytes(),
        );
        term.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=49374,c=4,r=5,q=2,C=1;/wAA/w==\x1b\\");
        assert!(publish_frame(&mut term, &frames));

        term.advance(
            b"\x1b[?2026h\x1b7\x1b[2;9H\x1b]2;New subject\x07\x1b_Ga=d,d=I,i=49374,q=2;\x1b\\",
        );
        assert!(publish_frame(&mut term, &frames));
        let previous = frames.lock().unwrap().take().unwrap();
        assert!(!previous.suspended);
        assert_eq!(previous.title, "New subject");
        assert_eq!(previous.cursor, Some((2, 3)));
        assert_eq!(previous.composer, Some([0, 3, 12, 4]));
        assert_eq!(previous.images[0].image.rgba, [255, 0, 0, 255]);

        term.advance(b"\x1b_Ga=T,f=32,s=1,v=1,i=49374,c=4,r=5,q=2,C=1;AAD//w==\x1b\\");
        assert!(!publish_frame(&mut term, &frames));
        term.advance(b"\x1b8\x1b[?2026l");
        assert!(publish_frame(&mut term, &frames));
        let current = frames.lock().unwrap().take().unwrap();
        assert_eq!(current.cursor, previous.cursor);
        assert_eq!(current.composer, previous.composer);
        assert!(!current.full && !current.content_changed && current.rows_dirty.is_empty());
        assert_eq!(current.images.len(), 1);
        assert_eq!(current.images[0].image.rgba, [0, 0, 255, 255]);
    }

    #[test]
    fn coalesced_updates_keep_pending_rows_and_the_latest_cursor() {
        let frames = Mutex::new(None);
        let mut term = Term::new(20, 8);
        term.snapshot();

        term.advance(b"\x1b[2;1HA\x1b]2;First update\x07");
        assert!(publish_frame(&mut term, &frames));
        term.advance(b"\x1b[4;1HB\x1b[?25l");
        assert!(publish_frame(&mut term, &frames));

        let frame = frames.lock().unwrap().take().unwrap();
        assert_eq!(frame.title, "First update");
        assert_eq!(frame.cursor, None);
        assert!(!frame.full);
        let rows: Vec<_> = frame.rows_dirty.iter().map(|(row, cells)| (*row, cells[0].cp)).collect();
        assert_eq!(rows, [(1, u32::from('A')), (3, u32::from('B'))]);
    }

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
        let scroll = Scrollbar::new(PhysicalSize::new(2560, 1408), 32, 32, 1.0, true);
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
        let scroll = Scrollbar::new(size, 64, 64, 2.0, false);
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
