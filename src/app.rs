use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Context;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::font::{Atlas, Face};
use crate::gpu::Renderer;
use crate::input::{self, Input};
use crate::npu::Scheduler;
use crate::pty::Pty;
use crate::term::Term;

#[derive(Debug)]
enum Wake {
    Frame,
    Exit,
}

pub struct Session {
    pty: Arc<Pty>,
    term: Arc<Mutex<Term>>,
    npu: Scheduler,
    reader: Option<JoinHandle<()>>,
}

impl Session {
    fn spawn(cols: u16, rows: u16, proxy: EventLoopProxy<Wake>) -> std::io::Result<Self> {
        let pty = Arc::new(Pty::spawn(cols, rows)?);
        let term = Arc::new(Mutex::new(Term::new(cols, rows)));
        let npu = Scheduler::start();
        let reader_fd = pty.reader_fd()?;
        let term_reader = Arc::clone(&term);
        let pty_writer = Arc::clone(&pty);
        let indexer = npu.indexer();
        tracing::debug!(
            npu = npu.status.present.load(std::sync::atomic::Ordering::Relaxed),
            "scheduler da NPU criado"
        );
        let reader = std::thread::Builder::new()
            .name("rustty-pty".into())
            .spawn(move || pty_loop(reader_fd, term_reader, pty_writer, indexer, proxy))
            .map_err(std::io::Error::other)?;
        Ok(Self {
            pty,
            term,
            npu,
            reader: Some(reader),
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        tracing::debug!(
            indexed = self.npu.status.indexed.load(std::sync::atomic::Ordering::Relaxed),
            "encerrando índice da NPU"
        );
        self.pty.hangup();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn pty_loop(
    fd: std::os::fd::OwnedFd,
    term: Arc<Mutex<Term>>,
    pty: Arc<Pty>,
    indexer: crate::npu::Indexer,
    proxy: EventLoopProxy<Wake>,
) {
    let mut buf = [0u8; 64 * 1024];
    loop {
        let mut fds = [PollFd::new(fd.as_fd(), PollFlags::POLLIN | PollFlags::POLLHUP)];
        match nix::poll::poll(&mut fds, nix::poll::PollTimeout::NONE) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(err) => {
                tracing::debug!(%err, "poll do pty encerrou");
                let _ = proxy.send_event(Wake::Exit);
                break;
            }
        }
        match nix::unistd::read(&fd, &mut buf) {
            Ok(0) => {
                let _ = proxy.send_event(Wake::Exit);
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
                let _ = proxy.send_event(Wake::Frame);
            }
            Err(Errno::EAGAIN) | Err(Errno::EINTR) => continue,
            Err(err) => {
                tracing::debug!(%err, "leitura do pty encerrou");
                let _ = proxy.send_event(Wake::Exit);
                break;
            }
        }
    }
}

struct App {
    session: Option<Session>,
    window: Option<Arc<Window>>,
    gpu: Option<Renderer>,
    face: Face,
    atlas: Atlas,
    mods: ModifiersState,
    font_size: f32,
    vsync: bool,
}

impl App {
    fn open_window(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Rustty")
                        .with_inner_size(LogicalSize::new(1100.0, 700.0)),
                )
                .context("criando janela")?,
        );
        self.rescale(window.scale_factor());
        let gpu = Renderer::new(Arc::clone(&window), self.vsync)?;
        self.window = Some(window);
        self.gpu = Some(gpu);
        self.fit();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        Ok(())
    }

    fn rescale(&mut self, scale: f64) {
        let px = (self.font_size * scale as f32).clamp(8.0, 64.0);
        if (px - self.face.px()).abs() < 0.1 && self.atlas.cell_w() == self.face.cell_w() {
            return;
        }
        self.face.set_px(px);
        self.atlas.sync_cell(self.face.cell_w(), self.face.cell_h());
        if let Some(session) = &self.session {
            session.term.lock().unwrap_or_else(|err| err.into_inner()).damage_all();
        }
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
        let cols = (size.width / self.face.cell_w().max(1)).max(2) as u16;
        let rows = (size.height / self.face.cell_h().max(1)).max(1) as u16;
        let Some(session) = &self.session else {
            return;
        };
        let mut term = session.term.lock().unwrap_or_else(|err| err.into_inner());
        if cols != term.cols() || rows != term.rows() {
            term.resize(cols, rows);
            drop(term);
            let _ = session.pty.resize(cols, rows);
        }
    }

    fn draw(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let snap = {
            let mut term = session.term.lock().unwrap_or_else(|err| err.into_inner());
            term.snapshot()
        };
        if snap.title_changed {
            if let Some(window) = &self.window {
                window.set_title(&snap.title);
            }
        }
        if snap.suspended || !snap.changed {
            return;
        }
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        gpu.upload(&mut self.atlas, &self.face, &snap);
        if let Err(err) = gpu.render() {
            tracing::warn!(%err, "frame");
        }
    }

    fn on_key(&mut self, event: &winit::event::KeyEvent) {
        let Some(session) = &self.session else {
            return;
        };
        if event.state == ElementState::Released {
            return;
        }
        let (app_cursor, rows) = {
            let term = session.term.lock().unwrap_or_else(|err| err.into_inner());
            (term.app_cursor(), term.rows())
        };
        match input::encode(event, self.mods, app_cursor, rows) {
            Some(Input::Bytes(bytes)) => {
                let _ = session.pty.write_all(&bytes);
            }
            Some(Input::Scroll(delta)) => {
                session
                    .term
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .scroll_view(delta);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            None => {}
        }
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
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(size) => {
                self.fit_size(size);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.rescale(scale_factor);
                self.fit();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.mods = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => self.on_key(&event),
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Wake) {
        match event {
            Wake::Frame => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            Wake::Exit => event_loop.exit(),
        }
    }
}

pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let font_size = std::env::var("RUSTTY_FONT_SIZE")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(15.0);
    let vsync = std::env::var("RUSTTY_VSYNC").map(|value| value != "0").unwrap_or(true);
    let face = Face::load(font_size).context("carregando fonte monoespaçada")?;
    let atlas = Atlas::new(face.cell_w(), face.cell_h());

    let event_loop = EventLoop::<Wake>::with_user_event().build().context("event loop")?;
    let proxy = event_loop.create_proxy();
    let session = Session::spawn(80, 24, proxy).context("abrindo pty")?;
    let mut app = App {
        session: Some(session),
        window: None,
        gpu: None,
        face,
        atlas,
        mods: ModifiersState::empty(),
        font_size,
        vsync,
    };
    event_loop.run_app(&mut app).context("loop da janela")?;
    // Dá um instante para o filho receber o SIGHUP antes do processo sair.
    std::thread::sleep(Duration::from_millis(20));
    Ok(())
}
